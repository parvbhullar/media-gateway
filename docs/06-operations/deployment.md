# Deployment

## Docker

### Community Edition

```bash
docker pull ghcr.io/restsend/rustpbx:latest
```

```bash
docker run -d --name supersip --net host \
  -v $(pwd)/config.toml:/app/config.toml \
  -v $(pwd)/config:/app/config \
  ghcr.io/restsend/rustpbx:latest --conf /app/config.toml
```

Create the first admin account:

```bash
docker exec supersip /app/rustpbx --conf /app/config.toml \
  --super-username admin --super-password changeme
```

### Commerce Edition

```bash
docker pull docker.cnb.cool/miuda.ai/rustpbx:latest
```

Run the same way as Community, replacing the image name.

### Docker Compose Example

```yaml
version: "3.8"
services:
  supersip:
    image: ghcr.io/restsend/rustpbx:latest
    network_mode: host
    restart: unless-stopped
    volumes:
      - ./config.toml:/app/config.toml:ro
      - ./config:/app/config
    command: ["--conf", "/app/config.toml"]
```

> **Network mode:** `host` is recommended so SIP/RTP ports are directly reachable without Docker NAT complications. If you must use bridge networking, publish the SIP port and the entire RTP range.

### Dockerfile Variants

| File | Purpose |
|------|---------|
| `Dockerfile` | Standard multi-arch build (pre-compiled binaries for amd64/arm64) |
| `Dockerfile.commerce` | Full source build with `--features commerce` (includes Wholesale, Voicemail Pro, etc.) |
| `Dockerfile.cross-aarch64` | Cross-compilation toolchain image for ARM64 targets |
| `Dockerfile.cross-x86_64` | Cross-compilation toolchain image for x86_64 targets |

## Build from Source

**Linux dependencies:**

```bash
apt-get install -y cmake pkg-config libasound2-dev libssl-dev libopus-dev
```

**macOS dependencies:**

```bash
brew install cmake openssl pkg-config
```

**Build and run:**

```bash
git clone https://github.com/restsend/rustpbx
cd rustpbx
cargo build --release
cargo run --bin rustpbx -- --conf config.toml
```

Cross-compile for ARM64 via [cross](https://github.com/cross-rs/cross):

```bash
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu
```

## Systemd

A basic systemd unit file for running SuperSip as a service:

```ini
[Unit]
Description=SuperSip SIP Proxy & PBX
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/rustpbx --conf /etc/supersip/config.toml
Restart=on-failure
RestartSec=5
LimitNOFILE=65536
WorkingDirectory=/var/lib/supersip

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now supersip
```

## TLS / ACME

SuperSip includes an ACME addon for automatic TLS certificate management. Enable it in the proxy addon list and configure your TLS listener:

```toml
[proxy]
addons = ["acme"]

# TLS listener (SIP over TLS on port 5061)
tls_port = 5061
```

The ACME addon handles Let's Encrypt certificate issuance and renewal automatically. See [Addons](../04-subsystems/addons.md) for the ACME addon details.

## Environment Variables

SuperSip is configured entirely via TOML files. There are no required environment variables. The config file path is specified with the `--conf` CLI argument (defaults to `rustpbx.toml`).

Key CLI arguments:

| Argument | Description |
|----------|-------------|
| `--conf <path>` | Path to the TOML configuration file |
| `--super-username <name>` | Create/reset a super-admin account |
| `--super-password <pass>` | Password for the super-admin account |

---
**Status:** ✅ Shipped
**Source:** `README.md`, `Dockerfile*`
**Last reviewed:** 2026-04-16
