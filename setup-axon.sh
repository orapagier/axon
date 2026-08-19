#!/usr/bin/env bash
# =============================================================================
#  Axon — dev environment bootstrap  (any Debian-based distro)
# -----------------------------------------------------------------------------
#  Takes a FRESH OS install to "can build, run, commit and deploy".
#  Safe to re-run: every step detects what is already present and skips it.
#
#  SUPPORTED
#    Any Debian derivative (Debian, Ubuntu, Mint, Pop!_OS, ...) on x86_64 or
#    arm64, running under WSL2 or on bare metal / a VM, desktop or headless.
#    Nothing is assumed beyond apt and a shell: sudo, curl, openssh-client and
#    openssl are all installed if missing, and it adapts to running as root
#    (fresh Debian often has no sudo) as well as to an unprivileged user.
#    Run it as your NORMAL user — not with sudo; it escalates per command so
#    your SSH key, Rust toolchain and checkout stay owned by you.
#
#  WHAT IT INSTALLS (and why — each choice is grounded in this repo)
#    build-essential, pkg-config
#        rusqlite uses features=["bundled"] (compiles SQLite from C) and russh
#        pins the `ring` backend — both need a C toolchain. No libsqlite3-dev
#        or libssl-dev needed: SQLite is vendored and TLS is rustls, not OpenSSL.
#        NOTE: protoc is deliberately NOT installed. qdrant-client pulls prost/
#        tonic, but prost-build/tonic-build (the only crates that shell out to
#        protoc) are absent from Cargo.lock and no crate has a build.rs, so the
#        protobuf types are pre-generated. Installing protoc would be cargo cult.
#    git identity + an ed25519 SSH key for GitHub
#        .claude/settings.json registers a Stop hook (.claude/hooks/auto-backup.sh)
#        that runs `git add -A`, commits, and PUSHES to origin. It silences its
#        own errors, so with an unset user.email it fails invisibly.
#        user.email is taken from your GitHub account when it is public; when
#        it is private the API returns null, so the script ASKS rather than
#        silently stamping commits with the users.noreply alias.
#        The script generates an SSH key, pins github.com in known_hosts (an
#        unknown host key would block that hook — it has no TTY to confirm at),
#        switches origin from HTTPS to git@github.com, and either uploads the
#        key via gh or prints it for https://github.com/settings/ssh/new. SSH is
#        used rather than an HTTPS token because a token expiring mid-session
#        makes the unattended push fail silently; a key does not expire.
#    Rust (rustup stable) + rustfmt + clippy
#        Both components are enforced by .github/workflows/ci.yml.
#    cargo-deny — fetched as a PREBUILT binary, not `cargo install`
#        CI runs cargo-deny-action against deny.toml. Building it from source
#        costs 5-10 min for a tool you never modify.
#    Node.js 22 LTS + npm  (NodeSource `nodistro` repo)
#        axon-ui is Vue 3 + Vite 5 (engines: ^18 || >=20). See the Node note in
#        the findings section below re: the EOL Node 20 pin in CI.
#    GitHub CLI (gh)      — cutting releases (README "Releasing"). Installed
#                           BEFORE the git/SSH step, because `gh auth login`
#                           offers to upload the SSH key itself, which saves you
#                           pasting it by hand.
#    Google Cloud CLI     — the GCP deploy targets in .deploy.env.example
#                           (AXON_GCP_INSTANCE / _ZONE / _USER).
#    crates/axon-agent/.env seeded from .env.example with a generated
#        AXON_MASTER_KEY (boot refuses weak/placeholder keys) and AXON_API_KEY.
#        An existing .env is never overwritten. Only boot-blocking problems are
#        reported: this is a single-operator setup, so the local instance
#        sharing a bot token with the server is intended, not a finding.
#    image-tool fonts provisioned from crates/axon-agent/assets/fonts into
#        crates/axon-agent/data/files/fonts, which is where image_tool.rs
#        actually looks and which is gitignored, so it cannot be committed.
#
#  OPT-IN EXTRAS (off by default — none are needed for the daily build/run loop)
#    --with-musl      <arch>-unknown-linux-musl target + musl-tools. Only used
#                     by scripts/package-release.sh for portable release builds.
#    --with-lld       faster linking for this 700+ crate workspace. Written to
#                     ~/.cargo/config.toml (user-global) — deliberately NOT to
#                     the repo's .cargo/config.toml, which is committed and
#                     hand-tuned, and would be auto-pushed by the Stop hook.
#    --with-qdrant    static Qdrant binary in ~/.local/bin for long-term-memory
#                     work. Does NOT run qdrant/install.sh — that one is tuned
#                     for a 1GB production VM (creates swap, systemd timers).
#    --with-graphify  the `graphifyy` package CLAUDE.md/AGENTS.md expect, via
#                     pipx (Python 3.14 here is PEP 668 / externally-managed,
#                     so plain `pip install --user` would be refused).
#    --verify         after install, run `cargo check -p axon-core` to prove the
#                     toolchain genuinely compiles this workspace.
#
#  SKIP FLAGS
#    --no-rust  --no-node  --no-gh  --no-gcloud  --no-deny  --no-env  --no-git
#
#  INSTALL ON A FRESH MACHINE (one line, nothing pre-installed)
#      curl -fsSL https://raw.githubusercontent.com/orapagier/axon/main/setup-axon.sh | bash
#
#    To pass flags through the pipe, use `bash -s --`:
#      curl -fsSL .../setup-axon.sh | bash -s -- --with-musl --with-lld
#
#    It installs everything, clones the repo to ~/dev/axon, and configures it.
#    Prompts still work under the pipe: stdin belongs to curl, so every prompt
#    reads from /dev/tty instead. With no terminal at all (CI, cron) it skips
#    the prompts and reports what is left to do rather than hanging.
#
#    Run from inside an existing checkout, it uses that one and clones nothing.
#    --dir PATH   where to clone to (default: ~/dev/axon)
#
#    Two copies exist by design: the one in the repo is canonical and versioned,
#    and a copy in ~/ is what you run before any checkout exists. The script
#    compares them and tells you when the ~/ copy has fallen behind.
#
#  Env overrides:  GIT_NAME, GIT_EMAIL, SSH_PASSPHRASE, AXON_DIR, AXON_REPO_URL
# =============================================================================
set -euo pipefail

B='\033[1m'; G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; C='\033[0;36m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
err()  { echo -e "${R}[✗]${N} $*" >&2; exit 1; }
info() { echo -e "${C}[→]${N} $*"; }
step() { echo -e "\n${B}━━━ $* ━━━${N}"; }

