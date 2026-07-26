#!/usr/bin/env bash
# =============================================================================
# axon-health — daily health check
# Checks RAM, disk, Qdrant responsiveness, and collection sizes
# Logs warnings when thresholds are approached
# =============================================================================

set -euo pipefail

QDRANT="http://localhost:6333"
LOG_PREFIX="[$(date '+%Y-%m-%d %H:%M:%S')] HEALTH"

# ── Thresholds ────────────────────────────────────────────────────────────────
RAM_WARN_PCT=80       # Warn if RAM usage > 80%
DISK_WARN_PCT=75      # Warn if disk usage > 75%
QDRANT_RAM_WARN_MB=350  # Warn if Qdrant RAM > 350MB (limit is 400)
VECTOR_WARN=50000     # Warn if any collection exceeds this

log()  { echo "$LOG_PREFIX $*"; }
warn() { echo "$LOG_PREFIX [WARN] ⚠  $*"; }
ok()   { echo "$LOG_PREFIX [OK]   ✓  $*"; }
crit() { echo "$LOG_PREFIX [CRIT] ✗  $*"; }

log "═══════════════════════════════════════"
log "Daily health check"
log "═══════════════════════════════════════"

ISSUES=0

# ── RAM check ─────────────────────────────────────────────────────────────────
TOTAL_RAM=$(free -m | awk 'NR==2{print $2}')
USED_RAM=$(free -m  | awk 'NR==2{print $3}')
FREE_RAM=$(free -m  | awk 'NR==2{print $4}')
RAM_PCT=$(( USED_RAM * 100 / TOTAL_RAM ))

log "RAM: ${USED_RAM}MB / ${TOTAL_RAM}MB used (${RAM_PCT}%) — ${FREE_RAM}MB free"

if [ "$RAM_PCT" -gt "$RAM_WARN_PCT" ]; then
  warn "RAM usage at ${RAM_PCT}% — approaching limit"
  ISSUES=$((ISSUES + 1))
else
  ok "RAM usage normal (${RAM_PCT}%)"
fi

# ── Qdrant process RAM ────────────────────────────────────────────────────────
QDRANT_RAM_KB=$(ps aux | grep '[q]drant' | awk '{sum += $6} END {print sum}')
QDRANT_RAM_MB=$(echo "scale=0; $QDRANT_RAM_KB / 1024" | bc)

log "Qdrant process RAM: ${QDRANT_RAM_MB}MB"

if [ "${QDRANT_RAM_MB:-0}" -gt "$QDRANT_RAM_WARN_MB" ]; then
  warn "Qdrant using ${QDRANT_RAM_MB}MB — close to 400MB limit"
  ISSUES=$((ISSUES + 1))
else
  ok "Qdrant RAM normal (${QDRANT_RAM_MB}MB)"
fi

# ── Swap usage ────────────────────────────────────────────────────────────────
SWAP_USED=$(free -m | awk 'NR==3{print $3}')
if [ "${SWAP_USED:-0}" -gt 50 ]; then
  warn "Swap in use: ${SWAP_USED}MB — memory pressure detected"
  ISSUES=$((ISSUES + 1))
else
  ok "Swap normal (${SWAP_USED}MB used)"
fi

# ── Disk check ────────────────────────────────────────────────────────────────
DISK_PCT=$(df / | awk 'NR==2{gsub(/%/,""); print $5}')
DISK_AVAIL=$(df -h / | awk 'NR==2{print $4}')
STORAGE_SIZE=$(du -sh /var/lib/qdrant/storage 2>/dev/null | cut -f1 || echo "unknown")
BACKUP_SIZE=$(du -sh /var/lib/qdrant/backups 2>/dev/null | cut -f1 || echo "0")
LOG_SIZE=$(du -sh /var/log/qdrant 2>/dev/null | cut -f1 || echo "0")

log "Disk: ${DISK_PCT}% used, ${DISK_AVAIL} available"
log "  Qdrant storage: $STORAGE_SIZE | Backups: $BACKUP_SIZE | Logs: $LOG_SIZE"

if [ "$DISK_PCT" -gt "$DISK_WARN_PCT" ]; then
  warn "Disk at ${DISK_PCT}% — ${DISK_AVAIL} remaining"
  ISSUES=$((ISSUES + 1))
else
  ok "Disk normal (${DISK_PCT}%)"
fi

# ── Qdrant health check ────────────────────────────────────────────────────────
if curl -sf "${QDRANT}/healthz" > /dev/null 2>&1; then
  ok "Qdrant HTTP responding on :6333"
