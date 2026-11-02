# Embedded trust anchors

`mozilla.der` holds the root certificates of Mozilla's store (NSS
`certdata.txt`) trusted for TLS server authentication, concatenated DER, and
`mozilla.txt` lists each one with its SHA-256 fingerprint and label. Both
are produced by `tools/mozilla-roots.py` (ADR 0067); the source revision is
named at the top of `mozilla.txt`.

`certdata.txt` is under the Mozilla Public License 2.0, whose copyleft stays
in these files (ADR 0063), as with the Public Suffix List extract in
`../psl/`.

To refresh: download `lib/ckfw/builtins/certdata.txt` from
https://github.com/nss-dev/nss, run `tools/mozilla-roots.py CERTDATA_TXT
SOURCE_URL`, and review the diff of `mozilla.txt`. To check a root:
`openssl x509 -inform der -noout -fingerprint -sha256`.
