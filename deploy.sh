#!/bin/bash
set -e

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Configuration ────────────────────────────────────────────────────────────
KEY="$ROOT_DIR/axonserver.key"
TARGET_SERVER="ubuntu@161.118.205.71"
REMOTE_DIR="/home/ubuntu"
DEPLOY_FILE="axon_deploy.tar.gz"
DIST_DIR="$ROOT_DIR/dist"

# ── Parse flags ──────────────────────────────────────────────────────────────
CLEAN=false
SKIP_BUILD=false
SKIP_DEPLOY=false

for arg in "$@"; do
    case "$arg" in
        --clean)       CLEAN=true ;;
        --skip-build)  SKIP_BUILD=true ;;
        --skip-deploy) SKIP_DEPLOY=true ;;
        --help)
            echo "Usage: bash deploy.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --clean        cargo clean before building (full rebuild)"
            echo "  --skip-build   skip build + bundle, deploy existing tar.gz"
            echo "  --skip-deploy  build + bundle only, no deploy to server"
            echo "  --help         show this help"
            exit 0
            ;;
    esac
done

echo "══════════════════════════════════════════════════════════════"
echo "  � Axon — Build, Bundle & Deploy"
echo "══════════════════════════════════════════════════════════════"
echo ""

if ! $SKIP_BUILD; then

    # ── Step 1: Build ────────────────────────────────────────────────────────
    rm -f "$ROOT_DIR/$DEPLOY_FILE"

    echo "� [1/5] Building Axon Agent (release)..."
    cd "$ROOT_DIR/axon"
    if $CLEAN; then
        echo "  🧹 Cleaning..."
        cargo clean
    fi
    cargo build --release
    echo "  ✅ Axon Agent built"
    echo ""

    echo "� [2/5] Building Axon MCP Server (release)..."
    cd "$ROOT_DIR/axon-mcp-server"
    if $CLEAN; then
        echo "  🧹 Cleaning..."
        cargo clean
    fi
    cargo build --release
    echo "  ✅ Axon MCP Server built"
    echo ""

    # ── Step 2: Bundle ───────────────────────────────────────────────────────
    echo "📦 [3/5] Creating deployment bundle..."
    cd "$ROOT_DIR"

    rm -rf "$DIST_DIR"
    mkdir -p "$DIST_DIR/axon"
    mkdir -p "$DIST_DIR/mcp"

    # ── Axon Agent binary ──
    if [ -f "axon/target/x86_64-unknown-linux-musl/release/axon" ]; then
        cp axon/target/x86_64-unknown-linux-musl/release/axon "$DIST_DIR/axon/"
        echo "  ✅ axon binary (musl) copied"
    elif [ -f "axon/target/release/axon" ]; then
        cp axon/target/release/axon "$DIST_DIR/axon/"
        echo "  ✅ axon binary copied"
    else
        echo "  ❌ Error: axon binary not found!"
        exit 1
    fi

    # ── Axon Agent assets ──
    cp -r axon/static "$DIST_DIR/axon/"
    cp -r axon/config "$DIST_DIR/axon/"
    cp -r axon/memory "$DIST_DIR/axon/"
    cp -r axon/tools "$DIST_DIR/axon/"
    rm -f "$DIST_DIR/axon/memory/"*.db

    cp axon/.env.example "$DIST_DIR/axon/.env.example"
    if [ -f "axon/.env" ]; then
        cp axon/.env "$DIST_DIR/axon/"
        echo "  ✅ .env copied"
    fi

    # ── MCP Server binary ──
    if [ -f "axon-mcp-server/target/x86_64-unknown-linux-musl/release/axon-mcp" ]; then
        cp axon-mcp-server/target/x86_64-unknown-linux-musl/release/axon-mcp "$DIST_DIR/mcp/"
        echo "  ✅ axon-mcp binary (musl) copied"
    elif [ -f "axon-mcp-server/target/release/axon-mcp" ]; then
        cp axon-mcp-server/target/release/axon-mcp "$DIST_DIR/mcp/"
        echo "  ✅ axon-mcp binary copied"
    else
        echo "  ❌ Error: axon-mcp binary not found!"
        exit 1
    fi

    # ── MCP Server assets ──
    if [ -f "axon-mcp-server/credentials.json" ]; then
        cp axon-mcp-server/credentials.json "$DIST_DIR/mcp/"
        echo "  ✅ credentials.json copied"
    fi
    if [ -f "axon/.env" ]; then
        cp axon/.env "$DIST_DIR/mcp/"
    fi

    # ── run.sh (systemd service manager) ──
    cat <<'EOF' > "$DIST_DIR/run.sh"
#!/bin/bash
DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ACTION=${1:-"start"}

