// Round-robins paired phones back over Bluetooth after a restart.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::time::{Duration, Instant};

use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::state::HelperState;

const PING_TIMEOUT: Duration = Duration::from_secs(1);
const FAST_INTERVAL: Duration = Duration::from_secs(1);
const FAST_ATTEMPTS: u32 = 15;
const SLOW_INTERVAL: Duration = Duration::from_secs(30);
const STALE: Duration = Duration::from_secs(10);
/// How often BlueZ is asked about paired phones the registry does not know.
const PAIR_SCAN: Duration = Duration::from_secs(10);
/// Android Auto starts on the phone when its HFP link to the car comes up;
/// CarPlay wireless starts on the iAP link. A phone that has never finished a
/// session has no protocol yet, so both pokes go out and the phone answers
/// the one it runs (the other fails as an unavailable profile).
const HFP_AG_UUID: &str = "0000111f-0000-1000-8000-00805f9b34fb";
const IAP_CLIENT_UUID: &str = "00000000-deca-fade-deca-deafdecacafe";

pub async fn run(conn: Connection, adapter: String, ap_iface: String, state: Arc<HelperState>) {
    let mut rr: usize = 0;
    let mut attempts: HashMap<String, u32> = HashMap::new();
    let mut next_try: HashMap<String, Instant> = HashMap::new();
    let mut stale_since: HashMap<String, Instant> = HashMap::new();
    let mut paired: Vec<(String, String)> = Vec::new();
    let mut scan_due = Instant::now();
    let mut woken: HashMap<String, Instant> = HashMap::new();

    loop {
        tokio::time::sleep(FAST_INTERVAL).await;
        let targets = state.reconnect_targets();
        attempts.retain(|mac, _| targets.iter().any(|(m, _)| m == mac));
        next_try.retain(|mac, _| targets.iter().any(|(m, _)| m == mac));
        stale_since.retain(|mac, _| targets.iter().any(|(m, _)| m == mac));

        // A phone that is paired for the first time is in no target list yet:
        // LIVI's registry only learns a phone from its first finished session,
        // so without this nothing would ever page it and the projection would
        // never start - the everyday user cannot be asked to kick it by hand.
        // Every paired phone outside the registry is woken here; the profile
        // link that creates makes the phone's own app start the session, the
        // session puts the phone into the registry, and the round-robin above
        // takes over from the next pass on.
        if scan_due.elapsed() >= PAIR_SCAN {
            scan_due = Instant::now();
            paired = paired_phones(&conn, &adapter).await.unwrap_or_default();
            woken.retain(|mac, _| paired.iter().any(|(m, _)| m == mac));
        }
        if !paired.is_empty() && crate::net::wlan_link_local(&ap_iface).is_some() {
            for (mac, path) in &paired {
                if targets.iter().any(|(m, _)| m == mac) || state.link_active(mac) {
                    continue;
                }
                if woken.get(mac).is_some_and(|t| t.elapsed() < SLOW_INTERVAL) {
                    continue;
                }
                woken.insert(mac.clone(), Instant::now());
                println!("[cp] reconnect: waking paired phone {mac}");
                let _ =
                    tokio::time::timeout(PING_TIMEOUT, page(&conn, path, Some(HFP_AG_UUID))).await;
                let _ =
                    tokio::time::timeout(PING_TIMEOUT, page(&conn, path, Some(IAP_CLIENT_UUID)))
                        .await;
            }
        }

        if targets.is_empty() {
            continue;
        }

        let (mac, uuid) = targets[rr % targets.len()].clone();
        rr = rr.wrapping_add(1);

        let path = device_path(&adapter, &mac);
        // No BlueZ device object for this MAC
        let connected = match device_connected(&conn, &path).await {
            Some(c) => c,
            None => continue,
        };

        if connected {
            // Out of the rotation: nudge the profile, disconnect after STALE so it re-pages.
            attempts.remove(&mac);
            next_try.remove(&mac);
            // A link in progress, for instance still waiting for the access point, is not stale.
            if state.link_active(&mac) {
                stale_since.remove(&mac);
                continue;
            }
            let stale = match stale_since.entry(mac.clone()) {
                Entry::Vacant(e) => {
                    e.insert(Instant::now());
                    false
                }
                Entry::Occupied(e) => {
                    e.get().elapsed() >= STALE && {
                        e.remove();
                        true
                    }
                }
            };
            if stale {
                println!("[cp] reconnect: {mac} connected but no session, disconnecting");
                let _ = disconnect(&conn, &path).await;
            } else if uuid.is_some() {
                let _ =
                    tokio::time::timeout(PING_TIMEOUT, page(&conn, &path, uuid.as_deref())).await;
            }
            continue;
        }

        stale_since.remove(&mac);

        // Absent phone: after the fast pokes, only poke every SLOW_INTERVAL.
        if next_try.get(&mac).is_some_and(|t| Instant::now() < *t) {
            continue;
        }

        if crate::net::wlan_link_local(&ap_iface).is_none() {
            continue;
        }
        println!("[cp] reconnect: paging {mac}");
        match tokio::time::timeout(PING_TIMEOUT, page(&conn, &path, uuid.as_deref())).await {
            Ok(Ok(())) => {
                println!("[cp] reconnect: {mac} connected");
                attempts.remove(&mac);
                next_try.remove(&mac);
                continue;
            }
            Ok(Err(e)) => println!("[cp] reconnect: {mac} page failed: {e}"),
            // The poke woke it; the next pass catches it.
            Err(_) => {}
        }

        // After FAST_ATTEMPTS quick pokes, drop to the slow poll.
        let n = attempts.entry(mac.clone()).or_insert(0);
        *n += 1;
        if *n >= FAST_ATTEMPTS {
            next_try.insert(mac, Instant::now() + SLOW_INTERVAL);
        }
    }
}

