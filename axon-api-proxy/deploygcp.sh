#!/usr/bin/env bash
set -e

# Axon API Proxy - Deploy to Google Cloud Platform
# Run this from within WSL: ./deploygcp.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── GCP Configuration ────────────────────────────────────────────────────────
GCP_INSTANCE="canchowlung"
GCP_ZONE="us-central1-a"
REMOTE_USER="canchowlung"
REMOTE_DIR="/home/$REMOTE_USER/axon-api-proxy"
GCLOUD_SSH="gcloud compute ssh $GCP_INSTANCE --zone=$GCP_ZONE --"
GCLOUD_SCP="gcloud compute scp"
GCLOUD_SCP_DEST="$GCP_INSTANCE:$REMOTE_DIR"

BINARY="target/release/axon-api-proxy"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  🚀 Axon API Proxy — Deploy to GCP"
echo "══════════════════════════════════════════════════════════════"
echo "Target Instance: $GCP_INSTANCE ($GCP_ZONE)"
echo "Target Dir:      $REMOTE_DIR"
echo ""

echo "[1/5] Building release binary..."
cargo build --release

if [ ! -f "$BINARY" ]; then
    echo "[ERROR] Binary not found at $BINARY after build"
    exit 1
fi

echo "[2/5] Preparing remote directory..."
$GCLOUD_SSH "mkdir -p $REMOTE_DIR/static"

echo "[3/5] Uploading files..."
# Stop service and remove old binary
$GCLOUD_SSH "sudo systemctl stop axon-api-proxy 2>/dev/null || true; rm -f $REMOTE_DIR/axon-api-proxy"

# Upload binary
$GCLOUD_SCP "$BINARY" "$GCLOUD_SCP_DEST/axon-api-proxy" --zone=$GCP_ZONE
# Upload service file
$GCLOUD_SCP "axon-api-proxy.service" "$GCLOUD_SCP_DEST/axon-api-proxy.service" --zone=$GCP_ZONE
# Upload dashboard
$GCLOUD_SCP "src/dashboard.html" "$GCLOUD_SCP_DEST/static/dashboard.html" --zone=$GCP_ZONE

# Upload .env and models.toml (Always overwrite to push local changes)
if [ -f ".env" ]; then
    echo "   Uploading .env..."
    $GCLOUD_SCP ".env" "$GCLOUD_SCP_DEST/.env" --zone=$GCP_ZONE
fi

if [ -f "models.toml" ]; then
    echo "   Uploading models.toml..."
    $GCLOUD_SCP "models.toml" "$GCLOUD_SCP_DEST/models.toml" --zone=$GCP_ZONE
fi

echo "[4/5] Setting permissions and installing service..."
$GCLOUD_SSH "chmod +x $REMOTE_DIR/axon-api-proxy && \
    CURR_USER=\$(whoami) && \
    sed -i \"s/User=.*/User=\$CURR_USER/; s/Group=.*/Group=\$CURR_USER/\" $REMOTE_DIR/axon-api-proxy.service && \
    sed -i \"s|/home/[^/]*/axon-api-proxy|$REMOTE_DIR|g\" $REMOTE_DIR/axon-api-proxy.service && \
    sudo cp $REMOTE_DIR/axon-api-proxy.service /etc/systemd/system/ && \
    sudo systemctl daemon-reload && \
    sudo systemctl enable axon-api-proxy && \
    sudo systemctl restart axon-api-proxy"

echo "[5/5] Checking service status..."
$GCLOUD_SSH "sudo systemctl status axon-api-proxy --no-pager"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  🎉 Deploy complete! Axon API Proxy is live on GCP."
echo "══════════════════════════════════════════════════════════════"
echo ""
