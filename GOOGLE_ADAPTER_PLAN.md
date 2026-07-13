# Native Google/Gemini provider adapter

## Context

Google/Gemini traffic currently flows through `providers/openai_compat.rs` — the generic
catch-all adapter shared by Groq, Cerebras, NVIDIA, OpenRouter, and plain OpenAI — because
Google publishes an OpenAI-compatibility shim (`.../v1beta/openai/`). Three Gemini/Gemma bugs
were fixed in that shared file this session (leaked `<thought>` tags, a `reasoning`/`thinking`
wording gap in a 400-retry, and a `thought_signature` requirement bolted on via an
`extra_content` envelope). Since the user wants to try many Gemini models and the compat shim
keeps leaking Gemini-native concepts through a thin translation layer, we're building a
dedicated `providers/google.rs` adapter that speaks Gemini's native `generateContent` /
`streamGenerateContent` REST API directly, and stripping the now-dead Google-specific
machinery back out of `openai_compat.rs` so it goes back to being generic/shared-only — per
the user's explicit direction: "take what works from openaicompat and remove it from there
then use what works in the new google adapter, so openai compat is solely for other
providers."

This is app-wide production model routing for every currently-configured `provider = "google"`
row, so correctness matters — get the wire format wrong and every Gemini call breaks, not just
a new feature.

## Key design facts (researched this session, not re-derived)

- **Auth**: header `x-goog-api-key: <api_key>` (not `Authorization: Bearer`).
- **Endpoints**: `POST {base}/models/{model_id}:generateContent` (non-streaming);
  `POST {base}/models/{model_id}:streamGenerateContent?alt=sse` (streaming — real SSE,
  `?alt=sse` required). Default base becomes `https://generativelanguage.googleapis.com/v1beta`
  (native — replaces the `/openai/`-suffixed compat URL currently in `provider_base_url`).
- **Roles**: Axon's internal `Message.role` is exactly `"user"` / `"assistant"` (confirmed in
  `types.rs::Message`). Map `"assistant"` → `"model"`, else → `"user"`.
- **Part is flat/untagged**: one object with mutually-exclusive optional fields — `text`,
  `inlineData{mimeType,data}`, `functionCall{id?,name,args}`, `functionResponse{id?,name,response}`,
  `thought:bool` (only appears if we request `includeThoughts` — we never do, but skip
  defensively anyway), `thoughtSignature:string` (**sibling field directly on the same Part as
  the functionCall** — no envelope needed, this natively fixes the thought-signature bug).
- **Tool-result shape mismatch vs OpenAI**: `openai_compat.rs` splits multiple tool results into
  separate messages (`role:"tool"`, one per call). Gemini wants the opposite: all tool results
  become `functionResponse` parts batched into a *single* `Content{role:"user"}`. Verified via
  `agent/loop.rs:2036-2043`: parallel tool results are built as **N separate `Message`s** (one
  `Message::tool_result()` call per result, `messages.extend(result_msgs)`), not one message with
  N blocks. Since `contents_from_messages` builds one `GenContent` per `Message` (all blocks in
  that message → parts of that Content) and then merges *adjacent same-role* Contents, the
  merge pass is what actually produces Gemini's desired shape for the common case today — it is
  **not just defensive padding**, don't let a future cleanup remove it as redundant.
- **`ContentBlock::ToolResult{tool_use_id, content}` has no tool name**, but Gemini's
  `functionResponse.name` is required. Must pre-scan `messages` for every
  `ContentBlock::ToolUse{id, name, ..}` to build an `id → name` lookup before converting results.
- **No distinct "tool_calls" finish reason** — Gemini always returns `finishReason:"STOP"`
  whether the turn produced text or a function call. `StopReason::ToolUse` must be **inferred**
  from whether any `functionCall` parts were parsed, never from `finishReason` directly. This is
  exactly the kind of subtle bug already hit twice this session — give it its own pure, tested
  function.
