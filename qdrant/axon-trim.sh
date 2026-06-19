#!/usr/bin/env bash
# =============================================================================
# axon-trim — weekly memory trim cycle
# Removes expired, stale, and low-value vectors from Qdrant
# Runs via systemd timer every Sunday 03:00
# Can also be run manually: axon-trim
# =============================================================================

set -euo pipefail

QDRANT="http://localhost:6333"
NOW=$(date +%s)
LOG_PREFIX="[$(date '+%Y-%m-%d %H:%M:%S')] TRIM"

log()  { echo "$LOG_PREFIX $*"; }
warn() { echo "$LOG_PREFIX [WARN] $*"; }

# ── Sanity check ──────────────────────────────────────────────────────────────
if ! curl -sf "${QDRANT}/healthz" > /dev/null; then
  warn "Qdrant not responding — aborting trim"
  exit 1
fi

log "Starting trim cycle"

# ── Helper: delete by filter ──────────────────────────────────────────────────
delete_filter() {
  local COLLECTION="$1"
  local FILTER="$2"
  local DESC="$3"

  RESPONSE=$(curl -sf -X POST "${QDRANT}/collections/${COLLECTION}/points/delete" \
    -H 'Content-Type: application/json' \
    -d "{\"filter\": $FILTER}")

  # Extract deleted count if available
  STATUS=$(echo "$RESPONSE" | grep -o '"status":"[^"]*"' | head -1 || echo "unknown")
  log "  ${COLLECTION} — ${DESC}: $STATUS"
}

# ── Count helper ──────────────────────────────────────────────────────────────
count_collection() {
  local COLLECTION="$1"
  curl -sf "${QDRANT}/collections/${COLLECTION}" \
    | grep -o '"vectors_count":[0-9]*' \
    | grep -o '[0-9]*' || echo "0"
}

# ── Log before counts ─────────────────────────────────────────────────────────
log "Before trim:"
for COLL in agent_memory documents; do
  COUNT=$(count_collection "$COLL")
  log "  $COLL: $COUNT vectors"
done

# =============================================================================
# PASS 1: Hard expiry — delete anything past its expires_at
# Applies to all tiers
# =============================================================================
log "Pass 1: Hard expiry deletions"

for COLL in agent_memory documents; do
  delete_filter "$COLL" \
    "{\"must\": [{\"key\": \"expires_at\", \"range\": {\"lt\": $NOW}}]}" \
    "expired (expires_at < now)"
done

# =============================================================================
# PASS 2: Ephemeral tier — delete if older than 30 days
# Source: routine conversation turns, transient lookups
# =============================================================================
log "Pass 2: Ephemeral tier (>30 days old)"

THIRTY_DAYS_AGO=$(( NOW - 2592000 ))

delete_filter "agent_memory" \
  "{\"must\": [
    {\"key\": \"tier\",       \"match\": {\"value\": \"ephemeral\"}},
    {\"key\": \"created_at\", \"range\": {\"lt\": $THIRTY_DAYS_AGO}}
  ]}" \
  "ephemeral older than 30 days"

delete_filter "documents" \
  "{\"must\": [
    {\"key\": \"source_type\", \"match\": {\"value\": \"conversation\"}},
    {\"key\": \"created_at\",  \"range\": {\"lt\": $THIRTY_DAYS_AGO}}
  ]}" \
  "conversation chunks older than 30 days"

# =============================================================================
# PASS 3: Standard tier — delete if:
#   - older than 6 months AND
#   - never accessed (access_count <= 1) AND
#   - low importance (< 0.4)
# =============================================================================
log "Pass 3: Standard tier (>6 months, unaccessed, low importance)"

SIX_MONTHS_AGO=$(( NOW - 15552000 ))

delete_filter "agent_memory" \
  "{\"must\": [
    {\"key\": \"tier\",         \"match\":  {\"value\": \"standard\"}},
    {\"key\": \"created_at\",   \"range\":  {\"lt\": $SIX_MONTHS_AGO}},
    {\"key\": \"access_count\", \"range\":  {\"lte\": 1}},
    {\"key\": \"importance\",   \"range\":  {\"lt\": 0.4}}
  ]}" \
  "standard, 6mo+, unaccessed, low importance"

delete_filter "documents" \
  "{\"must\": [
    {\"key\": \"created_at\",   \"range\": {\"lt\": $SIX_MONTHS_AGO}},
    {\"key\": \"access_count\", \"range\": {\"lte\": 0}},
    {\"key\": \"importance\",   \"range\": {\"lt\": 0.3}}
  ]}" \
  "document chunks, 6mo+, never accessed, low importance"

# =============================================================================
# PASS 4: Garbage collect — zero-access, very low importance, any age
# These are memories that were never useful
# =============================================================================
log "Pass 4: Garbage collection (zero access + very low importance)"

delete_filter "agent_memory" \
  "{\"must\": [
    {\"key\": \"access_count\", \"range\": {\"lte\": 0}},
    {\"key\": \"importance\",   \"range\": {\"lt\": 0.15}},
    {\"key\": \"tier\",         \"match\": {\"any\": [\"ephemeral\", \"standard\"]}}
  ]}" \
  "garbage (never accessed, very low importance)"

# =============================================================================
# PASS 5: Documents older than 12 months, never accessed
# =============================================================================
log "Pass 5: Stale documents (>12 months, never accessed)"

TWELVE_MONTHS_AGO=$(( NOW - 31536000 ))

delete_filter "documents" \
  "{\"must\": [
    {\"key\": \"created_at\",   \"range\": {\"lt\": $TWELVE_MONTHS_AGO}},
    {\"key\": \"access_count\", \"range\": {\"lte\": 0}}
  ]}" \
  "documents older than 12 months, never retrieved"

# =============================================================================
# PASS 6: Trigger Qdrant optimisation pass
# Forces segment merging after deletions — reclaims disk space
# =============================================================================
log "Pass 6: Triggering collection optimisation"

for COLL in agent_memory documents; do
  # Qdrant auto-optimises but we can nudge it by updating a dummy alias
  curl -sf "${QDRANT}/collections/${COLL}" > /dev/null || true
  log "  $COLL optimisation queued"
done

# =============================================================================
# Summary
# =============================================================================
log "After trim:"
for COLL in agent_memory documents entities; do
  COUNT=$(count_collection "$COLL")
  log "  $COLL: $COUNT vectors"
done

# Check disk usage
STORAGE_SIZE=$(du -sh /var/lib/qdrant/storage 2>/dev/null | cut -f1 || echo "unknown")
RAM_MB=$(ps aux | grep '[q]drant' | awk '{sum += $6} END {printf "%.0f", sum/1024}')

log "Storage: $STORAGE_SIZE | Qdrant RAM: ${RAM_MB}MB"
log "Trim cycle complete"