# Set KEY=VALUE in an env file. Deliberately no python3 and no sed: python3 is
# absent from minimal Debian/Ubuntu images, and sed would mangle values
# containing '/' or '+' (every base64 key). Here the value is only ever a
# printf ARGUMENT, never a format string or a pattern, so it lands byte-exact.
# Writes through `cat >` so the original file keeps its inode and 0600 mode.
set_env_var() {
  local file="$1" key="$2" value="$3" tmp line found=0
  tmp="$(mktemp)"
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      "$key"=*) printf '%s=%s\n' "$key" "$value"; found=1 ;;
      *)        printf '%s\n' "$line" ;;
    esac
  done < "$file" > "$tmp"
  [ "$found" -eq 0 ] && printf '%s=%s\n' "$key" "$value" >> "$tmp"
  cat "$tmp" > "$file"
  rm -f "$tmp"
}

# Privilege: a fresh Debian install is often root-only (sudo is not installed by
# default there, and the first user may not be in the sudo group). Running as
# root is legitimate for a provisioning script, so adapt instead of demanding
# sudo. Everything below calls $SUDO, which is empty when we are already root.
# Running the WHOLE script under sudo is a footgun: the clone, ~/.ssh/id_ed25519,
# ~/.cargo and ~/.local/bin would all end up root-owned, and the normal user
# could then neither push nor build. Escalation is per-command via $SUDO instead.
if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
  echo "Do not run this with sudo. Run it as $SUDO_USER:" >&2
  echo "    bash ${BASH_SOURCE[0]##*/}" >&2
  echo "It escalates only the commands that need root, so your SSH key, Rust" >&2
  echo "toolchain and the checkout stay owned by you." >&2
  exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
elif command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
else
  echo "Need root privileges: install sudo and add your user to it, or run this as root." >&2
  exit 1
fi

# Architecture: the prebuilt binaries this script fetches (cargo-deny, qdrant)
# and the Rust target triples are per-arch. Do not assume x86_64 — arm64 servers
# and Apple-silicon VMs are common.
case "$(uname -m)" in
  x86_64|amd64)  RUST_ARCH="x86_64" ;;
  aarch64|arm64) RUST_ARCH="aarch64" ;;
  *)             RUST_ARCH="" ;;
esac
MUSL_TARGET="${RUST_ARCH:-unknown}-unknown-linux-musl"
GNU_TARGET="${RUST_ARCH:-unknown}-unknown-linux-gnu"

# `curl -fsSL … | bash` binds stdin to the pipe carrying this script, so
# `[ "$INTERACTIVE" = 1 ]` is false and a bare `read` would consume the script's own source
# text. The controlling terminal is still reachable at /dev/tty, so every prompt
# below reads from there instead. Opening it is also the reliable interactivity
# test: with no controlling terminal (CI, cron, a container) the open fails
# outright rather than blocking, whereas `[ -r /dev/tty ]` only stats the node
# and wrongly reports success.
# The braces matter: redirections apply left to right, so a bare
# `exec 3</dev/tty 2>/dev/null` still prints bash's own "No such device or
# address" before the 2>/dev/null takes effect. Grouping silences the probe.
if { exec 3</dev/tty; } 2>/dev/null; then
  exec 3<&-
  INTERACTIVE=1
else
  INTERACTIVE=0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$ROOT/crates/axon-agent"
UI_DIR="$ROOT/axon-ui"

DO_RUST=1; DO_NODE=1; DO_GH=1; DO_GCLOUD=1; DO_DENY=1; DO_ENV=1; DO_GIT=1
W_MUSL=0; W_LLD=0; W_QDRANT=0; W_GRAPHIFY=0; VERIFY=0

# Where to clone if this script is run outside a checkout. Overridable so the
# bootstrap path is not hardcoded to one person's directory layout.
REPO_URL="${AXON_REPO_URL:-https://github.com/orapagier/axon}"
CLONE_DIR="${AXON_DIR:-$HOME/dev/axon}"

while [ $# -gt 0 ]; do
  case "$1" in
    --no-rust)       DO_RUST=0 ;;
    --no-node)       DO_NODE=0 ;;
    --no-gh)         DO_GH=0 ;;
    --no-gcloud)     DO_GCLOUD=0 ;;
    --no-deny)       DO_DENY=0 ;;
    --no-env)        DO_ENV=0 ;;
    --no-git)        DO_GIT=0 ;;
    --with-musl)     W_MUSL=1 ;;
    --with-lld)      W_LLD=1 ;;
    --with-qdrant)   W_QDRANT=1 ;;
    --with-graphify) W_GRAPHIFY=1 ;;
    --verify)        VERIFY=1 ;;
    --dir)           [ $# -ge 2 ] || err "--dir needs a PATH"; CLONE_DIR="$2"; shift ;;
    --all)           W_MUSL=1; W_LLD=1; W_QDRANT=1; W_GRAPHIFY=1; VERIFY=1 ;;
    # Pattern-bounded rather than a hardcoded line range, so editing the header
    # above can never silently truncate --help.
    --help|-h)       sed -n '2,/^# =\{20,\}$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)               err "unknown flag: $1 (try --help)" ;;
  esac
  shift
done

# ── Preflight ────────────────────────────────────────────────────────────────
# Deliberately does NOT require the repo: this script can be downloaded on its
# own and run to bootstrap a machine from nothing. The checkout is acquired in
# the "Repository" step below, once git (and optionally gh) are installed.
step "Preflight"
[ "$(uname -s)" = "Linux" ] || err "This targets Linux/WSL. On Windows use run.bat (see README Quick Start)."
command -v apt-get >/dev/null 2>&1 || err "apt-get not found — this script supports Debian/Ubuntu only."

CORES="$(nproc)"
MEM_GB="$(awk '/MemTotal/ {printf "%.1f", $2/1048576}' /proc/meminfo)"
SWAP_KB="$(awk '/SwapTotal/ {print $2}' /proc/meminfo)"
log "host: ${CORES} cores, ${MEM_GB} GiB RAM, $(( SWAP_KB / 1024 )) MiB swap"
# A release build (lto=true, codegen-units=1) is the memory-hungry one.
if [ "${MEM_GB%%.*}" -lt 8 ]; then
  warn "under 8 GiB RAM — if 'cargo build --release' gets OOM-killed, use 'cargo build --release --jobs 2'"
  [ "$SWAP_KB" -lt 2097152 ] && warn "swap is under 2 GiB; more headroom would help a release build"
fi

if [ -n "$SUDO" ]; then
  sudo -v || err "sudo access is required to install system packages."
  # Keep sudo warm: cargo-deny/npm steps can outlast the default 15-min timeout.
  ( while true; do sudo -n true; sleep 60; kill -0 "$$" 2>/dev/null || exit; done ) 2>/dev/null &
  SUDO_KEEPALIVE=$!
  trap 'kill "$SUDO_KEEPALIVE" 2>/dev/null || true' EXIT
  log "privileges: sudo as $(id -un)"
