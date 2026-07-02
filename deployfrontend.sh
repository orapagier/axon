#!/bin/bash
set -e

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Configuration (hosts live in untracked .deploy.env) ─────────────────────
[ -f "$ROOT_DIR/.deploy.env" ] && . "$ROOT_DIR/.deploy.env"
: "${CHAM_GCP_INSTANCE:?Set CHAM_GCP_INSTANCE in .deploy.env — see .deploy.env.example}"
: "${CHAM_GCP_ZONE:?Set CHAM_GCP_ZONE in .deploy.env — see .deploy.env.example}"
: "${CHAM_GCP_USER:?Set CHAM_GCP_USER in .deploy.env — see .deploy.env.example}"
: "${CHAM_REMOTE_DIR:?Set CHAM_REMOTE_DIR in .deploy.env — see .deploy.env.example}"
GCP_INSTANCE="$CHAM_GCP_INSTANCE"
GCP_ZONE="$CHAM_GCP_ZONE"
REMOTE_USER="$CHAM_GCP_USER"
REMOTE_DIR="$CHAM_REMOTE_DIR"
FRONTEND_DIR="$ROOT_DIR/axon-ui"
UI_TAR="axon_ui_hotpatch.tar.gz"

echo "══════════════════════════════════════════════════════════════"
echo "  🎨 Axon — Fast Frontend Deploy (no Rust rebuild)"
echo "══════════════════════════════════════════════════════════════"

# 1. Build the Frontend
echo "🔨 [1/4] Building frontend (Vite)..."
cd "$FRONTEND_DIR"

# Auto-fix WSL cross-OS bindings (same as deploygcp.sh)
if [ "$(uname -s)" = "Linux" ] && [ ! -d "node_modules/@rollup/rollup-linux-x64-gnu" ] && [ -d "node_modules" ]; then
    echo "  ⚠️  Cross-OS conflict detected. Reinstalling..."
    mv node_modules node_modules.bak 2>/dev/null || true
    rm -rf node_modules.bak package-lock.json 2>/dev/null || true
    rm -rf node_modules package-lock.json 2>/dev/null || true
fi
if [ ! -d "node_modules" ]; then
    npm install --no-fund --no-audit
fi

npm run build
echo "  ✅ Build complete"

# 2. Sync dist → crates/axon-agent/static (same as main deploy)
echo "📂 [2/4] Syncing to crates/axon-agent/static..."
rm -rf "$ROOT_DIR/crates/axon-agent/static"
mkdir -p "$ROOT_DIR/crates/axon-agent/static"
cp -r dist/* "$ROOT_DIR/crates/axon-agent/static/"
echo "  ✅ Local static synced"

# 3. Bundle and upload
echo "📦 [3/4] Bundling and uploading..."
cd "$ROOT_DIR"
tar -czf "$UI_TAR" -C "$FRONTEND_DIR/dist" .
gcloud compute scp "$UI_TAR" "$GCP_INSTANCE:$REMOTE_DIR/" --zone=$GCP_ZONE
echo "  ✅ Uploaded"

# 4. Extract on server and restart
echo "🔄 [4/4] Deploying on server..."
gcloud compute ssh "$GCP_INSTANCE" --zone="$GCP_ZONE" -- "bash -s" <<REMOTE
    set -e
    cd $REMOTE_DIR

    # Wipe old static and replace with new build
    echo "  🧹 Replacing static files..."
    sudo rm -rf $REMOTE_DIR/axon/core/static
    sudo mkdir -p $REMOTE_DIR/axon/core/static
    sudo tar -xzf $REMOTE_DIR/$UI_TAR -C $REMOTE_DIR/axon/core/static
    sudo chown -R \$(whoami):\$(whoami) $REMOTE_DIR/axon/core/static

    # Cleanup
    rm -f $REMOTE_DIR/$UI_TAR

    # Restart the agent so it picks up the new static files
    echo "  🔄 Restarting axon-agent..."
    sudo systemctl restart axon-agent
    sleep 2
    sudo systemctl is-active axon-agent && echo "  ✅ axon-agent: running" || echo "  ❌ axon-agent: failed"
REMOTE

# Cleanup local tar
rm -f "$UI_TAR"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  🎉 Frontend deployed! Hard-refresh your browser (Ctrl+F5)"
echo "══════════════════════════════════════════════════════════════"
