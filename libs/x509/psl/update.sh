#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
# Rebuilds icann-wildcard-bases.dat from the Public Suffix List (ADR 0062):
# the rules of its ICANN section that can name a wildcard's base, i.e. with
# at least two labels, plus every wildcard and exception rule; labels in
# punycode, as dNSNames carry them. Usage: update.sh [public_suffix_list.dat]
# (downloaded from publicsuffix.org when no file is given).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "$HOME/.aricode-psl.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
if [ $# -ge 1 ]; then
    cp "$1" "$tmp/list.dat"
else
    curl -sSfL -o "$tmp/list.dat" https://publicsuffix.org/list/public_suffix_list.dat
fi
python3 -I - "$tmp/list.dat" "$here/icann-wildcard-bases.dat" <<'PY'
import hashlib, sys

src, out = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
version = next((l.split(b"VERSION:", 1)[1].strip().decode()
                for l in data.splitlines() if b"VERSION:" in l), "unknown")


def ascii_label(label):
    # The list is lowercase NFC: raw punycode, no IDNA mapping (which
    # would turn the German sharp s into "ss").
    return label if label.isascii() else "xn--" + label.encode("punycode").decode()


rules = []
icann = False
markers = set()
for line in data.decode("utf-8").splitlines():
    line = line.strip()
    if "===BEGIN ICANN DOMAINS===" in line:
        icann = True
        markers.add("begin")
    elif "===END ICANN DOMAINS===" in line:
        icann = False
        markers.add("end")
    if not icann or not line or line.startswith("//"):
        continue
    rule = line.split()[0]
    prefix = "!" if rule.startswith("!") else ""
    body = rule[1:] if prefix else rule
    labels = [l if l == "*" else ascii_label(l) for l in body.split(".")]
    if prefix or labels[0] == "*" or len(labels) >= 2:
        rules.append(prefix + ".".join(labels))

# A captive portal page or a cut download must not replace the extract
# with an empty one (audit of ea368aa, finding 3).
if markers != {"begin", "end"} or len(rules) < 5000:
    sys.exit(f"not a whole Public Suffix List: ICANN markers {sorted(markers)}, {len(rules)} rules")

with open(out, "w", encoding="ascii", newline="\n") as f:
    f.write(
        "// This Source Code Form is subject to the terms of the Mozilla Public\n"
        "// License, v. 2.0. If a copy of the MPL was not distributed with this\n"
        "// file, You can obtain one at https://mozilla.org/MPL/2.0/.\n"
        "// SPDX-FileCopyrightText: Mozilla Foundation and the Public Suffix List contributors\n"
        "// SPDX-License-Identifier: MPL-2.0\n"
        "//\n"
        "// Extract of the Public Suffix List (https://publicsuffix.org/), ICANN\n"
        "// section: rules of two labels or more, wildcard and exception rules,\n"
        "// labels in punycode. Made by update.sh; do not edit by hand.\n"
        f"// Source version: {version}\n"
        f"// Source SHA-256: {hashlib.sha256(data).hexdigest()}\n"
        f"// Rules: {len(rules)}\n"
    )
    for r in rules:
        f.write(r + "\n")
print(f"{len(rules)} rules from list version {version}")
PY
