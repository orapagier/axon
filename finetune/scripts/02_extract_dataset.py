"""
Extract Axon agent transcripts into Qwen2.5 tool-calling JSONL training examples.

Ground truth is short_term (session_id, role, content, tool_name) + tool_calls
(joined via runs.id == tool_calls.run_id, runs.session_id == short_term.session_id).
NOT filtered by runs.status/iterations -- those columns are bugged in production
(every row is status in {failed,cancelled,running} with iterations=0, even for
runs that clearly succeeded; see finetune/README.md). A `runs` row is matched to
its triggering short_term user turn by exact content==task within the same
session, breaking ties by closest created_at.

Tool-list fidelity: Axon's runtime "hybrid" tool scope (agent/tool_discovery.rs)
sends a small embedding-routed subset + a search_tools meta-tool each turn, not
the full ~320-tool catalog (crates/axon-agent/src/tools/schema.rs ToolDefinition,
dumped via GET /api/tools into data/raw/tools.json). This script approximates
that by giving each example search_tools + only the tool(s) it actually uses,
rather than the full catalog -- matching real inference conditions without
blowing the token budget on a 1.5B CPU-trained model.

Known real-data hazards this script actively filters out (found by manual
inspection of the pulled production DB, not assumed):
  - Stuck tool-call loops (same tool_name+args repeated back to back -- one
    real run repeated an identical web_request 20+ times against the same
    URL). These get dropped, not trained on.
  - <think>...</think> reasoning leakage in some stored assistant turns
    (from a different backing model) -- stripped, since the target model
    is a plain instruct model, not a reasoning-tag model.
  - Near-duplicate trivial greetings ("hi"/"hey"/"how are you" repeated
    dozens of times) -- deduped, capped per normalized-text bucket, so a
    tiny dataset isn't dominated by small talk.
"""
import json
import re
import sqlite3
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
RAW_DB = REPO_ROOT / "finetune" / "data" / "raw" / "axon.db"
TOOLS_JSON = REPO_ROOT / "finetune" / "data" / "raw" / "tools.json"
OUT_DIR = REPO_ROOT / "finetune" / "data" / "processed"

MAX_TOOL_RESULT_CHARS = 4000
MAX_TOOL_CALLS_PER_EXAMPLE = 4
CONTEXT_WINDOW_TURNS = 6  # prior short_term rows (user+assistant) included before the current turn
DEDUP_CAP_PER_BUCKET = 2
WORKFLOW_SAMPLE_CAP = 3  # per distinct wf:... session, since heartbeat/prayer content repeats hourly
EVAL_FRACTION = 0.15

THINK_RE = re.compile(r"<think>.*?</think>\s*", re.DOTALL)

AXON_SYSTEM_PROMPT = (
    "You are Axon, a helpful AI agent. You MUST use the native JSON tool "
    "calling mechanism provided by the API to call tools. NEVER output raw "
    "JSON snippets, markdown code blocks, or XML tags like <tool_call> in "
    "your text response. Respond in plain text only."
)  # condensed from runtime_settings.rs::system_prompt() + system_context.rs
   # build_run_context() -- the full runtime prompt also injects a live
   # [Context] block (time/memory/observations) that's run-specific and
   # not reproducible from historical rows, so it's intentionally omitted.


def load_tools():
    data = json.loads(TOOLS_JSON.read_text(encoding="utf-8"))
    by_name = {t["name"]: t for t in data["tools"]}
    return by_name


def to_oai_tool(t):
    return {
        "type": "function",
        "function": {
            "name": t["name"],
            "description": t["description"],
            "parameters": {
                "type": "object",
                "properties": t.get("parameters", {}).get("properties", t.get("parameters", {})),
                "required": t.get("required", []),
            },
        },
    }


def strip_think(text):
    if not text:
        return text
    return THINK_RE.sub("", text).strip()


