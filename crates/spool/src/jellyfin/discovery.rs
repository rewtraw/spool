//! Lets players on the same network find this server without being told its address, the way
//! they find a Jellyfin server: they shout a question at everyone, and servers answer.

use crate::app::App;
use serde_json::json;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

const PORT: u16 = 7359;

/// The address of ours that the asker can reach: whichever one the system would use to reply.
async fn facing(peer: SocketAddr) -> Option<std::net::IpAddr> {
    let probe = UdpSocket::bind(("0.0.0.0", 0)).await.ok()?;
    probe.connect(peer).await.ok()?;
    probe.local_addr().ok().map(|a| a.ip())
}

pub fn reply(address: &str, id: &str) -> String {
    json!({"Address": address, "Id": id, "Name": super::SERVER_NAME, "EndpointAddress": null}).to_string()
}

pub async fn answer(app: App, port: u32) {
    let socket = match UdpSocket::bind(("0.0.0.0", PORT)).await {
        Ok(s) => s,
        Err(e) => {
            // Another server here already answers, or the port is not ours to use. Players can
            // still be given the address by hand.
            tracing::info!(error = %e, "players on the network will not find Spool by themselves");
            return;
        }
    };
    let _ = socket.set_broadcast(true);
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, peer)) = socket.recv_from(&mut buf).await else {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        };
        if !String::from_utf8_lossy(&buf[..n]).to_lowercase().contains("who is jellyfinserver?") {
            continue;
        }
        let Some(ip) = facing(peer).await else { continue };
        let _ = socket.send_to(reply(&format!("http://{ip}:{port}"), &super::server_id(&app)).as_bytes(), peer).await;
    }
}
