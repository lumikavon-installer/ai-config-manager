"""Register an application's existing local sync callback (Python AF_UNIX)."""

import argparse
from pathlib import Path
from sync_config import request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app_id")
    parser.add_argument("app_name")
    parser.add_argument("socket_path", type=Path)
    parser.add_argument("--config-id", default="settings")
    parser.add_argument("--label", default="Settings")
    parser.add_argument("--path", default="/config")
    parser.add_argument("--manager-socket", type=Path)
    args = parser.parse_args()
    agent_dir = Path.home() / ".config" / "ai-config-manager"
    token = (agent_dir / "agent.token").read_text(encoding="utf-8")
    request(args.manager_socket or agent_dir / "agent.sock", token, {
        "op": "sync_register_application",
        "request": {
            "app_id": args.app_id,
            "app_name": args.app_name,
            "config_id": args.config_id,
            "label": args.label,
            "sync_interface": {"socket_path": str(args.socket_path.absolute()), "path": args.path},
            "enabled": True,
        },
    })
    print("Application configuration registered.")


if __name__ == "__main__":
    main()
