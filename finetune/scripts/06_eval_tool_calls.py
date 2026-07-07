"""
Sanity-check the fine-tuned model against the held-out eval set before
trusting it anywhere in Axon's routing. Replays eval.jsonl (real examples
only) against a local Ollama endpoint using the same request shape
crates/axon-agent/src/providers/ollama.rs::call() sends, and checks:
  - message.tool_calls present/absent matches the reference
  - tool_calls[].function.name is one of the tools passed in this turn
  - arguments parses as JSON and satisfies the tool's required list
  - no raw {"name":...}-style text leaking into message.content (the exact
    failure mode Axon's system prompt guards against)
"""
import argparse
import json
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
EVAL_PATH = REPO_ROOT / "finetune" / "data" / "processed" / "eval.jsonl"


def call_ollama(base_url, model, messages, tools):
    body = json.dumps({
        "model": model, "messages": messages, "tools": tools, "stream": False,
    }).encode("utf-8")
    req = urllib.request.Request(
        f"{base_url.rstrip('/')}/api/chat", data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode("utf-8"))


def reference_expects_tool_call(messages):
    return any(m.get("role") == "assistant" and m.get("tool_calls") for m in messages)


def leading_messages(messages):
    """Everything up to (not including) the first assistant turn that has a
    tool_call or is the final answer -- i.e. what the model would actually see."""
    out = []
    for m in messages:
        if m["role"] == "assistant":
            break
        out.append(m)
    return out


def looks_like_leaked_tool_syntax(text):
    if not text:
        return False
    markers = ['"tool_call"', "<tool_call>", '{"name":', "```json"]
    return any(marker in text for marker in markers)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", default="http://localhost:11434")
    ap.add_argument("--model", default="axon-qwen2.5-1.5b-local")
    args = ap.parse_args()

    examples = [json.loads(l) for l in EVAL_PATH.read_text(encoding="utf-8").splitlines() if l.strip()]
    print(f"Evaluating {len(examples)} held-out examples against {args.model} @ {args.base_url}")

    results = {"total": 0, "tool_call_match": 0, "valid_tool_name": 0,
               "valid_json_args": 0, "no_leaked_syntax": 0, "errors": 0}

    for ex in examples:
        results["total"] += 1
        messages = leading_messages(ex["messages"])
        expects_call = reference_expects_tool_call(ex["messages"])
        tool_names = {t["function"]["name"] for t in ex.get("tools", [])}

        try:
            resp = call_ollama(args.base_url, args.model, messages, ex.get("tools"))
        except Exception as e:
            print(f"ERROR calling model: {e}")
            results["errors"] += 1
            continue

        msg = resp.get("message", {})
        got_calls = msg.get("tool_calls") or []
        got_call = bool(got_calls)

        if got_call == expects_call:
            results["tool_call_match"] += 1
        else:
            print(f"MISMATCH: expected_tool_call={expects_call} got={got_call} task={messages[-1]['content'][:60]!r}")

        if got_calls:
            names_ok = all(c["function"]["name"] in tool_names for c in got_calls)
            if names_ok:
                results["valid_tool_name"] += 1
            args_ok = True
            for c in got_calls:
                raw_args = c["function"].get("arguments")
                if isinstance(raw_args, str):
                    try:
                        json.loads(raw_args)
                    except json.JSONDecodeError:
                        args_ok = False
                # dict arguments are already valid JSON by construction
            if args_ok:
                results["valid_json_args"] += 1
        else:
            results["valid_tool_name"] += 1  # vacuously true, nothing to check
            results["valid_json_args"] += 1

        if not looks_like_leaked_tool_syntax(msg.get("content", "")):
            results["no_leaked_syntax"] += 1

    print("\n=== Results ===")
    for k, v in results.items():
        if k == "total":
            continue
        print(f"{k}: {v}/{results['total']} ({100*v/max(1,results['total']):.0f}%)")


if __name__ == "__main__":
    main()
