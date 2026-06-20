#!/usr/bin/env bash
# =============================================================================
# axon-status — quick live dashboard
# Shows RAM, disk, Qdrant stats, collection sizes at a glance
# =============================================================================

QDRANT="http://localhost:6333"
B='\033[1m'; G='\033[0;32m'; Y='\033[1;33m'; R='\033[0;31m'; C='\033[0;36m'; N='\033[0m'

clear
echo ""
echo -e "${B}╔═══════════════════════════════════════════════╗${N}"
echo -e "${B}║          Axon system status                   ║${N}"
echo -e "${B}║          $(date '+%Y-%m-%d %H:%M:%S')                  ║${N}"
echo -e "${B}╚═══════════════════════════════════════════════╝${N}"
echo ""

# ── RAM ───────────────────────────────────────────────────────────────────────
TOTAL_RAM=$(free -m | awk 'NR==2{print $2}')
USED_RAM=$(free -m  | awk 'NR==2{print $3}')
FREE_RAM=$(free -m  | awk 'NR==2{print $4}')
SWAP_USED=$(free -m | awk 'NR==3{print $3}')
RAM_PCT=$(( USED_RAM * 100 / TOTAL_RAM ))

# Progress bar
BAR_FULL=30
BAR_USED=$(( RAM_PCT * BAR_FULL / 100 ))
BAR_FREE=$(( BAR_FULL - BAR_USED ))
RAM_BAR="$(printf '#%.0s' $(seq 1 $BAR_USED))$(printf '.%.0s' $(seq 1 $BAR_FREE))"

if [ "$RAM_PCT" -gt 80 ]; then
  RAM_COLOR="$R"
elif [ "$RAM_PCT" -gt 60 ]; then
  RAM_COLOR="$Y"
else
  RAM_COLOR="$G"
fi

echo -e "${B}  MEMORY${N}"
echo -e "  RAM    [${RAM_COLOR}${RAM_BAR}${N}] ${RAM_PCT}% — ${USED_RAM}MB / ${TOTAL_RAM}MB (${FREE_RAM}MB free)"
[ "${SWAP_USED:-0}" -gt 0 ] && \
  echo -e "  Swap   ${Y}${SWAP_USED}MB in use${N} (should be near 0)" || \
  echo -e "  Swap   ${G}0MB${N}"

# Per-process RAM
QDRANT_RAM=$(ps aux | grep '[q]drant' | awk '{sum += $6} END {printf "%.0f", sum/1024}')
AGENT_RAM=$(ps aux | grep '[a]xon-mcp' | awk '{sum += $6} END {printf "%.0f", sum/1024}')
echo -e "  Qdrant ${QDRANT_RAM}MB | agent ${AGENT_RAM}MB"

echo ""

# ── Disk ──────────────────────────────────────────────────────────────────────
DISK_PCT=$(df / | awk 'NR==2{gsub(/%/,""); print $5}')
DISK_AVAIL=$(df -h / | awk 'NR==2{print $4}')
STORAGE_SIZE=$(du -sh /var/lib/qdrant/storage 2>/dev/null | cut -f1 || echo "?")
BACKUP_SIZE=$(du -sh /var/lib/qdrant/backups 2>/dev/null | cut -f1 || echo "0")

DISK_BAR_USED=$(( DISK_PCT * BAR_FULL / 100 ))
DISK_BAR_FREE=$(( BAR_FULL - DISK_BAR_USED ))
DISK_BAR="$(printf '#%.0s' $(seq 1 $DISK_BAR_USED))$(printf '.%.0s' $(seq 1 $DISK_BAR_FREE))"

[ "$DISK_PCT" -gt 80 ] && DISK_COLOR="$R" || DISK_COLOR="$G"

echo -e "${B}  DISK${N}"
echo -e "  Root   [${DISK_COLOR}${DISK_BAR}${N}] ${DISK_PCT}% used (${DISK_AVAIL} free)"
echo -e "  Qdrant storage: ${STORAGE_SIZE} | Backups: ${BACKUP_SIZE}"
echo ""

