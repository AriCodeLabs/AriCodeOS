#!/bin/sh
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# Generates rsa_vectors.txt (RSA verification vectors for src/rsa.rs) with
# OpenSSL: sh gen_rsa_vectors.sh > rsa_vectors.txt (run in a temp dir; it
# writes the keys to the current directory). One line per signature:
# <bits> <scheme> <hash> <n hex> <e hex> <message hex> <signature hex>
set -e
key() { # name bits exp
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:$2 -pkeyopt rsa_keygen_pubexp:$3 -out $1.pem 2>/dev/null
}
key k2048 2048 65537; key k4096 4096 65537
# A 1537-bit modulus exercises the PSS case emLen = k - 1 (OpenSSL rounds
# odd sizes of 2048 bits and more down to a multiple of 8).
key k1537 1537 65537
key k3072e3 3072 3
printf 'The quick brown fox jumps over the lazy dog' > msg
MSG=$(od -An -v -tx1 msg | tr -d ' \n')
for k in k2048 k4096 k1537 k3072e3; do
  N=$(openssl rsa -in $k.pem -noout -modulus 2>/dev/null | cut -d= -f2 | tr A-F a-f)
  E=$(openssl rsa -in $k.pem -noout -text 2>/dev/null | sed -n 's/^publicExponent: [0-9]* (0x\([0-9a-f]*\))/\1/p')
  # Whole bytes: pad odd-length hex with a leading zero.
  [ $(( ${#N} % 2 )) -eq 1 ] && N=0$N
  [ $(( ${#E} % 2 )) -eq 1 ] && E=0$E
  BITS=$(openssl rsa -in $k.pem -noout -text 2>/dev/null | sed -n 's/.*Private-Key: (\([0-9]*\) bit.*/\1/p')
  for h in sha256 sha384 sha512; do
    S1=$(openssl dgst -$h -sign $k.pem msg | od -An -v -tx1 | tr -d ' \n')
    echo "$BITS pkcs1 $h $N $E $MSG $S1"
    S2=$(openssl dgst -$h -sign $k.pem -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:digest msg | od -An -v -tx1 | tr -d ' \n')
    echo "$BITS pss $h $N $E $MSG $S2"
  done
done
