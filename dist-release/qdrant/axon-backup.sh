#!/usr/bin/env bash
# =============================================================================
# axon-backup — weekly Qdrant snapshot
# Creates snapshots of all collections and copies them off-instance
#
# Configure BACKUP_DEST below. Options:
#   rsync to another server:  rsync -avz ...
#   rclone to cloud storage:  rclone copy ...
#   scp to another host:      scp ...
#
# Free backup destinations:
#   - Backblaze B2 (10GB free, rclone compatible)
#   - Another Oracle Free Tier instance
#   - GitHub private repo (small snapshots only)
#   - Any SSH server you control
# =============================================================================

set -euo pipefail

QDRANT="http://localhost:6333"
SNAPSHOT_DIR="/var/lib/qdrant/snapshots"
BACKUP_DIR="/var/lib/qdrant/backups"   # local staging area
RETENTION_DAYS=30                      # keep 30 days of local backups
LOG_PREFIX="[$(date '+%Y-%m-%d %H:%M:%S')] BACKUP"

# ── Configure your remote destination here ────────────────────────────────────
# Examples — uncomment and edit ONE:

# Option A: rsync to another server
# BACKUP_DEST="user@backup-host.example.com:~/qdrant-backups/"
# BACKUP_CMD="rsync -avz --delete $BACKUP_DIR/ $BACKUP_DEST"

# Option B: rclone to Backblaze B2
# BACKUP_DEST="b2:your-bucket-name/qdrant-backups/"
# BACKUP_CMD="rclone sync $BACKUP_DIR $BACKUP_DEST --b2-hard-delete"

# Option C: scp to secondary Oracle instance
# BACKUP_DEST="ubuntu@second-oracle-instance:~/qdrant-backups/"
# BACKUP_CMD="rsync -avz -e 'ssh -i ~/.ssh/backup_key' $BACKUP_DIR/ $BACKUP_DEST"

# Option D: local only (no remote — NOT recommended for production)
BACKUP_DEST="local only"
BACKUP_CMD=""

log()  { echo "$LOG_PREFIX $*"; }
warn() { echo "$LOG_PREFIX [WARN] $*"; }
err()  { echo "$LOG_PREFIX [ERROR] $*"; }

# ── Sanity checks ─────────────────────────────────────────────────────────────
if ! curl -sf "${QDRANT}/healthz" > /dev/null; then
  err "Qdrant not responding — aborting backup"
  exit 1
fi

mkdir -p "$BACKUP_DIR"
DATE=$(date '+%Y-%m-%d')
log "Starting backup — $DATE"
log "Destination: $BACKUP_DEST"

# ── Get list of collections ───────────────────────────────────────────────────
COLLECTIONS=$(curl -sf "${QDRANT}/collections" \
  | grep -o '"name":"[^"]*"' \
  | grep -o ':"[^"]*"' \
  | tr -d ':"')

if [ -z "$COLLECTIONS" ]; then
  warn "No collections found — nothing to back up"
  exit 0
fi

log "Collections to back up: $(echo $COLLECTIONS | tr '\n' ' ')"

# ── Create snapshot for each collection ──────────────────────────────────────
BACKUP_DATE_DIR="$BACKUP_DIR/$DATE"
mkdir -p "$BACKUP_DATE_DIR"

for COLL in $COLLECTIONS; do
  log "Snapshotting: $COLL"

  # Trigger snapshot creation
  SNAP_RESPONSE=$(curl -sf -X POST "${QDRANT}/collections/${COLL}/snapshots")
  SNAP_NAME=$(echo "$SNAP_RESPONSE" | grep -o '"name":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')

  if [ -z "$SNAP_NAME" ]; then
    err "Failed to create snapshot for $COLL: $SNAP_RESPONSE"
    continue
  fi

  log "  Snapshot created: $SNAP_NAME"

  # Download snapshot from Qdrant
  SNAP_FILE="$BACKUP_DATE_DIR/${COLL}-${DATE}.snapshot"
  curl -sf "${QDRANT}/collections/${COLL}/snapshots/${SNAP_NAME}" \
    -o "$SNAP_FILE"

  SNAP_SIZE=$(du -sh "$SNAP_FILE" | cut -f1)
  log "  Downloaded: $SNAP_FILE ($SNAP_SIZE)"

  # Clean up snapshot from Qdrant's snapshot dir (save disk space)
  curl -sf -X DELETE "${QDRANT}/collections/${COLL}/snapshots/${SNAP_NAME}" > /dev/null || true
done

# ── Compress the backup ───────────────────────────────────────────────────────
log "Compressing backup..."
ARCHIVE="$BACKUP_DIR/qdrant-backup-${DATE}.tar.gz"
tar -czf "$ARCHIVE" -C "$BACKUP_DIR" "$DATE"
rm -rf "$BACKUP_DATE_DIR"

ARCHIVE_SIZE=$(du -sh "$ARCHIVE" | cut -f1)
log "Archive created: $ARCHIVE ($ARCHIVE_SIZE)"

# ── Copy to remote ────────────────────────────────────────────────────────────
if [ -n "$BACKUP_CMD" ]; then
  log "Copying to remote: $BACKUP_DEST"
  eval "$BACKUP_CMD" && log "Remote copy complete" || warn "Remote copy failed — local backup retained"
else
  warn "No remote backup configured. Local-only backup at $BACKUP_DIR"
  warn "Configure BACKUP_DEST in /usr/local/bin/axon-backup for off-instance storage"
fi

# ── Clean up old local backups ────────────────────────────────────────────────
log "Cleaning backups older than ${RETENTION_DAYS} days..."
find "$BACKUP_DIR" -name "qdrant-backup-*.tar.gz" -mtime "+${RETENTION_DAYS}" -delete
REMAINING=$(ls "$BACKUP_DIR"/qdrant-backup-*.tar.gz 2>/dev/null | wc -l)
log "Local backups retained: $REMAINING"

# ── Final summary ─────────────────────────────────────────────────────────────
TOTAL_BACKUP_SIZE=$(du -sh "$BACKUP_DIR" 2>/dev/null | cut -f1 || echo "unknown")
log "Backup complete. Total backup dir size: $TOTAL_BACKUP_SIZE"
log "Backup location: $BACKUP_DIR"
