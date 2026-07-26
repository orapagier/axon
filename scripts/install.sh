#!/usr/bin/env bash
# =============================================================================
#  Axon — Linux (Debian/Ubuntu) installer
# -----------------------------------------------------------------------------
#  Installs Axon from a GitHub release bundle and sets it up as a systemd
#  service, mirroring what deployaxongcp.sh does on a target Linux machine:
#  extract  ->  install Qdrant  ->  run.sh --install  ->  start.
#
#  Bootstrap behaviour (as requested):
#    • If the release tarball is already in the current directory, it is used.
#    • Otherwise the latest release tarball is downloaded from GitHub.
#
#  One-liners
#    From a downloaded copy in the current folder:
#       bash install.sh
#    Straight from GitHub (downloads the tarball itself):
#       curl -fsSL https://github.com/orapagier/axon/releases/latest/download/install.sh | bash
#
#  Flags
#    --dir PATH        install location            (default: $HOME/axon)
#    --version vX.Y.Z  pin a release               (default: latest)
#    --file PATH       use this local tarball
#    --with-qdrant     install Qdrant, no prompt
#    --no-qdrant       skip Qdrant
#    --no-service      don't install a systemd service (run in background)
#    --help
#
#  Env overrides: AXON_REPO, AXON_DIR
# =============================================================================
set -euo pipefail

# ── Colours / logging ────────────────────────────────────────────────────────
B='\033[1m'; G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; C='\033[0;36m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
err()  { echo -e "${R}[✗]${N} $*" >&2; exit 1; }
info() { echo -e "${C}[→]${N} $*"; }
step() { echo -e "\n${B}━━━ $* ━━━${N}"; }

# ── Config / defaults ────────────────────────────────────────────────────────
REPO="${AXON_REPO:-orapagier/axon}"
TARBALL="axon-linux-x86_64.tar.gz"
INSTALL_DIR="${AXON_DIR:-$HOME/axon}"
VERSION="latest"
LOCAL_FILE=""
WITH_QDRANT="ask"      # ask | yes | no
INSTALL_SERVICE="yes"  # yes | no

# ── Parse flags ──────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        --dir)          INSTALL_DIR="$2"; shift 2 ;;
        --version)      VERSION="$2";     shift 2 ;;
        --file)         LOCAL_FILE="$2";  shift 2 ;;
        --with-qdrant)  WITH_QDRANT="yes"; shift ;;
        --no-qdrant)    WITH_QDRANT="no";  shift ;;
        --no-service)   INSTALL_SERVICE="no"; shift ;;
        --help|-h)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) err "Unknown option: $1 (try --help)" ;;
    esac
done

echo -e "${B}"
echo "╔══════════════════════════════════════════╗"
echo "║            Axon — Installer              ║"
echo "╚══════════════════════════════════════════╝"
echo -e "${N}"

# ── Preflight ────────────────────────────────────────────────────────────────
step "Checking environment"
[ "$(uname -s)" = "Linux" ] || err "This installer targets Linux. On Windows use scripts/install.ps1."

ARCH="$(uname -m)"
if [ "$ARCH" != "x86_64" ]; then
    warn "Architecture is '$ARCH'. The release binary is x86_64 — it may not run here."
fi

command -v tar  >/dev/null 2>&1 || err "'tar' is required but not installed."
if ! command -v curl >/dev/null 2>&1; then
    err "'curl' is required. Install it with:  sudo apt-get update && sudo apt-get install -y curl"
fi
command -v apt-get >/dev/null 2>&1 || warn "apt-get not found — this looks like a non-Debian distro. Continuing anyway."

