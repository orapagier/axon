#!/usr/bin/env bash
# =============================================================================
#  oc-bridge.sh — opencode bridge bootstrap shared by scripts/dev.sh and
#  scripts/run.sh. Sourced, not executed.
#
#  Idempotent: reuses an already-running bridge (health check on
#  $OC_BRIDGE_BASE) and skips cleanly when opencode isn't installed or the
#  bridge can't be built/run, so the rest of the script (the agent) still runs.
#
#  The bridge is compiled Rust (crates/oc-bridge) — no node, no interpreter
#  on the runtime path.
#
#  Overrides
#    OC_BRIDGE_BASE    base URL of the bridge (default http://127.0.0.1:4010,
#                      must match the models.toml row's base_url)
#    OC_BRIDGE_BIN     path to a prebuilt oc-bridge binary (else picked from
#                      target/release or target/debug, else cargo-built)
#    OC_BRIDGE_LOG     bridge log file (default /tmp/opencode-bridge.log)
#
#  The bridge is deliberately left running after the script exits (next run
#  just reuses it); kill it with `pkill -f oc-bridge`.
# =============================================================================

OC_BRIDGE_BASE="${OC_BRIDGE_BASE:-http://127.0.0.1:4010}"
OC_BRIDGE_LOG="${OC_BRIDGE_LOG:-/tmp/opencode-bridge.log}"

# Standalone-safe: dev.sh/run.sh define fancier colored log/warn/info helpers;
# fall back to plain echo when sourced elsewhere.
for _oc_fn in log warn info; do
  type "$_oc_fn" >/dev/null 2>&1 || eval "$_oc_fn() { printf '%s\\n' \"\$*\"; }"
done
unset _oc_fn

oc_bridge_running() {
  command -v curl >/dev/null 2>&1 \
    && curl -fsS -m 2 "$OC_BRIDGE_BASE/health" >/dev/null 2>&1
}

oc_bridge_binary() {
  # Resolve the oc-bridge binary: explicit override, then release/debug build,
  # then build it on demand.
  local bin=""
  if [ -n "${OC_BRIDGE_BIN:-}" ]; then
    bin="$OC_BRIDGE_BIN"
  elif [ -x "$ROOT/target/release/oc-bridge" ]; then
    bin="$ROOT/target/release/oc-bridge"
  elif [ -x "$ROOT/target/debug/oc-bridge" ]; then
    bin="$ROOT/target/debug/oc-bridge"
  elif command -v cargo >/dev/null 2>&1; then
    info "building oc-bridge (crates/oc-bridge)…"
    ( cd "$ROOT" && cargo build -p oc-bridge ) >/dev/null 2>&1 || {
      warn "cargo build -p oc-bridge failed — skipping opencode bridge"
      return 1
    }
    bin="$ROOT/target/debug/oc-bridge"
  else
    warn "no oc-bridge binary and no cargo — skipping opencode bridge"
    return 1
  fi
  if [ ! -x "$bin" ]; then
    warn "oc-bridge binary '$bin' not executable — skipping opencode bridge"
    return 1
  fi
  printf '%s' "$bin"
}

# Start the opencode bridge if opencode is available; return 0 when up.
oc_ensure_bridge() {
  if oc_bridge_running; then
    log "opencode bridge already up ($OC_BRIDGE_BASE)"
    return 0
  fi
  local oc
  if command -v opencode >/dev/null 2>&1; then
    oc="$(command -v opencode)"
  elif [ -x "$HOME/.opencode/bin/opencode" ]; then
    oc="$HOME/.opencode/bin/opencode"
  else
    warn "opencode CLI not found — skipping opencode bridge (opencode models unavailable)"
    return 1
  fi
  local bin
  bin="$(oc_bridge_binary)" || return 1
  info "starting opencode bridge on $OC_BRIDGE_BASE"
  ( cd "$ROOT" && OPENCODE_BIN="$oc" setsid "$bin" >"$OC_BRIDGE_LOG" 2>&1 & ) || true
  for _ in $(seq 1 30); do
    oc_bridge_running && { log "opencode bridge up ($OC_BRIDGE_BASE)"; return 0; }
    sleep 1
  done
  warn "opencode bridge failed to start — see $OC_BRIDGE_LOG"
  return 1
}