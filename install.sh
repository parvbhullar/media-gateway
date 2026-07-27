#!/usr/bin/env bash
# install.sh — Setup script for rustpbx (SIP media gateway)
# Run: bash install.sh

set -euo pipefail

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

step() { echo -e "\n${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC} $1"; }

# ── 1. System Dependencies ────────────────────────────────────────────────────
step "Installing system dependencies..."
sudo apt-get update
sudo apt-get install -y \
  build-essential curl git pkg-config cmake \
  ca-certificates \
  libssl-dev \
  libopus-dev \
  sqlite3 libsqlite3-dev

# ── 2. Rust ───────────────────────────────────────────────────────────────────
step "Installing Rust..."
if command -v rustc &>/dev/null; then
  warn "Rust already installed ($(rustc --version)), skipping."
else
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
fi
source "$HOME/.cargo/env"

# ── 3. Build project ──────────────────────────────────────────────────────────
step "Building rustpbx (release)..."
cargo build --release

# ── 4. Runtime directories ────────────────────────────────────────────────────
step "Ensuring runtime directories exist..."
mkdir -p logs storage

# ── Done ──────────────────────────────────────────────────────────────────────
echo -e "\n${GREEN}Installation complete!${NC}"
echo ""
echo "Run the server:"
echo "  ./target/release/rustpbx --conf config.toml"
echo ""
echo "Open console:"
echo "  http://localhost:8080/console"
