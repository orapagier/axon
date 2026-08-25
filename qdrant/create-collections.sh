#!/usr/bin/env bash
# =============================================================================
# Create Qdrant collections with RAM-optimal settings
# Called by install.sh — can also be re-run safely (idempotent)
# =============================================================================

set -euo pipefail

QDRANT="http://localhost:6333"

log()  { echo -e "\033[0;32m[✓]\033[0m $*"; }
info() { echo -e "\033[0;36m[→]\033[0m $*"; }
warn() { echo -e "\033[1;33m[!]\033[0m $*"; }
err()  { echo -e "\033[0;31m[✗]\033[0m $*" >&2; exit 1; }

# Wait for Qdrant to be ready
for i in $(seq 1 10); do
  if curl -sf "${QDRANT}/healthz" > /dev/null; then break; fi
  info "Waiting for Qdrant... ($i/10)"
  sleep 2
done
curl -sf "${QDRANT}/healthz" > /dev/null || { echo "Qdrant not ready"; exit 1; }

# ── Helper: create collection if it doesn't exist ─────────────────────────────
create_collection() {
  local NAME="$1"
  local BODY="$2"

  # Check if collection already exists
  HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${QDRANT}/collections/${NAME}")
  if [ "$HTTP_CODE" = "200" ]; then
    warn "Collection '${NAME}' already exists — skipping"
    return
  fi

  info "Creating collection: ${NAME}"
  local RESPONSE
  RESPONSE=$(qdrant_put "${QDRANT}/collections/${NAME}" "$BODY" "creating collection '${NAME}'")

  if echo "$RESPONSE" | grep -q '"result":true'; then
    log "Collection '${NAME}' created"
  else
    echo "Failed to create '${NAME}': $RESPONSE"
    exit 1
  fi
}

# ── Helper: PUT to Qdrant, and say what went wrong when it fails ──────────────
#
# `curl -sf … >/dev/null` used to stand in for this. Under `set -e` that is a
# silent abort: curl exits 22 on any 4xx/5xx having thrown the body away, so the
# whole deploy stopped mid-phase with the last thing on screen being the step it
# was about to do. Every Qdrant failure worth reading is in that discarded body.
qdrant_put() {
  local URL="$1"
  local DATA="$2"
  local WHAT="$3"

  local RESPONSE RC CODE PAYLOAD
  RESPONSE=$(curl -s -m 30 -w $'\n%{http_code}' -X PUT "$URL" \
    -H 'Content-Type: application/json' \
    -d "$DATA") && RC=0 || RC=$?

  if [ "$RC" -ne 0 ]; then
    echo "" >&2
    err "Qdrant unreachable while ${WHAT} (curl exit ${RC}, URL ${URL})."
  fi

  CODE="${RESPONSE##*$'\n'}"
  PAYLOAD="${RESPONSE%$'\n'*}"

  if [ "${CODE#2}" != "$CODE" ]; then   # 2xx
    printf '%s' "$PAYLOAD"
    return 0
  fi

  # Everything below goes to stderr: this helper's stdout is the response body,
  # captured by the caller (or sent to /dev/null), so a diagnostic printed there
  # would be swallowed exactly like the one this replaced.
  echo "" >&2
  warn "Qdrant returned HTTP ${CODE} while ${WHAT}:" >&2
  echo "    ${PAYLOAD}" >&2

  # The one failure that looks like a Qdrant bug but never is: the service is
  # running as the user who installed it *last time*, while this install has
  # just chowned its storage to the user running it *now*. Reads keep working
  # (0755), so the collection check above says "already exists" and only the
  # first write fails.
  if echo "$PAYLOAD" | grep -qi 'permission denied'; then
    local RUNNING_PID RUNNING_USER
    RUNNING_PID=$(systemctl show -p MainPID --value qdrant 2>/dev/null || echo 0)
    RUNNING_USER=$(stat -c %U "/proc/${RUNNING_PID:-0}" 2>/dev/null || echo "unknown")
    echo "" >&2
    warn "Qdrant cannot write its own storage." >&2
    echo "    The service is running as '${RUNNING_USER}'; this install owns" >&2
    echo "    /var/lib/qdrant as '$(whoami)'. Restart it so the two agree:" >&2
    echo "      sudo systemctl daemon-reload && sudo systemctl restart qdrant" >&2
  fi

  err "Aborting: ${WHAT} failed."
}

# ── Create indexes helper ──────────────────────────────────────────────────────
create_payload_index() {
  local COLLECTION="$1"
  local FIELD="$2"
  local TYPE="$3"   # keyword | integer | float | bool | datetime

  info "  Indexing payload field: ${FIELD} (${TYPE})"
  qdrant_put "${QDRANT}/collections/${COLLECTION}/index" \
    "{\"field_name\": \"${FIELD}\", \"field_schema\": \"${TYPE}\"}" \
    "indexing ${COLLECTION}.${FIELD}" > /dev/null
}

