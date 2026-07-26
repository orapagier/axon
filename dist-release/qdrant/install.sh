#!/usr/bin/env bash
# =============================================================================
# axon-qdrant install.sh
# Full Qdrant setup optimized for Oracle E2 Micro (1GB RAM), no Docker
# Run as your normal user (not root). Will sudo when needed.
# Usage: bash install.sh
# =============================================================================

set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
B='\033[1m'; G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; C='\033[0;36m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
err()  { echo -e "${R}[✗]${N} $*" >&2; exit 1; }
info() { echo -e "${C}[→]${N} $*"; }
step() { echo -e "\n${B}━━━ $* ━━━${N}"; }

# ── Config ────────────────────────────────────────────────────────────────────
QDRANT_VERSION="v1.9.2"           # pin — don't auto-update
QDRANT_BIN="/usr/local/bin/qdrant"
QDRANT_DATA="/var/lib/qdrant"
QDRANT_CFG="/etc/qdrant"
QDRANT_USER="$(whoami)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  x86_64)  QDRANT_ARCH="x86_64-unknown-linux-musl" ;;
  aarch64) QDRANT_ARCH="aarch64-unknown-linux-musl" ;;
  *)       err "Unsupported architecture: $ARCH" ;;
esac

echo ""
echo -e "${B}╔═══════════════════════════════════════════╗${N}"
echo -e "${B}║   axon-qdrant setup — Oracle E2 optimised ║${N}"
echo -e "${B}╚═══════════════════════════════════════════╝${N}"
echo ""
info "Architecture : $ARCH"
info "Qdrant ver   : $QDRANT_VERSION"
info "Data dir     : $QDRANT_DATA"
info "Config dir   : $QDRANT_CFG"
info "Running as   : $QDRANT_USER"
echo ""

# ── Phase 1: System preparation ───────────────────────────────────────────────
step "Phase 1: System preparation"

info "Updating package list..."
sudo apt-get update -qq

info "Installing required packages..."
sudo apt-get install -y -qq curl wget logrotate jq bc cron

# Create swap file (512MB — emergency buffer only)
if [ ! -f /swapfile ]; then
  info "Creating 512MB swap file (emergency buffer)..."
  sudo fallocate -l 512M /swapfile
  sudo chmod 600 /swapfile
  sudo mkswap /swapfile
  sudo swapon /swapfile
  echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab > /dev/null
  # Make swap reluctant — last resort only
  echo 'vm.swappiness=5' | sudo tee -a /etc/sysctl.conf > /dev/null
  sudo sysctl -p > /dev/null
  log "Swap created (swappiness=5)"
else
  warn "Swap file already exists — skipping"
fi

# ── Phase 2: Install Qdrant binary ────────────────────────────────────────────
step "Phase 2: Install Qdrant binary"

DOWNLOAD_URL="https://github.com/qdrant/qdrant/releases/download/${QDRANT_VERSION}/qdrant-${QDRANT_ARCH}.tar.gz"
TARBALL="/tmp/qdrant-${QDRANT_VERSION}.tar.gz"

if [ -f "$QDRANT_BIN" ]; then
  CURRENT_VER=$("$QDRANT_BIN" --version 2>/dev/null | awk '{print $2}' || echo "unknown")
  warn "Qdrant already installed (version: $CURRENT_VER)"
  # Unattended: assume no reinstall to prevent deploy script from hanging
  REINSTALL="N"
  [[ "${REINSTALL,,}" != "y" ]] && { log "Keeping existing binary."; } || {
    info "Downloading Qdrant $QDRANT_VERSION ($QDRANT_ARCH)..."
    wget -q --show-progress "$DOWNLOAD_URL" -O "$TARBALL"
    tar -xzf "$TARBALL" -C /tmp/
    sudo mv /tmp/qdrant "$QDRANT_BIN"
    sudo chmod +x "$QDRANT_BIN"
    rm -f "$TARBALL"
    log "Qdrant $QDRANT_VERSION installed"
  }
else
  info "Downloading Qdrant $QDRANT_VERSION ($QDRANT_ARCH)..."
  wget -q --show-progress "$DOWNLOAD_URL" -O "$TARBALL"
  tar -xzf "$TARBALL" -C /tmp/
  sudo mv /tmp/qdrant "$QDRANT_BIN"
  sudo chmod +x "$QDRANT_BIN"
  rm -f "$TARBALL"
  log "Qdrant $QDRANT_VERSION installed → $QDRANT_BIN"
fi

# Verify
"$QDRANT_BIN" --version || err "Qdrant binary not working"

# ── Phase 3: Directories and permissions ──────────────────────────────────────
step "Phase 3: Directories and permissions"

sudo mkdir -p "$QDRANT_DATA/storage"
sudo mkdir -p "$QDRANT_DATA/snapshots"
sudo mkdir -p "$QDRANT_CFG"
sudo mkdir -p /var/log/qdrant

sudo chown -R "$QDRANT_USER:$QDRANT_USER" "$QDRANT_DATA"
sudo chown -R "$QDRANT_USER:$QDRANT_USER" /var/log/qdrant
sudo chown -R "$QDRANT_USER:$QDRANT_USER" "$QDRANT_CFG"
sudo chmod 755 "$QDRANT_DATA"

log "Directories created and permissions set"

# ── Phase 4: Qdrant config ────────────────────────────────────────────────────
step "Phase 4: Writing Qdrant config (RAM-optimised)"

