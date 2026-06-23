#!/bin/sh
# installed by zynk
# managed by zynk; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# ZYNK_INTEGRATION_ID=droid
# ZYNK_INTEGRATION_VERSION=2

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/zynk-droid-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session) ;;
  *) exit 0 ;;
esac

[ "${ZYNK_ENV:-${ZYNK_ENV:-}}" = "1" ] || exit 0
[ -n "${ZYNK_SOCKET_PATH:-${ZYNK_SOCKET_PATH:-}}" ] || exit 0
[ -n "${ZYNK_PANE_ID:-${ZYNK_PANE_ID:-}}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

ZYNK_ACTION="$action" ZYNK_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "zynk:droid"
pane_id = os.environ.get("ZYNK_PANE_ID") or os.environ.get("ZYNK_PANE_ID")
socket_path = os.environ.get("ZYNK_SOCKET_PATH") or os.environ.get("ZYNK_SOCKET_PATH")
hook_input_file = os.environ.get("ZYNK_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

session_id = hook_input.get("session_id")
if not isinstance(session_id, str) or not session_id:
    raise SystemExit(0)

request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
request = {
    "id": request_id,
    "method": "pane.report_agent_session",
    "params": {
        "pane_id": pane_id,
        "source": source,
        "agent": "droid",
        "agent_session_id": session_id,
        "seq": report_seq,
    },
}

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
