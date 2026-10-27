#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
# Regenerates the test certificates of aricode-x509 with the host's OpenSSL.
# Private keys stay in a temporary directory; only the DER certificates are
# written next to this script. Dates are fixed so the tests can pick `now`.
set -eu
out=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

CA_FROM=20250101000000Z
CA_TO=20350101000000Z
LEAF_FROM=20260101000000Z
LEAF_TO=20270101000000Z

printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n' >ca.ext
printf 'basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign\n' >ca0.ext

key() { # name algorithm-options...
    name=$1
    shift
    openssl genpkey -quiet "$@" -out "$name.key"
}

# self-signed root: name subject digest-option from to
root() {
    openssl req -x509 -new -key "$1.key" -subj "$2" $3 -not_before "$4" -not_after "$5" \
        -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign' \
        -out "$1.pem"
}

# issued certificate: name subject issuer extfile from to [extra signing options]
issue() {
    name=$1 subj=$2 issuer=$3 ext=$4 from=$5 to=$6
    shift 6
    openssl req -new -key "$name.key" -subj "$subj" -out "$name.csr"
    openssl x509 -req -in "$name.csr" -CA "$issuer.pem" -CAkey "$issuer.key" -set_serial "0x$(openssl rand -hex 8)" \
        -extfile "$ext" -not_before "$from" -not_after "$to" "$@" -out "$name.pem" 2>/dev/null
}

der() {
    for n in "$@"; do openssl x509 -in "$n.pem" -outform der -out "$out/$n.der"; done
}

# RSA: root (SHA-256, notAfter in 2055 = GeneralizedTime) -> intermediate (SHA-384) -> leaves.
key rsa-root -algorithm RSA -pkeyopt rsa_keygen_bits:2048
root rsa-root '/O=AriCode Test/CN=AriCode Test RSA Root' -sha256 $CA_FROM 20550101000000Z
key rsa-int -algorithm RSA -pkeyopt rsa_keygen_bits:2048
issue rsa-int '/O=AriCode Test/CN=AriCode Test RSA Intermediate' rsa-root ca.ext $CA_FROM $CA_TO -sha384
key rsa-leaf -algorithm RSA -pkeyopt rsa_keygen_bits:2048
printf 'basicConstraints=CA:FALSE\nsubjectAltName=DNS:*.example.com,DNS:example.com\nextendedKeyUsage=serverAuth\n' >rsa-leaf.ext
issue rsa-leaf '/CN=example.com' rsa-int rsa-leaf.ext $LEAF_FROM $LEAF_TO -sha512
key rsa-pss-leaf -algorithm RSA -pkeyopt rsa_keygen_bits:2048
printf 'subjectAltName=DNS:pss.example.com\n' >pss.ext
issue rsa-pss-leaf '/CN=pss.example.com' rsa-int pss.ext $LEAF_FROM $LEAF_TO -sha256 \
    -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:digest
der rsa-root rsa-int rsa-leaf rsa-pss-leaf

# EC: P-384 root -> P-384 intermediate -> P-256 leaf signed with SHA-256.
key ec-root -algorithm EC -pkeyopt ec_paramgen_curve:P-384
root ec-root '/O=AriCode Test/CN=AriCode Test EC Root' -sha384 $CA_FROM $CA_TO
key ec-int -algorithm EC -pkeyopt ec_paramgen_curve:P-384
issue ec-int '/O=AriCode Test/CN=AriCode Test EC Intermediate' ec-root ca.ext $CA_FROM $CA_TO -sha384
key ec-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'subjectAltName=DNS:www.example.org\n' >ec-leaf.ext
issue ec-leaf '/CN=www.example.org' ec-int ec-leaf.ext $LEAF_FROM $LEAF_TO -sha256
der ec-root ec-int ec-leaf

# A leaf that names its host only in the subject CN (incident 0048).
key cn-only-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'basicConstraints=CA:FALSE\n' >cn-only-leaf.ext
issue cn-only-leaf '/CN=www.example.org' ec-int cn-only-leaf.ext $LEAF_FROM $LEAF_TO -sha256
der cn-only-leaf

