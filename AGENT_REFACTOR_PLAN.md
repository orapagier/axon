# Agent-Loop Refactor Plan — Claude Code Harness Principles

> **Purpose of this file:** durable, self-contained execution plan. If a session is interrupted
> (rate limit, crash), the next session reads this file, finds the first unchecked box, and
> continues. Update checkboxes + commit + push after every phase.
>
> Approved 2026-07-03. References: Anthropic's *Building Effective Agents*,
> *Writing Effective Tools for Agents*, *Claude Code best practices*.

## Why

Axon's loop was built defensively around weak free-tier models: the tool router hides most
tools from the model, dashboard chats feed only the last 5 messages, reasoning is off by
default, and a forest of guards regex-patches the failures this causes (TOOL_NAME_REMAPS,
service-mismatch remaps, repair module, LLM quality judge). Claude Code's harness does the
opposite — give the model maximum context and agency, verify with ground truth — and that is
where most of its agentic ability comes from. This refactor moves Axon to that philosophy so
any reasonably strong model (provider-agnostic) performs close to frontier-level agentic
ability inside Axon.

User decisions locked in:
1. One strong model in the capable role — **provider-agnostic**, works with whatever model/provider is configured.
2. Conversation window: 20 messages (10 exchanges) — decided by Claude.
3. Reasoning ON.
4. Tools that teach (schema-bearing errors, when-to-use descriptions).
5. Plan-then-execute for multi-step tasks.
6. Guard/QC consolidation at Claude's judgment (keep deterministic ground-truth guards, narrow the LLM judge, metric-gate deletions).

## Architecture anchors (verified 2026-07-03)

| Fact | Where |
|---|---|
| Phase-aware role selection `simple_tasks` / `complex_tasks` already exists | `crates/axon-agent/src/agent/loop.rs:922` |
| No model is assigned to either role → silent fallback to general pool | `crates/axon-agent/config/models.toml`, `router/model_router.rs:148-173` |
| Provider dispatch: `anthropic`→anthropic.rs, `ollama`→ollama.rs, everything else→openai_compat.rs | `providers/mod.rs:47-51` |
| `reasoning_effort` only consumed by openai_compat | `providers/openai_compat.rs:599`; gate at `loop.rs:965` (default `""` = off) |
| Tool routing hides tools; tier2 costs an extra LLM call per iteration; full list only on total failure | `loop.rs:815-903`, `router/tool_router.rs:571-679` |
| In-loop tool results are already verbatim; compressor only writes cross-run observations | `loop.rs:1633-1858`, `memory/compressor.rs` |
| In-loop context limiter: `trim_tool_results_by_budget(&mut messages, 50_000)` chars | `loop.rs:905`, impl at `loop.rs:1936` |
| Dashboard history window default 5 messages | `agent/system_context.rs:53` (`memory.dashboard_context_window`) |
| Guard pipeline (claim guard/receipts, refusal nudge, promise-only, blank, raw-syntax, LLM QC) | `loop.rs:371-536` (`validate_response`), `agent/quality.rs` |
| Guard fire counts persisted per run for before/after measurement | `runs` table: `nudge_count`, `claim_guard_count`, `qc_correction_count` |
| ToolDefinition fields: name, description, parameters, required, source, enabled, is_mutating | `tools/schema.rs:20` |

## Phases

### Phase 0 — This file
- [x] Write `AGENT_REFACTOR_PLAN.md` to repo root, commit + push.

### Phase 1 — Provider-agnostic strong-model slot (config only)
- [x] `config/models.toml`: add commented template `[[models]]` entries for `role = "complex_tasks"` and `role = "simple_tasks"` showing any-provider usage (`anthropic` / `google` / `openrouter` / `nvidia` / `groq` / custom `base_url`), with notes: `${VAR}` api_key resolves DB-then-env and the var name must match exactly (mismatch ⇒ "All models exhausted"); tool-use/correction/multi-iteration turns route to `complex_tasks`, chit-chat to `simple_tasks`, both fall back to the general pool when empty.
- [x] `router/model_router.rs`: role fidelity patch — the sticky pass (Pass 0) ran BEFORE the role pool pass, so a general-pool model could shadow a configured `complex_tasks` model for the whole run. Sticky is now skipped when the requested role has available models and the sticky model isn't in that role (same-role stickiness preserved).

### Phase 2 — Show the model everything
- [x] New setting `agent.tool_scope` = `"all"` (new default) | `"routed"` (rollback path). Seeded in `db/seed.sql`.
- [x] `loop.rs` + `agent/system_context.rs`: when `"all"` and task not CONVERSATIONAL, the model gets the full enabled tool list every iteration (sorted by name for provider-side prompt-cache stability). `ctx.allowed_tools` manual filtering (workflow nodes) and the conversational zero-tool short-circuit preserved. Router (incl. tier2 LLM call) fully bypassed in `"all"` mode, including the initial routing in `build_run_context`.
- [x] Tools UI event skipped in `"all"` mode (ToolStart events show live usage).
- [x] `memory.dashboard_context_window` 5 → 20: seed.sql (fresh installs), normalize.sql WHERE-guarded update (existing installs still on the old default), inline code default.
- [x] New setting `agent.tool_result_budget_chars` default 100_000 (was hardcoded 50_000). Seeded.
- [x] Remap-hit evidence for Phase 6: existing `tracing::info!("Auto-remapped hallucinated tool ...")` (loop.rs) and `tracing::warn!("Service mismatch fix ...")` lines serve as the counters — grep logs for these after 1-2 weeks.

