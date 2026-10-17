#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
"""Host reproductions; extracts the reviewed functions without changing them.

Usage: python3 probe.py /path/to/525b550 /var/tmp/aricode-review-probes
Expected on 525b550: three regression assertions fail (not compiler errors).
Only device/network/filesystem boundaries are simulated. No production files
are modified. Generated Rust stays in the output directory for inspection.
"""
from pathlib import Path
import json
import subprocess
import sys

root, out = map(lambda s: Path(s).resolve(), sys.argv[1:3])
(out / "src").mkdir(parents=True, exist_ok=True)

def function(file, name, indent=""):
    text = (root / file).read_text()
    start = text.index(f"{indent}fn {name}(")
    end = text.index(f"\n{indent}}}", start) + len(indent) + 2
    return text[start:end]

deps = {n: root / "libs" / p for n, p in [
    ("aricode-http", "http"), ("aricode-update-index", "update-index"),
    ("aricode-usb-storage", "usb-storage"), ("aricode-ehci", "ehci")
]}
(out / "Cargo.toml").write_text(
    '[package]\nname="audit-525b550-probes"\nversion="0.0.0"\nedition="2021"\n'
    '[workspace]\n[dependencies]\n' + ''.join(
        f'{name} = {{ path = {json.dumps(str(path))} }}\n' for name, path in deps.items()))

installer = (Path(__file__).with_name("installer.rs.in")).read_text()
for name in ["file_size", "read_all", "write_file", "read_state", "write_state_section"]:
    installer = installer.replace(f"@@{name}@@", function("apps/installer/src/main.rs", name))
ehci = (Path(__file__).with_name("ehci.rs.in")).read_text()
ehci = ehci.replace("@@scsi@@", function("services/usb-ehci/src/main.rs", "scsi", "    "))
http = Path(__file__).with_name("http.rs.in").read_text()
(out / "src/lib.rs").write_text('#![allow(dead_code)]\nextern crate alloc;\n' + installer + ehci + http)
result = subprocess.run(["cargo", "test", "--manifest-path", str(out / "Cargo.toml"),
                         "--", "--nocapture"], cwd=root)
sys.exit(result.returncode)
