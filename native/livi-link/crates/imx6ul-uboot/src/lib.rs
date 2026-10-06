//! The vendor U-Boot keeps the first three erase blocks of the kernel encrypted with a key only the
//! SoC holds. A new kernel goes in as three staging blocks encrypted with the UPGkey from four
//! fuses, which U-Boot's `heweiencrypt` script converts once the environment block fails its check.
//!
//! The update block also holds the tail of U-Boot and its signature, so it is only ever read.

use std::io::Read;

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use flate2::read::GzDecoder;

/// Flash offsets and sizes.
pub const UPDATE_BLOCK: usize = 0x20000;
pub const ENV_BLOCK: usize = 0x30000;
pub const KERNEL: usize = 0x40000;
pub const ERASE: usize = 0x10000;
/// What U-Boot keeps in device form.
pub const HEAD: usize = 3 * ERASE;

const ENV_SIZE: usize = 0x10000;
const ENV_VARS_END: usize = 0x800;
const ZIMAGE_MAGIC: u32 = 0x016f_2818;
const LOAD: u32 = 0x8080_0000;

#[derive(Clone, Copy, Debug)]
pub struct Fuses {
    pub cfg0: u32,
    pub cfg1: u32,
    pub mac0: u32,
    pub mac1: u32,
}

impl Fuses {
    /// From the OCOTP as a mainline kernel exposes it (nvmem, one little-endian word per shadow
    /// register): CFG0 and CFG1 at words 1 and 2, MAC0 and MAC1 at words 0x22 and 0x23.
    pub fn from_ocotp(nvmem: &[u8]) -> Option<Self> {
        let word =
            |i: usize| Some(u32::from_le_bytes(nvmem.get(i * 4..i * 4 + 4)?.try_into().ok()?));
        Some(Self { cfg0: word(1)?, cfg1: word(2)?, mac0: word(0x22)?, mac1: word(0x23)? })
    }
}

pub struct UpgKey {
    key: [u8; 16],
    iv: [u8; 16],
}

impl UpgKey {
    /// Derived the way the vendor U-Boot does it.
    pub fn new(f: Fuses) -> Self {
        let text = format!("{:08x}{:08x}{:08x}{:08x}", f.cfg0, f.cfg1, f.mac0, f.mac1);
        let (key, iv) = text.as_bytes().split_at(16);
        Self { key: key.try_into().unwrap(), iv: iv.try_into().unwrap() }
    }

    /// Each call starts again from the IV, like one U-Boot `mm` call.
    pub fn encrypt(&self, data: &mut [u8]) {
        let (chunks, rest) = data.as_chunks_mut::<16>();
        assert!(rest.is_empty());
        let aes = Aes128::new(&self.key.into());
        let mut prev = self.iv;
        for chunk in chunks {
            chunk.iter_mut().zip(prev).for_each(|(b, p)| *b ^= p);
            let mut block = (*chunk).into();
            aes.encrypt_block(&mut block);
            *chunk = block.into();
            prev = *chunk;
        }
    }

    pub fn decrypt(&self, data: &mut [u8]) {
        let (chunks, rest) = data.as_chunks_mut::<16>();
        assert!(rest.is_empty());
        let aes = Aes128::new(&self.key.into());
        let mut prev = self.iv;
        for chunk in chunks {
            let cipher = *chunk;
            let mut block = cipher.into();
            aes.decrypt_block(&mut block);
            let plain: [u8; 16] = block.into();
            *chunk = std::array::from_fn(|i| plain[i] ^ prev[i]);
            prev = cipher;
        }
    }
}

