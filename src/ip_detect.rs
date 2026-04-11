use std::time::Duration;
use tracing::{info, warn};

/// Detect the public IP of this machine.
///
/// Tries sources in order with individual timeouts. Total budget is ~5 s.
/// Always returns *something* — falls back to the local interface IP so the
/// server can start even with no internet access.
pub async fn detect_public_ip() -> Option<String> {
    // 1. AWS EC2 instance metadata (works inside AWS VPC, fast)
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .danger_accept_invalid_certs(false)
        .build()
        .ok()?;

    if let Ok(resp) = client
        .get("http://169.254.169.254/latest/meta-data/public-ipv4")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if looks_like_ip(&ip) {
                    info!(ip, "external_ip detected via AWS EC2 metadata");
                    return Some(ip);
                }
            }
        }
    }

    // 2. GCP instance metadata
    let gcp = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .ok()?;
    if let Ok(resp) = gcp
        .get(
            "http://metadata.google.internal/computeMetadata/v1/\
             instance/network-interfaces/0/access-configs/0/externalIp",
        )
        .header("Metadata-Flavor", "Google")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if looks_like_ip(&ip) {
                    info!(ip, "external_ip detected via GCP metadata");
                    return Some(ip);
                }
            }
        }
    }

    // 3. Public API — works on any internet-connected host
    let pub_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    if let Ok(resp) = pub_client.get("https://api.ipify.org").send().await {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if looks_like_ip(&ip) {
                    info!(ip, "external_ip detected via api.ipify.org");
                    return Some(ip);
                }
            }
        }
    }

    // 4. Local interface fallback — UDP connect trick (no packets sent)
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                warn!(ip, "external_ip falling back to local interface address");
                return Some(ip);
            }
        }
    }

    warn!("external_ip detection failed — all sources unavailable");
    None
}

fn looks_like_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}
