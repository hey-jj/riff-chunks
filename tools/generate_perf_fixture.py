#!/usr/bin/env python3
import struct
from pathlib import Path


DATA_SIZE = 8 * 1024 * 1024 * 1024
LIST_OFFSET = 80 + DATA_SIZE
FILE_LEN = LIST_OFFSET + 26


def main() -> None:
    path = Path(__file__).resolve().parents[1] / "tests" / "data" / "fixtures" / "perf-sparse-rf64.wav"
    with path.open("wb") as output:
        output.truncate(FILE_LEN)
        output.seek(0)
        output.write(b"RF64" + struct.pack("<I", 0xFFFFFFFF) + b"WAVE")
        output.write(b"ds64" + struct.pack("<IQQQI", 28, FILE_LEN - 8, DATA_SIZE, 0, 0))
        output.write(b"fmt " + struct.pack("<I", 16) + bytes(16))
        output.write(b"data" + struct.pack("<I", 0xFFFFFFFF))
        output.seek(LIST_OFFSET)
        output.write(b"LIST" + struct.pack("<I", 18) + b"INFO")
        output.write(b"ISFT" + struct.pack("<I", 5) + b"perf!\0")


if __name__ == "__main__":
    main()