- **Streaming is structurally simpler than OpenAI's**: each SSE frame is a *complete*
  `GenerateContentResponse`, and `functionCall.args` always arrives whole in one frame (Gemini
  doesn't token-stream call arguments). No `BTreeMap<usize, PartialToolCall>` fragment-merge
  needed — only `text` parts need concatenating across frames.
- **Thinking control**: reuse the existing `ModelRecord.thinking_mode` column (already used by
  `anthropic.rs` with `"adaptive"`/`"budget"`) with two new opt-in values for Google rows:
  `"level"` (Gemini 3.x `thinkingLevel`: low/medium/high) and `"budget"` (Gemini 2.5-era
  `thinkingBudget`, same 2048/8192/16384 effort-scaling and `max_tokens.saturating_sub(1024)`
  floor-drop Anthropic already uses). Unlike Anthropic, do **not** force-drop `temperature` when
  thinking is active (no evidence Gemini requires it). Since this is opt-in per model (no
  `thinking_mode` set = never send `thinkingConfig`), the risk of a Groq-Gemma-style "thinking
  not supported" 400 is structurally much lower than the old blind-send approach — but add the
  same one-shot retry safety net anyway, reusing `ModelRecord.no_reasoning` (already exists,
  currently only flipped by `openai_compat.rs`) as a generic "stop sending the thinking control
  param to this model" flag.
- **Vision parity**: `openai_compat.rs` already handles `ContentBlock::Image` for Google traffic
  today (`image_url` data-URI). Map it to `inlineData{mimeType,data}` in the new adapter so this
  isn't a silent regression.
- **Dispatch has a latent alias bug**: `providers/mod.rs::call_provider_with_options` matches on
  raw `model.provider.as_str()`, never calling `normalize_provider_name`. Harmless today since
  `"google"`/`"gemini"` both fall into the same catch-all, but once a `"google"` arm exists, a
  row configured with `provider = "gemini"` would silently keep falling through to
  `openai_compat::call`, which (after the `provider_base_url` change below) would build a broken
  URL for it. Fix by normalizing once at the top of the match.

## Implementation

### 1. New file `crates/axon-agent/src/providers/google.rs`

Layout mirrors `openai_compat.rs`: wire types → `HTTP_CLIENT` static → request-building →
response-parsing → `call_streaming` → `call` → `#[cfg(test)] mod tests`.

**Wire types** (all `#[serde(rename_all = "camelCase")]` at struct level, not manual renames):
`GenReq{contents, system_instruction?, tools?, tool_config?, generation_config?}` (derives
`Clone` — reused for streaming-then-fallback like `OaiReq` today), `GenContent{role?, parts}`,
`GenPart{text?, inline_data?, function_call?, function_response?, thought?, thought_signature?}`
(flat/all-optional, matches the untagged wire shape), `GenInlineData{mime_type, data}`,
`GenFnCall{id?, name, args:Value}`, `GenFnResponse{id?, name, response:Value}`,
`GenTool{function_declarations: Vec<GenFnDecl>}`, `GenFnDecl{name, description, parameters}`,
`GenToolConfig{function_calling_config: GenFnCallingConfig{mode}}`,
`GenGenerationConfig{max_output_tokens?, temperature?, thinking_config?}`,
`GenThinkingConfig{thinking_budget?: i32, thinking_level?: String}`,
`GenResp{candidates: Vec<GenCandidate>, usage_metadata?}`,
`GenCandidate{content?, finish_reason?}`, `GenUsage{prompt_token_count?, candidates_token_count?}`.
No `fileData` support — no existing `ContentBlock` maps to a remote file URI, out of scope.

**Request building**:
- `fn tool_names_by_id(messages: &[Message]) -> HashMap<String, String>` — pre-scan every
  `ContentBlock::ToolUse{id, name, ..}` across all messages. Missing lookups (shouldn't happen)
  fall back to `"unknown_function"` + `tracing::warn!`, never panic.
- `fn contents_from_messages(messages: &[Message]) -> Vec<GenContent>` — one `GenContent` per
  `Message` (role mapped, all blocks in that message become `parts` in original order: Text→
  text part, ToolUse→functionCall part carrying `id`/`args`/`thoughtSignature` from `signature`,
  ToolResult→functionResponse part using the name lookup + `response: json!({"result": content})`,
  Image→inlineData, Thinking→skipped for cross-provider-failover tolerance). Drop messages that
  produced zero parts. Then fold-merge adjacent same-role entries (concatenate `parts`) — this is
  the load-bearing mechanism described above, comment it as such, not as "defensive only".
- `fn to_gen_tools(tools: &[ToolDefinition]) -> Vec<GenTool>` — **single-element** Vec, all
  declarations under one `functionDeclarations` array. Reuse the existing already-object-schema
  unwrap logic from `openai_compat::to_oai_tools`, call `openai_compat::sanitize_schema` (made
  `pub`, see below).
- `fn build_tool_config(...) -> Option<GenToolConfig>` — `Required→"ANY"`, `None→"NONE"`, else
  omit (mirrors `anthropic.rs:128-136`).
- `fn build_generation_config(model, max_tokens, options) -> Option<GenGenerationConfig>` —
  always `Some` for `max_output_tokens`; `thinking_config` per the thinking-mode mapping above,
  `None` whenever `model.no_reasoning` is set. Temperature always passes through unconditionally.

**Response parsing**:
- `fn infer_stop_reason(tool_blocks: &[ContentBlock], finish_reason: Option<&str>) -> StopReason`
  — non-empty `tool_blocks` → `ToolUse` regardless of `finish_reason`; else `"MAX_TOKENS"` →
  `MaxTokens`; else `EndTurn`. Own pure function, directly tested.
- `fn parts_to_blocks(parts: Vec<GenPart>) -> (String, Vec<ContentBlock>)` — skip `thought==true`
  parts entirely (never shown to user, same anti-leak posture as the `<thought>` tag fix
  elsewhere this session); concatenate text; each `functionCall` → `ContentBlock::ToolUse` with
  `signature` from that same part's `thoughtSignature`.
- `fn finalize_response(text, tool_blocks, finish_reason, usage) -> UnifiedResponse` — local
  helper, deliberately **not** sharing `openai_compat::build_unified_response_from_parts` (that
  one expects string `arguments` needing JSON-parse; Gemini's `args` arrive as already-parsed
  `Value` — forcing a shared function would mean a pointless string round-trip).

**Streaming** (`async fn call_streaming(model, url, payload, stream_sink) -> Result<UnifiedResponse>`):
uses the new shared `openai_compat::drain_sse_frames` (below) to get each frame's `data:` payload,
parses each as a complete `GenResp`, accumulates text (concatenate + `stream_sink.send`) and
tool blocks (push directly, no fragment-merge needed), last-value-wins `usageMetadata`. No
`[DONE]` sentinel (OpenAI-specific, real Gemini SSE just ends the connection). No Groq
hallucinated-tool-call 400 handling (Groq-specific wording, stays local to `openai_compat.rs`).

**`pub async fn call(...)`**: resolve base via `model.base_url` or `provider_base_url("google")`;
build `generateContent`/`streamGenerateContent?alt=sse` URLs; build the payload via the functions
above; try streaming first if `stream_sink` present with the same
try-then-fall-back-to-non-streaming-if-no-tokens-sent control flow as `openai_compat.rs:671-684`;
POST with `x-goog-api-key` header; reuse `openai_compat::parse_rl_headers("google", ...)` and
`retry_after_header`; error handling in order: 429 → `"rate limit{suffix}: {body}"` (exact string
shape `model_router.rs`'s error classifier depends on), 400-with-thinking-rejection → set
`model.no_reasoning = true` and retry once without `thinking_config` (recursion-bounded same
pattern as `openai_compat.rs`), else → `"provider error {status} at {url}: {body}"`. On success,
parse `candidates[0]`, run `parts_to_blocks` + `infer_stop_reason`, return via
`finalize_response`.

**Unit tests** (pure-function, no network, mirrors the 3 being removed from `openai_compat.rs`):
role mapping; **adjacent-same-role merge using two separate `Message::tool_result()` calls**
(the actual shape `agent/loop.rs` produces — most important test given the finding above);
functionResponse name resolution via the id→name lookup; thinking-only messages produce zero
Contents; `to_gen_tools` wraps all declarations in one Tool and sanitizes non-standard schema
types; `infer_stop_reason` prefers ToolUse over a `"STOP"` finish reason, maps `MAX_TOKENS`,
defaults to `EndTurn` otherwise; `build_generation_config` for both `"level"` and `"budget"`
modes including the sub-1024 floor-drop case and the `no_reasoning` suppression case.

### 2. Strip dead weight from `crates/axon-agent/src/providers/openai_compat.rs`

Remove (all added this session for the now-obsolete compat-shim thought-signature fix):
`OaiTc::extra_content` field + doc comment, `OaiTcDelta::extra_content` field,
`PartialToolCall::signature` field, `fn extract_thought_signature`, the `extra_content:
signature.map(...)` line in `to_oai_msgs` (revert its `ContentBlock::ToolUse` destructure to
`{ id, name, input, .. }`), the three `entry.signature = ...` / `.signature = tc.extra_content...`
capture sites (streaming delta loop, streaming non-delta message loop, non-streaming response
loop), the `signature:` line from all `PartialToolCall{...}` literals (Groq hallucination-fix
mocks ×2, the function_call fallback), and hardcode `signature: None` in
`build_unified_response_from_parts`'s `ContentBlock::ToolUse` construction (the field stays on
the shared enum — `google.rs` and `anthropic.rs` use it — but nothing left in this file ever
populates it). Delete the entire `#[cfg(test)] mod tests` block (all 3 tests test the removed
behavior) — don't leave an empty stub.

**Do not touch**: the `lower.contains("reasoning") || lower.contains("thinking")` widened 400-
retry check (Groq/Gemma-specific, unrelated to Google's routing change, stays exactly as-is);
the Groq hallucinated-tool-call 400 handling; `to_oai_tools` itself.

**Add**:
- `fn sanitize_schema` → `pub fn sanitize_schema` (doc comment note: now shared with `google.rs`).
- New `pub fn drain_sse_frames(buffer: &mut String) -> Vec<String>` — extract the existing
  `buffer.find("\n\n")` / `.drain()` / strip-`"data:"`-prefix loop out of `call_streaming` into
  this generic, provider-agnostic helper (the `"[DONE]"` check is OpenAI-specific and stays
  local to `call_streaming`'s use of the returned frames). Refactor `call_streaming` to use it.
  Add one new test for it (`drain_sse_frames_splits_multiple_events_and_partial_buffer`) in a
  freshly-recreated (now near-empty) `mod tests` block — this is new/moved logic, distinct from
  the 3 tests being deleted.

### 3. `providers/mod.rs`

Add `pub mod google;`. Fix the dispatch alias bug by normalizing once at the top:
```rust
match normalize_provider_name(&model.provider).as_str() {
    "anthropic" => anthropic::call(...).await,
    "google" => google::call(...).await,
    "ollama" => ollama::call(...).await,
    _ => openai_compat::call(...).await,
}
```

### 4. `providers/types.rs`

- `provider_base_url("google")`: `.../v1beta/openai/` → `https://generativelanguage.googleapis.com/v1beta`
  (native base, no `/openai/` suffix, no trailing slash — matches the style of the groq/cerebras/
  nvidia/openrouter entries). Add a doc comment (there isn't one today) noting Google's is the
  native `generateContent` base for `providers::google`, not a compat shim.
- Update `normalizes_provider_aliases` test's expected URL to match.
- Broaden `ModelRecord.thinking_mode`'s doc comment to document Google's `"level"`/`"budget"`
  values alongside Anthropic's `"adaptive"`/`"budget"`.

### 5. `crates/axon-agent/config/models.toml`

In the existing `AGENT LOOP ROUTING` comment block: update the provider-list bullet to show
`google` as its own native adapter (not folded into "anything else"), and update the
`thinking_mode` bullet list to document the new Google values. No `[[models]]` row changes —
all 9 existing `gemini-*` rows already use `provider = "google"` with no `base_url` override,
which is exactly what the new default expects.

**Explicitly out of scope**: `axon-ui/src/pages/ModelsPage.vue` (generic dropdown, no per-
provider logic to change), `axon-ui/src/pages/DocsPage.vue` (cosmetic), `memory/embeddings.rs`
(confirmed entirely separate hand-rolled HTTP client, doesn't call into `providers/` at all),
`router/model_router.rs` (confirmed fully generic over `ModelRecord`, no changes needed),
`agent/repair.rs`'s unrelated `"search" | "google" => "web_search_tool"` tool-name heuristic.

## Verification

1. `cargo check -p axon` — clean compile. Watch for stray references to the removed
   `PartialToolCall::signature`/`extract_thought_signature`, and that `google.rs` can see
   `super::openai_compat::{sanitize_schema, drain_sse_frames, parse_rl_headers,
   retry_after_header}` (same import precedent `anthropic.rs` already uses).
2. `cargo test -p axon --lib` — full suite. Expect: all new `providers::google::tests::*`
   passing, the one surviving/new `providers::openai_compat::tests::drain_sse_frames_*` test,
   updated `providers::types::tests::normalizes_provider_aliases`, and no regressions elsewhere
   (`agent::repair`, `providers::ollama`, `providers::anthropic` already pass `signature: None`
   and need no changes).
3. **Cannot be verified from this sandbox** — no live API key, no network egress. A live smoke
   test against the real Gemini API is required before trusting this in production:
   - One plain text-only turn (no tools) — confirms auth header, URL shape, basic parsing.
   - One multi-turn tool-calling conversation with thinking enabled (requires opting a test
     model into `thinking_mode = "level"` or `"budget"` in `models.toml` first, since none of
     the current 9 rows have it set) — watch specifically for: leaked reasoning text (the
     `thought==true` skip is untested against a real response), a `no_reasoning` retry actually
     firing if a given model_id rejects `thinkingConfig`, and — the whole point of this
     migration — a second tool-calling turn *not* 400ing with "missing thought_signature".
   - A turn that produces a tool call, confirming `infer_stop_reason` correctly reports
     `ToolUse` even though Gemini's `finishReason` will say `"STOP"` (novel, zero prior
     production mileage).
   - A turn with 2+ parallel tool calls, confirming the adjacent-same-role merge produces one
     accepted `Content{role:"user"}` with multiple `functionResponse` parts (novel).
   - A deliberately-triggered 429 and 400, confirming `model_router.rs`'s string-based error
     classifier still buckets Gemini's native error bodies correctly.

### Critical files
- `crates/axon-agent/src/providers/google.rs` (new)
- `crates/axon-agent/src/providers/openai_compat.rs`
- `crates/axon-agent/src/providers/mod.rs`
- `crates/axon-agent/src/providers/types.rs`
- `crates/axon-agent/config/models.toml`