else
  log "privileges: running as root"
fi
[ -n "$RUST_ARCH" ] && log "arch: $(uname -m)" \
  || warn "arch $(uname -m) is not x86_64/arm64 — prebuilt cargo-deny and qdrant downloads will be skipped"

# ── Base packages ────────────────────────────────────────────────────────────
# apt is noisy (dpkg triggers, "Selecting previously unselected package", ...).
# Everything goes to a log; it is only surfaced if a step actually fails.
APT_LOG="$(mktemp)"
apt_install() {
  $SUDO DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" >>"$APT_LOG" 2>&1 \
    || { warn "apt-get install failed for: $*"; tail -20 "$APT_LOG" >&2; return 1; }
}
apt_update() { $SUDO apt-get update -qq >>"$APT_LOG" 2>&1 || warn "apt-get update reported an error"; }

step "Base packages"
apt_update
# No libssl-dev / libsqlite3-dev / protobuf-compiler: see the header note.
# openssh-client and openssl are NOT guaranteed on a minimal Debian image, and
# this script needs ssh-keygen/ssh-keyscan for the GitHub key and `openssl rand`
# for the master key. Installing them explicitly rather than hoping.
apt_install build-essential pkg-config curl ca-certificates gnupg git unzip jq \
            openssh-client openssl
log "build toolchain, curl, git, openssh-client, openssl, jq"

# On WSL there is no Linux browser, so `gh auth login` and `gcloud auth login`
# fail with "exec: xdg-open,x-www-browser,www-browser,wslview: not found".
# The usual answer is the wslu package, but it is not published for every Ubuntu
# release (26.04 "resolute" has no candidate), so depending on it breaks the run.
# WSL interop is always available instead: a tiny xdg-open shim hands the URL to
# the Windows default browser. No package required, works on any release.
IS_WSL=0; grep -qi microsoft /proc/version 2>/dev/null && IS_WSL=1
if [ "$IS_WSL" = 0 ]; then
  # Bare-metal/VM Debian: a desktop already ships xdg-open. A headless server
  # has no browser at all, which is fine — gh and gcloud both fall back to
  # printing a URL to open elsewhere. Say so rather than installing a shim that
  # would have nothing to launch.
  command -v xdg-open >/dev/null 2>&1 \
    && log "xdg-open present — gh/gcloud can open a browser" \
    || info "headless (no xdg-open) — gh/gcloud will print URLs to open on another machine"
fi
if [ "$IS_WSL" = 1 ] \
   && ! command -v xdg-open >/dev/null 2>&1 && ! command -v wslview >/dev/null 2>&1; then
  mkdir -p "$HOME/.local/bin"
  cat >"$HOME/.local/bin/xdg-open" <<'SHIM'
#!/usr/bin/env bash
# WSL shim: open a URL/file with the Windows default handler.
# Created by axon setup-axon.sh because wslu has no package on this release.
[ $# -ge 1 ] || { echo "usage: xdg-open <url|file>" >&2; exit 2; }
if command -v powershell.exe >/dev/null 2>&1; then
  exec powershell.exe -NoProfile -NonInteractive -Command "Start-Process '$1'"
elif [ -x /mnt/c/Windows/explorer.exe ]; then
  /mnt/c/Windows/explorer.exe "$1"; exit 0   # explorer returns 1 even on success
else
  echo "No Windows browser reachable; open this manually: $1" >&2; exit 1
fi
SHIM
  chmod +x "$HOME/.local/bin/xdg-open"
  log "xdg-open shim installed (gh/gcloud can open your Windows browser)"
fi
# Make sure the shim — and anything else in ~/.local/bin — is reachable now and
# in future shells.
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) export PATH="$HOME/.local/bin:$PATH"
     if [ -f "$HOME/.bashrc" ] && ! grep -q '\.local/bin' "$HOME/.bashrc" 2>/dev/null; then
       printf '\n# added by axon setup-axon.sh\nexport PATH="$HOME/.local/bin:$PATH"\n' >>"$HOME/.bashrc"
       log "added ~/.local/bin to PATH in ~/.bashrc"
     fi ;;
esac

# ── GitHub CLI ───────────────────────────────────────────────────────────────
if [ "$DO_GH" = 1 ]; then
  step "GitHub CLI"
  if command -v gh >/dev/null 2>&1; then
    log "gh present: $(gh --version | head -1)"
  else
    info "installing gh (cli.github.com apt repo — codename-independent 'stable' suite)"
    $SUDO mkdir -p -m 755 /etc/apt/keyrings
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      | $SUDO tee /etc/apt/keyrings/githubcli-archive-keyring.gpg >/dev/null
    $SUDO chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      | $SUDO tee /etc/apt/sources.list.d/github-cli.list >/dev/null
    apt_update
    apt_install gh
  fi
  # No `gh auth setup-git` here: origin now speaks SSH, so pushes authenticate
  # with the key above rather than a credential helper. gh is still worth
  # authenticating for `gh release create` (README "Releasing") and for
  # uploading the SSH key on a re-run of this script.
  if gh auth status >/dev/null 2>&1; then
    log "gh authenticated"
  else
    warn "gh is not authenticated (only needed for cutting releases)."
    if [ "$INTERACTIVE" = 1 ]; then
      echo ""
      echo -e "  Authenticate now? gh prints a one-time code and a URL to open."
      read -r -p "  Run 'gh auth login' now? [y/N] " ans < /dev/tty
      # --git-protocol ssh matches the SSH origin set below, and gh's own flow
      # offers to upload the key it finds — which is why gh runs before the
      # git/SSH step rather than after it.
      case "$ans" in
        [Yy]*) gh auth login --hostname github.com --git-protocol ssh --web < /dev/tty || warn "gh login did not complete" ;;
      esac
    fi
  fi
fi

# ── Repository ───────────────────────────────────────────────────────────────
# Runs after git (and gh) are installed so a bootstrap from a bare machine
# works: download this one file, run it, and it fetches the checkout itself.
# Placed after the gh step specifically so a private repo can authenticate.
step "Repository"
is_axon_repo() { [ -f "$1/Cargo.toml" ] && [ -d "$1/axon-ui" ]; }

if is_axon_repo "$ROOT"; then
  log "already inside the repo: $ROOT"
elif is_axon_repo "$CLONE_DIR"; then
  ROOT="$CLONE_DIR"
  log "using existing checkout: $ROOT"
