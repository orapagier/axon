"""
Generate synthetic tool-calling examples to supplement the real dataset
(71 examples from 02_extract_dataset.py -- too few to fine-tune on alone,
per the approved plan's synthetic-fallback decision).

Approach: for each (task, candidate_tool_names) pair below, ask Gemini 2.5
Flash (already configured for Axon at models.toml P2, "proven reliable tool
calling") to role-play a FULL synthetic transcript in one JSON-structured
call -- which tool(s) it would call with what arguments, a plausible
fabricated result, and the final natural-language reply -- rather than a
live tool-calling round trip. This is deliberate: a live round trip would
mean actually executing side-effecting actions (sending real emails,
posting to real Facebook/Instagram, writing real CRM records) against the
user's real connected accounts, which this script must never do.

Reads a GEMINI_API_KEY_* value from crates/axon-agent/.env (first match)
and calls Gemini's OpenAI-compatible endpoint directly.
"""
import itertools
import json
import os
import random
import re
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
TOOLS_JSON = REPO_ROOT / "finetune" / "data" / "raw" / "tools.json"
ENV_FILE = REPO_ROOT / "crates" / "axon-agent" / ".env"
OUT_PATH = REPO_ROOT / "finetune" / "data" / "processed" / "train_synthetic.jsonl"

GEMINI_URL = "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
GEMINI_MODEL = "gemini-2.5-flash"

AXON_SYSTEM_PROMPT = (
    "You are Axon, a helpful AI agent. You MUST use the native JSON tool "
    "calling mechanism provided by the API to call tools. NEVER output raw "
    "JSON snippets, markdown code blocks, or XML tags like <tool_call> in "
    "your text response. Respond in plain text only."
)

# (user task, candidate real tool names to offer -- mirrors the "hybrid"
# scope's small routed subset rather than the full 320-tool catalog)
TASKS = [
    ("Send an email to jane@example.com saying the report is ready", ["gmail_send", "gmail_create_draft"]),
    ("Check my last 5 unread emails", ["gmail_list"]),
    ("Forward the email about the invoice to accounting@example.com", ["gmail_forward", "gmail_list"]),
    ("Create a draft reply thanking them for the update", ["gmail_create_draft"]),
    ("Schedule a meeting with the design team tomorrow at 2pm for 1 hour", ["gcal_create_event", "datetime_now"]),
    ("What's on my calendar this week?", ["gcal_list_events"]),
    ("Cancel my 3pm meeting today", ["gcal_delete_event", "gcal_list_events"]),
    ("Move my dentist appointment to next Friday", ["gcal_update_event", "gcal_list_events"]),
    ("Am I free tomorrow afternoon?", ["gcal_get_freebusy"]),
    ("Add a new deal for Acme Corp worth $5000", ["crm_deal_create"]),
    ("Show me all deals in the pipeline", ["crm_deal_get", "crm_dashboard_summary"]),
    ("Log that I called the client about the contract", ["crm_activity_log"]),
    ("What tasks are overdue in the CRM?", ["task_overdue"]),
    ("Create a follow-up task for next Monday", ["task_create"]),
    ("Mark the invoice task as complete", ["task_complete", "task_list"]),
    ("Add a new row to the budget spreadsheet with this month's expenses", ["gsheets_append_rows"]),
    ("Read the data from the sales sheet", ["gsheets_batch_read"]),
    ("Bold the header row in the sheet", ["gsheets_bold_row"]),
    ("Post this update to our Facebook page: 'New product launch next week!'", ["fb_create_post"]),
    ("Reply to the comment on our latest Instagram post", ["ig_list_comments", "ig_reply_to_comment"]),
    ("Get engagement insights for our Instagram account", ["ig_get_insights"]),
    ("Upload this file to Google Drive", ["gdrive_upload_binary"]),
    ("Find the presentation I uploaded last week", ["gdrive_search"]),
    ("Share the budget spreadsheet with my accountant", ["gdrive_share", "gdrive_search"]),
    ("Take a note: remember to renew the domain in August", ["note_create"]),
    ("Search my notes for anything about the Camella project", ["note_search"]),
    ("Add a new contact: Maria Santos, phone 09171234567", ["contact_create"]),
    ("Look up Maria's phone number", ["contact_search"]),
    ("What time is it in Tokyo right now?", ["datetime_now", "datetime_convert"]),
    ("How many days until December 25th?", ["datetime_diff", "datetime_now"]),
    ("Search the web for the latest exchange rate of USD to PHP", ["web_search"]),
    ("Check disk space on the server", ["shell_tool"]),
    ("Restart the axon-agent service", ["shell_tool"]),
    ("SSH into the production server and check memory usage", ["ssh_tool"]),
    ("Send a message on Telegram to the ops channel that deploy finished", ["telegram_send_message"]),
    ("Reply to the WhatsApp message from the supplier", ["whatsapp_send_message"]),
    ("Create a Google Task to buy office supplies", ["gtasks_create_task"]),
    ("List my Microsoft Teams meetings for today", ["mscal_list_events"]),
    ("Send an Outlook email with the quarterly report attached", ["outlook_send_with_attachment"]),
    ("Search Outlook for emails from the finance team", ["outlook_search"]),
    ("What's my next event in Microsoft Calendar?", ["mscal_list_events"]),
    ("Decline the meeting invite from HR", ["mscal_decline_event", "mscal_list_events"]),
    ("Update the CRM deal amount to 75000 for the Davao client", ["crm_deal_get", "crm_deal_create"]),
    ("Back up the CRM database", ["crm_backup_db"]),
    ("List all my Google Sheets", ["gsheets_add_sheet", "gdrive_search"]),
    ("Create a new sheet called Q3 Expenses", ["gsheets_add_sheet"]),
    ("Get the latest 3 posts from our Facebook page", ["fb_get_post"]),
    ("Delete that spam comment on Instagram", ["ig_list_comments", "ig_delete_comment"]),
    ("Convert 2pm EST to Manila time", ["datetime_convert"]),
    ("What's the weather like for outdoor posting today?", ["web_search"]),
]


