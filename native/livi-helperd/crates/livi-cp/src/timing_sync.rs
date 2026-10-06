//! Without a request a second the phone ends the session after a few seconds.

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::net;
use crate::timing::TimingClock;

const REQUEST_EVERY: Duration = Duration::from_secs(1);

pub struct TimingSync {
    clock: Arc<Mutex<TimingClock>>,
    sock: Arc<UdpSocket>,
    port: u16,
    tasks: Vec<JoinHandle<()>>,
}

fn lock(clock: &Mutex<TimingClock>) -> std::sync::MutexGuard<'_, TimingClock> {
    clock.lock().unwrap_or_else(|e| e.into_inner())
}

impl TimingSync {
    pub fn listen() -> io::Result<Self> {
        let sock = Arc::new(net::udp_socket()?);
        let port = net::local_port(sock.local_addr());
        let clock = Arc::new(Mutex::new(TimingClock::new()));
        let answering = tokio::spawn(answer(sock.clone(), clock.clone()));
        Ok(Self { clock, sock, port, tasks: vec![answering] })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn start(&mut self, peer: SocketAddr) {
        println!("[cpTiming] driving clock sync to {peer}");
        let peer = net::v6(peer);
        let (sock, clock) = (self.sock.clone(), self.clock.clone());
        self.tasks.push(tokio::spawn(async move {
            loop {
                let pkt = lock(&clock).request();
                let _ = sock.send_to(&pkt, peer).await;
                tokio::time::sleep(REQUEST_EVERY).await;
            }
        }));
    }

    pub fn synced_ntp(&self) -> u64 {
        lock(&self.clock).synced_ntp()
    }
}

impl Drop for TimingSync {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn answer(sock: Arc<UdpSocket>, clock: Arc<Mutex<TimingClock>>) {
    let mut buf = [0u8; 512];
    loop {
        match sock.recv_from(&mut buf).await {
            Ok((n, from)) => {
                let reply = lock(&clock).on_packet(&buf[..n]);
                if let Some(reply) = reply {
                    let _ = sock.send_to(&reply, from).await;
                }
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::PACKET_LEN;

    #[tokio::test]
    async fn requests_go_out_and_requests_get_answered() {
        let phone = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut sync = TimingSync::listen().unwrap();
        assert!(sync.port() > 0);
        sync.start(phone.local_addr().unwrap());

        let mut buf = [0u8; 64];
        let (n, from) = phone.recv_from(&mut buf).await.unwrap();
        assert_eq!(n, PACKET_LEN);
        assert_eq!(buf[1], 210);
        assert_eq!(from.port(), sync.port());

        let mut ask = [0u8; PACKET_LEN];
        ask[0] = 0x80;
        ask[1] = 210;
        ask[24..32].copy_from_slice(&7u64.to_be_bytes());
        phone.send_to(&ask, ("127.0.0.1", sync.port())).await.unwrap();
        loop {
            let (n, _) = phone.recv_from(&mut buf).await.unwrap();
            if buf[1] == 211 {
                assert_eq!(n, PACKET_LEN);
                assert_eq!(&buf[8..16], &7u64.to_be_bytes());
                break;
            }
        }
        assert!(sync.synced_ntp() > 0);
    }
}