else
  if [ -e "$CLONE_DIR" ] && [ -n "$(ls -A "$CLONE_DIR" 2>/dev/null)" ]; then
    err "$CLONE_DIR exists and is not an axon checkout. Move it aside, or pass --dir PATH."
  fi
  info "no checkout found — cloning $REPO_URL"
  info "  into $CLONE_DIR  (override with --dir PATH or AXON_DIR=)"
  mkdir -p "$(dirname "$CLONE_DIR")"
  # Prefer gh when it is authenticated AND the URL is actually a github.com one
  # — `gh repo clone` only accepts OWNER/REPO, so a custom AXON_REPO_URL (another
  # host, or a local path) must go through plain git. gh is worth trying first
  # because it carries credentials for a private repo; git is the fallback.
  CLONED=0
  case "$REPO_URL" in
    https://github.com/*)
      if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        gh repo clone "${REPO_URL#https://github.com/}" "$CLONE_DIR" && CLONED=1 \
          || warn "gh repo clone failed — falling back to git"
      fi ;;
  esac
  # HTTPS at this stage: the SSH key may not exist yet. The Git step below
  # rewrites origin to SSH once the key is in place.
  [ "$CLONED" -eq 1 ] || git clone "$REPO_URL" "$CLONE_DIR" \
    || err "clone failed — check the URL ($REPO_URL) and your network"
  is_axon_repo "$CLONE_DIR" || err "clone finished but $CLONE_DIR is not an axon checkout"
  ROOT="$CLONE_DIR"
  log "cloned to $ROOT"
fi
# Everything downstream keys off these, so recompute after a clone.
AGENT_DIR="$ROOT/crates/axon-agent"
UI_DIR="$ROOT/axon-ui"

# WSL: building on a Windows drive mount is dramatically slower and breaks
# rollup's platform-specific optional dep (README calls this out for node_modules).
case "$ROOT" in
  /mnt/*) warn "repo lives on a Windows mount ($ROOT). Rust+npm builds here are slow and"
          warn "  rollup may fail with 'Cannot find module @rollup/rollup-linux-x64-gnu'."
          warn "  Strongly consider moving it under ~/ on the Linux filesystem." ;;
  *)      log "on the Linux filesystem (correct for WSL build performance)" ;;
esac

# .cargo/config.toml pins jobs= and is hand-tuned for a specific host; flag a
# mismatch rather than editing a committed file.
CFG_JOBS="$(sed -nE 's/^[[:space:]]*jobs[[:space:]]*=[[:space:]]*([0-9]+).*/\1/p' "$ROOT/.cargo/config.toml" 2>/dev/null | head -1)"
if [ -n "$CFG_JOBS" ]; then
  log ".cargo/config.toml pins jobs=$CFG_JOBS (committed + hand-tuned; left untouched)"
  [ "$CFG_JOBS" -gt "$CORES" ] && warn "jobs=$CFG_JOBS exceeds this host's $CORES cores — expect oversubscription."
fi

# Disk: a debug + release target/ for this workspace runs to several GiB.
AVAIL_GB="$(df -BG --output=avail "$ROOT" | tail -1 | tr -dc '0-9')"
[ "${AVAIL_GB:-99}" -lt 15 ] && warn "only ${AVAIL_GB}G free — target/ can exceed 10G." || log "disk: ${AVAIL_GB}G free"

# The canonical copy of this script lives in the repo; a copy in ~/ is what you
# run to bootstrap a machine that has no checkout yet. Two copies drift, so say
# so instead of letting the ~/ one silently rot.
SELF_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
REPO_COPY="$ROOT/setup-axon.sh"
if [ "$SELF_PATH" != "$REPO_COPY" ] && [ -f "$REPO_COPY" ] && ! cmp -s "$SELF_PATH" "$REPO_COPY"; then
  warn "the repo's setup-axon.sh differs from the copy you ran"
  warn "  running: $SELF_PATH"
  warn "  repo:    $REPO_COPY"
  warn "  refresh with:  cp '$REPO_COPY' '$SELF_PATH'"
fi