def load_env_key_candidates():
    text = ENV_FILE.read_text(encoding="utf-8", errors="ignore")
    keys = []
    for line in text.splitlines():
        m = re.match(r'^(GEMINI_API_KEY\w*)\s*=\s*"?([^"\r\n]+)"?\s*$', line.strip())
        if m:
            keys.append(m.group(2).strip())
    if not keys:
        raise SystemExit("No GEMINI_API_KEY* found in crates/axon-agent/.env")
    return keys


def find_working_keys(candidates):
    """Some keys in .env may be revoked/invalid, and even valid free-tier
    keys hit low per-key RPM limits fast (Axon's own P2 tier round-robins
    9 of them for exactly this reason) -- probe all candidates and keep
    every one that authenticates, so calls can round-robin across them."""
    probe_body = json.dumps({
        "model": GEMINI_MODEL,
        "messages": [{"role": "user", "content": "reply with the single word: ok"}],
    }).encode("utf-8")
    working = []
    for key in candidates:
        req = urllib.request.Request(
            GEMINI_URL, data=probe_body,
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}"},
        )
        try:
            with urllib.request.urlopen(req, timeout=15):
                working.append(key)
        except Exception:
            continue
        time.sleep(1)  # stay polite to the free-tier RPM limit while probing
    if not working:
        raise SystemExit(f"None of the {len(candidates)} GEMINI_API_KEY* candidates authenticated.")
    print(f"{len(working)}/{len(candidates)} Gemini keys authenticated, will round-robin across them")
    return working


def load_tools_by_name():
    data = json.loads(TOOLS_JSON.read_text(encoding="utf-8"))
    return {t["name"]: t for t in data["tools"]}


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