else
  crit "Qdrant NOT responding on :6333 — attempting restart"
  systemctl restart qdrant && log "Restart triggered" || crit "Restart failed"
  ISSUES=$((ISSUES + 1))
fi

# ── Qdrant service status ──────────────────────────────────────────────────────
if systemctl is-active --quiet qdrant; then
  UPTIME=$(systemctl show qdrant --property=ActiveEnterTimestamp | cut -d= -f2)
  ok "Qdrant service active (since: $UPTIME)"
else
  crit "Qdrant service NOT active"
  ISSUES=$((ISSUES + 1))
fi

# ── Collection check ──────────────────────────────────────────────────────────
log "Collections:"
COLLECTIONS=$(curl -sf "${QDRANT}/collections" \
  | grep -o '"name":"[^"]*"' \
  | grep -o ':"[^"]*"' \
  | tr -d ':"' 2>/dev/null || echo "")

if [ -z "$COLLECTIONS" ]; then
  warn "No collections found or Qdrant not responding"
  ISSUES=$((ISSUES + 1))
else
  for COLL in $COLLECTIONS; do
    COLL_DATA=$(curl -sf "${QDRANT}/collections/${COLL}" || echo "{}")
    VEC_COUNT=$(echo "$COLL_DATA" | grep -o '"vectors_count":[0-9]*' | grep -o '[0-9]*' || echo "0")
    IDX_COUNT=$(echo "$COLL_DATA" | grep -o '"indexed_vectors_count":[0-9]*' | grep -o '[0-9]*' || echo "0")
    STATUS=$(echo "$COLL_DATA" | grep -o '"status":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"' || echo "unknown")

    log "  $COLL: $VEC_COUNT vectors ($IDX_COUNT indexed) — status: $STATUS"

    if [ "${VEC_COUNT:-0}" -gt "$VECTOR_WARN" ]; then
      warn "  $COLL has ${VEC_COUNT} vectors — consider running axon-trim"
      ISSUES=$((ISSUES + 1))
    fi

    if [ "$STATUS" != "green" ] && [ "$STATUS" != "ok" ] && [ -n "$STATUS" ]; then
      warn "  $COLL status is '$STATUS' (expected green)"
      ISSUES=$((ISSUES + 1))
    fi
  done
fi

# ── Timers check ──────────────────────────────────────────────────────────────
log "Systemd timers:"
for TIMER in axon-trim.timer axon-backup.timer axon-health.timer; do
  if systemctl is-active --quiet "$TIMER"; then
    NEXT=$(systemctl show "$TIMER" --property=NextElapseUSecRealtime 2>/dev/null \
      | cut -d= -f2 | xargs -I{} date -d @$(echo "scale=0; {}/1000000" | bc) '+%Y-%m-%d %H:%M' 2>/dev/null \
      || echo "unknown")
    ok "$TIMER active (next: $NEXT)"
  else
    warn "$TIMER NOT active"
    ISSUES=$((ISSUES + 1))
  fi
done

# ── Last backup check ─────────────────────────────────────────────────────────
LAST_BACKUP=$(ls /var/lib/qdrant/backups/qdrant-backup-*.tar.gz 2>/dev/null \
  | sort | tail -1 | xargs -I{} basename {} .tar.gz | sed 's/qdrant-backup-//' || echo "none")

if [ "$LAST_BACKUP" = "none" ]; then
  warn "No backups found in /var/lib/qdrant/backups/ — run axon-backup"
  ISSUES=$((ISSUES + 1))
else
  DAYS_SINCE=$(( ($(date +%s) - $(date -d "$LAST_BACKUP" +%s 2>/dev/null || echo 0)) / 86400 ))
  if [ "${DAYS_SINCE:-999}" -gt 8 ]; then
    warn "Last backup was ${DAYS_SINCE} days ago ($LAST_BACKUP) — expected weekly"
    ISSUES=$((ISSUES + 1))
  else
    ok "Last backup: $LAST_BACKUP (${DAYS_SINCE} days ago)"
  fi
fi

# ── Final result ───────────────────────────────────────────────────────────────
log "═══════════════════════════════════════"
if [ "$ISSUES" -eq 0 ]; then
  log "✅ All checks passed — system healthy"
else
  log "⚠  Health check complete — $ISSUES issue(s) found (see WARN/CRIT above)"
fi
log "═══════════════════════════════════════"

exit 0