/// The update environment: 0x10000 bytes, CRC32 of the variables up front, DTB at 0x800. It sits at
/// the end of the update block, and the last word of the block holds its length.
pub fn read_env(block: &[u8], key: &UpgKey) -> Result<Vec<u8>, String> {
    if block.len() != ERASE {
        return Err(format!("update block is {} bytes, not {ERASE}", block.len()));
    }
    let len = u32::from_le_bytes(block[ERASE - 4..].try_into().unwrap()) as usize;
    if len == 0 || len > ERASE || !len.is_multiple_of(16) {
        return Err(format!("update block holds no environment (length word {len:#x})"));
    }
    let mut payload = block[ERASE - len..].to_vec();
    key.decrypt(&mut payload);

    let mut env = Vec::with_capacity(ENV_SIZE);
    GzDecoder::new(&payload[..])
        .take(ENV_SIZE as u64 + 1)
        .read_to_end(&mut env)
        .map_err(|e| format!("update environment does not unpack with this dongle's key: {e}"))?;
    if env.len() != ENV_SIZE {
        return Err(format!("update environment is {:#x} bytes, not {ENV_SIZE:#x}", env.len()));
    }
    let mut crc = flate2::Crc::new();
    crc.update(&env[4..ENV_VARS_END]);
    if crc.sum() != u32::from_le_bytes(env[..4].try_into().unwrap()) {
        return Err("update environment fails its CRC".into());
    }
    Ok(env)
}

pub fn env_var<'a>(env: &'a [u8], name: &str) -> Option<&'a str> {
    env[4..ENV_VARS_END.min(env.len())]
        .split(|&b| b == 0)
        .take_while(|v| !v.is_empty())
        .filter_map(|v| std::str::from_utf8(v).ok())
        .find_map(|v| v.strip_prefix(name)?.strip_prefix('='))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    /// How many bytes `norboot` reads from the kernel partition, the zImage has to fit in there.
    pub kernel_len: usize,
    /// Flash offset of the first block `heweiencrypt` converts.
    pub staging: usize,
}

/// The layout differs between vendor U-Boot builds, so it comes from the environment.
pub fn layout(env: &[u8], kernel_part: usize) -> Result<Layout, String> {
    let norboot = commands(env_var(env, "norboot").ok_or("no norboot in the update environment")?);
    let kernel_len = norboot
        .iter()
        .find_map(|c| match c.as_slice() {
            ["sf", "read", load, "0x40000", len] if hex(load) == Some(LOAD) => hex(len),
            _ => None,
        })
        .ok_or("norboot does not read the kernel from 0x40000")? as usize;
    let head: Vec<u32> = norboot
        .iter()
        .filter_map(|c| match c.as_slice() {
            ["mm", "w", at, "0x10000"] => hex(at),
            _ => None,
        })
        .collect();
    if head != [LOAD, LOAD + 0x10000, LOAD + 0x20000] {
        return Err("norboot does not decrypt exactly the first three blocks".into());
    }

    let script =
        commands(env_var(env, "heweiencrypt").ok_or("no heweiencrypt in the update environment")?);
    let mut from = None;
    let mut pairs = Vec::new();
    for c in &script {
        match c.as_slice() {
            ["sf", "read", load, src, "0x10000"] if hex(load) == Some(LOAD) => from = hex(src),
            ["sf", "update", load, dst, "0x10000"] if hex(load) == Some(LOAD) => {
                if let (Some(src), Some(dst)) = (from.take(), hex(dst)) {
                    pairs.push((src as usize, dst as usize));
                }
            }
            _ => {}
        }
    }
    let staging = pairs.first().map(|p| p.0).ok_or("heweiencrypt converts no kernel blocks")?;
    let expected: Vec<(usize, usize)> =
        (0..3).map(|i| (staging + i * ERASE, KERNEL + i * ERASE)).collect();
    if pairs.iter().filter(|p| p.1 != ENV_BLOCK).cloned().collect::<Vec<_>>() != expected {
        return Err("heweiencrypt does not convert three consecutive staging blocks".into());
    }

    if kernel_len <= HEAD || staging < KERNEL + kernel_len || staging + HEAD > KERNEL + kernel_part
    {
        return Err(format!(
            "kernel {kernel_len:#x} and staging at {staging:#x} do not fit the kernel partition"
        ));
    }
    Ok(Layout { kernel_len, staging })
}

