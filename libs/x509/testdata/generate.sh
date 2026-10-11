#!/bin/sh
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

# Ed25519 root -> Ed25519 leaf without subjectAltName (common name fallback).
key ed-root -algorithm ED25519
root ed-root '/O=AriCode Test/CN=AriCode Test Ed25519 Root' '' $CA_FROM $CA_TO
key ed-leaf -algorithm ED25519
printf 'basicConstraints=CA:FALSE\n' >ed-leaf.ext
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