install_service() {
    echo "⚙️ Installing systemd services..."
    USER_NAME=$(whoami)

    sudo bash -c "cat <<SVC > /etc/systemd/system/axon-agent.service
[Unit]
Description=Axon Agent
After=network.target

[Service]
Type=simple
User=$USER_NAME
WorkingDirectory=$DEPLOY_DIR/axon
ExecStart=$DEPLOY_DIR/axon/axon
Restart=always
RestartSec=5
StandardOutput=append:$DEPLOY_DIR/agent.log
StandardError=append:$DEPLOY_DIR/agent.log

[Install]
WantedBy=multi-user.target
SVC"

    sudo bash -c "cat <<SVC > /etc/systemd/system/axon-mcp.service
[Unit]
Description=Axon MCP Server
After=network.target

[Service]
Type=simple
User=$USER_NAME
WorkingDirectory=$DEPLOY_DIR/mcp
ExecStart=$DEPLOY_DIR/mcp/axon-mcp
Restart=always
RestartSec=5
StandardOutput=append:$DEPLOY_DIR/mcp.log
StandardError=append:$DEPLOY_DIR/mcp.log

[Install]
WantedBy=multi-user.target
SVC"

    sudo systemctl daemon-reload
    sudo systemctl enable axon-agent axon-mcp
    echo "✅ Services installed and enabled."
}

case "$ACTION" in
    "--install")
        install_service
        ;;
    "start")
        if systemctl is-active --quiet axon-agent; then
            echo "🔄 Restarting services via systemd..."
            sudo systemctl restart axon-agent axon-mcp
        else
            echo "🚀 Starting services..."
            if [ -f "/etc/systemd/system/axon-agent.service" ]; then
                sudo systemctl start axon-agent axon-mcp
            else
                pkill -f axon-mcp || true
                pkill -f axon || true
                sleep 1
                cd "$DEPLOY_DIR/mcp" && ./axon-mcp < /dev/null > "$DEPLOY_DIR/mcp.log" 2>&1 &
                cd "$DEPLOY_DIR/axon" && ./axon > "$DEPLOY_DIR/agent.log" 2>&1 &
                echo "⚠️ Started in background. Use './run.sh --install' for auto-restart."
            fi
        fi
        echo "📊 Use 'journalctl -u axon-agent -f' for logs."
        ;;
    "stop")
        echo "🛑 Stopping services..."
        sudo systemctl stop axon-agent axon-mcp 2>/dev/null || true
        pkill -f axon-mcp || true
        pkill -f axon || true
        ;;
    "restart")
        $0 stop
        sleep 1
        $0 start
        ;;
    "status")
        systemctl status axon-agent axon-mcp
        ;;
    *)
        echo "Usage: ./run.sh [start|stop|restart|status|--install]"
        exit 1
        ;;
esac

echo "🛡️ Verifying SSH permissions..."
sudo chown -R ubuntu:ubuntu /home/ubuntu/.ssh 2>/dev/null || true
sudo chmod 755 /home/ubuntu 2>/dev/null || true
sudo chmod 700 /home/ubuntu/.ssh 2>/dev/null || true
sudo chmod 600 /home/ubuntu/.ssh/authorized_keys 2>/dev/null || true
EOF
    chmod +x "$DIST_DIR/run.sh"

    # ── Create archive ──
    echo "  🗜️ Creating $DEPLOY_FILE..."
    tar -czf "$ROOT_DIR/$DEPLOY_FILE" -C "$DIST_DIR" .
    echo "  ✅ Bundle created"
    echo ""

else
    echo "⏭️  Skipping build (--skip-build)..."
    echo ""
    if [ ! -f "$ROOT_DIR/$DEPLOY_FILE" ]; then
        echo "❌ Error: $DEPLOY_FILE not found. Run without --skip-build first."
        exit 1
    fi
fi

# ── Step 3: Deploy ───────────────────────────────────────────────────────────
if $SKIP_DEPLOY; then
    echo "⏭️  Skipping deploy (--skip-deploy)"
    echo ""
    echo "✅ Build + bundle complete! File: $DEPLOY_FILE"
    exit 0
fi

echo "🌐 [4/5] Uploading to server..."
chmod 600 "$KEY" 2>/dev/null || true

ssh -i "$KEY" "$TARGET_SERVER" "rm -f $REMOTE_DIR/$DEPLOY_FILE"
scp -i "$KEY" "$ROOT_DIR/$DEPLOY_FILE" "${TARGET_SERVER}:${REMOTE_DIR}/"
echo "  ✅ Uploaded"
echo ""

echo "🔄 [5/5] Extracting and restarting services..."
ssh -i "$KEY" "$TARGET_SERVER" "\
    cd $REMOTE_DIR && \
    sudo tar -xzf $DEPLOY_FILE && \
    sudo chmod +x run.sh && \
    sudo ./run.sh --install && \
    sudo ./run.sh restart"
echo "  ✅ Services restarted"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  🎉 Deploy complete! Axon is live."
echo "══════════════════════════════════════════════════════════════"
echo ""
echo "Check status:"
echo "  ssh -i $KEY $TARGET_SERVER 'sudo systemctl status axon-agent'"
echo "  ssh -i $KEY $TARGET_SERVER 'sudo systemctl status axon-mcp'"
