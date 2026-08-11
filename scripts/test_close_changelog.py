#!/usr/bin/env python3
"""Regression tests for the system release changelog closer."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile


CLOSER = Path(__file__).with_name("close-changelog.py")


def close(path: Path, version: str, date: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CLOSER), str(path), version, date],
        check=False,
        capture_output=True,
        text=True,
    )


def count_header(text: str, version: str) -> int:
    return len(re.findall(rf"^## \[{re.escape(version)}\][^\n]*$", text, re.MULTILINE))


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="dracon-system-changelog-") as raw:
        path = Path(raw) / "CHANGELOG.md"
        path.write_text(
            "## [Unreleased]\n\n"
            "### Added\n\n"
            "- regression fixture\n\n"
            "## [0.1.0] - 2026-01-01\n"
        )

        first = close(path, "0.2.0", "2026-08-11")
        assert first.returncode == 0, first.stderr
        text = path.read_text()
        assert count_header(text, "0.2.0") == 1
        assert "- regression fixture" in text

        snapshot = path.read_bytes()
        second = close(path, "0.2.0", "2099-01-01")
        assert second.returncode == 0, second.stderr
        assert path.read_bytes() == snapshot
        assert "already closed; leaving unchanged" in second.stderr

    print("close-changelog regression tests: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