# ── Git identity + credentials (load-bearing for the auto-backup Stop hook) ──
if [ "$DO_GIT" = 1 ]; then
  step "Git identity & credentials"
  GIT_NAME="${GIT_NAME:-$(git config --get remote.origin.url 2>/dev/null | sed -nE 's#.*github\.com[:/]([^/]+)/.*#\1#p')}"
  GIT_EMAIL="${GIT_EMAIL:-}"

  if git config --get user.name >/dev/null 2>&1; then
    log "user.name  already set: $(git config --get user.name)"
  elif [ -n "$GIT_NAME" ]; then
    git config --global user.name "$GIT_NAME"
    log "user.name  set to '$GIT_NAME' (from the origin remote; override with GIT_NAME=)"
  else
    warn "user.name unset and not inferable — run: git config --global user.name 'Your Name'"
  fi

  # Derive the email rather than just warning about it: an unset user.email
  # makes every `git commit` fail, and the Stop hook swallows that error, so
  # the failure is invisible until you notice nothing has been pushed.
  EMAIL_SRC=""
  if [ -z "$GIT_EMAIL" ] && command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    GIT_EMAIL="$(gh api user --jq '.email // empty' 2>/dev/null || true)"
    [ -n "$GIT_EMAIL" ] && EMAIL_SRC=" (your public GitHub email)"
    # With "Keep my email private" on, the API returns null. Do NOT silently
    # substitute the ID+login@users.noreply address: it is valid for
    # attribution, but it is very likely not the address you actually want on
    # your commits. Ask, and offer noreply only as the default.
    if [ -z "$GIT_EMAIL" ] && [ "$INTERACTIVE" = 1 ]; then
      _uid="$(gh api user --jq '.id // empty' 2>/dev/null || true)"
      _ulogin="$(gh api user --jq '.login // empty' 2>/dev/null || true)"
      _noreply=""
      [ -n "$_uid" ] && [ -n "$_ulogin" ] && _noreply="${_uid}+${_ulogin}@users.noreply.github.com"
      echo ""
      echo "  Your GitHub email is private, so it can't be read from the API."
      [ -n "$_noreply" ] && echo "  Press Enter to use GitHub's private-email alias: $_noreply"
      read -r -p "  Git commit email: " _typed < /dev/tty
      GIT_EMAIL="${_typed:-$_noreply}"
      [ -n "$_typed" ] && EMAIL_SRC="" || EMAIL_SRC=" (GitHub private-email alias)"
    fi
  fi

  if git config --get user.email >/dev/null 2>&1; then
    log "user.email already set: $(git config --get user.email)"
  elif [ -n "$GIT_EMAIL" ]; then
    git config --global user.email "$GIT_EMAIL"
    log "user.email set to '$GIT_EMAIL'${EMAIL_SRC} — override anytime with GIT_EMAIL="
  elif [ "$INTERACTIVE" = 1 ]; then
    read -r -p "  Git commit email: " _e < /dev/tty
    if [ -n "$_e" ]; then
      git config --global user.email "$_e"; log "user.email set to '$_e'"
    else
      warn "left unset — 'git commit' will fail until you set it"
    fi
  else
    warn "user.email is UNSET — 'git commit' will fail and the Stop hook hides the error"
    warn "     git config --global user.email 'you@example.com'"
  fi

  # Sensible defaults for a repo whose .gitattributes forces LF everywhere.
  git config --global --get init.defaultBranch >/dev/null 2>&1 || git config --global init.defaultBranch main
  git config --global --get pull.rebase        >/dev/null 2>&1 || git config --global pull.rebase true
  # core.autocrlf must stay false on Linux; .gitattributes ('* text=auto eol=lf')
  # already governs normalisation, and autocrlf=true would fight it.
  CRLF="$(git config --get core.autocrlf || echo false)"
  [ "$CRLF" = "true" ] && { git config --global core.autocrlf false; warn "core.autocrlf was true — set to false (repo pins eol=lf)"; } || log "core.autocrlf=$CRLF (correct for this repo's .gitattributes)"

  # ── SSH key for GitHub ─────────────────────────────────────────────────────
  # SSH rather than HTTPS: the auto-backup Stop hook pushes unattended, and an
  # HTTPS token expiring mid-session fails silently. A key does not expire.
  step "GitHub SSH key"
  SSH_KEY="$HOME/.ssh/id_ed25519"
  if [ -f "$SSH_KEY" ]; then
    log "existing key reused: $SSH_KEY"
  else
    mkdir -p "$HOME/.ssh"; chmod 700 "$HOME/.ssh"
    KC="$(git config --get user.email || echo "axon-dev@$(hostname)")"
    # Empty passphrase is deliberate: the Stop hook pushes with no TTY to type
    # one into. Set SSH_PASSPHRASE=... to override, but then you must run an
    # ssh-agent or that hook's push will hang and be killed.
    info "generating an ed25519 key (no passphrase, so the auto-push hook can run unattended)"
    ssh-keygen -t ed25519 -C "$KC" -f "$SSH_KEY" -N "${SSH_PASSPHRASE:-}" -q
    chmod 600 "$SSH_KEY"
    log "created $SSH_KEY"
  fi

  # Pre-trust github.com. Without this the first push has no TTY to answer
  # "Are you sure you want to continue connecting?" and simply fails.
  touch "$HOME/.ssh/known_hosts"; chmod 600 "$HOME/.ssh/known_hosts"
  if ssh-keygen -F github.com >/dev/null 2>&1; then
    log "github.com already in known_hosts"
  else
    ssh-keyscan -t rsa,ecdsa,ed25519 github.com >>"$HOME/.ssh/known_hosts" 2>/dev/null
    log "github.com host keys pinned in known_hosts"
  fi

  # Point origin at SSH so pushes use the key.
  ORIGIN="$(git -C "$ROOT" remote get-url origin 2>/dev/null || echo '')"
  case "$ORIGIN" in
    git@github.com:*) log "origin already uses SSH: $ORIGIN" ;;
    https://github.com/*)
      SLUG="${ORIGIN#https://github.com/}"; SLUG="${SLUG%.git}"
      git -C "$ROOT" remote set-url origin "git@github.com:${SLUG}.git"
      log "origin switched HTTPS -> git@github.com:${SLUG}.git" ;;
    *) warn "unrecognised origin '$ORIGIN' — left as-is" ;;
  esac

  # Is the key actually registered on GitHub? Authenticating prints
  # "Hi <user>! You've successfully authenticated" and exits 1 by design.
  # `ssh -T` to GitHub always exits 1 (no shell access), so this must NOT be a
  # pipeline: under `set -o pipefail` that exit code would override grep's match
  # and report a working key as broken. Capture, then test the string.
  GH_SSH_OUT="$(ssh -o StrictHostKeyChecking=yes -o BatchMode=yes -T git@github.com 2>&1 || true)"
  if [ "${GH_SSH_OUT#*successfully authenticated}" != "$GH_SSH_OUT" ]; then
    log "SSH key is registered on GitHub — pushes will work"
  else
    PUBKEY="$(cat "${SSH_KEY}.pub")"
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
      info "gh is authenticated — uploading the key for you"
      gh ssh-key add "${SSH_KEY}.pub" --title "axon-dev-$(hostname)" \
        && log "key uploaded to your GitHub account" \
        || warn "upload failed — paste it manually (below)"
    fi
    GH_SSH_OUT2="$(ssh -o BatchMode=yes -T git@github.com 2>&1 || true)"
    if [ "${GH_SSH_OUT2#*successfully authenticated}" = "$GH_SSH_OUT2" ]; then
      echo ""
      echo -e "${B}ACTION REQUIRED — add this public key to GitHub${N}"
      echo -e "  1. Open: ${C}https://github.com/settings/ssh/new${N}"
      echo "  2. Title: axon-dev-$(hostname)      Key type: Authentication Key"
      echo "  3. Paste this line, then Add SSH key:"
      echo ""
      echo -e "${G}${PUBKEY}${N}"
      echo ""
      echo "  4. Verify with:  ssh -T git@github.com"
    fi
  fi
fi

# ── Rust ─────────────────────────────────────────────────────────────────────
if [ "$DO_RUST" = 1 ]; then
  step "Rust toolchain"
  export PATH="$HOME/.cargo/bin:$PATH"
  if command -v rustup >/dev/null 2>&1; then
    log "rustup present — updating stable"; rustup update stable
  else
    info "installing rustup (stable)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
  fi
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  rustup component add rustfmt clippy
  log "rustfmt + clippy added (both gate CI)"

  if [ "$W_MUSL" = 1 ]; then
    if [ -z "$RUST_ARCH" ]; then
      warn "skipping musl: no known target triple for $(uname -m)"
    else
      info "adding $MUSL_TARGET + musl-tools (portable release builds)"
      rustup target add "$MUSL_TARGET"
      apt_install musl-tools
    fi
  fi

  if [ "$W_LLD" = 1 ]; then
    info "enabling lld for faster links (user-global ~/.cargo/config.toml)"
    apt_install lld clang
    UCFG="$HOME/.cargo/config.toml"
    if grep -q "linker-features=+lld\|fuse-ld=lld" "$UCFG" 2>/dev/null; then
      log "lld already configured in $UCFG"
    else
      mkdir -p "$HOME/.cargo"
      # Unquoted heredoc so $GNU_TARGET expands — the target triple is per-arch.
      cat >>"$UCFG" <<EOF

