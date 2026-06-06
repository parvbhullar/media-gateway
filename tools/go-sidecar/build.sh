#!/usr/bin/env bash
# build.sh — install Go sidecar dependencies and build the binary on a server.
# Usage: bash build.sh [--output /path/to/sip_bridge_sidecar]
set -euo pipefail

OUTPUT="${1:-./sip_bridge_sidecar}"

# ── 1. System library dependencies ────────────────────────────────────────────
install_deps() {
    if command -v apt-get &>/dev/null; then
        echo "[build] Installing libopus + libopusfile via apt…"
        apt-get update -qq
        apt-get install -y --no-install-recommends \
            libopus-dev libopusfile-dev pkg-config gcc
    elif command -v yum &>/dev/null; then
        echo "[build] Installing libopus + libopusfile via yum…"
        yum install -y opus-devel opusfile-devel pkgconfig gcc
    elif command -v dnf &>/dev/null; then
        echo "[build] Installing libopus + libopusfile via dnf…"
        dnf install -y opus-devel opusfile-devel pkgconfig gcc
    else
        echo "[build] ERROR: unsupported package manager — install libopus-dev and libopusfile-dev manually"
        exit 1
    fi
}

if ! pkg-config --exists opus 2>/dev/null || ! pkg-config --exists opusfile 2>/dev/null; then
    install_deps
else
    echo "[build] libopus and libopusfile already present, skipping install"
fi

# ── 2. Go toolchain ───────────────────────────────────────────────────────────
GO_MIN="1.22"

find_go() {
    for candidate in go /usr/local/go/bin/go /home/*/go/bin/go /root/go/bin/go; do
        if command -v "$candidate" &>/dev/null 2>&1; then
            echo "$candidate"; return
        fi
    done
}

GO_BIN=$(find_go || true)

if [[ -z "$GO_BIN" ]]; then
    echo "[build] Go not found — installing Go 1.24 to /usr/local/go …"
    ARCH=$(uname -m)
    case "$ARCH" in
        x86_64)  GOARCH="amd64" ;;
        aarch64) GOARCH="arm64" ;;
        *)        echo "[build] Unsupported arch: $ARCH"; exit 1 ;;
    esac
    GO_VERSION="1.24.3"
    TARBALL="go${GO_VERSION}.linux-${GOARCH}.tar.gz"
    curl -fsSL "https://go.dev/dl/${TARBALL}" -o "/tmp/${TARBALL}"
    rm -rf /usr/local/go
    tar -C /usr/local -xzf "/tmp/${TARBALL}"
    rm "/tmp/${TARBALL}"
    GO_BIN="/usr/local/go/bin/go"
    export PATH="$PATH:/usr/local/go/bin"
    echo "[build] Go installed: $($GO_BIN version)"
else
    echo "[build] Found Go: $($GO_BIN version)"
fi

# ── 3. Build ──────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "[build] Building sip_bridge_sidecar…"
CGO_ENABLED=1 "$GO_BIN" build \
    -ldflags="-s -w" \
    -o "$OUTPUT" \
    .

echo "[build] Done: $OUTPUT"
"$OUTPUT" --help 2>/dev/null || true
