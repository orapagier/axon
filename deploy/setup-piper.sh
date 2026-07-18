#!/bin/bash
#
# Installs the Piper offline TTS engine (binary + voice models) into Axon's
# data dir, so `tts.base_url = "piper"` (Settings → Voice Replies) works.
# Standalone and idempotent — not part of deploycham.sh/deployaxongcp.sh, run
# it directly wherever spoken replies should use Piper:
#   local dev (Git Bash):  bash deploy/setup-piper.sh
#   server (over SSH):     ssh user@host 'bash -s' < deploy/setup-piper.sh
#
# Voice ids to install are given as args (default: en_US-hfc_female-medium).
# Browse more at https://huggingface.co/rhasspy/piper-voices — id format is
# <lang>_<REGION>-<name>-<quality>, e.g. en_US-lessac-medium.
set -e

PIPER_RELEASE="2023.11.14-2"
VOICES=("$@")
if [ ${#VOICES[@]} -eq 0 ]; then
    VOICES=("en_US-hfc_female-medium")
fi

# Mirrors axon_core::data_dir(): AXON_DATA_DIR when set, else the platform
# local-data dir joined with "axon-mcp".
if [ -n "$AXON_DATA_DIR" ]; then
    DATA_DIR="$AXON_DATA_DIR"
elif [ -n "$LOCALAPPDATA" ]; then
    DATA_DIR="$LOCALAPPDATA/axon-mcp"
elif [ "$(uname -s)" = "Darwin" ]; then
    DATA_DIR="$HOME/Library/Application Support/axon-mcp"
else
    DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/axon-mcp"
fi
PIPER_DIR="$DATA_DIR/piper"
MODELS_DIR="$PIPER_DIR/models"
mkdir -p "$MODELS_DIR"
echo "Installing into: $PIPER_DIR"

# ── Binary (skip if already present — re-run to add voices without re-fetching) ──
BIN="$PIPER_DIR/piper"
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) BIN="$PIPER_DIR/piper.exe"; ASSET="piper_windows_amd64.zip" ;;
    Darwin) [ "$(uname -m)" = "arm64" ] && ASSET="piper_macos_aarch64.tar.gz" || ASSET="piper_macos_x64.tar.gz" ;;
    *) [ "$(uname -m)" = "aarch64" ] && ASSET="piper_linux_aarch64.tar.gz" || ASSET="piper_linux_x86_64.tar.gz" ;;
esac

if [ -f "$BIN" ]; then
    echo "Binary already installed: $BIN"
else
    echo "Downloading $ASSET (release $PIPER_RELEASE)..."
    TMP="$(mktemp -d)"
    URL="https://github.com/rhasspy/piper/releases/download/$PIPER_RELEASE/$ASSET"
    curl -sL --fail -o "$TMP/piper.archive" "$URL"
    case "$ASSET" in
        *.zip) unzip -q "$TMP/piper.archive" -d "$TMP" ;;
        *.tar.gz) tar -xzf "$TMP/piper.archive" -C "$TMP" ;;
    esac
    # The archive extracts a top-level "piper/" folder; its contents (the exe
    # plus sibling .dll/.so libs + espeak-ng-data/) must sit directly in
    # PIPER_DIR since the binary loads them from its own directory.
    cp -r "$TMP"/piper/* "$PIPER_DIR/"
    rm -rf "$TMP"
    chmod +x "$BIN" 2>/dev/null || true
    echo "Installed binary: $BIN"
fi

# ── Voice models ──
for voice in "${VOICES[@]}"; do
    onnx="$MODELS_DIR/$voice.onnx"
    if [ -f "$onnx" ]; then
        echo "Voice already installed: $voice"
        continue
    fi
    locale="${voice%%-*}"          # e.g. en_US
    lang="${locale%%_*}"           # e.g. en
    rest="${voice#*-}"             # e.g. hfc_female-medium
    tier="${rest##*-}"             # e.g. medium
    name="${rest%-*}"              # e.g. hfc_female
    base="https://huggingface.co/rhasspy/piper-voices/resolve/main/$lang/$locale/$name/$tier"
    echo "Downloading voice $voice..."
    curl -sL --fail -o "$onnx" "$base/$voice.onnx"
    curl -sL --fail -o "$onnx.json" "$base/$voice.onnx.json"
    echo "Installed voice: $voice"
done

echo ""
echo "Done. In Settings -> Voice Replies, set tts.base_url = piper and pick a"
echo "voice from the tts.model dropdown (or type one of: ${VOICES[*]})."