# Added by axon setup-axon.sh: link with lld instead of GNU ld.
# Deliberately user-global rather than in axon/.cargo/config.toml, which is
# committed, hand-tuned, and would be auto-pushed by the auto-backup Stop hook.
[target.$GNU_TARGET]
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
EOF
      log "lld enabled in $UCFG (applies to all your Rust projects; remove that block to undo)"
    fi
  fi

  if [ "$DO_DENY" = 1 ]; then
    if command -v cargo-deny >/dev/null 2>&1; then
      log "cargo-deny present: $(cargo-deny --version)"
    else
      info "installing cargo-deny (prebuilt binary — avoids a 5-10 min source build)"
      DENY_OK=0
      DENY_TAG=""
      # Upstream publishes x86_64 and aarch64 musl tarballs only.
      if [ -n "$RUST_ARCH" ]; then
        DENY_TAG="$(curl -fsSL https://api.github.com/repos/EmbarkStudios/cargo-deny/releases/latest | jq -r .tag_name || true)"
      fi
      if [ -n "${DENY_TAG:-}" ] && [ "$DENY_TAG" != "null" ]; then
        TMP="$(mktemp -d)"
        ASSET="cargo-deny-${DENY_TAG}-${RUST_ARCH}-unknown-linux-musl.tar.gz"
        if curl -fsSL "https://github.com/EmbarkStudios/cargo-deny/releases/download/${DENY_TAG}/${ASSET}" -o "$TMP/d.tar.gz"; then
          tar -xzf "$TMP/d.tar.gz" -C "$TMP" --strip-components=1
          install -m755 "$TMP/cargo-deny" "$HOME/.cargo/bin/cargo-deny" && DENY_OK=1
          log "cargo-deny $DENY_TAG installed"
        fi
        rm -rf "$TMP"
      fi
      [ "$DENY_OK" = 0 ] && { warn "prebuilt fetch failed — falling back to a source build"; cargo install cargo-deny --locked; }
    fi
  fi
  log "$(rustc -V)  |  $(cargo -V)"
fi

# ── Node.js ──────────────────────────────────────────────────────────────────
if [ "$DO_NODE" = 1 ]; then
  step "Node.js 22 LTS"
  NEED_NODE=1
  if command -v node >/dev/null 2>&1; then
    NODE_MAJOR="$(node -v | sed -E 's/^v([0-9]+).*/\1/')"
    if [ "$NODE_MAJOR" -ge 20 ]; then log "node $(node -v) satisfies Vite 5 (^18 || >=20)"; NEED_NODE=0
    else warn "node $(node -v) is too old for Vite 5 — upgrading"; fi
  fi
  if [ "$NEED_NODE" = 1 ]; then
    # NodeSource's 'nodistro' suite is codename-independent — important here,
    # because this release's codename has no dist on their CDN, and Ubuntu's own
    # npm package (9.2.0) is mismatched against its nodejs 22.
    # Keyring + repo written explicitly rather than piping their setup script
    # into `sudo bash`.
    info "adding the NodeSource repo (nodistro suite) and installing Node 22"
    $SUDO mkdir -p -m 755 /etc/apt/keyrings
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
      | $SUDO gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg
    $SUDO chmod go+r /etc/apt/keyrings/nodesource.gpg
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_22.x nodistro main" \
      | $SUDO tee /etc/apt/sources.list.d/nodesource.list >/dev/null
    apt_update
    apt_install nodejs
  fi
  log "node $(node -v)  |  npm $(npm -v)"

  step "axon-ui dependencies"
  if [ -d "$UI_DIR/node_modules" ]; then
    log "node_modules present — skipping (rm -rf it to force a clean install)"
  else
    # npm ci, not npm install: package-lock.json is committed and CI uses `npm ci`.
    # --no-fund/--no-audit match what deployaxongcp.sh already uses. The audit
    # findings are all transitive dev-dependency noise (vite/eslint toolchain,
    # never shipped to a browser) and scroll the real output away; run
    # `npm audit` in axon-ui/ deliberately when you want to review them.
    info "npm ci in axon-ui/ (lockfile-exact, matches CI)"
    ( cd "$UI_DIR" && NPM_CONFIG_UPDATE_NOTIFIER=false npm ci --no-fund --no-audit --loglevel=error )
  fi
fi

# ── Google Cloud CLI ─────────────────────────────────────────────────────────
if [ "$DO_GCLOUD" = 1 ]; then
  step "Google Cloud CLI"
  if command -v gcloud >/dev/null 2>&1; then
    log "gcloud present: $(gcloud --version | head -1)"
  else
    info "installing google-cloud-cli (deploy targets live in .deploy.env.example)"
    apt_install apt-transport-https
    curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg \
      | $SUDO gpg --dearmor -o /usr/share/keyrings/cloud.google.gpg
    echo "deb [signed-by=/usr/share/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" \
      | $SUDO tee /etc/apt/sources.list.d/google-cloud-sdk.list >/dev/null
    apt_update
    apt_install google-cloud-cli
  fi
  if gcloud auth list --filter=status:ACTIVE --format="value(account)" 2>/dev/null | grep -q .; then
    log "gcloud authenticated as $(gcloud auth list --filter=status:ACTIVE --format='value(account)' 2>/dev/null | head -1)"
  else
    warn "gcloud is not authenticated."
    # --no-launch-browser is the right flow under WSL: there is no browser on
    # the Linux side, so gcloud prints a URL to open on the Windows side and
    # waits for you to paste the verification code back.
    if [ "$INTERACTIVE" = 1 ]; then
      echo ""
      echo "  gcloud will print a URL — open it in your Windows browser, approve,"
      echo "  then paste the verification code back here."
      read -r -p "  Run 'gcloud auth login' now? [y/N] " ans < /dev/tty
      case "$ans" in
        # No Application Default Credentials step. ADC is for client libraries;
        # deployaxongcp.sh only shells out to `gcloud compute ssh` / `scp`,
        # which use this user credential. Requesting ADC through the
        # --no-launch-browser flow also reliably fails with
        # "Scope has changed from ... to ...", so it was pure noise.
        [Yy]*) gcloud auth login --no-launch-browser < /dev/tty || warn "gcloud login did not complete" ;;
      esac
    else
      warn "  run: gcloud auth login --no-launch-browser"
    fi
  fi
fi

# ── Optional: Qdrant for long-term-memory work ───────────────────────────────
if [ "$W_QDRANT" = 1 ]; then
  step "Qdrant (dev, local binary)"
  if command -v qdrant >/dev/null 2>&1 || [ -x "$HOME/.local/bin/qdrant" ]; then
    log "qdrant already installed"
  else
    # Version matched to the pin in qdrant/install.sh so dev mirrors production.
    QV="$(sed -nE 's/^QDRANT_VERSION="?(v[0-9.]+)"?.*/\1/p' "$ROOT/qdrant/install.sh" 2>/dev/null | head -1)"
    QV="${QV:-v1.9.2}"
    if [ -z "$RUST_ARCH" ]; then
      warn "no qdrant release for $(uname -m) — skipping (the agent runs fine without it)"
      QV=""
    fi
    [ -n "$QV" ] && info "fetching qdrant $QV (static musl) into ~/.local/bin"
    mkdir -p "$HOME/.local/bin"; TMP="$(mktemp -d)"
    if curl -fsSL "https://github.com/qdrant/qdrant/releases/download/${QV}/qdrant-${RUST_ARCH}-unknown-linux-musl.tar.gz" -o "$TMP/q.tar.gz"; then
      tar -xzf "$TMP/q.tar.gz" -C "$TMP"
      install -m755 "$TMP/qdrant" "$HOME/.local/bin/qdrant"
      log "qdrant $QV -> ~/.local/bin/qdrant   (start it with: qdrant)"
      log "  .env already points QDRANT_URL at http://127.0.0.1:6333"
    else
      warn "qdrant download failed — skipping (the agent runs fine without it)"
    fi
    rm -rf "$TMP"
  fi
  case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) warn "~/.local/bin is not on PATH — add it in ~/.bashrc" ;; esac