/// The head stays as on the device until U-Boot replaces it from the staging blocks, the rest of
/// the zImage lies there in the clear.
pub fn kernel_partition(
    current: &[u8],
    zimage: &[u8],
    layout: &Layout,
    key: &UpgKey,
) -> Result<Vec<u8>, String> {
    if zimage.len() <= HEAD || zimage[0x24..0x28] != ZIMAGE_MAGIC.to_le_bytes() {
        return Err("not an ARM zImage".into());
    }
    if zimage.len() > layout.kernel_len {
        return Err(format!(
            "zImage is {} bytes, this dongle's U-Boot reads {}",
            zimage.len(),
            layout.kernel_len
        ));
    }
    let staging = layout.staging - KERNEL;
    if staging + HEAD > current.len() {
        return Err("staging blocks lie outside the kernel partition".into());
    }

    let mut part = vec![0xff; current.len()];
    part[..HEAD].copy_from_slice(&current[..HEAD]);
    part[HEAD..zimage.len()].copy_from_slice(&zimage[HEAD..]);
    let head = &mut part[staging..staging + HEAD];
    head.copy_from_slice(&zimage[..HEAD]);
    for block in head.as_chunks_mut::<ERASE>().0 {
        key.encrypt(block);
    }
    Ok(part)
}

pub fn stage(zimage: &[u8], kernel: &[u8], uboot: &[u8], fuses: Fuses) -> Result<Vec<u8>, String> {
    let key = UpgKey::new(fuses);
    let block = uboot
        .get(UPDATE_BLOCK..UPDATE_BLOCK + ERASE)
        .ok_or("the uboot partition ends before the update block")?;
    let env = read_env(block, &key)?;
    kernel_partition(kernel, zimage, &layout(&env, kernel.len())?, &key)
}

fn commands(script: &str) -> Vec<Vec<&str>> {
    script
        .split(';')
        .map(|c| c.split_whitespace().collect::<Vec<_>>())
        .filter(|c| !c.is_empty())
        .collect()
}

