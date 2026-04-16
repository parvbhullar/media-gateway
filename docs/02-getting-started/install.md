# Install

## Docker (Recommended)

### Community Edition

```bash
docker pull ghcr.io/restsend/rustpbx:latest
```

### Commerce Edition

```bash
docker pull docker.cnb.cool/miuda.ai/rustpbx:latest
```

The Commerce Edition includes VoIP Wholesale, IVR Visual Editor, Voicemail Pro,
and Enterprise Auth (LDAP/SAML/MFA). See the [Editions](../01-overview/editions.md)
page for a full comparison.

### Minimal Configuration

Create a `config.toml` alongside your Docker container:

```toml
http_addr = "0.0.0.0:8080"
database_url = "sqlite://rustpbx.sqlite3"

[console]
base_path = "/console"
allow_registration = false

[proxy]
addr = "0.0.0.0"
udp_port = 5060
modules = ["auth", "registrar", "call"]

[[proxy.user_backends]]
type = "memory"
users = [{ username = "1001", password = "password" }]

[sipflow]
type = "local"
root = "./config/cdr"
subdirs = "hourly"
```

### Run the Container

```bash
docker run -d --name supersip --net host \
  -v $(pwd)/config.toml:/app/config.toml \
  -v $(pwd)/config:/app/config \
  ghcr.io/restsend/rustpbx:latest --conf /app/config.toml
```

### Create First Admin

```bash
docker exec supersip /app/rustpbx --conf /app/config.toml \
  --super-username admin --super-password changeme
```

### Verify

- **Web console**: <http://localhost:8080/console/>
- **SIP proxy**: `udp://localhost:5060`

Log in to the web console with `admin` / `changeme` to confirm everything is
running.

---

## Build from Source

### Dependencies

**Linux:**

```bash
apt-get install -y cmake pkg-config libasound2-dev libssl-dev libopus-dev
```

**macOS:**

```bash
brew install cmake openssl pkg-config
```

### Build & Run

```bash
git clone https://github.com/restsend/rustpbx
cd rustpbx
cargo build --release
cargo run --bin rustpbx -- --conf config.toml.example
```

### Cross-Compilation

Use [cross](https://github.com/cross-rs/cross) for `aarch64` or `x86_64`
targets:

```bash
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu
```

---

## Troubleshooting

**SIP 401 behind NAT / Docker** -- set the realm explicitly so digest auth
matches the address your clients see:

```toml
[proxy]
realms = ["your-public-ip:5060"]
```

---
**Status:** Shipped