# =============================================================================
# Collection 1: axon_memory
# Main episodic memory — conversation context, facts, summaries
# Embedding: 1024 dims (Voyage-4)
# =============================================================================
create_collection "axon_memory" '{
  "vectors": {
    "size": 1024,
    "distance": "Cosine",
    "on_disk": true
  },
  "hnsw_config": {
    "m": 8,
    "ef_construct": 64,
    "on_disk": true,
    "full_scan_threshold": 10000
  },
  "quantization_config": {
    "scalar": {
      "type": "int8",
      "quantile": 0.99,
      "always_ram": true
    }
  },
  "optimizers_config": {
    "memmap_threshold": 5000,
    "indexing_threshold": 5000,
    "max_segment_number": 4,
    "flush_interval_sec": 30
  },
  "wal_config": {
    "wal_capacity_mb": 16,
    "wal_segments_ahead": 0
  }
}'

# Payload indexes for fast filtered search
create_payload_index "axon_memory" "tier"         "keyword"
create_payload_index "axon_memory" "source"       "keyword"
create_payload_index "axon_memory" "created_at"   "integer"
create_payload_index "axon_memory" "expires_at"   "integer"
create_payload_index "axon_memory" "importance"   "float"
create_payload_index "axon_memory" "access_count" "integer"
create_payload_index "axon_memory" "last_accessed" "integer"
create_payload_index "axon_memory" "agent_id"     "keyword"
log "axon_memory indexes created"

# =============================================================================
# Collection 2: documents
# Document chunks — emails, files, notes indexed for retrieval
# =============================================================================
create_collection "documents" '{
  "vectors": {
    "size": 1024,
    "distance": "Cosine",
    "on_disk": true
  },
  "hnsw_config": {
    "m": 8,
    "ef_construct": 64,
    "on_disk": true,
    "full_scan_threshold": 10000
  },
  "quantization_config": {
    "scalar": {
      "type": "int8",
      "quantile": 0.99,
      "always_ram": true
    }
  },
  "optimizers_config": {
    "memmap_threshold": 5000,
    "indexing_threshold": 5000,
    "max_segment_number": 4,
    "flush_interval_sec": 60
  },
  "wal_config": {
    "wal_capacity_mb": 16,
    "wal_segments_ahead": 0
  }
}'

create_payload_index "documents" "source_type"  "keyword"
create_payload_index "documents" "source_id"    "keyword"
create_payload_index "documents" "created_at"   "integer"
create_payload_index "documents" "expires_at"   "integer"
create_payload_index "documents" "chunk_index"  "integer"
create_payload_index "documents" "importance"   "float"
create_payload_index "documents" "access_count" "integer"
log "documents indexes created"

# =============================================================================
# Collection 3: entities
# Long-term entity memory — people, companies, projects, concepts
# Small collection, never trimmed (permanent)
# =============================================================================
create_collection "entities" '{
  "vectors": {
    "size": 1024,
    "distance": "Cosine",
    "on_disk": true
  },
  "hnsw_config": {
    "m": 16,
    "ef_construct": 100,
    "on_disk": true,
    "full_scan_threshold": 5000
  },
  "quantization_config": {
    "scalar": {
      "type": "int8",
      "quantile": 0.99,
      "always_ram": true
    }
  },
  "optimizers_config": {
    "memmap_threshold": 1000,
    "indexing_threshold": 1000,
    "max_segment_number": 2
  },
  "wal_config": {
    "wal_capacity_mb": 8,
    "wal_segments_ahead": 0
  }
}'

create_payload_index "entities" "entity_type" "keyword"
create_payload_index "entities" "name"        "keyword"
create_payload_index "entities" "updated_at"  "integer"
create_payload_index "entities" "importance"  "float"
log "entities indexes created"

# =============================================================================
# Verify all collections
# =============================================================================
echo ""
info "Verifying collections..."
COLLECTIONS=$(curl -sf "${QDRANT}/collections" | jq -r '.result.collections[].name' 2>/dev/null || echo "jq not available")
echo "$COLLECTIONS" | while read -r c; do
  COUNT=$(curl -sf "${QDRANT}/collections/${c}" | jq -r '.result.vectors_count // 0' 2>/dev/null || echo "0")
  log "  ${c}: ${COUNT} vectors"
done

echo ""
log "All collections ready."
echo ""
echo "Collections created:"
echo "  axon_memory   — episodic memory (conversations, facts, summaries)"
echo "  documents     — document chunks (emails, files, notes)"
echo "  entities      — long-term entity knowledge (people, companies)"
echo ""
echo "Embedding dimension: 1024 (Voyage-4)"
echo "Change vector.size if using OpenAI (1536) or larger models."
