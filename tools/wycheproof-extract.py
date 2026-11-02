#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
"""Extracts the Wycheproof vectors `libs/crypto/tests/wycheproof.rs` uses
(ADR 0066) from a checkout of https://github.com/C2SP/wycheproof, keeping
only the fields the test reads: keys, messages, signatures, results and
flags. The other encodings of the same keys (PEM, DER, JWK) and comments
are dropped.

    tools/wycheproof-extract.py WYCHEPROOF_CHECKOUT
"""
import json
import pathlib
import subprocess
import sys

FILES = [
    "aes_gcm_test.json",
    "chacha20_poly1305_test.json",
    "ed25519_test.json",
    "x25519_test.json",
    "ecdsa_secp256r1_sha256_test.json",
    "ecdsa_secp384r1_sha256_test.json",
    "ecdsa_secp384r1_sha384_test.json",
    "rsa_signature_2048_sha256_test.json",
    "rsa_signature_2048_sha384_test.json",
    "rsa_signature_2048_sha512_test.json",
    "rsa_signature_3072_sha256_test.json",
    "rsa_signature_4096_sha256_test.json",
    "rsa_pss_2048_sha256_mgf1_32_test.json",
    "rsa_pss_2048_sha384_mgf1_48_test.json",
    "rsa_pss_4096_sha512_mgf1_64_test.json",
    "hmac_sha256_test.json",
    "hmac_sha384_test.json",
    "hkdf_sha256_test.json",
    "hkdf_sha384_test.json",
]
GROUP_KEYS = {"sha", "mgfSha", "sLen", "keySize", "ivSize", "tagSize"}
KEY_KEYS = {"pk", "uncompressed", "modulus", "publicExponent"}
TEST_KEYS = {
    "tcId", "flags", "result", "msg", "sig", "key", "iv", "aad", "ct",
    "tag", "public", "private", "shared", "ikm", "salt", "info", "size", "okm",
}


def strip(doc):
    groups = []
    for g in doc["testGroups"]:
        out = {k: v for k, v in g.items() if k in GROUP_KEYS}
        if isinstance(g.get("publicKey"), dict):
            out["publicKey"] = {k: v for k, v in g["publicKey"].items() if k in KEY_KEYS}
        out["tests"] = [{k: v for k, v in t.items() if k in TEST_KEYS} for t in g["tests"]]
        groups.append(out)
    return {"algorithm": doc["algorithm"], "numberOfTests": doc["numberOfTests"], "testGroups": groups}


def main():
    src = pathlib.Path(sys.argv[1])
    dst = pathlib.Path(__file__).resolve().parent.parent / "libs/crypto/tests/wycheproof"
    commit = subprocess.run(
        ["git", "-C", str(src), "rev-parse", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()
    for name in FILES:
        doc = json.loads((src / "testvectors_v1" / name).read_text())
        (dst / name).write_text(json.dumps(strip(doc), separators=(",", ":")) + "\n")
    (dst / "COMMIT").write_text(commit + "\n")
    print(f"{len(FILES)} files from {commit}")


if __name__ == "__main__":
    main()