GEN_INSTRUCTIONS = """You are generating ONE synthetic training example for fine-tuning a small \
tool-calling assistant. You are given a user task and a list of real tool schemas that would be \
available to the assistant. Decide whether the task needs 0, 1, or 2 tool calls (most real tasks \
need exactly 1; only chain 2 if genuinely necessary, e.g. look up a contact before emailing them). \
Fabricate a plausible, realistic RESULT for each tool call as if it succeeded (invented but \
believable data -- names, ids, dates, counts). Then write the assistant's final natural-language \
reply to the user, as Axon would.

Respond with ONLY a JSON object, no markdown fences, no commentary, in this exact shape:
{
  "tool_calls": [{"name": "<tool name from the list>", "arguments": {...}, "fabricated_result": {...or a string}}],
  "final_reply": "<assistant's final plain-text reply to the user>"
}
If no tool call is needed, use "tool_calls": [].
"""


def call_gemini(key_cycle, task, tool_defs, retries=4):
    tool_list_text = "\n".join(
        f"- {t['name']}: {t['description']}" for t in tool_defs
    )
    prompt = f"{GEN_INSTRUCTIONS}\n\nUser task: {task}\n\nAvailable tools:\n{tool_list_text}"
    body = json.dumps({
        "model": GEMINI_MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.8,
        "response_format": {"type": "json_object"},
    }).encode("utf-8")

    last_err = None
    for attempt in range(retries):
        api_key = next(key_cycle)
        req = urllib.request.Request(
            GEMINI_URL, data=body,
            headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                payload = json.loads(resp.read().decode("utf-8"))
            return payload["choices"][0]["message"]["content"]
        except urllib.error.HTTPError as e:
            last_err = e
            if e.code == 429:
                time.sleep(2 * (attempt + 1))  # back off and try the next key in the cycle
                continue
            raise
    raise last_err


def build_example(task, candidate_names, tools_by_name, key_cycle):
    tool_defs = [tools_by_name[n] for n in candidate_names if n in tools_by_name]
    search_def = tools_by_name.get("search_tools")
    raw = call_gemini(key_cycle, task, tool_defs)
    gen = json.loads(raw)

    example_tools = []
    if search_def:
        example_tools.append(to_oai_tool(search_def))
    seen = set()
    for c in gen.get("tool_calls", []):
        if c["name"] in tools_by_name and c["name"] not in seen:
            example_tools.append(to_oai_tool(tools_by_name[c["name"]]))
            seen.add(c["name"])

    messages = [
        {"role": "system", "content": AXON_SYSTEM_PROMPT},
        {"role": "user", "content": task},
    ]
    for idx, c in enumerate(gen.get("tool_calls", [])):
        if c["name"] not in tools_by_name:
            continue  # model hallucinated a tool name -- drop that call, keep the rest
        call_id = f"call_synth_{idx}"
        messages.append({
            "role": "assistant", "content": "",
            "tool_calls": [{"id": call_id, "type": "function",
                             "function": {"name": c["name"], "arguments": c["arguments"]}}],
        })
        result = c.get("fabricated_result", "")
        result_text = json.dumps(result) if not isinstance(result, str) else result
        messages.append({"role": "tool", "tool_call_id": call_id, "content": result_text})

    messages.append({"role": "assistant", "content": gen.get("final_reply", "")})
    return {"source": "synthetic", "task": task, "tools": example_tools, "messages": messages}


def main():
    working_keys = find_working_keys(load_env_key_candidates())
    key_cycle = itertools.cycle(working_keys)
    tools_by_name = load_tools_by_name()
    random.shuffle(TASKS)

    examples = []
    errors = []
    for task, candidates in TASKS:
        try:
            examples.append(build_example(task, candidates, tools_by_name, key_cycle))
            print(f"ok: {task[:60]}")
        except Exception as e:
            errors.append((task, str(e)))
            print(f"FAILED: {task[:60]} -- {e}")
        time.sleep(1)  # spread load across the key pool instead of bursting

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT_PATH, "w", encoding="utf-8") as f:
        for ex in examples:
            f.write(json.dumps(ex, ensure_ascii=False) + "\n")

    print(f"\nWrote {len(examples)} synthetic examples to {OUT_PATH}")
    if errors:
        print(f"{len(errors)} tasks failed to generate:")
        for task, err in errors:
            print(f"  - {task[:60]}: {err}")


if __name__ == "__main__":
    main()
