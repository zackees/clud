"""Drive the real settings window with X11 input, without an approval backdoor."""

from __future__ import annotations

import json
import sys
from pathlib import Path

from tests import process


def main() -> None:
    binary, repo, target, action, mutation_json = sys.argv[1:]
    child = process.Popen(
        [binary, "settings", *json.loads(mutation_json)],
        cwd=repo, stdin=process.DEVNULL, stdout=process.PIPE, stderr=process.PIPE,
        text=True,
    )
    try:
        window = process.run(
            ["xdotool", "search", "--sync", "--onlyvisible", "--name",
             "^clud settings approval$"],
            capture_output=True, text=True, timeout=20, check=True,
        ).stdout.strip().splitlines()[0]
        process.run(["xdotool", "windowfocus", "--sync", window], timeout=10, check=True)
        if action == "changed":
            Path(target).write_text('{"concurrent":true}', encoding="utf-8")
        if action == "target":
            settings = Path(target)
            original = settings.read_bytes()
            settings.parent.rename(settings.parent.with_name("previous-config"))
            replacement = settings.parent.with_name("replacement-config")
            replacement.mkdir()
            (replacement / settings.name).write_bytes(original)
            settings.parent.symlink_to(replacement, target_is_directory=True)
        keys = {"cancel": ["Escape"], "enter": ["Return"]}.get(action, ["Tab", "Return"])
        process.run(
            ["xdotool", "key", "--clearmodifiers", "--delay", "150", *keys],
            timeout=10, check=True,
        )
        stdout, stderr = child.communicate(timeout=20)
        print(json.dumps({"returncode": child.returncode, "stdout": stdout, "stderr": stderr}))
    finally:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=10)


if __name__ == "__main__":
    main()