# Ed25519 root -> Ed25519 leaf without subjectAltName (common name fallback).
key ed-root -algorithm ED25519
root ed-root '/O=AriCode Test/CN=AriCode Test Ed25519 Root' '' $CA_FROM $CA_TO
key ed-leaf -algorithm ED25519
printf 'basicConstraints=CA:FALSE\nsubjectAltName=DNS:ed.example.net\n' >ed-leaf.ext
issue ed-leaf '/CN=ed.example.net' ed-root ed-leaf.ext $LEAF_FROM $LEAF_TO
der ed-root ed-leaf

# A certificate issued by the RSA leaf, which is not a CA.
key notca-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'subjectAltName=DNS:sub.example.com\n' >notca.ext
issue notca-leaf '/CN=sub.example.com' rsa-leaf notca.ext $LEAF_FROM $LEAF_TO -sha256
der notca-leaf

# pathlen:0 intermediate that issues another CA.
key plen-int0 -algorithm EC -pkeyopt ec_paramgen_curve:P-256
issue plen-int0 '/CN=AriCode Test Pathlen 0' rsa-root ca0.ext $CA_FROM $CA_TO -sha256
key plen-int1 -algorithm EC -pkeyopt ec_paramgen_curve:P-256
issue plen-int1 '/CN=AriCode Test Pathlen Child' plen-int0 ca.ext $CA_FROM $CA_TO -sha256
key plen-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'subjectAltName=DNS:deep.example.com\n' >plen.ext
issue plen-leaf '/CN=deep.example.com' plen-int1 plen.ext $LEAF_FROM $LEAF_TO -sha256
der plen-int0 plen-int1 plen-leaf

# Seven intermediates below the EC root (one more than the chain limit).
parent=ec-root
for i in 1 2 3 4 5 6 7; do
    key long-int$i -algorithm EC -pkeyopt ec_paramgen_curve:P-256
    issue long-int$i "/CN=AriCode Test Long $i" $parent ca.ext $CA_FROM $CA_TO -sha256
    der long-int$i
    parent=long-int$i
done
key long-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'subjectAltName=DNS:long.example.org\n' >long.ext
issue long-leaf '/CN=long.example.org' $parent long.ext $LEAF_FROM $LEAF_TO -sha256
der long-leaf

# Purpose restrictions under the EC intermediate (RFC 5280 EKU, RFC 8446
# digitalSignature): a control with both, one for clients only, one whose
# key may not sign.
for n in purpose-ok purpose-client purpose-nosign; do
    key $n -algorithm EC -pkeyopt ec_paramgen_curve:P-256
done
printf 'basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:purpose.example.org\nextendedKeyUsage=critical,serverAuth\nkeyUsage=critical,digitalSignature\n' >purpose-ok.ext
printf 'basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:purpose.example.org\nextendedKeyUsage=critical,clientAuth\nkeyUsage=critical,digitalSignature\n' >purpose-client.ext
printf 'basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:purpose.example.org\nextendedKeyUsage=critical,serverAuth\nkeyUsage=critical,keyEncipherment\n' >purpose-nosign.ext
for n in purpose-ok purpose-client purpose-nosign; do
    issue $n '/CN=purpose.example.org' ec-int $n.ext $LEAF_FROM $LEAF_TO -sha256
    der $n
done

# Leaf with an unknown critical extension.
key crit-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'subjectAltName=DNS:crit.example.org\n1.3.6.1.4.1.55555.1=critical,ASN1:NULL\n' >crit.ext
issue crit-leaf '/CN=crit.example.org' ec-int crit.ext $LEAF_FROM $LEAF_TO -sha256
der crit-leaf

# Constraints on intermediates (audit 2026-10-25 D, H1 and H2), under a
# root of their own. nameConstraints limited to good.example, non-critical
# and critical; with an excluded subtree; an intermediate whose
# extendedKeyUsage is e-mail only, and one with serverAuth.
key nc-root -algorithm EC -pkeyopt ec_paramgen_curve:P-256
root nc-root '/O=AriCode Test/CN=AriCode Test Constraints Root' -sha256 $CA_FROM $CA_TO
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nnameConstraints=permitted;DNS:good.example\n' >nc-int.ext
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nnameConstraints=critical,permitted;DNS:good.example\n' >nc-crit-int.ext
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nnameConstraints=critical,permitted;DNS:good.example,excluded;DNS:bad.good.example\n' >nc-excl-int.ext
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nextendedKeyUsage=emailProtection\n' >eku-mail-int.ext
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nextendedKeyUsage=serverAuth,clientAuth\n' >eku-tls-int.ext
for n in nc-int nc-crit-int nc-excl-int eku-mail-int eku-tls-int; do
    key $n -algorithm EC -pkeyopt ec_paramgen_curve:P-256
    issue $n "/O=AriCode Test/CN=AriCode Test $n" nc-root $n.ext $CA_FROM $CA_TO -sha256
    der $n
