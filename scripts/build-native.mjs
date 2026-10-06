// Usage: node scripts/build-native.mjs [--arch=x64|arm64]
// Linux runners are arch-native, only macOS cross-compiles (arm64 host -> x64 app).
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const archArg = process.argv.find((a) => a.startsWith('--arch='))?.slice(7)
const wantArch = archArg === 'x64' ? 'x64' : archArg === 'arm64' ? 'arm64' : process.arch
const cross = process.platform === 'darwin' && wantArch !== process.arch
const triple = wantArch === 'x64' ? 'x86_64-apple-darwin' : 'aarch64-apple-darwin'

// gst-host builds against the GStreamer framework's headers on macOS, the app
// ships its own bundle of the libraries.
const MAC_GST_PKGCONFIG = '/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/pkgconfig'

function cargoEnv() {
  const env = { ...process.env, PKG_CONFIG_ALLOW_CROSS: '1' }
  if (process.platform === 'darwin' && existsSync(MAC_GST_PKGCONFIG)) {
    env.PKG_CONFIG_PATH = [MAC_GST_PKGCONFIG, env.PKG_CONFIG_PATH].filter(Boolean).join(':')
  }
  return env
}

function cargoBuild(manifest, pkg) {
  const args = ['build', '--release', '-p', pkg, '--manifest-path', manifest]
  if (cross) {
    execFileSync('rustup', ['target', 'add', triple], { stdio: 'inherit' })
    args.push('--target', triple)
  }
  execFileSync('cargo', args, { stdio: 'inherit', env: cargoEnv() })
  return join(targetDir(manifest), ...(cross ? [triple] : []), 'release')
}

// CARGO_TARGET_DIR or a target-dir in the cargo config may move the build out of the tree.
function targetDir(manifest) {
  const meta = execFileSync(
    'cargo',
    ['metadata', '--format-version', '1', '--no-deps', '--manifest-path', manifest],
    { encoding: 'utf8', env: cargoEnv(), maxBuffer: 16 * 1024 * 1024 }
  )
  return JSON.parse(meta).target_directory
}

function place(src, destDir, destName) {
  mkdirSync(destDir, { recursive: true })
  copyFileSync(src, join(destDir, destName))
  console.log(`[build-native] ${destName} <- ${src}`)
}

const gstManifest = join(root, 'native', 'livi-gst-video', 'rust', 'Cargo.toml')
const gstDest = join(root, 'native', 'livi-gst-video', 'build', 'Release')
const hostOut = cargoBuild(gstManifest, 'gst-video-host')
place(join(hostOut, 'livi-gst-host'), gstDest, 'livi-gst-host')

if (process.platform === 'darwin') {
  const addonOut = cargoBuild(gstManifest, 'gst-video-addon')
  place(join(addonOut, 'libgst_video_addon.dylib'), gstDest, 'gst_video.node')
}

if (process.platform === 'linux') {
  const compManifest = join(root, 'native', 'livi-compositor', 'rust', 'Cargo.toml')
  const compOut = cargoBuild(compManifest, 'livi-compositor')
  place(join(compOut, 'livi-compositor'), join(root, 'out', 'compositor'), 'livi-compositor')
}

const helperManifest = join(root, 'native', 'livi-helperd', 'Cargo.toml')
const helperDest = join(root, 'native', 'livi-helperd', 'build', 'Release')
const helperOut = cargoBuild(helperManifest, 'livi-helperd')
place(join(helperOut, 'livi-helperd'), helperDest, 'livi-helperd')
const coreOut = cargoBuild(helperManifest, 'livi-core')
place(join(coreOut, 'livi-core'), helperDest, 'livi-core')

if (process.platform === 'linux') {
  // The native UI, staged where livi-core resolves it (see resources.rs repo layout).
  const uiManifest = join(root, 'native', 'livi-ui', 'Cargo.toml')
  const uiOut = cargoBuild(uiManifest, 'livi-ui')
  place(join(uiOut, 'livi-ui'), join(root, 'out', 'ui'), 'livi-ui')
}
