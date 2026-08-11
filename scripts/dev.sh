#!/usr/bin/env bash
# =============================================================================
#  Axon — local dev instance (Linux / WSL)
# -----------------------------------------------------------------------------
#  The Linux counterpart of run.bat: build the frontend, sync it into the
#  agent's static/ dir, then run the backend from crates/axon-agent so it
#  picks up the .env sitting there.
#
#  Modes
#    (default)   build axon-ui once, serve everything from the agent on :PORT.
#    --hmr       run vite's dev server alongside the agent instead. Vite
#                proxies /api and /ws to the agent (see axon-ui/vite.config.js),
#                so you get hot reload on the UI at :5173 while the agent
#                keeps serving the API on :PORT. Best for frontend work.
#
#  Flags
#    --hmr             vite dev server + agent, hot reload  (UI on :5173)
#    --no-ui           skip the frontend entirely, just run the agent
#    --release         build/run the agent in release mode
#    --port N          override AXON_PORT for this run
#    --help
#
#  Note: this starts Telegram polling and can 409 against the live server's
#  bot. Stop it when you're done.
# =============================================================================
set -euo pipefail

B='\033[1m'; G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; C='\033[0;36m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
err()  { echo -e "${R}[✗]${N} $*" >&2; exit 1; }
info() { echo -e "${C}[→]${N} $*"; }
step() { echo -e "\n${B}━━━ $* ━━━${N}"; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENT_DIR="$ROOT/crates/axon-agent"
UI_DIR="$ROOT/axon-ui"

HMR=0; SKIP_UI=0; RELEASE=0; PORT_OVERRIDE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --hmr)     HMR=1 ;;
    --no-ui)   SKIP_UI=1 ;;
    --release) RELEASE=1 ;;
    --port)    PORT_OVERRIDE="${2:-}"; shift ;;
    --help|-h) sed -n '2,/^# =\{20,\}$/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'; exit 0 ;;
    *)         err "unknown flag: $1 (try --help)" ;;
  esac
  shift
done

# ── Preflight ────────────────────────────────────────────────────────────────
step "Preflight"
command -v cargo >/dev/null || err "cargo not on PATH. Use a login shell (bash -l) so ~/.profile is read."
[ "$SKIP_UI" = 1 ] || command -v npm >/dev/null || err "npm not on PATH. Use a login shell (bash -l)."
[ -f "$AGENT_DIR/.env" ] || warn ".env missing at crates/axon-agent/.env — the agent will refuse to start unless AXON_MASTER_KEY or AXON_DEV=1 is exported."

PORT="${PORT_OVERRIDE:-$(sed -nE 's/^AXON_PORT=([0-9]+).*/\1/p' "$AGENT_DIR/.env" 2>/dev/null | head -1)}"
PORT="${PORT:-3000}"
log "repo   $ROOT"
log "agent  port $PORT"

if pgrep -x axon >/dev/null 2>&1; then
  warn "an 'axon' process is already running — stopping it so the rebuild isn't blocked"
  pkill -x axon || true
  sleep 1
fi

if command -v ss >/dev/null && ss -ltn "sport = :$PORT" 2>/dev/null | grep -q LISTEN; then
  warn "port $PORT is already listening — the agent will fail to bind"
fi

# ── Frontend ─────────────────────────────────────────────────────────────────
if [ "$SKIP_UI" = 0 ]; then
  step "Frontend"
  cd "$UI_DIR"
  if [ ! -x node_modules/.bin/vite ]; then
    info "node_modules missing or built for another OS — running npm install"
    npm install
  fi
fi

# ── HMR mode: agent in the background, vite in the foreground ────────────────
if [ "$HMR" = 1 ]; then
  [ "$SKIP_UI" = 1 ] && err "--hmr and --no-ui are contradictory"
  step "Agent (background)"
  cd "$AGENT_DIR"
  CARGO_ARGS=(run); [ "$RELEASE" = 1 ] && CARGO_ARGS+=(--release)
  AXON_PORT="$PORT" cargo "${CARGO_ARGS[@]}" &
  AGENT_PID=$!
  cleanup() { info "stopping agent ($AGENT_PID)"; kill "$AGENT_PID" 2>/dev/null || true; }
  trap cleanup EXIT INT TERM

  step "Vite dev server"
  info "UI  http://localhost:5173  (hot reload, proxies /api + /ws to :$PORT)"
  info "API http://localhost:$PORT"
  cd "$UI_DIR"
  npm run dev
  exit 0
fi

# ── Served mode: build UI into the agent's static/, then run the agent ───────
if [ "$SKIP_UI" = 0 ]; then
  info "building axon-ui"
  npm run build
  step "Sync static"
  mkdir -p "$AGENT_DIR/static"
  # Copy over the top rather than wiping first: index.html and assets/ are
  # gitignored, but favicon.png and apple-touch-icon.png are tracked, and
  # index.html only ever references the new hashed asset names.
  cp -r "$UI_DIR/dist/." "$AGENT_DIR/static/"
  log "synced axon-ui/dist -> crates/axon-agent/static"
fi

step "Agent"
cd "$AGENT_DIR"
CARGO_ARGS=(run); [ "$RELEASE" = 1 ] && CARGO_ARGS+=(--release)
info "dashboard  http://localhost:$PORT"
info "Ctrl+C to stop"
exec env AXON_PORT="$PORT" cargo "${CARGO_ARGS[@]}"
