"""
Fetch live tool schemas from a running axon-agent instance's GET /api/tools.

Auth: crates/axon-agent/src/dashboard/auth.rs allows unauthenticated access
when AXON_MASTER_KEY isn't set on the server (local-dev bypass) -- so running
axon-agent locally with AXON_DEV=1 and no AXON_MASTER_KEY needs no token at
all. Against a server that does have a master key set, pass it via
--api-key (or the AXON_MASTER_KEY env var) as a Bearer token.
"""
import argparse
import json
import os
import sys
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
OUT_PATH = REPO_ROOT / "finetune" / "data" / "raw" / "tools.json"


def fetch_tools(base_url: str, api_key: str | None) -> dict:
    req = urllib.request.Request(f"{base_url.rstrip('/')}/api/tools")
    if api_key:
        req.add_header("Authorization", f"Bearer {api_key}")
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read().decode("utf-8"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", default=os.environ.get("AXON_API_URL", "http://localhost:3000"))
    ap.add_argument("--api-key", default=os.environ.get("AXON_MASTER_KEY"))
    args = ap.parse_args()

    data = fetch_tools(args.base_url, args.api_key)
    tools = data.get("tools", [])
    if not tools:
        sys.exit("No tools returned -- check base URL / auth.")

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUT_PATH.write_text(json.dumps(data, indent=2), encoding="utf-8")
    print(f"Wrote {len(tools)} tool schemas to {OUT_PATH}")


if __name__ == "__main__":
    main()
