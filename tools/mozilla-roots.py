#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
"""Extracts the TLS server trust anchors from Mozilla's certdata.txt (NSS,
MPL-2.0) into libs/x509/roots/ (ADR 0067):

- mozilla.der: the root certificates, concatenated DER (each one is
  self-delimiting);
- mozilla.txt: one line per root, its SHA-256 fingerprint and label, for
  review and diffs.

A root is taken when its trust object says CKT_NSS_TRUSTED_DELEGATOR for
server authentication. Left out, since a plain trust anchor cannot
express what Mozilla applies to them:
- roots with a server distrust-after date (Mozilla distrusts what they
  issued after it);
- roots with name constraints Mozilla applies in NSS's code rather than in
  the certificate (CCADB's "Mozilla Applied Constraints", for example
  TUBITAK's root limited to `*.tr`). certdata.txt does not carry them, so
  CCADB's report of included roots is required (audit of ADR 0067, M2).

    tools/mozilla-roots.py CERTDATA_TXT SOURCE_DESCRIPTION CCADB_CSV

CCADB_CSV: https://ccadb.my.salesforce-sites.com/mozilla/IncludedCACertificateReportPEMCSV
"""
import csv
import hashlib
import pathlib
import re
import sys


def objects(text):
    """Yields each object as a dict of attribute -> (type, value)."""
    obj, lines = {}, iter(text.splitlines())
    for line in lines:
        if not line.strip() or line.startswith("#"):
            if obj:
                yield obj
                obj = {}
            continue
        if line.startswith("BEGINDATA"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 2:
            continue
        name, kind = parts[0], parts[1]
        if kind == "MULTILINE_OCTAL":
            data = bytearray()
            for l2 in lines:
                if l2.strip() == "END":
                    break
                data += bytes(int(o, 8) for o in re.findall(r"\\(\d{3})", l2))
            obj[name] = (kind, bytes(data))
        else:
            obj[name] = (kind, parts[2] if len(parts) > 2 else "")
    if obj:
        yield obj


def applied_constraints(path):
    """SHA-256 fingerprints (lowercase hex) of roots CCADB lists with
    Mozilla-applied constraints."""
    out = set()
    with open(path, encoding="utf-8") as f:
        for row in csv.DictReader(f):
            c = row.get("Mozilla Applied Constraints", "").strip()
            if c and c != "No name constraints are to be applied":
                out.add(row["SHA-256 Fingerprint"].replace(":", "").lower())
    if not out:
        sys.exit("no Mozilla-applied constraints found: is this CCADB's included-roots report?")
    return out


def main():
    text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
    source = sys.argv[2]
    constrained = applied_constraints(sys.argv[3])
    certs, trusted = {}, set()
    for o in objects(text):
        cls = o.get("CKA_CLASS", ("", ""))[1]
        key = (o.get("CKA_ISSUER", ("", b""))[1], o.get("CKA_SERIAL_NUMBER", ("", b""))[1])
        if cls == "CKO_CERTIFICATE":
            distrust = o.get("CKA_NSS_SERVER_DISTRUST_AFTER", ("", ""))
            certs[key] = (o["CKA_LABEL"][1].strip('"'), o["CKA_VALUE"][1], distrust[0] == "MULTILINE_OCTAL")
        elif cls == "CKO_NSS_TRUST" and o.get("CKA_TRUST_SERVER_AUTH", ("", ""))[1] == "CKT_NSS_TRUSTED_DELEGATOR":
            trusted.add(key)
    def left_out(der, distrusted):
        return distrusted or hashlib.sha256(der).hexdigest() in constrained

    chosen = sorted(
        (label, der) for k, (label, der, d) in certs.items() if k in trusted and not left_out(der, d)
    )
    skipped = sorted(label for k, (label, der, d) in certs.items() if k in trusted and left_out(der, d))
    out = pathlib.Path(__file__).resolve().parent.parent / "libs/x509/roots"
    (out / "mozilla.der").write_bytes(b"".join(der for _, der in chosen))
    listing = [
        "# TLS server trust anchors extracted by tools/mozilla-roots.py from",
        f"# {source}",
        "# (Mozilla NSS certdata.txt, Mozilla Public License 2.0).",
        f"# {len(chosen)} roots; left out for a server distrust-after date or Mozilla-applied",
        f"# name constraints (CCADB): {', '.join(skipped) or 'none'}.",
        "# SHA-256 of the certificate, then its label.",
    ]
    listing += [f"{hashlib.sha256(der).hexdigest()}  {label}" for label, der in chosen]
    (out / "mozilla.txt").write_text("\n".join(listing) + "\n", encoding="utf-8")
    print(f"{len(chosen)} roots, {len(skipped)} left out")


if __name__ == "__main__":
    main()