### Phase 3 — Reasoning ON, provider-agnostic
- [x] Default `agent.reasoning_effort` `""` → `"medium"` (seeded; "off" disables; applied on `complex_tasks` turns per existing gate).
- [x] `providers/openai_compat.rs`: on HTTP 400 naming `reasoning`, retry once without the field and flag the model `no_reasoning` (process lifetime) so it's omitted subsequently. Reasoning content in unknown response fields (`reasoning_content`) is ignored by serde — never reaches users.
- [x] `providers/anthropic.rs`: `reasoning_effort` → `thinking` param via per-model `thinking_mode` (models.toml field + `models` DB column via migration 0015): `"adaptive"` (Claude 4.6+), `"budget"` (low=2048/med=8192/high=16384, ≥1024 answer headroom; e.g. Haiku 4.5), unset = off. Temperature dropped when thinking active (API requirement). New `ContentBlock::Thinking` variant round-trips signed thinking blocks on multi-turn tool use; other providers skip it.
- [x] `providers/ollama.rs`: `think: true` when reasoning_effort set; response `message.thinking` ignored.
- [x] Leak guard: `strip_reasoning` now also removes `<think>...</think>` blocks (DeepSeek/Qwen-style inline reasoning); `as_text()`/`text_content()` never include Thinking blocks. Unit test added.

### Phase 4 — Tools that teach
- [x] `ToolDefinition::teaching_block()` (tools/schema.rs): description + parameter schema (capped 1500 chars) + required list + skeleton example call.
- [x] Missing-args diversion, internal tool errors, and external tool errors all include the teaching block so the model self-corrects in one step.
- [x] Unknown tool name diverted pre-execution with "No tool named X. Closest available tools: [top-3]" via `closest_tool_names` (levenshtein + service-prefix + containment ranking) — dynamic replacement groundwork for the static remap table.
- [x] Description audit: "Use this when …" trigger sentences appended to the tools the remap table proves get confused — gmail_list, gcal_list_events, gdrive_list (axon-google), outlook_list_emails, mscal_list_events, onedrive_list (axon-microsoft), fb_list_messenger_chats, fb_list_posts (axon-facebook). Note: there are no repo-tracked Python tools; agent-written temp tools live in `tools_temp/` on the deployment only.
- [x] Unit tests: teaching_block rendering + closest_tool_names ranking.

### Phase 5 — Plan-then-execute
- [x] `agent/plan.rs` (run-id-keyed state, cleared in `finalize` on every exit) + internal tool `update_plan` (registered in tools/registry.rs with `is_mutating=false` so plan bookkeeping can never vouch for a fabricated write in the claim guard; dispatched in agent/internal_tools.rs).
- [x] Plan visibility: every `update_plan` tool result returns the full rendered `[PLAN]` checklist, so current state rides in conversation history without mutating the system prompt per iteration (provider prompt caches stay valid).
- [x] PLANNING system-prompt instruction gated on MULTISTEP / `is_bulk_task` + `agent.planning` (seeded, default true); skipped for conversational, tool-free, and workflow-node (allow-listed) runs.
- [x] `validate_response`: one-shot PLAN CHECK retry (new `RetryReason::PlanIncomplete`) listing open steps when a final answer arrives early; shares the global correction budget.
- [x] Unit test: plan lifecycle (render/open/remind-once/clear).

### Phase 6 — Guard consolidation (data-driven)
- [x] Kept unchanged: execution receipts + claim guard, blank check, promise-only guard, raw-tool-syntax check, stall detection, correction/token budgets, refusal nudge.
- [x] QC correction cap 3 → 1 (`should_qc` gate); `quality_check_mode = "mutating"` unchanged.
- [ ] DEFERRED (needs ~1-2 weeks of production metrics after deploying Phases 1-5): if `runs` guard counters ≈ 0 and the "Auto-remapped hallucinated tool" / "Service mismatch fix" log lines stop appearing, then (a) flip `agent.quality_check` default to false, (b) delete TOOL_NAME_REMAPS and the pre-execution service remap in loop.rs, (c) trim the hardest `agent/repair.rs` paths (keep the blocked-hallucination path). Evidence: `SELECT SUM(nudge_count), SUM(claim_guard_count), SUM(qc_correction_count) FROM runs WHERE created_at > <deploy date>` + grep the logs.

## Verification (each phase)

1. `cargo fmt --check` · `cargo test -p axon` · `cargo build`.
2. New unit tests: schema-bearing error messages, plan-block rendering, unknown-tool suggestions, reasoning-field strip-retry.
3. Live on LOCAL dev instance only (the live Telegram bot is the SERVER instance — deploy there only after local validation): (a) multi-step task → expect plan creation, full tool list, reasoning events, receipts, zero remap log lines; (b) chit-chat → simple_tasks path, zero tools; (c) deliberately wrong tool call → schema-bearing teaching error, one-step self-correction.
4. Compare `runs` / `run_iterations`: total_tokens per run, guard counters, iterations-to-completion.
5. `graphify update .` after each phase.
6. Commit AND push each phase to main; check `git diff origin/main` before pushing (global auto-backup hook may hold other sessions' work).
