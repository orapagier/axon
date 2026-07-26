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
#     ./core/bin/wake-model-builder   (wake-word enrollment companion binary,
#                                       if the build succeeded — optional)
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

R='\033[0;31m'; G='\033[0;32m'; Y='\033[1;33m'; C='\033[0;36m'; B='\033[1m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
info() { echo -e "${C}[→]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
err()  { echo -e "${R}[✗]${N} $*" >&2; exit 1; }
step() { echo -e "\n${B}━━━ $* ━━━${N}"; }

# ── Flags ────────────────────────────────────────────────────────────────────
# BUILD_MODE: auto (prefer musl, fall back to glibc) | musl (require) | native (glibc)
BUILD_MODE="auto"
for arg in "$@"; do
    case "$arg" in
        --musl)   BUILD_MODE="musl" ;;
        --native) BUILD_MODE="native" ;;
        --help|-h)
            echo "Usage: bash scripts/package-release.sh [--musl|--native]"
            echo "  (default) auto : static musl if available, else native glibc"
            echo "  --musl         : require a static musl build (fails if not set up)"
            echo "  --native       : native glibc build (works on glibc >= this host's)"
            exit 0 ;;
        *) err "Unknown option: $arg (try --help)" ;;
    esac
done

# ── 1. Build the dashboard (Vue) ─────────────────────────────────────────────
step "Building dashboard (axon-ui)"
UI_SRC="$ROOT_DIR/axon-ui"
UI_BUILD="$UI_SRC"
# When the repo lives on a Windows mount (/mnt/... under WSL), axon-ui/node_modules
# is shared with Windows and holds the wrong platform's native rollup/esbuild binary
# (e.g. rollup-win32-x64-msvc, not rollup-linux-x64-gnu) — a Linux build then fails
# with "Cannot find module @rollup/rollup-linux-x64-gnu". Build the UI on the Linux
# filesystem in an isolated dir with its own Linux node_modules — the same trick
# deployaxongcp.sh uses. Sources are mirrored with tar (no rsync dependency); the
# isolated node_modules/dist are preserved across runs.
case "$ROOT_DIR" in
    /mnt/*)
        if [ "$(uname -s)" = "Linux" ]; then
            UI_BUILD="${XDG_CACHE_HOME:-$HOME/.cache}/axon-ui-build"
            info "Windows-mounted repo — building UI on the Linux filesystem:"
            info "  $UI_BUILD"
            mkdir -p "$UI_BUILD"
            ( cd "$UI_SRC" && tar --exclude=node_modules --exclude=dist --exclude=graphify-out -cf - . ) \
                | ( cd "$UI_BUILD" && tar -xf - )
        fi ;;
esac

pushd "$UI_BUILD" >/dev/null
# npm ci is expensive; run it only when the lockfile actually changed.
NM_STAMP="node_modules/.axon-install-stamp"
if [ ! -d node_modules ] || ! cmp -s package-lock.json "$NM_STAMP" 2>/dev/null; then
    info "npm ci ..."
    npm ci --no-fund --no-audit
    cp package-lock.json "$NM_STAMP" 2>/dev/null || true
fi
AXON_NODE_TYPES_OUT="$ROOT_DIR/crates/axon-agent/assets/node_types.json" npm run build
popd >/dev/null
rm -rf "$ROOT_DIR/crates/axon-agent/static"
mkdir -p "$ROOT_DIR/crates/axon-agent/static"
cp -r "$UI_BUILD/dist/"* "$ROOT_DIR/crates/axon-agent/static/"
log "Dashboard built and synced into crates/axon-agent/static"

# ── 2. Build the agent ───────────────────────────────────────────────────────
# Prefer a static musl binary (runs on ANY Linux, any glibc version — ideal for a
# public release). This project builds cleanly for musl: TLS is rustls+ring (no
# openssl), and the only C dependency, libsqlite3-sys, is bundled and compiles
# with musl-gcc. Fall back to a native glibc build if the musl toolchain isn't set
# up — that binary works, but only on glibc >= the build host's (e.g. built on
# Ubuntu 24.04 it won't run on 22.04). To force one or the other: --musl / --native.
step "Building agent (release)"
BIN=""
INSTALLED_TARGETS="$(rustup target list --installed 2>/dev/null || true)"
docker_up() { docker info >/dev/null 2>&1; }
have_musl_target() { case "$INSTALLED_TARGETS" in *"$TARGET"*) return 0;; *) return 1;; esac; }

build_glibc() {
    warn "Building a NATIVE glibc binary (not statically linked)."
    warn "It needs glibc >= this host's version, so users on older distros may see"
    warn "  \"version 'GLIBC_2.xx' not found\". For a run-anywhere binary, enable musl:"
    warn "      rustup target add $TARGET && sudo apt-get install -y musl-tools"
    warn "  then re-run this script (or pass --musl)."
    cargo build --release -p axon
    BIN="target/release/axon"
}

if [ "$BUILD_MODE" != "native" ] && command -v cross >/dev/null 2>&1 && docker_up; then
    info "Using cross (Docker) → static musl ..."
    cross build --release --target "$TARGET" -p axon
    BIN="target/$TARGET/release/axon"
elif [ "$BUILD_MODE" != "native" ] && have_musl_target && command -v musl-gcc >/dev/null 2>&1; then
    info "Using cargo target $TARGET → static musl ..."
    cargo build --release --target "$TARGET" -p axon
    BIN="target/$TARGET/release/axon"
elif [ "$BUILD_MODE" = "musl" ]; then
    err "--musl requested but the musl toolchain isn't ready. Set it up with:
      rustup target add $TARGET && sudo apt-get install -y musl-tools"
else
    if [ "$BUILD_MODE" != "native" ] && have_musl_target && ! command -v musl-gcc >/dev/null 2>&1; then
        warn "musl target is installed but 'musl-gcc' is missing (needed to link libsqlite3-sys)."
        warn "Install it with:  sudo apt-get install -y musl-tools"
    fi
    build_glibc
fi
[ -f "$BIN" ] || { echo "Build did not produce $BIN"; exit 1; }
log "Binary: $BIN"

# ── 2b. Build wake-model-builder (wake-word enrollment companion binary) ─────
# Standalone crate with its own workspace — see wake-model-builder/Cargo.toml
# for why (rustpotter's candle-core dependency pin conflicts with axon-agent's
# own tree, so axon-agent shells out to this binary instead of linking
# rustpotter directly). Built natively (not cross/musl) for simplicity; if
# this step fails the release still succeeds — /api/wakeword/build just
# reports "not found" until an admin builds and drops the binary in bin/.
step "Building wake-model-builder (wake-word enrollment)"
WAKE_BUILDER_BIN=""
if (cd "$ROOT_DIR/wake-model-builder" && cargo build --release); then
    candidate="$ROOT_DIR/wake-model-builder/target/release/wake-model-builder"
    [ -f "$candidate" ] && WAKE_BUILDER_BIN="$candidate"
fi
if [ -n "$WAKE_BUILDER_BIN" ]; then
    log "wake-model-builder built: $WAKE_BUILDER_BIN"
else
    warn "wake-model-builder build failed or skipped — the dashboard's wake-word"
    warn "  enrollment endpoint will report 'not found' until it's built separately."
fi

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
if [ -n "$WAKE_BUILDER_BIN" ]; then
    mkdir -p "$DIST/core/bin"
    cp "$WAKE_BUILDER_BIN" "$DIST/core/bin/wake-model-builder"
fi

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
