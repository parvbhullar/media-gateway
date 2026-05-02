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
DEPLOY_BRANCH="${DEPLOY_BRANCH:-main}"
# ──────────────────────────────────────────────────────────────────────────────

: "${SERVER_USER:?SERVER_USER not set in .env.local}"
: "${SERVER_HOST:?SERVER_HOST not set in .env.local}"
: "${SSH_KEY:?SSH_KEY not set in .env.local}"
: "${GIT_REPO:?GIT_REPO not set in .env.local}"

SSH_OPTS="-i ${SSH_KEY} -o StrictHostKeyChecking=no"

BINARY="target/deploy/rustpbx"

echo "==> Syncing repo on server (branch: ${DEPLOY_BRANCH})..."
ssh $SSH_OPTS "${SERVER_USER}@${SERVER_HOST}" "
  if [ ! -d '${SERVER_DIR}/.git' ]; then
    echo 'Initialising repo in existing directory...'
    cd ${SERVER_DIR}
    git init
    git remote add origin ${GIT_REPO}
    git fetch origin
    git checkout ${DEPLOY_BRANCH}
  else
    echo 'Updating repo...'
    cd ${SERVER_DIR}
    git fetch origin
    git checkout ${DEPLOY_BRANCH}
    git pull
  fi
"

echo "==> Building deploy binary locally..."
cargo build --profile deploy

echo "==> Binary size: $(du -sh $BINARY | cut -f1)"

# strip = "symbols" in Cargo.toml handles this automatically in Rust 1.77+
# but run strip as safety net for older toolchains
strip "$BINARY" 2>/dev/null || true

echo "==> Uploading binary..."
rsync -avz --progress -e "ssh $SSH_OPTS" "$BINARY" "${SERVER_USER}@${SERVER_HOST}:${SERVER_DIR}/rustpbx"

echo "==> Deploy complete."
