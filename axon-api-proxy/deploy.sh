#!/usr/bin/env bash
set -e

# Axon API Proxy - Deploy to Server (WSL Version)
# Run this from within WSL: ./deploy.sh

SERVER="34.61.3.40"
SSH_USER="canchowlung"
REMOTE="${SSH_USER}@${SERVER}"
DEST="/home/canchowlung/axon-api-proxy"
BINARY="target/release/axon-api-proxy"

echo ""
echo "=== Axon API Proxy Deploy (WSL) ==="
echo "Target: ${REMOTE}:${DEST}"
echo ""

echo "[1/5] Building release binary..."
cargo build --release

if [ ! -f "$BINARY" ]; then
    echo "[ERROR] Binary not found at $BINARY after build"
    exit 1
fi

echo "[2/5] Creating remote directory..."
ssh "$REMOTE" "mkdir -p $DEST/static"

echo "[3/5] Uploading files..."
ssh "$REMOTE" "sudo systemctl stop axon-api-proxy 2>/dev/null || true; rm -f $DEST/axon-api-proxy"
scp "$BINARY" "${REMOTE}:${DEST}/axon-api-proxy"
scp axon-api-proxy.service "${REMOTE}:${DEST}/axon-api-proxy.service"
scp src/dashboard.html "${REMOTE}:${DEST}/static/dashboard.html"

# Upload .env only if missing on server (preserve dashboard edits)
if ssh "$REMOTE" "test ! -f $DEST/.env"; then
    if [ -f ".env" ]; then
        echo "   Uploading .env [first deploy]..."
        scp .env "${REMOTE}:${DEST}/.env"
    fi
else
    echo "   Skipping .env [already on server — preserving dashboard changes]"
fi

# Upload models.toml only if missing on server (preserve dashboard edits)
if ssh "$REMOTE" "test ! -f $DEST/models.toml"; then
    echo "   Uploading models.toml [first deploy]..."
    scp models.toml "${REMOTE}:${DEST}/models.toml"
else
    echo "   Skipping models.toml [already on server — preserving dashboard changes]"
fi

echo "[4/5] Setting permissions and installing service..."
ssh "$REMOTE" "chmod +x $DEST/axon-api-proxy && \
    sudo cp $DEST/axon-api-proxy.service /etc/systemd/system/ && \
    sudo systemctl daemon-reload && \
    sudo systemctl enable axon-api-proxy && \
    sudo systemctl restart axon-api-proxy"

echo "[5/5] Checking service status..."
ssh "$REMOTE" "sudo systemctl status axon-api-proxy --no-pager"

echo ""
echo "=== Deploy complete ==="
echo ""
