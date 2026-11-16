"""Register a third-party file and upload it through the running desktop agent.

Requires Python with socket.AF_UNIX (Linux/macOS). On Windows use the Rust CLI:
ai-config sync-add --path C:\\configs\\app.json --label MyApp
ai-config sync
"""

import argparse
import json
from pathlib import Path
import socket


def request(socket_path, token, operation):
    payload = json.dumps({"version": 1, "token": token, "op": operation}).encode() + b"\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(600)
        connection.connect(str(socket_path))
        connection.sendall(payload)
        with connection.makefile("rb") as reader:
            reply = reader.readline(2 * 1024 * 1024 + 1)
    if len(reply) > 2 * 1024 * 1024 or not reply.endswith(b"\n"):
        raise RuntimeError("AGENT_PROTOCOL")
    result = json.loads(reply)
    if not result["ok"]:
        raise RuntimeError(result["error"]["code"])
    return result["data"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", help="Configuration file to register")
    parser.add_argument("--label", default="Third-party application")
    parser.add_argument("--socket", type=Path, help="Override with the socket from ai-config status")
    args = parser.parse_args()
    if not hasattr(socket, "AF_UNIX"):
        parser.error("This Python build lacks AF_UNIX; use ai-config sync-add and ai-config sync.")
    agent_dir = Path.home() / ".config" / "ai-config-manager"
    token = (agent_dir / "agent.token").read_text(encoding="utf-8")
    socket_path = args.socket or agent_dir / "agent.sock"
    path = str(Path(args.path).absolute())
    state = request(socket_path, token, {"op": "sync_read"})
    item = next((item for item in state["items"] if item["path"] == path), None)
    if item is None:
        request(socket_path, token, {
            "op": "sync_add_item", "path": path, "label": args.label, "enabled": True,
        })
    elif not item["enabled"]:
        request(socket_path, token, {"op": "sync_set_item_enabled", "id": item["id"], "enabled": True})
    state = request(socket_path, token, {"op": "sync_run"})
    print(json.dumps({"state": state["status"]["state"], "item_count": state["status"]["item_count"]}))


if __name__ == "__main__":
    main()