# ── Services ──────────────────────────────────────────────────────────────────
echo -e "${B}  SERVICES${N}"
for SVC in qdrant axon-mcp; do
  if systemctl is-active --quiet "$SVC" 2>/dev/null; then
    echo -e "  ${G}●${N} $SVC  running"
  else
    echo -e "  ${R}●${N} $SVC  ${R}stopped${N}"
  fi
done
echo ""

# ── Timers ────────────────────────────────────────────────────────────────────
echo -e "${B}  TIMERS${N}"
for TIMER in axon-trim axon-backup axon-health; do
  if systemctl is-active --quiet "${TIMER}.timer" 2>/dev/null; then
    LAST=$(systemctl show "${TIMER}.service" --property=LastTriggerUSec 2>/dev/null \
      | cut -d= -f2 | head -c 10 || echo "never")
    echo -e "  ${G}●${N} ${TIMER}  active (last: $LAST)"
  else
    echo -e "  ${Y}●${N} ${TIMER}.timer  ${Y}not active${N}"
  fi
done
echo ""

# ── Qdrant collections ────────────────────────────────────────────────────────
echo -e "${B}  QDRANT COLLECTIONS${N}"
if curl -sf "${QDRANT}/healthz" > /dev/null 2>&1; then
  COLLECTIONS=$(curl -sf "${QDRANT}/collections" \
    | grep -o '"name":"[^"]*"' \
    | grep -o ':"[^"]*"' \
    | tr -d ':"' 2>/dev/null || echo "")

  if [ -z "$COLLECTIONS" ]; then
    echo -e "  ${Y}No collections found${N}"
  else
    for COLL in $COLLECTIONS; do
      DATA=$(curl -sf "${QDRANT}/collections/${COLL}" 2>/dev/null || echo "{}")
      VECS=$(echo "$DATA" | grep -o '"vectors_count":[0-9]*' | grep -o '[0-9]*' || echo "?")
      IDXD=$(echo "$DATA" | grep -o '"indexed_vectors_count":[0-9]*' | grep -o '[0-9]*' || echo "?")
      STAT=$(echo "$DATA" | grep -o '"status":"[^"]*"' | head -1 | tr -d '"' | cut -d: -f2 || echo "?")
      [ "$STAT" = "green" ] && SC="$G" || SC="$Y"
      echo -e "  ${SC}●${N} ${COLL}  ${VECS} vectors (${IDXD} indexed) — ${SC}${STAT}${N}"
    done
  fi

  # Qdrant version
  VER=$(curl -sf "${QDRANT}/" 2>/dev/null | grep -o '"version":"[^"]*"' | grep -o '"[^"]*"$' | tr -d '"' || echo "unknown")
  echo -e "  ${C}Qdrant ${VER}${N} — ${G}API responding on :6333${N}"
else
  echo -e "  ${R}Qdrant not responding on :6333${N}"
fi

echo ""

# ── Last backup ───────────────────────────────────────────────────────────────
LAST_BACKUP=$(ls /var/lib/qdrant/backups/qdrant-backup-*.tar.gz 2>/dev/null \
  | sort | tail -1 | xargs -I{} basename {} .tar.gz 2>/dev/null \
  | sed 's/qdrant-backup-//' || echo "none")

echo -e "${B}  BACKUP${N}"
if [ "$LAST_BACKUP" = "none" ]; then
  echo -e "  ${R}No backup found${N} — run: axon-backup"
else
  DAYS=$(( ($(date +%s) - $(date -d "$LAST_BACKUP" +%s 2>/dev/null || echo 0)) / 86400 ))
  [ "$DAYS" -gt 8 ] && BC="$Y" || BC="$G"
  echo -e "  Last backup: ${BC}${LAST_BACKUP}${N} (${DAYS} days ago)"
fi

echo ""
echo -e "  ${C}axon-status${N}  ${C}axon-health${N}  ${C}axon-trim${N}  ${C}axon-backup${N}  ${C}axon-restore${N}"
echo ""
