#!/usr/bin/env bash
# =============================================================================
#  Axon — build a SANITIZED Linux release bundle for GitHub Releases
# -----------------------------------------------------------------------------
#  Produces:  axon-linux-x86_64.tar.gz
#     ./core/axon              (static musl binary — portable across distros)
#     ./core/static            (built Vue dashboard)
#     ./core/config            (models.toml + config, minus ssh secrets)
#     ./core/tools             (empty — populated at runtime)
#     ./core/memory            (schema only, no *.db)
#     ./core/.env.example
#     ./core/credentials.example.json
#     ./qdrant/*               (Qdrant setup scripts)
#     ./run.sh                 (systemd service manager)
#
#  Unlike the private deploy*.sh scripts, this ships NO real .env, NO
#  credentials.json, and NO SSH keys — it is safe to publish publicly.
#
#  Run from the repo root (under WSL or Git Bash). Needs: node/npm, and either
#  `cross` (Docker) or the x86_64-unknown-linux-musl Rust target.
#     bash scripts/package-release.sh
# =============================================================================
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"
DIST="$ROOT_DIR/dist-release"
OUT="$ROOT_DIR/axon-linux-x86_64.tar.gz"
TARGET="x86_64-unknown-linux-musl"

G='\033[0;32m'; Y='\033[1;33m'; C='\033[0;36m'; B='\033[1m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
info() { echo -e "${C}[→]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
step() { echo -e "\n${B}━━━ $* ━━━${N}"; }

# ── 1. Build the dashboard (Vue) ─────────────────────────────────────────────
step "Building dashboard (axon-ui)"
pushd axon-ui >/dev/null
if [ ! -d node_modules ]; then
    info "npm ci ..."
    npm ci --no-fund --no-audit
fi
AXON_NODE_TYPES_OUT="$ROOT_DIR/crates/axon-agent/assets/node_types.json" npm run build
popd >/dev/null
rm -rf crates/axon-agent/static
mkdir -p crates/axon-agent/static
cp -r axon-ui/dist/* crates/axon-agent/static/
log "Dashboard built and synced into crates/axon-agent/static"

# ── 2. Build the agent (static musl binary) ──────────────────────────────────
step "Building agent (release, $TARGET)"
BIN=""
INSTALLED_TARGETS="$(rustup target list --installed 2>/dev/null || true)"
if command -v cross >/dev/null 2>&1; then
    info "Using cross (Docker) ..."
    cross build --release --target "$TARGET" -p axon
    BIN="target/$TARGET/release/axon"
else
    case "$INSTALLED_TARGETS" in
        *"$TARGET"*)
            info "Using cargo with target $TARGET ..."
            cargo build --release --target "$TARGET" -p axon
            BIN="target/$TARGET/release/axon" ;;
        *)
            warn "No musl toolchain — falling back to a native release build."
            warn "The resulting binary is NOT statically linked and needs a matching glibc."
            cargo build --release -p axon
            BIN="target/release/axon" ;;
    esac
fi
[ -f "$BIN" ] || { echo "Build did not produce $BIN"; exit 1; }
log "Binary: $BIN"

# ── 3. Assemble the sanitized bundle ─────────────────────────────────────────
step "Assembling bundle"
rm -rf "$DIST"; mkdir -p "$DIST/core"
cp "$BIN" "$DIST/core/axon"
cp -r crates/axon-agent/static "$DIST/core/"
cp -r crates/axon-agent/config "$DIST/core/"
# Strip any local SSH secrets that may sit under config/.
rm -rf "$DIST/core/config/ssh_keys" "$DIST/core/config/ssh_servers.json"
mkdir -p "$DIST/core/tools" "$DIST/core/memory"
# Ship memory schema/assets but never a database file.
if [ -d crates/axon-agent/memory ]; then
    find crates/axon-agent/memory -type f \
        ! -name '*.db' ! -name '*.db-wal' ! -name '*.db-shm' ! -name '*.sqlite*' \
        -exec cp {} "$DIST/core/memory/" \; 2>/dev/null || true
fi
cp crates/axon-agent/.env.example "$DIST/core/.env.example"

# A safe stub so users know the credentials.json shape (real one is a secret).
cat > "$DIST/core/credentials.example.json" <<'JSON'
{
  "note": "OAuth app client IDs/secrets for in-process integrations. Copy to credentials.json and fill in, OR add them from the dashboard Services page.",
  "google":    { "client_id": "", "client_secret": "" },
  "microsoft": { "client_id": "", "client_secret": "" },
  "facebook":  { "app_id": "", "app_secret": "" }
}
JSON

# Qdrant setup scripts.
[ -d qdrant ] && cp -r qdrant "$DIST/qdrant"

# run.sh — the systemd service manager shipped in the bundle.
cat > "$DIST/run.sh" <<'RUNSH'
#!/bin/bash
DEPLOY_DIR="$(cd "$(dirname "$0")" && pwd)"
ACTION=${1:-"start"}
CURR_USER=$(whoami)

install_service() {
    echo "⚙️ Installing systemd service..."
    sudo bash -c "cat <<SVC > /etc/systemd/system/axon-agent.service
[Unit]
Description=Axon Agent
After=network.target

[Service]
Type=simple
User=$CURR_USER
WorkingDirectory=$DEPLOY_DIR/core
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
    sudo systemctl daemon-reload
    sudo systemctl enable axon-agent
    echo "✅ Service installed and enabled."
}

case "$ACTION" in
    "--install") install_service ;;
    "start")
        if systemctl is-active --quiet axon-agent; then
            sudo systemctl restart axon-agent
        elif [ -f "/etc/systemd/system/axon-agent.service" ]; then
            sudo systemctl start axon-agent
        else
            pkill -f 'core/axon' || true; sleep 1
            cd "$DEPLOY_DIR/core" && MALLOC_ARENA_MAX=2 ./axon > "$DEPLOY_DIR/agent.log" 2>&1 &
            echo "⚠️ Started in background. Use './run.sh --install' for auto-restart."
        fi
        echo "📊 Logs: journalctl -u axon-agent -f   (or tail $DEPLOY_DIR/agent.log)"
        ;;
    "stop")    sudo systemctl stop axon-agent 2>/dev/null || true; pkill -f 'core/axon' || true ;;
    "restart") $0 stop; sleep 1; $0 start ;;
    "status")  systemctl status axon-agent ;;
    *) echo "Usage: ./run.sh [start|stop|restart|status|--install]"; exit 1 ;;
esac
RUNSH
chmod +x "$DIST/run.sh"

# ── 4. Archive ───────────────────────────────────────────────────────────────
step "Creating archive"
rm -f "$OUT"
tar -czf "$OUT" -C "$DIST" .

# ── 5. Verify no secrets leaked in ───────────────────────────────────────────
# Capture matches (no `grep -q`: it closes the pipe early and, under pipefail,
# a found secret could read as "not found" — the exact failure we must not have).
SECRET_HITS="$(tar -tzf "$OUT" | grep -E '(^|/)\.env$|(^|/)credentials\.json$|ssh_keys/|(^|/)tokens\.json$|\.key$' || true)"
if [ -n "$SECRET_HITS" ]; then
    echo -e "${Y}[!] SECRET DETECTED in the archive — aborting. Do NOT upload.${N}"
    echo "$SECRET_HITS"
    exit 1
fi

echo ""
log "Clean bundle ready: $OUT  ($(du -h "$OUT" | cut -f1))"
info "Upload this to the GitHub release as: axon-linux-x86_64.tar.gz"