done
# leaf name issuer: SAN given as "DNS:a,DNS:b" or "-" for none (CN only)
leaf() {
    key $1 -algorithm EC -pkeyopt ec_paramgen_curve:P-256
    if [ "$2" = - ]; then
        printf 'basicConstraints=CA:FALSE\n' >$1.ext
    else
        printf 'basicConstraints=CA:FALSE\nsubjectAltName=%s\n' "$2" >$1.ext
    fi
    issue $1 "/CN=$4" $3 $1.ext $LEAF_FROM $LEAF_TO -sha256
    der $1
}
leaf nc-ok DNS:www.good.example nc-int www.good.example
leaf nc-evil DNS:www.evil.example nc-int www.evil.example
leaf nc-mixed DNS:www.good.example,DNS:www.evil.example nc-int www.good.example
leaf nc-wild DNS:*.good.example nc-int www.good.example
leaf nc-cn - nc-int www.good.example
leaf nc-crit-ok DNS:www.good.example nc-crit-int www.good.example
leaf nc-crit-evil DNS:www.evil.example nc-crit-int www.evil.example
leaf nc-excl-ok DNS:www.good.example nc-excl-int www.good.example
leaf nc-excl-bad DNS:x.bad.good.example nc-excl-int x.bad.good.example
leaf nc-excl-wild DNS:*.good.example nc-excl-int www.good.example
leaf eku-mail-leaf DNS:www.eku.example eku-mail-int www.eku.example
leaf eku-tls-leaf DNS:www.eku.example eku-tls-int www.eku.example
# Restrictions on roots, two constrained CAs in one path, and IP hosts
# (audit of incident 0029, L1 and L4).
constrained_root() { # name subject extension
    key $1 -algorithm EC -pkeyopt ec_paramgen_curve:P-256
    openssl req -x509 -new -key "$1.key" -subj "$2" -sha256 -not_before $CA_FROM -not_after $CA_TO \
        -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign' \
        -addext "$3" -out "$1.pem"
    der $1
}
constrained_root ncr-root '/O=AriCode Test/CN=AriCode Test Constrained Root' 'nameConstraints=permitted;DNS:good.example'
constrained_root ekur-root '/O=AriCode Test/CN=AriCode Test Mail Root' 'extendedKeyUsage=emailProtection'
leaf ncr-ok DNS:www.good.example ncr-root www.good.example
leaf ncr-evil DNS:www.evil.example ncr-root www.evil.example
leaf ekur-leaf DNS:www.eku.example ekur-root www.eku.example
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nnameConstraints=permitted;DNS:sub.good.example\n' >nc2-int.ext
key nc2-int -algorithm EC -pkeyopt ec_paramgen_curve:P-256
issue nc2-int '/O=AriCode Test/CN=AriCode Test nc2-int' nc-int nc2-int.ext $CA_FROM $CA_TO -sha256
der nc2-int
leaf nc2-ok DNS:www.sub.good.example nc2-int www.sub.good.example
leaf nc2-out DNS:www.good.example nc2-int www.good.example
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\nnameConstraints=permitted;IP:192.168.0.0/255.255.0.0\n' >ip-int.ext
key ip-int -algorithm EC -pkeyopt ec_paramgen_curve:P-256
issue ip-int '/O=AriCode Test/CN=AriCode Test ip-int' nc-root ip-int.ext $CA_FROM $CA_TO -sha256
der ip-int
leaf ip-leaf DNS:10.0.2.2,DNS:www.ip.example ip-int 10.0.2.2
# A CA certificate presented as a server's (audit 2026-10-25 D, L2).
key ca-as-leaf -algorithm EC -pkeyopt ec_paramgen_curve:P-256
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,digitalSignature\nsubjectAltName=DNS:www.good.example\n' >ca-as-leaf.ext
issue ca-as-leaf '/CN=www.good.example' nc-root ca-as-leaf.ext $CA_FROM $CA_TO -sha256
der ca-as-leaf nc-root