def truncate_result(text):
    if text is None:
        return ""
    if len(text) <= MAX_TOOL_RESULT_CHARS:
        return text
    return text[:MAX_TOOL_RESULT_CHARS] + f"...[truncated, {len(text)} chars total]"


def is_degenerate_loop(calls):
    """Detects the stuck-loop failure pattern: 3+ calls with identical (tool, args)."""
    seen = defaultdict(int)
    for c in calls:
        key = (c["tool_name"], c["args"])
        seen[key] += 1
        if seen[key] >= 3:
            return True
    return len(calls) > MAX_TOOL_CALLS_PER_EXAMPLE


def normalize_for_dedup(text):
    return re.sub(r"[^a-z0-9]+", "", (text or "").lower())


def fetch_all(con):
    con.row_factory = sqlite3.Row
    cur = con.cursor()
    short_term = cur.execute(
        "SELECT rowid, session_id, role, content, tool_name, created_at "
        "FROM short_term ORDER BY session_id, rowid"
    ).fetchall()
    runs = cur.execute(
        "SELECT id, session_id, task, created_at FROM runs ORDER BY session_id, created_at"
    ).fetchall()
    tool_calls = cur.execute(
        "SELECT rowid, run_id, tool_name, args, result, error, created_at "
        "FROM tool_calls ORDER BY run_id, rowid"
    ).fetchall()
    return short_term, runs, tool_calls