fn device_path(adapter: &str, mac: &str) -> String {
    format!("/org/bluez/{}/dev_{}", adapter, mac.replace(':', "_").to_uppercase())
}

async fn device_connected(conn: &Connection, path: &str) -> Option<bool> {
    let reply = conn
        .call_method(
            Some("org.bluez"),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.bluez.Device1", "Connected"),
        )
        .await
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    bool::try_from(value).ok()
}

async fn page(conn: &Connection, path: &str, uuid: Option<&str>) -> Result<(), zbus::Error> {
    match uuid {
        Some(uuid) => {
            conn.call_method(
                Some("org.bluez"),
                path,
                Some("org.bluez.Device1"),
                "ConnectProfile",
                &(uuid,),
            )
            .await?;
        }
        None => {
            conn.call_method(Some("org.bluez"), path, Some("org.bluez.Device1"), "Connect", &())
                .await?;
        }
    }
    Ok(())
}

async fn disconnect(conn: &Connection, path: &str) -> Result<(), zbus::Error> {
    conn.call_method(Some("org.bluez"), path, Some("org.bluez.Device1"), "Disconnect", &()).await?;
    Ok(())
}

/// Every paired phone on the adapter, by MAC and BlueZ object path: phones the
/// registry does not know are woken from this. Audio devices, computers and
/// the like are left alone; a phone that never set a class is kept because
/// that is what several models do.
async fn paired_phones(
    conn: &Connection,
    adapter: &str,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let reply = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
        reply.body().deserialize()?;
    let prefix = format!("/org/bluez/{adapter}/");
    let mut out = Vec::new();
    for (path, interfaces) in objects {
        if !path.as_str().starts_with(&prefix) {
            continue;
        }
        let Some(props) = interfaces.get("org.bluez.Device1") else { continue };
        let value = |key: &str| props.get(key).and_then(|v| v.try_clone().ok());
        if value("Paired").and_then(|v| bool::try_from(v).ok()) != Some(true) {
            continue;
        }
        let class = value("Class").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        if class != 0 && (class >> 8) & 0x1f != 2 {
            continue;
        }
        let Some(mac) = value("Address").and_then(|v| String::try_from(v).ok()) else { continue };
        out.push((mac.to_uppercase(), path.as_str().to_string()));
    }
    Ok(out)
}
