# Dev Setup

## System Dependencies

**Linux:**
```bash
apt-get install -y cmake pkg-config libasound2-dev libssl-dev libopus-dev
```

**macOS:**
```bash
brew install cmake openssl pkg-config
```

## Build

```bash
git clone <repo-url>
cd media-gateway
cargo build --release
```

## Run

```bash
cargo run --bin rustpbx -- --conf config.toml.example
```

- Web console: http://localhost:8080/console/
- SIP proxy: udp://localhost:5060

## Create Admin User

```bash
cargo run --bin rustpbx -- --conf config.toml.example \
  --super-username admin --super-password changeme
```

## Feature Flags

SuperSip uses Cargo feature flags to control optional subsystems. The `default` set ships the community edition; `commerce` bundles all commercial plugins.

| Feature | Description |
|---------|-------------|
| `opus` | Opus audio codec support (via `audio-codec/opus`) |
| `console` | Built-in web management UI (requires `minijinja`) |
| `addon-acme` | Automatic TLS certificate provisioning via ACME (Let's Encrypt) |
| `addon-wholesale` | VoIP wholesale / VOS3000-alternative (commercial) |
| `addon-archive` | Call recording archive with compression |
| `addon-endpoint-manager` | Phone auto-provisioning (commercial) |
| `addon-enterprise-auth` | LDAP/SAML/MFA authentication (commercial, requires `ldap3` + `ring`) |
| `addon-transcript` | Post-call transcription via SenseVoice (offline) |
| `addon-voicemail` | Voicemail Pro (commercial) |
| `addon-ivr-editor` | Visual IVR editor (commercial) |
| `parallel-trunk-dial` | Enable parallel dialing across trunk group members |
| `addon-observability` | Prometheus metrics exporter |
| `addon-telemetry` | Full OpenTelemetry stack (traces + metrics via OTLP/gRPC) |
| `commerce` | Meta-feature: enables all commercial addons + telemetry |
| `cross` | Cross-compilation support (enables `aws-lc-rs/bindgen`) |
| `integration-test` | Gates integration test code paths |

**Default features:** `opus`, `console`, `addon-acme`, `addon-transcript`, `addon-archive`, `addon-observability`.

Build with a specific feature:
```bash
cargo build --release --features addon-telemetry
```

Build the full commercial edition:
```bash
cargo build --release --features commerce
```

## Cross-Compilation

For `aarch64` / `x86_64` targets via [cross](https://github.com/cross-rs/cross):
```bash
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu
```

## IDE

- Recommended: rust-analyzer
- Format: `cargo fmt`
- Lint: `cargo clippy`