fi

# ── Optional: graphify (CLAUDE.md / AGENTS.md tooling) ───────────────────────
if [ "$W_GRAPHIFY" = 1 ]; then
  step "graphify"
  if command -v graphify >/dev/null 2>&1; then
    log "graphify present"
  else
    # Python here is PEP 668 externally-managed, so pip --user is refused; pipx
    # is the supported route for a CLI tool.
    info "installing graphifyy via pipx"
    apt_install pipx
    pipx install graphifyy && pipx ensurepath || warn "graphify install failed — it is optional agent tooling, not a build dep"
  fi
fi

# ── Fonts for the image tool ─────────────────────────────────────────────────
# tools/image_tool.rs resolves fonts via app_data_files_dir().join("fonts") —
# i.e. data/files/fonts, relative to the agent's cwd or exe. That path is
# gitignored (.gitignore: **/data/files/) and is never shipped by the deploy
# script, so it has to be provisioned from the tracked copy in assets/fonts.
step "Fonts (image tool)"
FONT_SRC="$AGENT_DIR/assets/fonts"
FONT_DST="$AGENT_DIR/data/files/fonts"
if [ -d "$FONT_SRC" ]; then
  mkdir -p "$FONT_DST"
  # Never clobber: fonts added at runtime through the Files page stay put.
  cp -r --update=none "$FONT_SRC/." "$FONT_DST/" 2>/dev/null \
    || cp -rn "$FONT_SRC/." "$FONT_DST/" 2>/dev/null || true
  log "$(find "$FONT_DST" -type f | wc -l) fonts ready in data/files/fonts"
  # The image tool's built-in default; missing it means every un-styled render
  # silently falls back to a system font.
  [ -f "$FONT_DST/Playball-Regular.ttf" ] \
    || warn "Playball-Regular.ttf absent — it is image_tool.rs's default font"
else
  warn "no $FONT_SRC — the image tool will fall back to system fonts"
fi

# ── Agent .env ───────────────────────────────────────────────────────────────
if [ "$DO_ENV" = 1 ]; then
  step "crates/axon-agent/.env"
  if [ -f "$AGENT_DIR/.env" ]; then
    log ".env exists — left untouched (your file, never overwritten)"
    chmod 600 "$AGENT_DIR/.env" 2>/dev/null || true

    # No production-vs-dev audit here. This is a single-operator deployment:
    # the local instance sharing a bot token with the server is intentional, so
    # flagging it every run was noise rather than a finding. Only genuinely
    # boot-blocking problems are reported, below.
    val() { sed -nE "s/^$1=(.*)$/\1/p" "$AGENT_DIR/.env" 2>/dev/null | head -1; }
    if [ -z "$(val AXON_MASTER_KEY)" ] && [ -z "${AXON_DEV:-}" ]; then
      warn "AXON_MASTER_KEY is empty — the agent refuses to boot without it."
      if [ "$INTERACTIVE" = 1 ]; then
        echo ""
        echo "  Press Enter to generate a strong key, or paste an existing one"
        echo "  (e.g. your production key, to decrypt a copy of that database)."
        NEWKEY=""
        while :; do
          read -r -p "  AXON_MASTER_KEY: " NEWKEY < /dev/tty
          [ -z "$NEWKEY" ] && { NEWKEY="$(openssl rand -base64 32)"; echo "  generated a 32-byte random key"; }
          # Same rules as crypto::weak_key_reason, so a key accepted here can
          # never be rejected at boot.
          if [ "${#NEWKEY}" -lt 16 ]; then
            warn "  too short (${#NEWKEY} chars, minimum 16) — try again"; continue
          fi
          _low="$(printf '%s' "$NEWKEY" | tr '[:upper:]' '[:lower:]')"; _bad=""
          for f in changeme "your-" example placeholder secret123; do
            case "$_low" in *"$f"*) _bad="$f"; break ;; esac
          done
          [ -n "$_bad" ] && { warn "  contains placeholder text '$_bad' — try again"; continue; }
          if [ "$(printf '%s' "$NEWKEY" | fold -w1 | sort -u | wc -l)" -lt 5 ]; then
            warn "  fewer than 5 distinct characters — try again"; continue
          fi
          break
        done
        set_env_var "$AGENT_DIR/.env" AXON_MASTER_KEY "$NEWKEY"
        chmod 600 "$AGENT_DIR/.env"
        log "AXON_MASTER_KEY written to crates/axon-agent/.env (not echoed here)"
      else
        warn "  re-run interactively to be prompted for one, or set AXON_DEV=1"
      fi
    fi
  else
    # Verified safe: crates/axon-agent/.gitignore ignores .env, so the generated
    # master key cannot be swept up by the Stop hook's `git add -A` and pushed.
    if ! git -C "$ROOT" check-ignore -q "$AGENT_DIR/.env"; then
      err "REFUSING to write .env — it is not gitignored, and the auto-backup Stop hook would commit and push your master key."
    fi
    info "seeding .env from .env.example with generated secrets"
    cp "$AGENT_DIR/.env.example" "$AGENT_DIR/.env"
    MASTER_KEY="$(openssl rand -base64 32)"
    API_KEY="$(openssl rand -hex 24)"
    set_env_var "$AGENT_DIR/.env" AXON_MASTER_KEY "$MASTER_KEY"
    set_env_var "$AGENT_DIR/.env" AXON_API_KEY    "$API_KEY"
    chmod 600 "$AGENT_DIR/.env"
    log "wrote $AGENT_DIR/.env (mode 600; secrets not echoed here)"
  fi
fi

# ── Optional verification ────────────────────────────────────────────────────
if [ "$VERIFY" = 1 ] && [ "$DO_RUST" = 1 ]; then
  step "Verify"
  export PATH="$HOME/.cargo/bin:$PATH"
  info "cargo fetch (populating the registry cache)"
  ( cd "$ROOT" && cargo fetch )
  info "cargo check -p axon-core  (small crate — proves the toolchain compiles this workspace)"
  ( cd "$ROOT" && cargo check -p axon-core ) && log "toolchain verified against the real workspace"
