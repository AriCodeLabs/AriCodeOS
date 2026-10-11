# Embedded trust anchors

DER copies of the root certificates in `aricode_x509::roots::DEFAULT`, taken from
Debian's ca-certificates (`/etc/ssl/certs/*.pem`, converted with
`openssl x509 -outform der`). Baltimore CyberTrust Root is left out because it
expired on 2025-05-12. To check a file: `openssl x509 -inform der -in <file>
-noout -fingerprint -sha256`.

| File | Subject CN | Key | Expires (UTC) | SHA-256 fingerprint |
|------|------------|-----|---------------|---------------------|
| `amazon-root-ca-1.der` | Amazon Root CA 1 | RSA 2048 | 2038-01-17 | `8E:CD:E6:88:4F:3D:87:B1:12:5B:A3:1A:C3:FC:B1:3D:70:16:DE:7F:57:CC:90:4F:E1:CB:97:C6:AE:98:19:6E` |
| `amazon-root-ca-2.der` | Amazon Root CA 2 | RSA 4096 | 2040-05-26 | `1B:A5:B2:AA:8C:65:40:1A:82:96:01:18:F8:0B:EC:4F:62:30:4D:83:CE:C4:71:3A:19:C3:9C:01:1E:A4:6D:B4` |
| `amazon-root-ca-3.der` | Amazon Root CA 3 | EC P-256 | 2040-05-26 | `18:CE:6C:FE:7B:F1:4E:60:B2:E3:47:B8:DF:E8:68:CB:31:D0:2E:BB:3A:DA:27:15:69:F5:03:43:B4:6D:B3:A4` |
| `amazon-root-ca-4.der` | Amazon Root CA 4 | EC P-384 | 2040-05-26 | `E3:5D:28:41:9E:D0:20:25:CF:A6:90:38:CD:62:39:62:45:8D:A5:C6:95:FB:DE:A3:C2:2B:0B:FB:25:89:70:92` |
| `digicert-global-root-ca.der` | DigiCert Global Root CA | RSA 2048 | 2031-11-10 | `43:48:A0:E9:44:4C:78:CB:26:5E:05:8D:5E:89:44:B4:D8:4F:96:62:BD:26:DB:25:7F:89:34:A4:43:C7:01:61` |
| `digicert-global-root-g2.der` | DigiCert Global Root G2 | RSA 2048 | 2038-01-15 | `CB:3C:CB:B7:60:31:E5:E0:13:8F:8D:D3:9A:23:F9:DE:47:FF:C3:5E:43:C1:14:4C:EA:27:D4:6A:5A:B1:CB:5F` |
| `digicert-global-root-g3.der` | DigiCert Global Root G3 | EC P-384 | 2038-01-15 | `31:AD:66:48:F8:10:41:38:C7:38:F3:9E:A4:32:01:33:39:3E:3A:18:CC:02:29:6E:F9:7C:2A:C9:EF:67:31:D0` |
| `globalsign-ecc-root-r5.der` | GlobalSign (GlobalSign ECC Root CA - R5) | EC P-384 | 2038-01-19 | `17:9F:BC:14:8A:3D:D0:0F:D2:4E:A1:34:58:CC:43:BF:A7:F5:9C:81:82:D7:83:A5:13:F6:EB:EC:10:0C:89:24` |
| `globalsign-root-r6.der` | GlobalSign (GlobalSign Root CA - R6) | RSA 4096 | 2034-12-10 | `2C:AB:EA:FE:37:D0:6C:A2:2A:BA:73:91:C0:03:3D:25:98:29:52:C4:53:64:73:49:76:3A:3A:B5:AD:6C:CF:69` |
| `gts-root-r1.der` | GTS Root R1 | RSA 4096 | 2036-06-22 | `D9:47:43:2A:BD:E7:B7:FA:90:FC:2E:6B:59:10:1B:12:80:E0:E1:C7:E4:E4:0F:A3:C6:88:7F:FF:57:A7:F4:CF` |
| `gts-root-r2.der` | GTS Root R2 | RSA 4096 | 2036-06-22 | `8D:25:CD:97:22:9D:BF:70:35:6B:DA:4E:B3:CC:73:40:31:E2:4C:F0:0F:AF:CF:D3:2D:C7:6E:B5:84:1C:7E:A8` |
| `gts-root-r3.der` | GTS Root R3 | EC P-384 | 2036-06-22 | `34:D8:A7:3E:E2:08:D9:BC:DB:0D:95:65:20:93:4B:4E:40:E6:94:82:59:6E:8B:6F:73:C8:42:6B:01:0A:6F:48` |
| `gts-root-r4.der` | GTS Root R4 | EC P-384 | 2036-06-22 | `34:9D:FA:40:58:C5:E2:63:12:3B:39:8A:E7:95:57:3C:4E:13:13:C8:3F:E6:8F:93:55:6C:D5:E8:03:1B:3C:7D` |
| `isrg-root-x1.der` | ISRG Root X1 | RSA 4096 | 2035-06-04 | `96:BC:EC:06:26:49:76:F3:74:60:77:9A:CF:28:C5:A7:CF:E8:A3:C0:AA:E1:1A:8F:FC:EE:05:C0:BD:DF:08:C6` |
| `isrg-root-x2.der` | ISRG Root X2 | EC P-384 | 2040-09-17 | `69:72:9B:8E:15:A8:6E:FC:17:7A:57:AF:B7:17:1D:FC:64:AD:D2:8C:2F:CA:8C:F1:50:7E:34:45:3C:CB:14:70` |
| `usertrust-ecc.der` | USERTrust ECC Certification Authority | EC P-384 | 2038-01-18 | `4F:F4:60:D5:4B:9C:86:DA:BF:BC:FC:57:12:E0:40:0D:2B:ED:3F:BC:4D:4F:BD:AA:86:E0:6A:DC:D2:A9:AD:7A` |
| `usertrust-rsa.der` | USERTrust RSA Certification Authority | RSA 4096 | 2038-01-18 | `E7:93:C9:B0:2F:D8:AA:13:E2:1C:31:22:8A:CC:B0:81:19:64:3B:74:9C:89:89:64:B1:74:6D:46:C3:D4:CB:D2` |