sudo cp "$SCRIPT_DIR/qdrant.yaml" "$QDRANT_CFG/config.yaml"
log "Config written to $QDRANT_CFG/config.yaml"

# ── Phase 5: systemd services ─────────────────────────────────────────────────
step "Phase 5: Installing systemd services"

info "Patching systemd services with user $QDRANT_USER..."
for svc in "$SCRIPT_DIR"/*.service; do
  sed -i "s/User=ubuntu/User=$QDRANT_USER/" "$svc"
  sed -i "s/Group=ubuntu/Group=$(id -gn $QDRANT_USER)/" "$svc"
done

sudo cp "$SCRIPT_DIR/qdrant.service"              /etc/systemd/system/
sudo cp "$SCRIPT_DIR/axon-trim.service"           /etc/systemd/system/
sudo cp "$SCRIPT_DIR/axon-trim.timer"             /etc/systemd/system/
sudo cp "$SCRIPT_DIR/axon-backup.service"         /etc/systemd/system/
sudo cp "$SCRIPT_DIR/axon-backup.timer"           /etc/systemd/system/
sudo cp "$SCRIPT_DIR/axon-health.service"         /etc/systemd/system/
sudo cp "$SCRIPT_DIR/axon-health.timer"           /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable qdrant
sudo systemctl enable axon-trim.timer
sudo systemctl enable axon-backup.timer
sudo systemctl enable axon-health.timer

log "systemd units installed and enabled"

# ── Phase 6: Scripts ──────────────────────────────────────────────────────────
step "Phase 6: Installing maintenance scripts"

sudo cp "$SCRIPT_DIR/axon-trim.sh"    /usr/local/bin/axon-trim
sudo cp "$SCRIPT_DIR/axon-backup.sh"  /usr/local/bin/axon-backup
sudo cp "$SCRIPT_DIR/axon-health.sh"  /usr/local/bin/axon-health
sudo cp "$SCRIPT_DIR/axon-restore.sh" /usr/local/bin/axon-restore
sudo cp "$SCRIPT_DIR/axon-status.sh"  /usr/local/bin/axon-status

sudo chmod +x /usr/local/bin/axon-{trim,backup,health,restore,status}
log "Scripts installed to /usr/local/bin/"

# ── Phase 7: Log rotation ─────────────────────────────────────────────────────
step "Phase 7: Log rotation"

sudo tee /etc/logrotate.d/qdrant > /dev/null <<'EOF'
/var/log/qdrant/*.log {
    daily
    rotate 7
    compress
    delaycompress
    missingok
    notifempty
    maxsize 20M
    postrotate
        systemctl kill -s HUP qdrant 2>/dev/null || true
    endscript
}
EOF

sudo tee /etc/logrotate.d/axon-mcp > /dev/null <<'EOF'
/var/log/axon-mcp/*.log {
    daily
    rotate 7
    compress
    delaycompress
    missingok
    notifempty
    maxsize 20M
}
EOF

log "Log rotation configured"

# ── Phase 8: Start Qdrant ─────────────────────────────────────────────────────
step "Phase 8: Starting Qdrant"

sudo systemctl start qdrant
sleep 3

if sudo systemctl is-active --quiet qdrant; then
  log "Qdrant is running"
else
  err "Qdrant failed to start. Check: sudo journalctl -u qdrant -n 50"
fi

# Verify HTTP endpoint
for i in 1 2 3 4 5; do
  if curl -sf http://localhost:6333/healthz > /dev/null; then
    log "Qdrant health check passed"
    break
  fi
  sleep 2
  [ $i -eq 5 ] && err "Qdrant not responding on :6333 after 10s"
done

# ── Phase 9: Create collections ───────────────────────────────────────────────
step "Phase 9: Creating optimised collections"

bash "$SCRIPT_DIR/create-collections.sh"

# ── Phase 10: Start timers ────────────────────────────────────────────────────
step "Phase 10: Starting maintenance timers"

sudo systemctl start axon-trim.timer
sudo systemctl start axon-backup.timer
sudo systemctl start axon-health.timer

log "All timers started"

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo -e "${B}═══════════════════════════════════════════════════${N}"
echo -e "${G}${B}  ✅  axon-qdrant setup complete!${N}"
echo -e "${B}═══════════════════════════════════════════════════${N}"
echo ""
echo -e "  Qdrant API  : ${C}http://localhost:6333${N}"
echo -e "  gRPC        : ${C}localhost:6334${N}"
echo -e "  Data        : ${C}$QDRANT_DATA/storage${N}"
echo -e "  Snapshots   : ${C}$QDRANT_DATA/snapshots${N}"
echo -e "  Config      : ${C}$QDRANT_CFG/config.yaml${N}"
echo ""
echo -e "  Quick commands:"
echo -e "    ${B}axon-status${N}   — RAM, disk, vector counts"
echo -e "    ${B}axon-health${N}   — full health check"
echo -e "    ${B}axon-backup${N}   — manual snapshot now"
echo -e "    ${B}axon-trim${N}     — manual trim cycle now"
echo -e "    ${B}axon-restore${N}  — restore from snapshot"
echo ""
echo -e "  Timers active:"
sudo systemctl list-timers axon-* --no-pager 2>/dev/null | grep axon || true
echo ""
warn "IMPORTANT: Configure backup destination in /usr/local/bin/axon-backup"
warn "           before your first automated backup runs."
echo ""