fi

# ── Final report ─────────────────────────────────────────────────────────────
# Everything below RE-CHECKS live state rather than replaying messages collected
# earlier in the run. That matters: `gh auth login` uploads the SSH key as part
# of its own flow, so a "paste your key into GitHub" note gathered before that
# point would be stale and wrong by the time it printed. Anything already
# resolved simply does not appear.
step "Summary"

TODO=()      # things that still need a human
ok()   { printf "  ${G}✓${N} %-13s %s\n" "$1" "$2"; }
miss() { printf "  ${Y}!${N} %-13s %s\n" "$1" "$2"; }

# ── Toolchain ──
command -v git   >/dev/null 2>&1 && ok "git"    "$(git --version | awk '{print $3}')"
if command -v rustc >/dev/null 2>&1; then ok "rust" "$(rustc -V | awk '{print $2}')"
elif [ "$DO_RUST" = 1 ]; then miss "rust" "not installed"; TODO+=("Rust did not install — re-run, or see $APT_LOG"); fi
command -v cargo-deny >/dev/null 2>&1 && ok "cargo-deny" "$(cargo-deny --version | awk '{print $2}')"
if command -v node >/dev/null 2>&1; then ok "node" "$(node -v) / npm $(npm -v)"
elif [ "$DO_NODE" = 1 ]; then miss "node" "not installed"; TODO+=("Node did not install — re-run, or see $APT_LOG"); fi
command -v gh     >/dev/null 2>&1 && ok "gh"     "$(gh --version | head -1 | awk '{print $3}')"
command -v gcloud >/dev/null 2>&1 && ok "gcloud" "$(gcloud --version 2>/dev/null | head -1 | awk '{print $NF}')"
command -v qdrant >/dev/null 2>&1 && ok "qdrant" "local binary"
[ -x "$HOME/.cargo/bin/cargo" ] && [ "$W_MUSL" = 1 ] && \
  ("$HOME/.cargo/bin/rustup" target list --installed 2>/dev/null | grep -q musl) && ok "musl" "target installed"

# ── Can you commit and push? ──
echo ""
GN="$(git config --get user.name  || true)"
GE="$(git config --get user.email || true)"
if [ -n "$GN" ] && [ -n "$GE" ]; then ok "git identity" "$GN <$GE>"
else miss "git identity" "incomplete"; TODO+=("Set your git identity: git config --global user.email 'you@example.com'"); fi

if [ "$DO_GIT" = 1 ]; then
  SSH_PROBE="$(ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -T git@github.com 2>&1 || true)"
  if [ "${SSH_PROBE#*successfully authenticated}" != "$SSH_PROBE" ]; then
    ok "github ssh" "key accepted — push works"
  else
    miss "github ssh" "key not accepted yet"
    TODO+=("Add ~/.ssh/id_ed25519.pub at https://github.com/settings/ssh/new — until then git push and the auto-backup hook fail")
  fi
fi

# ── Cloud / release tooling (only reported when the tool is actually present) ──
if command -v gh >/dev/null 2>&1; then
  gh auth status >/dev/null 2>&1 && ok "gh auth" "authenticated" \
    || { miss "gh auth" "not authenticated"; TODO+=("Run 'gh auth login' when you want to cut releases"); }
fi
if command -v gcloud >/dev/null 2>&1; then
  GACC="$(gcloud auth list --filter=status:ACTIVE --format='value(account)' 2>/dev/null | head -1)"
  if [ -n "$GACC" ]; then
    GPROJ="$(gcloud config get-value project 2>/dev/null | grep -v '^$' | grep -v unset || true)"
    if [ -n "$GPROJ" ]; then ok "gcloud" "$GACC ($GPROJ)"
    else ok "gcloud" "$GACC"; TODO+=("No GCP project selected: gcloud config set project <PROJECT_ID>"); fi
  else
    miss "gcloud" "not authenticated"; TODO+=("Run 'gcloud auth login --no-launch-browser' before deploying")
  fi
fi

# ── Project state ──
echo ""
FONT_N=$(find "$AGENT_DIR/data/files/fonts" -type f 2>/dev/null | wc -l)
[ "$FONT_N" -gt 0 ] && ok "fonts" "$FONT_N in data/files/fonts" \
  || { miss "fonts" "none provisioned"; TODO+=("Image-tool fonts missing from crates/axon-agent/data/files/fonts"); }

[ -d "$UI_DIR/node_modules" ] && ok "axon-ui" "dependencies installed" \
  || { miss "axon-ui" "node_modules missing"; TODO+=("Run: cd axon-ui && npm ci"); }

if [ -f "$AGENT_DIR/.env" ]; then
  # Only report what would actually stop the agent booting. Sharing a bot token
  # with the server is deliberate on a single-operator setup, so it is not
  # flagged; the master key genuinely is required (crypto::validate_master_key).
  if [ -n "$(sed -nE 's/^AXON_MASTER_KEY=(.*)$/\1/p' "$AGENT_DIR/.env" 2>/dev/null | head -1)" ]; then
    ok ".env" "present, master key set"
  else
    miss ".env" "AXON_MASTER_KEY empty"
    TODO+=("Set AXON_MASTER_KEY in crates/axon-agent/.env (or export AXON_DEV=1) — the agent refuses to boot without it")
  fi
else
  miss ".env" "missing"; TODO+=("crates/axon-agent/.env is missing — the agent will refuse to boot")
fi

# Deploy prerequisites: only mention them if something is actually absent.
ls "$ROOT"/deploy*.sh >/dev/null 2>&1 \
  || TODO+=("No deploy*.sh (gitignored, so it never came with the clone): cp deploy.sh.example deployaxongcp.sh")
[ -f "$ROOT/.deploy.env" ] \
  || TODO+=("No .deploy.env — deployaxongcp.sh aborts without it: cp .deploy.env.example .deploy.env")

# ── Verdict ──
echo ""
if [ ${#TODO[@]} -eq 0 ]; then
  echo -e "${G}${B}✓ Setup complete — your dev environment is ready.${N}"
else
  echo -e "${Y}${B}✓ Setup complete — ${#TODO[@]} item(s) still need you:${N}"
  i=1; for t in "${TODO[@]}"; do echo -e "  ${Y}$i.${N} $t"; i=$((i+1)); done
fi

echo ""
echo -e "${B}Start working${N}"
echo "  source \$HOME/.cargo/env     # or open a new shell"
echo "  bash scripts/dev.sh         # build UI + run agent -> http://localhost:3000"
echo "  bash scripts/dev.sh --hmr   # vite HMR on :5173"
echo "  cargo test --workspace && cargo deny check      # the CI gates"
rm -f "$APT_LOG" 2>/dev/null || true
