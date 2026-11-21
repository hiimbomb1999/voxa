#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
root="$HOME/Library/Application Support/local.voxa.translate/vieneu"
mkdir -p "$root"
export UV_PYTHON_INSTALL_DIR="$root/python" HF_HOME="$root/models" HF_HUB_DISABLE_TELEMETRY=1
if [ ! -x "$root/venv/bin/python" ]; then
    uv venv --managed-python --python 3.12 "$root/venv"
fi
uv pip install --python "$root/venv/bin/python" 'vieneu==3.8.5'

manifest=$(mktemp "$root/voices.XXXXXX")
trap 'rm -f "$manifest"' EXIT
"$root/venv/bin/python" -I src-tauri/sidecars/vieneu.py --install > "$manifest"
mv -f "$manifest" "$root/voices.json"
printf '%s\n' 'Đã cài VieNeu-TTS. Mở lại Voxa để chọn giọng.'
