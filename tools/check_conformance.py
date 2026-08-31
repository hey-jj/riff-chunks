#!/usr/bin/env python3
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "tests" / "data"


def main() -> None:
    manifest = json.loads((DATA / "conformance-vectors.json").read_text())
    checked = 0
    for vector in manifest["vectors"]:
        if vector["kind"] == "perf":
            continue
        payload = (DATA / vector["file"]).read_bytes()
        assert len(payload) == vector["len"], vector["id"]
        assert hashlib.sha256(payload).hexdigest() == vector["sha256"], vector["id"]
        checked += 1
    assert checked == 35


if __name__ == "__main__":
    main()

