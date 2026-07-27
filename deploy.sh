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
SERVICE_NAME="${SERVICE_NAME:-rustpbx.service}"
DEPLOY_BRANCH="${DEPLOY_BRANCH:-main}"
# ──────────────────────────────────────────────────────────────────────────────

: "${SERVER_USER:?SERVER_USER not set in .env.local}"
: "${SERVER_HOST:?SERVER_HOST not set in .env.local}"
: "${SSH_KEY:?SSH_KEY not set in .env.local}"
: "${GIT_REPO:?GIT_REPO not set in .env.local}"

SSH_OPTS="-i ${SSH_KEY} -o StrictHostKeyChecking=no"

BINARY="target/deploy/rustpbx"
REMOTE_BINARY="${SERVER_DIR}/rustpbx"
REMOTE_BACKUP="${SERVER_DIR}/rustpbx.prev"

# ── Rollback mode: `./deploy.sh rollback` ───────────────────────────────────
# Restores the binary saved by the previous deploy and restarts the service.
# The currently-installed (bad) binary is kept as rustpbx.failed for
# inspection. ponytail: single-level rollback (one previous version); add
# timestamped history if you ever need to roll back more than one deploy.
if [[ "${1:-}" == "rollback" ]]; then
  echo "==> Rolling back to previous binary on ${SERVER_HOST}..."
  ssh $SSH_OPTS "${SERVER_USER}@${SERVER_HOST}" "
    if [ ! -f '${REMOTE_BACKUP}' ]; then
      echo 'ERROR: ${REMOTE_BACKUP} not found — no previous version to roll back to.'
      exit 1
    fi
    [ -f '${REMOTE_BINARY}' ] && cp -f '${REMOTE_BINARY}' '${SERVER_DIR}/rustpbx.failed'
    cp -f '${REMOTE_BACKUP}' '${REMOTE_BINARY}'
    sudo systemctl restart ${SERVICE_NAME}
    sleep 2
    if systemctl is-active --quiet ${SERVICE_NAME}; then
      echo 'Rollback complete: ${SERVICE_NAME} is active.'
    else
      echo 'WARNING: ${SERVICE_NAME} not active. Check: journalctl -u ${SERVICE_NAME} -n 30'
      exit 1
    fi
  "
  exit 0
fi

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

# ── Guard: never ship a non-Linux binary ────────────────────────────────────
# The server is Linux; a macOS (Mach-O) build gives "Exec format error" and
# crash-loops the service. Abort before touching the server if this isn't ELF.
# ponytail: checks ELF vs Mach-O only; tighten to match the server arch
# (e.g. grep 'x86-64') if you build for multiple Linux targets.
if ! file "$BINARY" | grep -q 'ELF'; then
  echo "ERROR: $BINARY is not a Linux ELF binary — refusing to deploy:"
  file "$BINARY"
  echo "Build this from a Linux x86_64 host (or cross-compile) and retry."
  exit 1
fi

echo "==> Binary size: $(du -sh $BINARY | cut -f1)"

# Strip intentionally disabled: stripping the deploy binary has been
# correlated with corrupted IceCandidate port serialization on cross-
# toolchain runs (built on Ubuntu 22.04, run on Ubuntu 24.04 Oracle VM).
# Cargo.toml's [profile.deploy] also has `strip = false`. Keep the
# unstripped binary even at the cost of ~30MB extra over the wire.
# strip "$BINARY" 2>/dev/null || true

echo "==> Backing up current binary on server (rollback point)..."
ssh $SSH_OPTS "${SERVER_USER}@${SERVER_HOST}" "
  if [ -f '${REMOTE_BINARY}' ]; then
    cp -f '${REMOTE_BINARY}' '${REMOTE_BACKUP}'
    echo 'Saved current binary -> ${REMOTE_BACKUP}'
  else
    echo 'No existing binary to back up (first deploy).'
  fi
"

echo "==> Uploading binary..."
rsync -avz --progress -e "ssh $SSH_OPTS" "$BINARY" "${SERVER_USER}@${SERVER_HOST}:${REMOTE_BINARY}"

echo "==> Deploy complete."
echo "==> Restarting service..."
ssh $SSH_OPTS "${SERVER_USER}@${SERVER_HOST}" "sudo systemctl restart ${SERVICE_NAME}
echo 'Done.'"
echo "==> If this build misbehaves, roll back with:  ./deploy.sh rollback"
