#!/usr/bin/env bash
# Downloads cloudflared release binaries and places them where the AgentAPI app expects
# to find them: app/src/main/jniLibs/<abi>/libcloudflared.so
#
# Run from the AgentAPI project root (or anywhere — path is resolved relative to this script):
#   bash scripts/fetch_cloudflared.sh
#
# WHY THIS IS A SEPARATE SCRIPT YOU RUN YOURSELF:
# The AI coding session that built CloudflaredManager.kt could not fetch these binaries
# directly — its sandbox only allows a small allowlist of hosts, and GitHub's release-asset
# CDN (objects.githubusercontent.com) plus Google's Maven repo were both unreachable from it.
# Your machine has normal internet access, so this one-time step is on you.
#
# After running this, rebuild the app (packagingOptions.jniLibs.useLegacyPackaging in
# build.gradle makes sure these get extracted as real executable files on-device, not
# mmap'd in place like a normal .so). Then set your Cloudflare Tunnel token in AgentAPI's
# Settings screen (or POST/PATCH /config with {"cloudflared_token": "..."}).
#
# Get a token: https://one.dash.cloudflare.com -> Networks -> Tunnels -> Create a tunnel
# -> Cloudflared connector -> copy the token. Add a Public Hostname pointing at
# http://127.0.0.1:<port> (default port 8080) while you're there.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JNI_DIR="$ROOT/app/src/main/jniLibs"

# Android ABI -> cloudflared's release asset architecture suffix
ABIS="arm64-v8a:arm64 armeabi-v7a:arm x86_64:amd64 x86:386"

echo "Fetching cloudflared binaries into $JNI_DIR ..."
echo ""

any_ok=0
for pair in $ABIS; do
  abi="${pair%%:*}"
  cf_arch="${pair##*:}"
  url="https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${cf_arch}"
  dest_dir="$JNI_DIR/$abi"
  dest="$dest_dir/libcloudflared.so"
  mkdir -p "$dest_dir"

  echo "[$abi] $url"
  if curl -fsSL "$url" -o "$dest"; then
    chmod +x "$dest"
    size=$(du -h "$dest" 2>/dev/null | cut -f1)
    echo "  -> saved to $dest ($size)"
    any_ok=1
  else
    echo "  !! download failed for $abi — skipping (remove $dest_dir if you don't need this ABI)"
    rm -f "$dest"
  fi
  echo ""
done

if [ "$any_ok" -eq 0 ]; then
  echo "Nothing downloaded successfully — check your internet connection and try again."
  exit 1
fi

echo "Done. arm64-v8a covers virtually every phone released since ~2017; the others are optional."
echo "Next: rebuild the app, then set your Cloudflare Tunnel token in Settings."