fn hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.strip_prefix("0x")?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    const FUSES: Fuses = Fuses { cfg0: 0x0123abcd, cfg1: 0x89ef4567, mac0: 0xdeadbeef, mac1: 1 };

    fn env_with(vars: &[&str]) -> Vec<u8> {
        let mut env = vec![0u8; ENV_SIZE];
        let text = vars.join("\0");
        env[4..4 + text.len()].copy_from_slice(text.as_bytes());
        env[0x800..0x804].copy_from_slice(&0xd00dfeed_u32.to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&env[4..ENV_VARS_END]);
        env[..4].copy_from_slice(&crc.sum().to_le_bytes());
        env
    }

    /// The update block as the vendor updater leaves it, with filler in front of the environment.
    fn update_block(env: &[u8], key: &UpgKey) -> Vec<u8> {
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(env).unwrap();
        let mut payload = gz.finish().unwrap();
        payload.resize(payload.len().div_ceil(0x1000) * 0x1000, 0);
        key.encrypt(&mut payload);
        let mut block = vec![0x5a; ERASE];
        block[ERASE - payload.len()..].copy_from_slice(&payload);
        block[ERASE - 4..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        block
    }

    /// Shaped like the environment of the unit this was worked out on.
    fn vendor_env(kernel_len: usize) -> Vec<u8> {
        let norboot = [
            "run norargs".to_string(),
            "sf probe 0".into(),
            "cp 0x80800800 0x83000000 0xf800".into(),
            format!("sf read 0x80800000 0x40000 {kernel_len:#x}"),
            "mm w 0x80800000 0x10000".into(),
            "mm w 0x80810000 0x10000".into(),
            "mm w 0x80820000 0x10000".into(),
            "bootz 0x80800000 - 0x83000000".into(),
        ];
        let mut hewei = vec!["sf probe 0".to_string()];
        for i in 0..3 {
            hewei
                .push(format!("sf read 0x80800000 {:#x} 0x10000", KERNEL + kernel_len + i * ERASE));
            hewei.push("mm d 0x80800000 0x10000".into());
            hewei.push("mm m 0x80800000 0x10000".into());
            hewei.push(format!("sf update 0x80800000 {:#x}  0x10000", KERNEL + i * ERASE));
        }
        hewei.extend([
            " sf probe 0".to_string(),
            "sf read 0x80810000 0x2d000 0x3000".into(),
            "mm d 0x80810000 0x3000".into(),
            "mm z 0x80810000 0x2ff0 0x80800000 0x10000".into(),
            "mm m 0x80800000 0x10000".into(),
            "sf update 0x80800000 0x30000  0x10000".into(),
        ]);
        env_with(&[
            "bootdelay=3",
            &format!("norboot={};", norboot.join(";")),
            &format!("heweiencrypt={};", hewei.join(";")),
        ])
    }

    fn zimage(len: usize) -> Vec<u8> {
        let mut z: Vec<u8> = (0..len).map(|i| (i * 7 + i / 251) as u8).collect();
        z[0x24..0x28].copy_from_slice(&ZIMAGE_MAGIC.to_le_bytes());
        z
    }

    #[test]
    fn the_upgkey_is_the_fuses_in_hex() {
        let k = UpgKey::new(FUSES);
        assert_eq!(&k.key, b"0123abcd89ef4567");
        assert_eq!(&k.iv, b"deadbeef00000001");
    }

    #[test]
    fn the_fuses_are_read_from_the_ocotp_words() {
        let mut nvmem = vec![0u8; 0x100];
        for (word, value) in
            [(1, FUSES.cfg0), (2, FUSES.cfg1), (0x22, FUSES.mac0), (0x23, FUSES.mac1)]
        {
            nvmem[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        let f = Fuses::from_ocotp(&nvmem).unwrap();
        assert_eq!(
            (f.cfg0, f.cfg1, f.mac0, f.mac1),
            (FUSES.cfg0, FUSES.cfg1, FUSES.mac0, FUSES.mac1)
        );
        assert!(Fuses::from_ocotp(&nvmem[..0x88]).is_none());
    }

    #[test]
    fn cbc_matches_the_published_test_vector() {
        let k = UpgKey {
            key: 0x2b7e151628aed2a6abf7158809cf4f3c_u128.to_be_bytes(),
            iv: 0x000102030405060708090a0b0c0d0e0f_u128.to_be_bytes(),
        };
        let plain = [
            0x6bc1bee22e409f96e93d7e117393172a_u128.to_be_bytes(),
            0xae2d8a571e03ac9c9eb76fac45af8e51_u128.to_be_bytes(),
        ]
        .concat();
        let mut data = plain.clone();
        k.encrypt(&mut data);
        assert_eq!(data[..16], 0x7649abac8119b246cee98e9b12e9197d_u128.to_be_bytes());
        assert_eq!(data[16..], 0x5086cb9b507219ee95db113a917678b2_u128.to_be_bytes());
        k.decrypt(&mut data);
        assert_eq!(data, plain);
    }

    #[test]
    fn the_update_environment_unpacks_and_checks_its_crc() {
        let key = UpgKey::new(FUSES);
        let env = vendor_env(0x302fd8);
        assert_eq!(read_env(&update_block(&env, &key), &key).unwrap(), env);

        let mut bad = env.clone();
        bad[0x10] ^= 1;
        assert!(read_env(&update_block(&bad, &key), &key).unwrap_err().contains("CRC"));

        let other = UpgKey::new(Fuses { mac1: 2, ..FUSES });
        assert!(read_env(&update_block(&env, &key), &other).is_err());
    }

    #[test]
    fn the_layout_comes_from_norboot_and_heweiencrypt() {
        let layout = layout(&vendor_env(0x302fd8), 0x340000).unwrap();
        assert_eq!(layout, Layout { kernel_len: 0x302fd8, staging: 0x342fd8 });
        assert_eq!(env_var(&vendor_env(0x302fd8), "bootdelay"), Some("3"));
    }

    #[test]
    fn a_layout_that_leaves_the_partition_is_refused() {
        assert!(layout(&vendor_env(0x302fd8), 0x320000).is_err());
        assert!(layout(&env_with(&["norboot=bootz 0x80800000"]), 0x340000).is_err());
    }

    #[test]
    fn the_head_goes_into_the_staging_blocks_and_the_rest_stays_clear() {
        let key = UpgKey::new(FUSES);
        let layout = Layout { kernel_len: 0x300000, staging: 0x340000 };
        let current: Vec<u8> = (0..0x340000).map(|i| (i % 13) as u8).collect();
        let z = zimage(0x2f0001);
        let part = kernel_partition(&current, &z, &layout, &key).unwrap();

        assert_eq!(part[..HEAD], current[..HEAD]);
        assert_eq!(part[HEAD..z.len()], z[HEAD..]);
        assert!(part[z.len()..0x300000].iter().all(|&b| b == 0xff));
        let mut staged = part[0x300000..0x330000].to_vec();
        for block in staged.as_chunks_mut::<ERASE>().0 {
            key.decrypt(block);
        }
        assert_eq!(staged, z[..HEAD]);
        assert!(part[0x330000..].iter().all(|&b| b == 0xff));
    }

    #[test]
    fn staging_reads_the_layout_from_the_uboot_partition() {
        let key = UpgKey::new(FUSES);
        let mut uboot = vec![0x5a; ENV_BLOCK];
        uboot[UPDATE_BLOCK..].copy_from_slice(&update_block(&vendor_env(0x302fd8), &key));
        let current: Vec<u8> = (0..0x340000).map(|i| (i % 13) as u8).collect();
        let z = zimage(0x2f0001);

        let layout = Layout { kernel_len: 0x302fd8, staging: 0x342fd8 };
        assert_eq!(
            stage(&z, &current, &uboot, FUSES).unwrap(),
            kernel_partition(&current, &z, &layout, &key).unwrap()
        );
        assert!(stage(&z, &current, &uboot, Fuses { mac1: 2, ..FUSES }).is_err());
        assert!(stage(&z, &current, &uboot[..UPDATE_BLOCK], FUSES).is_err());
    }

    #[test]
    fn a_zimage_uboot_would_cut_off_is_refused() {
        let key = UpgKey::new(FUSES);
        let layout = Layout { kernel_len: 0x100000, staging: 0x140000 };
        let current = vec![0; 0x340000];
        assert!(kernel_partition(&current, &zimage(0x100001), &layout, &key).is_err());
        assert!(kernel_partition(&current, &vec![0; 0x100000], &layout, &key).is_err());
    }

    /// Against a real unit, never in CI: packing its own stock kernel again has to give back the
    /// kernel partition byte for byte. LIVI_IMX6UL_FUSES=cfg0,cfg1,mac0,mac1 LIVI_IMX6UL_MTD0=…
    /// LIVI_IMX6UL_MTD1=… LIVI_IMX6UL_ZIMAGE=… cargo test -p imx6ul-uboot -- --ignored
    #[test]
    #[ignore]
    fn the_stock_kernel_packs_back_to_the_flash() {
        let var = |n: &str| std::env::var(n).unwrap_or_else(|_| panic!("{n} not set"));
        let f: Vec<u32> = var("LIVI_IMX6UL_FUSES")
            .split(',')
            .map(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).unwrap())
            .collect();
        let key = UpgKey::new(Fuses { cfg0: f[0], cfg1: f[1], mac0: f[2], mac1: f[3] });
        let mtd0 = std::fs::read(var("LIVI_IMX6UL_MTD0")).unwrap();
        let mtd1 = std::fs::read(var("LIVI_IMX6UL_MTD1")).unwrap();
        let zimage = std::fs::read(var("LIVI_IMX6UL_ZIMAGE")).unwrap();

        let env = read_env(&mtd0[UPDATE_BLOCK..UPDATE_BLOCK + ERASE], &key).unwrap();
        let layout = layout(&env, mtd1.len()).unwrap();
        eprintln!("{layout:?}");
        let part = kernel_partition(&mtd1, &zimage, &layout, &key).unwrap();
        let diff: Vec<usize> = (0..part.len()).filter(|&i| part[i] != mtd1[i]).collect();
        assert!(
            diff.is_empty(),
            "{} bytes differ, first at {:#x}, last at {:#x}",
            diff.len(),
            diff[0] + KERNEL,
            diff[diff.len() - 1] + KERNEL
        );
    }
}
