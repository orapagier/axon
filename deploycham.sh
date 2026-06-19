#!/bin/bash
set -e

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Configuration ────────────────────────────────────────────────────────────
TARGET_SERVER="canchowlung@34.61.3.40"
TARGET_SERVER="${TARGET_SERVER//$'\r'/}"
REMOTE_DIR="/home/canchowlung"
REMOTE_DIR="${REMOTE_DIR//$'\r'/}"
DEPLOY_FILE="axon_deploy.tar.gz"
DEPLOY_FILE="${DEPLOY_FILE//$'\r'/}"
DIST_DIR="$ROOT_DIR/dist"
DIST_DIR="${DIST_DIR//$'\r'/}"

# SSH options: using default SSH agent/keys
SSH_OPTS=""

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
echo "  🚀 Axon — Build, Bundle & Deploy"
echo "══════════════════════════════════════════════════════════════"
echo ""

if ! $SKIP_BUILD; then

    # ── Step 1: Build ────────────────────────────────────────────────────────
    if [ -f "$ROOT_DIR/$DEPLOY_FILE" ]; then
        mv "$ROOT_DIR/$DEPLOY_FILE" "$ROOT_DIR/$DEPLOY_FILE.old" 2>/dev/null || true
        rm -f "$ROOT_DIR/$DEPLOY_FILE.old" 2>/dev/null || true
        if [ -f "$ROOT_DIR/$DEPLOY_FILE" ]; then
            powershell.exe -Command "Remove-Item -Path '$ROOT_DIR/$DEPLOY_FILE' -Force" 2>/dev/null || true
        fi
    fi

    echo "🔨 [1/6] Building Axon Agent (release)..."
    cd "$ROOT_DIR/axon-agent"
    if $CLEAN; then
        echo "  🧹 Cleaning..."
        cargo clean
    fi
    cargo build --release
    echo "  ✅ Axon Agent built"
    echo ""

    echo "🔨 [2/6] Building Axon MCP Server (release)..."
    cd "$ROOT_DIR/axon-mcp-server"
    if $CLEAN; then
        echo "  🧹 Cleaning..."
        cargo clean
    fi
    cargo build --release
    echo "  ✅ Axon MCP Server built"
    echo ""

    echo "🔨 [3/6] Building Axon UI (Vue)..."
    cd "$ROOT_DIR/axon-ui"
    if $CLEAN; then
        echo "  🧹 Cleaning node_modules and dist..."
        rm -rf node_modules package-lock.json dist
    fi
    
    # Auto-fix Windows/WSL conflict: if running on Linux but missing Linux Rollup binaries
    if [ "$(uname -s)" = "Linux" ] && [ ! -d "node_modules/@rollup/rollup-linux-x64-gnu" ] && [ -d "node_modules" ]; then
        echo "  ⚠️  Cross-OS conflict detected (missing Linux native bindings). Reinstalling frontend dependencies..."
        # Use mv as a workaround for Windows file locks in WSL
        mv node_modules node_modules.bak 2>/dev/null || true
        rm -rf node_modules.bak package-lock.json 2>/dev/null || true
        rm -rf node_modules package-lock.json 2>/dev/null || true
    fi

    if [ ! -d "node_modules" ]; then
        npm install --no-fund --no-audit
    fi
    npm run build
    
    # Update axon-agent/static with the new build
    rm -rf "$ROOT_DIR/axon-agent/static"
    mkdir -p "$ROOT_DIR/axon-agent/static"
    cp -r dist/* "$ROOT_DIR/axon-agent/static/"
    echo "  ✅ Axon UI built and synced to axon-agent/static"
    echo ""

    # ── Step 2: Bundle ───────────────────────────────────────────────────────
    echo "📦 [4/6] Creating deployment bundle..."
    cd "$ROOT_DIR"

    # Use a robust cleanup to handle Windows/OneDrive file locks
    if [ -d "$DIST_DIR" ]; then
        mv "$DIST_DIR" "$DIST_DIR.old" 2>/dev/null || true
        rm -rf "$DIST_DIR.old" 2>/dev/null || true
        if [ -d "$DIST_DIR" ]; then
            powershell.exe -Command "Remove-Item -Path '$DIST_DIR' -Recurse -Force" 2>/dev/null || true
        fi
    fi
    mkdir -p "$DIST_DIR/core"
    mkdir -p "$DIST_DIR/mcp"

    # ── Axon Agent binary ──
    if [ -f "axon-agent/target/x86_64-unknown-linux-musl/release/axon" ]; then
        cp axon-agent/target/x86_64-unknown-linux-musl/release/axon "$DIST_DIR/core/"
        echo "  ✅ axon binary (musl) copied"
    elif [ -f "axon-agent/target/release/axon" ]; then
        cp axon-agent/target/release/axon "$DIST_DIR/core/"
        echo "  ✅ axon binary copied"
    else
        echo "  ❌ Error: axon binary not found!"
        exit 1
    fi

    # ── Axon Agent assets ──
    cp -r axon-agent/static "$DIST_DIR/core/"
    cp -r axon-agent/config "$DIST_DIR/core/"
    cp -r axon-agent/tools "$DIST_DIR/core/"
    if [ -d "axon-agent/data" ]; then
        cp -r axon-agent/data "$DIST_DIR/core/"
    fi
    # Copy memory assets but skip locked/local database files
    mkdir -p "$DIST_DIR/core/memory"
    if [ -d "axon-agent/memory" ]; then
        find axon-agent/memory -type f ! -name "*.db" ! -name "*.sqlite" ! -name "*.db-wal" ! -name "*.db-shm" -exec cp {} "$DIST_DIR/core/memory/" \; 2>/dev/null || true
    fi

    [ -f "axon-agent/.env.example" ] && cp axon-agent/.env.example "$DIST_DIR/core/.env.example"
    if [ -f "axon-agent/.env" ]; then
        cp axon-agent/.env "$DIST_DIR/core/"
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
    elif [ -f "axon-mcp-server/credentials.example.json" ]; then
        cp axon-mcp-server/credentials.example.json "$DIST_DIR/mcp/credentials.json"
        echo "  ⚠️  credentials.example.json copied as credentials.json (update with real values on server)"
    fi
    if [ -f "axon-mcp-server/.env" ]; then
        cp axon-mcp-server/.env "$DIST_DIR/mcp/"
        echo "  [ok] axon-mcp .env copied"
    elif [ -f "axon-mcp-server/.env.example" ]; then
        cp axon-mcp-server/.env.example "$DIST_DIR/mcp/.env.example"
        echo "  [warn] axon-mcp .env.example copied (create .env on server if needed)"
    fi

    # ── Qdrant ──
    if [ -d "qdrant" ]; then
        cp -r qdrant "$DIST_DIR/qdrant/"
        echo "  ✅ qdrant setup scripts copied"
    fi

    # ── run.sh (systemd service manager) ──
    cat <<'EOF' > "$DIST_DIR/run.sh"
#!/bin/bash
DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ACTION=${1:-"start"}
CURR_USER=$(whoami)
HOME_DIR="$HOME"

install_service() {
    echo "⚙️ Installing systemd services..."

    sudo bash -c "cat <<SVC > /etc/systemd/system/axon-agent.service
[Unit]
Description=Axon Agent
After=network.target

[Service]
Type=simple
User=$CURR_USER
WorkingDirectory=$DEPLOY_DIR/core
ExecStart=$DEPLOY_DIR/core/axon
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
User=$CURR_USER
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
                cd "$DEPLOY_DIR/core" && ./axon > "$DEPLOY_DIR/agent.log" 2>&1 &
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
sudo chown -R "$CURR_USER:$CURR_USER" "$HOME_DIR/.ssh" 2>/dev/null || true
sudo chmod 755 "$HOME_DIR" 2>/dev/null || true
sudo chmod 700 "$HOME_DIR/.ssh" 2>/dev/null || true
sudo chmod 600 "$HOME_DIR/.ssh/authorized_keys" 2>/dev/null || true
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

echo "🌐 [5/6] Uploading to server..."

ssh $SSH_OPTS "$TARGET_SERVER" "sudo rm -f $REMOTE_DIR/$DEPLOY_FILE"
scp $SSH_OPTS "$ROOT_DIR/$DEPLOY_FILE" "${TARGET_SERVER}:${REMOTE_DIR}/"
echo "  ✅ Uploaded"
echo ""

echo "🔄 [6/6] Deploying to server..."
ssh $SSH_OPTS "$TARGET_SERVER" "bash -s" <<REMOTE
    set -e
    cd $REMOTE_DIR

    echo "  ⏹ Stopping services..."
    sudo systemctl stop axon-agent axon-mcp 2>/dev/null || true
    sleep 1

    # ── Save auth tokens + database before wipe ──
    echo "  🔑 Backing up auth tokens..."
    BACKUP_DIR="/tmp/axon_deploy_backup_\$\$"
    mkdir -p "\$BACKUP_DIR"
    for f in $REMOTE_DIR/axon/mcp/tokens.json $REMOTE_DIR/axon/mcp/credentials.json \
             $REMOTE_DIR/axon/mcp/.env \
             $REMOTE_DIR/mcp/tokens.json $REMOTE_DIR/mcp/credentials.json \
             $REMOTE_DIR/mcp/.env \
             $REMOTE_DIR/axon/core/config/ssh_servers.json \
             $REMOTE_DIR/axon/config/ssh_servers.json; do
        [ -f "\$f" ] && cp -f "\$f" "\$BACKUP_DIR/" 2>/dev/null || true
    done

    # ── Backup SQLite database (axon.db + WAL/SHM) ──
    echo "  💾 Backing up database..."
    DB_BACKUP_DIR="\$BACKUP_DIR/db"
    mkdir -p "\$DB_BACKUP_DIR"
    for db_dir in $REMOTE_DIR/axon/core/memory $REMOTE_DIR/axon/memory; do
        if [ -d "\$db_dir" ]; then
            for ext in db db-wal db-shm sqlite sqlite-wal sqlite-shm; do
                for dbf in \$db_dir/*.\$ext; do
                    [ -f "\$dbf" ] && cp -f "\$dbf" "\$DB_BACKUP_DIR/" 2>/dev/null || true
                done
            done
            echo "    ✅ Database backed up from \$db_dir"
            break
        fi
    done

    # ── Wipe old deployment (database will be restored) ──
    echo "  🧹 Wiping $REMOTE_DIR/axon/ ..."
    sudo rm -rf $REMOTE_DIR/mcp $REMOTE_DIR/qdrant $REMOTE_DIR/run.sh
    sudo rm -rf $REMOTE_DIR/axon
    mkdir -p $REMOTE_DIR/axon

    mv $REMOTE_DIR/$DEPLOY_FILE $REMOTE_DIR/axon/
    cd $REMOTE_DIR/axon

    # ── Extract new files ──
    echo "  📦 Extracting new deployment..."
    sudo tar -xzf $DEPLOY_FILE
    sudo chown -R \$(whoami):\$(whoami) . 2>/dev/null || true

    # ── Install Qdrant ──
    if [ -d "qdrant" ]; then
        echo "  🗄️ Installing/Verifying Qdrant..."
        sudo chmod +x qdrant/install.sh
        (cd qdrant && bash install.sh)
    fi

    # ── Restore auth tokens ──
    echo "  🔑 Restoring auth tokens..."
    # Always restore tokens.json (contains user login session)
    [ -f "\$BACKUP_DIR/tokens.json" ] && cp -f "\$BACKUP_DIR/tokens.json" mcp/ 2>/dev/null || true
    
    # Only restore credentials and .env if they are MISSING from the new bundle
    # This allows local dev updates to "take over" on the server
    [ ! -f "mcp/credentials.json" ] && [ -f "\$BACKUP_DIR/credentials.json" ] && cp -f "\$BACKUP_DIR/credentials.json" mcp/ 2>/dev/null || true
    [ ! -f "mcp/.env" ] && [ -f "\$BACKUP_DIR/.env" ] && cp -f "\$BACKUP_DIR/.env" mcp/ 2>/dev/null || true
    
    [ -f "\$BACKUP_DIR/ssh_servers.json" ] && [ ! -f "core/config/ssh_servers.json" ] && cp -f "\$BACKUP_DIR/ssh_servers.json" core/config/ 2>/dev/null || true

    # ── Restore database ──
    DB_BACKUP_DIR="\$BACKUP_DIR/db"
    if [ -d "\$DB_BACKUP_DIR" ] && ls \$DB_BACKUP_DIR/*.db \$DB_BACKUP_DIR/*.sqlite 2>/dev/null | head -1 >/dev/null 2>&1; then
        echo "  💾 Restoring database..."
        mkdir -p core/memory
        for dbf in \$DB_BACKUP_DIR/*; do
            [ -f "\$dbf" ] && cp -f "\$dbf" core/memory/ 2>/dev/null || true
        done
        echo "    ✅ Database restored to core/memory/"
    else
        echo "  ⚠️  No database backup found — starting fresh"
    fi

    rm -rf "\$BACKUP_DIR"

    # ── Restart ──
    echo "  🛑 Ensuring all old processes are stopped..."
    sudo systemctl stop axon-agent axon-mcp 2>/dev/null || true
    sudo pkill -9 -f axon-mcp 2>/dev/null || true
    sudo pkill -9 -f 'core/axon' 2>/dev/null || true
    sudo fuser -k 3000/tcp 2>/dev/null || true
    sudo fuser -k 8080/tcp 2>/dev/null || true
    sleep 2

    sudo chmod +x run.sh core/axon mcp/axon-mcp
    sudo ./run.sh --install

    echo "  🚀 Starting services..."
    sudo systemctl start axon-mcp
    sleep 1
    sudo systemctl start axon-agent
    sleep 3

    echo "  📋 Service status:"
    sudo systemctl is-active axon-agent && echo "    ✅ axon-agent: running" || echo "    ❌ axon-agent: failed"
    sudo systemctl is-active axon-mcp && echo "    ✅ axon-mcp: running" || echo "    ❌ axon-mcp: failed"
    echo "  ✅ Deploy complete"
REMOTE
echo "  ✅ Services restarted"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  🎉 Deploy complete! Axon is live."
echo "══════════════════════════════════════════════════════════════"
echo ""
echo "Check status:"
echo "  ssh $SSH_OPTS $TARGET_SERVER 'sudo systemctl status axon-agent'"
echo "  ssh $SSH_OPTS $TARGET_SERVER 'sudo systemctl status axon-mcp'"