# systemd present (PID 1)? Determines whether we install a service or run in bg.
HAS_SYSTEMD="no"
[ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1 && HAS_SYSTEMD="yes"
log "Target dir : $INSTALL_DIR"
log "Repository : $REPO"
log "systemd    : $HAS_SYSTEMD"

# ── Acquire the release bundle ───────────────────────────────────────────────
step "Locating release bundle"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
BUNDLE="$WORKDIR/$TARBALL"

if [ -n "$LOCAL_FILE" ]; then
    [ -f "$LOCAL_FILE" ] || err "--file '$LOCAL_FILE' not found."
    info "Using local bundle: $LOCAL_FILE"
    cp "$LOCAL_FILE" "$BUNDLE"
elif [ -f "./$TARBALL" ]; then
    info "Found ./$TARBALL in the current directory — using it."
    cp "./$TARBALL" "$BUNDLE"
else
    if [ "$VERSION" = "latest" ]; then
        URL="https://github.com/$REPO/releases/latest/download/$TARBALL"
    else
        URL="https://github.com/$REPO/releases/download/$VERSION/$TARBALL"
    fi
    info "No local bundle found — downloading:"
    info "  $URL"
    curl -fL --proto '=https' --tlsv1.2 -o "$BUNDLE" "$URL" \
        || err "Download failed. Check the version/tag, or place $TARBALL next to this script."
    log "Downloaded $(du -h "$BUNDLE" | cut -f1)"
fi

# Sanity: bundles that carry secrets must never be installed from a public source.
# Capture the match (no `grep -q`, which closes the pipe early and, under
# pipefail, could make a found secret read as "not found").
SECRET_HITS="$(tar -tzf "$BUNDLE" | grep -E '(^|/)\.env$|(^|/)credentials\.json$|ssh_keys/' || true)"
if [ -n "$SECRET_HITS" ]; then
    warn "This bundle contains a real .env / credentials.json / ssh_keys (private data)."
    warn "That is expected only for your OWN private bundle, never a public release."
fi

# ── Install ──────────────────────────────────────────────────────────────────
step "Installing to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
# tar never deletes files absent from the archive, so an existing .env, memory
# database and credentials.json are preserved across a re-install automatically.
tar -xzf "$BUNDLE" -C "$INSTALL_DIR"
[ -f "$INSTALL_DIR/core/axon" ] || err "Extraction did not produce core/axon — is this the right bundle?"
chmod +x "$INSTALL_DIR/run.sh" "$INSTALL_DIR/core/axon" 2>/dev/null || true
log "Files extracted."

# ── First-run .env (generate a valid master key so boot doesn't refuse) ──────
step "Configuring environment"
ENV_FILE="$INSTALL_DIR/core/.env"
# Read a BOUNDED chunk of randomness (so nothing closes the pipe early — a
# `tr /dev/urandom | head` pipeline dies with SIGPIPE under `set -o pipefail`),
# then take 44 alphanumerics via a bash substring. High entropy, no shell-unsafe
# characters, and comfortably passes the boot-time master-key validator.
gen_key() {
    local raw
    raw="$(LC_ALL=C tr -dc 'A-Za-z0-9' < <(head -c 600 /dev/urandom))"
    printf '%s' "${raw:0:44}"
}
set_env_var() { # file key value  — replaces KEY=... or appends if missing
    awk -v k="$2" -v v="$3" '
        $0 ~ "^"k"=" { print k"="v; found=1; next }
        { print }
        END { if (!found) print k"="v }
    ' "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

GENERATED_KEY=""
if [ -f "$ENV_FILE" ]; then
    info "Existing core/.env kept as-is."
else
    cp "$INSTALL_DIR/core/.env.example" "$ENV_FILE"
    GENERATED_KEY="$(gen_key)"
    set_env_var "$ENV_FILE" "AXON_MASTER_KEY" "$GENERATED_KEY"
    set_env_var "$ENV_FILE" "AXON_API_KEY"    "$(gen_key)"
    log "Created core/.env with a freshly generated master key."
fi

# ── Qdrant (long-term memory) — optional ─────────────────────────────────────
step "Qdrant vector database"
if [ "$WITH_QDRANT" = "ask" ]; then
    if [ "$HAS_SYSTEMD" = "yes" ] && [ -r /dev/tty ]; then
        printf "Install Qdrant now for long-term memory? [Y/n] " > /dev/tty
        read -r _ans < /dev/tty || _ans=""
        case "${_ans:-Y}" in [Nn]*) WITH_QDRANT="no" ;; *) WITH_QDRANT="yes" ;; esac
    else
        # Non-interactive or no systemd: default to skipping; Axon runs without it.
        WITH_QDRANT="no"
        info "Skipping Qdrant (non-interactive or no systemd). Axon runs fine without it;"
        info "long-term semantic memory is simply disabled until you install it."
    fi
fi
if [ "$WITH_QDRANT" = "yes" ] && [ -f "$INSTALL_DIR/qdrant/install.sh" ]; then
    info "Running qdrant/install.sh ..."
    chmod +x "$INSTALL_DIR/qdrant/install.sh"
    ( cd "$INSTALL_DIR/qdrant" && bash install.sh ) || warn "Qdrant install reported an error — Axon will still run without it."
else
    info "Qdrant not installed."
fi

# ── Service / launch ─────────────────────────────────────────────────────────
step "Starting Axon"
if [ "$INSTALL_SERVICE" = "yes" ] && [ "$HAS_SYSTEMD" = "yes" ]; then
    ( cd "$INSTALL_DIR" && sudo ./run.sh --install && sudo ./run.sh start )
    RUNNING_VIA="systemd service 'axon-agent'"
else
    [ "$INSTALL_SERVICE" = "yes" ] && warn "No systemd detected — starting in the background instead of as a service."
    ( cd "$INSTALL_DIR/core" && nohup ./axon > "$INSTALL_DIR/agent.log" 2>&1 & )
    RUNNING_VIA="background process (logs: $INSTALL_DIR/agent.log)"
fi

# ── Done ─────────────────────────────────────────────────────────────────────
PORT="$(grep -E '^AXON_PORT=' "$ENV_FILE" | cut -d= -f2 | tr -d '[:space:]')"
PORT="${PORT:-3000}"
echo ""
echo -e "${B}${G}✅ Axon is installed.${N}"
echo -e "   Dashboard : ${C}http://localhost:$PORT${N}"
echo -e "   Running as: $RUNNING_VIA"
echo -e "   Install   : $INSTALL_DIR"
if [ -n "$GENERATED_KEY" ]; then
    echo ""
    echo -e "${Y}${B}   Save your master key — it is your dashboard login AND the key every${N}"
    echo -e "${Y}${B}   stored secret is encrypted under. It is not recoverable if lost:${N}"
    echo -e "${B}       AXON_MASTER_KEY = ${GENERATED_KEY}${N}"
fi
echo ""
echo -e "   Next: add at least one LLM provider key in ${C}$ENV_FILE${N}, then:"
if [ "$RUNNING_VIA" = "systemd service 'axon-agent'" ]; then
    echo -e "       cd \"$INSTALL_DIR\" && ./run.sh restart      # apply .env changes"
    echo -e "       journalctl -u axon-agent -f                  # live logs"
else
    echo -e "       cd \"$INSTALL_DIR\" && ./run.sh restart"
fi
echo ""
