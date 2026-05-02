#!/usr/bin/env bash
set -euo pipefail

# ── Load .env.local ───────────────────────────────────────────────────────────
ENV_FILE="$(dirname "$0")/.env.local"
if [[ -f "$ENV_FILE" ]]; then
  set -a; source "$ENV_FILE"; set +a
else
  echo "ERROR: .env.local not found at $ENV_FILE"; exit 1
fi

# ── Config (override in .env.local) ───────────────────────────────────────────
SERVER_DIR="${SERVER_DIR:-/opt/rustpbx}"
SERVICE_NAME="${SERVICE_NAME:-rustpbx}"
# ──────────────────────────────────────────────────────────────────────────────

: "${SERVER_USER:?SERVER_USER not set in .env.local}"
: "${SERVER_HOST:?SERVER_HOST not set in .env.local}"

BINARY="target/release/rustpbx"

echo "==> Building release binary..."
cargo build --release

echo "==> Binary size before strip: $(du -sh $BINARY | cut -f1)"

# strip = "symbols" in Cargo.toml handles this automatically in Rust 1.77+
# but run strip as safety net for older toolchains
strip "$BINARY" 2>/dev/null || true

echo "==> Binary size after strip:  $(du -sh $BINARY | cut -f1)"

echo "==> Creating deploy directory on server..."
ssh "${SERVER_USER}@${SERVER_HOST}" "sudo mkdir -p ${SERVER_DIR} && sudo chown ${SERVER_USER}:${SERVER_USER} ${SERVER_DIR}"

echo "==> Uploading binary..."
rsync -avz --progress "$BINARY" "${SERVER_USER}@${SERVER_HOST}:${SERVER_DIR}/rustpbx"

echo "==> Uploading config (skip if already present)..."
rsync -avz --ignore-existing config.toml "${SERVER_USER}@${SERVER_HOST}:${SERVER_DIR}/config.toml"

echo "==> Restarting service..."
ssh "${SERVER_USER}@${SERVER_HOST}" "sudo systemctl restart ${SERVICE_NAME} && sudo systemctl status ${SERVICE_NAME} --no-pager"

echo "==> Deploy complete."
