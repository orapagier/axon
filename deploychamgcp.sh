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
REMOTE_DIR="${CHAM_REMOTE_DIR//$'\r'/}"
DEPLOY_FILE="axon_deploy.tar.gz"
DEPLOY_FILE="${DEPLOY_FILE//$'\r'/}"
DIST_DIR="$ROOT_DIR/dist"
DIST_DIR="${DIST_DIR//$'\r'/}"

# gcloud SSH/SCP helpers
GCLOUD_SSH="gcloud compute ssh $GCP_INSTANCE --zone=$GCP_ZONE --"
GCLOUD_SCP="gcloud compute scp"
GCLOUD_SCP_DEST="$GCP_INSTANCE:$REMOTE_DIR/"

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
    cd "$ROOT_DIR/crates/axon-agent"
    if $CLEAN; then
        echo "  🧹 Cleaning..."
        cargo clean
    fi
    cargo build --release
    echo "  ✅ Axon Agent built"
    echo ""

    echo "🔨 [2/6] Integration services — built into axon-agent (no separate build)"
    echo "  ℹ️  Integration tools (Google/Microsoft/Facebook/Instagram/CRM) run"
    echo "      inside axon-agent — no separate binary or process."
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
    
    # Update crates/axon-agent/static with the new build
    rm -rf "$ROOT_DIR/crates/axon-agent/static"
    mkdir -p "$ROOT_DIR/crates/axon-agent/static"
    cp -r dist/* "$ROOT_DIR/crates/axon-agent/static/"
    echo "  ✅ Axon UI built and synced to crates/axon-agent/static"
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

    # ── Axon Agent binary ──
    if [ -f "target/x86_64-unknown-linux-musl/release/axon" ]; then
        cp target/x86_64-unknown-linux-musl/release/axon "$DIST_DIR/core/"
        echo "  ✅ axon binary (musl) copied"
    elif [ -f "target/release/axon" ]; then
        cp target/release/axon "$DIST_DIR/core/"
        echo "  ✅ axon binary copied"
    else
        echo "  ❌ Error: axon binary not found!"
        exit 1
    fi

    # ── Axon Agent assets ──
    cp -r crates/axon-agent/static "$DIST_DIR/core/"
    cp -r crates/axon-agent/config "$DIST_DIR/core/"
    cp -r crates/axon-agent/tools "$DIST_DIR/core/"
    if [ -d "crates/axon-agent/data" ]; then
        cp -r crates/axon-agent/data "$DIST_DIR/core/"
    fi
    # Copy memory assets but skip locked/local database files
    mkdir -p "$DIST_DIR/core/memory"
    if [ -d "crates/axon-agent/memory" ]; then
        find crates/axon-agent/memory -type f ! -name "*.db" ! -name "*.sqlite" ! -name "*.db-wal" ! -name "*.db-shm" -exec cp {} "$DIST_DIR/core/memory/" \; 2>/dev/null || true
    fi

    [ -f "crates/axon-agent/.env.example" ] && cp crates/axon-agent/.env.example "$DIST_DIR/core/.env.example"
    # ── Canchowlung (cham) instance env ──
    # This server runs a DIFFERENT Telegram bot token from the main instance, so
    # ship canchowlung.env renamed to .env instead of the main .env.
    if [ -f "crates/axon-agent/canchowlung.env" ]; then
        cp crates/axon-agent/canchowlung.env "$DIST_DIR/core/.env"
        echo "  ✅ canchowlung.env copied as .env"
    elif [ -f "crates/axon-agent/.env" ]; then
        cp crates/axon-agent/.env "$DIST_DIR/core/"
        echo "  ⚠️  canchowlung.env not found — fell back to main .env"
    fi

    # ── OAuth credentials for the in-process integrations ──
    # Integrations run inside axon-agent, so ship credentials.json into the
    # agent's working dir where axon_core::Storage looks for it first.
    if [ -f "crates/axon-agent/credentials.json" ]; then
        cp crates/axon-agent/credentials.json "$DIST_DIR/core/"
        echo "  ✅ credentials.json copied into core/"
    elif [ -f "crates/axon-agent/credentials.example.json" ]; then
        cp crates/axon-agent/credentials.example.json "$DIST_DIR/core/credentials.json"
        echo "  ⚠️  credentials.example.json copied as core/credentials.json (update with real values on server)"
    fi
    # NOTE: any AXON_PUBLIC_BASE_URL / AXON_CALLBACK_HOST that lived in a
    # standalone .env must now be present in the agent's core/.env (or set as
    # Instagram settings in the dashboard) for OAuth redirects + IG media URLs.

    # ── Qdrant ──
    if [ -d "qdrant" ]; then
        cp -r qdrant "$DIST_DIR/qdrant/"
        echo "  ✅ qdrant setup scripts copied"
    fi

    # ── TLS reverse proxy (Caddy) — only if CHAM_DOMAIN is set in .deploy.env ──
    # Rendered here (not on the server) so run.sh never needs CHAM_DOMAIN in its
    # own environment — it just ships or doesn't ship a ready-to-use Caddyfile.
    if [ -n "${CHAM_DOMAIN:-}" ]; then
        echo "  🔒 Rendering Caddyfile for $CHAM_DOMAIN..."
        sed -e "s/{\$AXON_DOMAIN}/$CHAM_DOMAIN/g" -e "s/{\$AXON_PORT}/${AXON_PORT:-3000}/g" \
            "$ROOT_DIR/deploy/Caddyfile.example" > "$DIST_DIR/Caddyfile"
        echo "  ✅ Caddyfile rendered"
    else
        echo "  ⚠️  CHAM_DOMAIN not set in .deploy.env — this deploy will be HTTP-only."
        echo "      See README.md 'Deployment' for how to enable TLS via Caddy."
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
# glibc malloc tuning: cap arenas and return freed heap promptly — large RSS
# saving for a multithreaded tokio server on a 1GB box, negligible CPU cost.
Environment=MALLOC_ARENA_MAX=2
Environment=MALLOC_TRIM_THRESHOLD_=131072
ExecStart=$DEPLOY_DIR/core/axon
Restart=always
RestartSec=5
StandardOutput=append:$DEPLOY_DIR/agent.log
StandardError=append:$DEPLOY_DIR/agent.log

[Install]
WantedBy=multi-user.target
SVC"

    # Remove any legacy integration service so it stops consuming RAM and a
    # port on this 1GB box (integrations now run inside axon-agent).
    sudo systemctl disable --now axon-mcp 2>/dev/null || true
    sudo rm -f /etc/systemd/system/axon-mcp.service

    sudo systemctl daemon-reload
    sudo systemctl enable axon-agent
    echo "✅ Service installed and enabled."
}

install_caddy() {
    # No-op if the deploy script didn't render a Caddyfile (CHAM_DOMAIN unset
    # in .deploy.env) — stays additive, doesn't break domain-less deploys.
    if [ ! -f "$DEPLOY_DIR/Caddyfile" ]; then
        return 0
    fi
    echo "🔒 Installing Caddy (TLS reverse proxy)..."
    if ! command -v caddy >/dev/null 2>&1; then
        sudo apt-get update -qq
        sudo apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl
        curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
        curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
        sudo apt-get update -qq
        sudo apt-get install -y -qq caddy
    fi
    sudo cp "$DEPLOY_DIR/Caddyfile" /etc/caddy/Caddyfile
    sudo systemctl enable caddy
    sudo systemctl restart caddy
    echo "✅ Caddy installed and running (TLS via automatic Let's Encrypt)."
}

case "$ACTION" in
    "--install")
        install_service
        install_caddy
        ;;
    "start")
        if systemctl is-active --quiet axon-agent; then
            echo "🔄 Restarting service via systemd..."
            sudo systemctl restart axon-agent
        else
            echo "🚀 Starting service..."
            if [ -f "/etc/systemd/system/axon-agent.service" ]; then
                sudo systemctl start axon-agent
            else
                pkill -f axon || true
                sleep 1
                cd "$DEPLOY_DIR/core" && MALLOC_ARENA_MAX=2 MALLOC_TRIM_THRESHOLD_=131072 ./axon > "$DEPLOY_DIR/agent.log" 2>&1 &
                echo "⚠️ Started in background. Use './run.sh --install' for auto-restart."
            fi
        fi
        echo "📊 Use 'journalctl -u axon-agent -f' for logs."
        ;;
    "stop")
        echo "🛑 Stopping service..."
        sudo systemctl stop axon-agent 2>/dev/null || true
        pkill -f axon || true
        ;;
    "restart")
        $0 stop
        sleep 1
        $0 start
        ;;
    "status")
        systemctl status axon-agent
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

$GCLOUD_SSH "rm -f $REMOTE_DIR/$DEPLOY_FILE"
$GCLOUD_SCP "$ROOT_DIR/$DEPLOY_FILE" "$GCLOUD_SCP_DEST" --zone=$GCP_ZONE
echo "  ✅ Uploaded"
echo ""

echo "🔄 [6/6] Deploying to server..."
$GCLOUD_SSH "bash -s" <<REMOTE
    set -e
    cd $REMOTE_DIR

    echo "  ⏹ Stopping services..."
    sudo systemctl stop axon-agent axon-mcp 2>/dev/null || true
    sleep 1

    # ── Save auth tokens + database before wipe ──
    echo "  🔑 Backing up auth tokens..."
    BACKUP_DIR="/tmp/axon_deploy_backup_\$\$"
    mkdir -p "\$BACKUP_DIR"
    # Newest/authoritative copies live in core/ — list them LAST so they win
    # when an older copy also exists (everything is flattened into one dir).
    for f in $REMOTE_DIR/mcp/tokens.json $REMOTE_DIR/mcp/credentials.json $REMOTE_DIR/mcp/.env \
             $REMOTE_DIR/axon/mcp/tokens.json $REMOTE_DIR/axon/mcp/credentials.json $REMOTE_DIR/axon/mcp/.env \
             $REMOTE_DIR/axon/config/ssh_servers.json \
             $REMOTE_DIR/axon/core/tokens.json $REMOTE_DIR/axon/core/credentials.json \
             $REMOTE_DIR/axon/core/config/ssh_servers.json; do
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

    # ── Restore: preserve server-only state, let local config win ──
    echo "  🔑 Restoring auth tokens..."
    # tokens.json = the server's live OAuth/login session; it only ever exists on
    # the server (never shipped in the bundle), so ALWAYS restore it.
    [ -f "\$BACKUP_DIR/tokens.json" ] && cp -f "\$BACKUP_DIR/tokens.json" core/ 2>/dev/null || true

    # credentials.json + ssh_servers.json = config you edit locally and ship in
    # the bundle, so the LOCAL copy wins and the server always gets your updates.
    # Fall back to the server's backup only when the bundle didn't ship one.
    [ ! -f "core/credentials.json" ] && [ -f "\$BACKUP_DIR/credentials.json" ] && cp -f "\$BACKUP_DIR/credentials.json" core/ 2>/dev/null || true
    [ ! -f "core/config/ssh_servers.json" ] && [ -f "\$BACKUP_DIR/ssh_servers.json" ] && cp -f "\$BACKUP_DIR/ssh_servers.json" core/config/ 2>/dev/null || true

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
    sleep 2

    sudo chmod +x run.sh core/axon
    sudo ./run.sh --install

    echo "  🚀 Starting service..."
    sudo systemctl start axon-agent
    sleep 3

    echo "  📋 Service status:"
    sudo systemctl is-active axon-agent && echo "    ✅ axon-agent: running" || echo "    ❌ axon-agent: failed"
    echo "  ✅ Deploy complete"
REMOTE
echo "  ✅ Services restarted"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  🎉 Deploy complete! Axon is live."
echo "══════════════════════════════════════════════════════════════"
echo ""
echo "Check status:"
echo "  gcloud compute ssh $GCP_INSTANCE --zone=$GCP_ZONE -- 'sudo systemctl status axon-agent'"