def build_examples():
    con = sqlite3.connect(str(RAW_DB))
    short_term, runs, tool_calls = fetch_all(con)
    tools_by_name = load_tools()
    search_tools_def = tools_by_name.get("search_tools")

    tool_calls_by_run = defaultdict(list)
    for tc in tool_calls:
        tool_calls_by_run[tc["run_id"]].append(dict(tc))

    # runs matched to their triggering short_term user row: exact (session_id, task==content)
    runs_by_session = defaultdict(list)
    for r in runs:
        runs_by_session[r["session_id"]].append(dict(r))

    sessions = defaultdict(list)
    for row in short_term:
        sessions[row["session_id"]].append(dict(row))

    examples = []
    dedup_counts = defaultdict(int)
    workflow_counts = defaultdict(int)
    stats = {"total_user_turns": 0, "matched_run": 0, "dropped_loop": 0,
             "dropped_missing_tool": 0, "dropped_dedup": 0, "dropped_no_reply": 0,
             "kept_with_tools": 0, "kept_plain": 0}

    for session_id, rows in sessions.items():
        is_workflow = session_id.startswith("wf:")
        # available runs for this session, consumed as matched so duplicates
        # (e.g. multiple 'hi' tasks) each match a distinct run in order
        available_runs = list(runs_by_session.get(session_id, []))

        history = []  # rolling prior-turn context: list of {"role","content"}
        i = 0
        while i < len(rows):
            row = rows[i]
            if row["role"] == "trace":
                i += 1
                continue
            if row["role"] != "user":
                history.append({"role": row["role"], "content": strip_think(row["content"])})
                history = history[-CONTEXT_WINDOW_TURNS:]
                i += 1
                continue

            stats["total_user_turns"] += 1
            user_text = row["content"] or ""

            # find the next non-trace assistant reply
            reply = None
            j = i + 1
            while j < len(rows):
                if rows[j]["role"] == "assistant":
                    reply = rows[j]
                    break
                if rows[j]["role"] == "user":
                    break  # no reply before the next user turn
                j += 1
            if reply is None:
                stats["dropped_no_reply"] += 1
                history.append({"role": "user", "content": user_text})
                history = history[-CONTEXT_WINDOW_TURNS:]
                i += 1
                continue

            if is_workflow:
                if workflow_counts[session_id] >= WORKFLOW_SAMPLE_CAP:
                    i = j + 1
                    continue
                workflow_counts[session_id] += 1

            dedup_key = (is_workflow, normalize_for_dedup(user_text))
            if dedup_counts[dedup_key] >= DEDUP_CAP_PER_BUCKET:
                stats["dropped_dedup"] += 1
                history.append({"role": "user", "content": user_text})
                history.append({"role": "assistant", "content": strip_think(reply["content"])})
                history = history[-CONTEXT_WINDOW_TURNS:]
                i = j + 1
                continue
            dedup_counts[dedup_key] += 1

            # match this user turn to a runs row (exact task text, in session order)
            matched_run = None
            for idx, r in enumerate(available_runs):
                if r["task"] == user_text:
                    matched_run = available_runs.pop(idx)
                    break

            calls = tool_calls_by_run.get(matched_run["id"], []) if matched_run else []

            messages = [{"role": "system", "content": AXON_SYSTEM_PROMPT}]
            messages.extend(history)
            messages.append({"role": "user", "content": user_text})

            example_tools = []
            if search_tools_def:
                example_tools.append(to_oai_tool(search_tools_def))

            if calls:
                stats["matched_run"] += 1
                if is_degenerate_loop(calls):
                    stats["dropped_loop"] += 1
                    history.append({"role": "user", "content": user_text})
                    history.append({"role": "assistant", "content": strip_think(reply["content"])})
                    history = history[-CONTEXT_WINDOW_TURNS:]
                    i = j + 1
                    continue

                missing = [c["tool_name"] for c in calls if c["tool_name"] not in tools_by_name]
                if missing:
                    stats["dropped_missing_tool"] += 1
                    history.append({"role": "user", "content": user_text})
                    history.append({"role": "assistant", "content": strip_think(reply["content"])})
                    history = history[-CONTEXT_WINDOW_TURNS:]
                    i = j + 1
                    continue

                seen_tool_names = set()
                for idx, c in enumerate(calls):
                    if c["tool_name"] not in seen_tool_names:
                        example_tools.append(to_oai_tool(tools_by_name[c["tool_name"]]))
                        seen_tool_names.add(c["tool_name"])
                    call_id = f"call_{matched_run['id'][:8]}_{idx}"
                    try:
                        arguments = json.loads(c["args"]) if c["args"] else {}
                    except json.JSONDecodeError:
                        arguments = {}
                    messages.append({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {"name": c["tool_name"], "arguments": arguments},
                        }],
                    })
                    result_text = c["error"] if c["error"] else truncate_result(c["result"])
                    messages.append({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": result_text or "",
                    })
                stats["kept_with_tools"] += 1
            else:
                stats["kept_plain"] += 1

            final_text = strip_think(reply["content"]) if reply["content"] else ""
            messages.append({"role": "assistant", "content": final_text})

            examples.append({
                "source": "real",
                "session_id": session_id,
                "created_at": row["created_at"],
                "tools": example_tools,
                "messages": messages,
            })

            history.append({"role": "user", "content": user_text})
            history.append({"role": "assistant", "content": final_text})
            history = history[-CONTEXT_WINDOW_TURNS:]
            i = j + 1

    return examples, stats


def main():
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    examples, stats = build_examples()
    examples.sort(key=lambda e: e["created_at"])

    n_eval = max(1, int(len(examples) * EVAL_FRACTION)) if examples else 0
    train, eval_ = examples[:-n_eval] if n_eval else examples, examples[-n_eval:] if n_eval else []

    with open(OUT_DIR / "train_real.jsonl", "w", encoding="utf-8") as f:
        for ex in train:
            f.write(json.dumps(ex, ensure_ascii=False) + "\n")
    with open(OUT_DIR / "eval.jsonl", "w", encoding="utf-8") as f:
        for ex in eval_:
            f.write(json.dumps(ex, ensure_ascii=False) + "\n")

    print(f"Wrote {len(train)} train_real.jsonl + {len(eval_)} eval.jsonl examples")
    print("Stats:", json.dumps(stats, indent=2))


if __name__ == "__main__":
    main()
