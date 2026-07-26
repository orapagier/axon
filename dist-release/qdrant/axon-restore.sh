#!/usr/bin/env bash
# =============================================================================
# axon-restore — restore Qdrant collections from a backup snapshot
# Usage: axon-restore [backup-file.tar.gz]
#        axon-restore          (interactive — lists available backups)
# =============================================================================

set -euo pipefail

QDRANT="http://localhost:6333"
BACKUP_DIR="/var/lib/qdrant/backups"
RESTORE_TMP="/tmp/axon-qdrant-restore"

G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; C='\033[0;36m'; B='\033[1m'; N='\033[0m'
log()  { echo -e "${G}[✓]${N} $*"; }
warn() { echo -e "${Y}[!]${N} $*"; }
err()  { echo -e "${R}[✗]${N} $*" >&2; exit 1; }
info() { echo -e "${C}[→]${N} $*"; }

echo ""
echo -e "${B}axon-qdrant restore${N}"
echo ""

# ── Select backup file ────────────────────────────────────────────────────────
if [ -n "${1:-}" ]; then
  BACKUP_FILE="$1"
else
  # Interactive selection
  BACKUPS=($(ls "$BACKUP_DIR"/qdrant-backup-*.tar.gz 2>/dev/null | sort -r || true))

  if [ ${#BACKUPS[@]} -eq 0 ]; then
    err "No backups found in $BACKUP_DIR"
  fi

  echo "Available backups:"
  for i in "${!BACKUPS[@]}"; do
    SIZE=$(du -sh "${BACKUPS[$i]}" | cut -f1)
    DATE=$(basename "${BACKUPS[$i]}" .tar.gz | sed 's/qdrant-backup-//')
    echo "  [$i] $DATE  ($SIZE)"
  done
  echo ""
  read -rp "Select backup number [0]: " CHOICE
  CHOICE="${CHOICE:-0}"
  BACKUP_FILE="${BACKUPS[$CHOICE]}"
fi

[ -f "$BACKUP_FILE" ] || err "Backup file not found: $BACKUP_FILE"
BACKUP_DATE=$(basename "$BACKUP_FILE" .tar.gz | sed 's/qdrant-backup-//')
info "Restoring from: $BACKUP_FILE ($BACKUP_DATE)"

# ── Confirmation ──────────────────────────────────────────────────────────────
echo ""
warn "This will DELETE all current data in Qdrant and restore from backup."
warn "Current collections will be lost."
echo ""
read -rp "$(echo -e "${R}Type 'yes' to confirm restore:${N} ")" CONFIRM
[ "$CONFIRM" = "yes" ] || { echo "Aborted."; exit 0; }

# ── Check Qdrant is running ───────────────────────────────────────────────────
if ! curl -sf "${QDRANT}/healthz" > /dev/null; then
  warn "Qdrant not running — starting it"
  sudo systemctl start qdrant
  sleep 5
fi

# ── Extract backup ────────────────────────────────────────────────────────────
info "Extracting backup..."
rm -rf "$RESTORE_TMP"
mkdir -p "$RESTORE_TMP"
tar -xzf "$BACKUP_FILE" -C "$RESTORE_TMP"

SNAPSHOT_DIR=$(find "$RESTORE_TMP" -name "*.snapshot" -type f -exec dirname {} \; | head -1)
[ -n "$SNAPSHOT_DIR" ] || err "No snapshot files found in backup"

log "Snapshots found:"
ls "$SNAPSHOT_DIR"/*.snapshot 2>/dev/null | while read -r f; do
  echo "  $(basename $f)"
done

# ── Delete existing collections ───────────────────────────────────────────────
info "Removing existing collections..."
EXISTING=$(curl -sf "${QDRANT}/collections" \
  | grep -o '"name":"[^"]*"' \
  | grep -o ':"[^"]*"' \
  | tr -d ':"' 2>/dev/null || echo "")

for COLL in $EXISTING; do
  info "  Deleting: $COLL"
  curl -sf -X DELETE "${QDRANT}/collections/${COLL}" > /dev/null
done
log "Existing collections removed"

# ── Restore each snapshot ─────────────────────────────────────────────────────
info "Uploading snapshots..."

for SNAP_FILE in "$SNAPSHOT_DIR"/*.snapshot; do
  SNAP_NAME=$(basename "$SNAP_FILE" .snapshot)
  # Extract collection name from snapshot filename (format: collectionname-YYYY-MM-DD.snapshot)
  COLL_NAME=$(echo "$SNAP_NAME" | sed 's/-[0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}$//')

  info "  Restoring collection: $COLL_NAME"

  RESPONSE=$(curl -sf -X POST "${QDRANT}/collections/${COLL_NAME}/snapshots/upload" \
    -H 'Content-Type: multipart/form-data' \
    -F "snapshot=@${SNAP_FILE}")

  if echo "$RESPONSE" | grep -q '"result"'; then
    log "  $COLL_NAME restored"
  else
    warn "  $COLL_NAME restore may have issues: $RESPONSE"
  fi
done

# ── Wait for indexing ─────────────────────────────────────────────────────────
info "Waiting for collections to index (30 seconds)..."
sleep 30

# ── Verify ────────────────────────────────────────────────────────────────────
log "Verification:"
RESTORED=$(curl -sf "${QDRANT}/collections" \
  | grep -o '"name":"[^"]*"' \
  | grep -o ':"[^"]*"' \
  | tr -d ':"' 2>/dev/null || echo "")

for COLL in $RESTORED; do
  COUNT=$(curl -sf "${QDRANT}/collections/${COLL}" \
    | grep -o '"vectors_count":[0-9]*' | grep -o '[0-9]*' || echo "0")
  log "  $COLL: $COUNT vectors"
done

# ── Cleanup ───────────────────────────────────────────────────────────────────
rm -rf "$RESTORE_TMP"

echo ""
log "Restore complete from backup: $BACKUP_DATE"
