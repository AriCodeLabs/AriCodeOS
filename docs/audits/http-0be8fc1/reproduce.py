#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
"""Run the HTTP review regressions; failures are expected at 0be8fc1.

Builds and tests on the host only. Replaces NetClient with a simulated transport
and loads the original HTTP module, without modifying production files.
"""

from pathlib import Path
import os
import shutil
import subprocess


def main():
    root = Path(__file__).resolve().parents[3]
    evidence = root / "build/audit/0be8fc1"
    project = evidence / "probes"
    (project / "src").mkdir(parents=True, exist_ok=True)
    cargo = os.environ.get("CARGO") or shutil.which("cargo")
    if not cargo:
        raise SystemExit("cargo is not available")

    manifest = [
        '[package]',
        'name = "http-review-probes"',
        'version = "0.0.0"',
        'edition = "2021"',
        '[workspace]',
        '[dependencies]',
    ]
    for name, relative in [
        ("aricode-http", "libs/http"),
        ("aricode-net", "libs/net"),
        ("aricode-proto", "sdk/proto"),
    ]:
        manifest.append(f'{name} = {{ path = "{root / relative}" }}')
    (project / "Cargo.toml").write_text("\n".join(manifest) + "\n")
    source = Path(__file__).with_name("probes.rs").read_text()
    (project / "src/lib.rs").write_text(
        source.replace("__ARICODEOS_ROOT__", str(root))
    )

    failed = False
    for profile in ["debug", "release"]:
        command = [cargo, "test", "--manifest-path", str(project / "Cargo.toml")]
        if profile == "release":
            command.append("--release")
        log = evidence / f"probes-{profile}.log"
        with log.open("w") as output:
            result = subprocess.run(
                command, cwd=root, stdout=output, stderr=subprocess.STDOUT
            )
        print(f"{profile}: exit code {result.returncode}; {log}")
        failed |= result.returncode != 0
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
