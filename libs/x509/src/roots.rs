// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Embedded trust anchors (see `roots/README.md` for subjects and fingerprints).

/// DER root certificates trusted by default.
pub const DEFAULT: &[&[u8]] = &[
    include_bytes!("../roots/isrg-root-x1.der"),
    include_bytes!("../roots/isrg-root-x2.der"),
    include_bytes!("../roots/gts-root-r1.der"),
    include_bytes!("../roots/gts-root-r2.der"),
    include_bytes!("../roots/gts-root-r3.der"),
    include_bytes!("../roots/gts-root-r4.der"),
    include_bytes!("../roots/digicert-global-root-ca.der"),
    include_bytes!("../roots/digicert-global-root-g2.der"),
    include_bytes!("../roots/digicert-global-root-g3.der"),
    include_bytes!("../roots/amazon-root-ca-1.der"),
    include_bytes!("../roots/amazon-root-ca-2.der"),
    include_bytes!("../roots/amazon-root-ca-3.der"),
    include_bytes!("../roots/amazon-root-ca-4.der"),
    include_bytes!("../roots/usertrust-rsa.der"),
    include_bytes!("../roots/usertrust-ecc.der"),
    include_bytes!("../roots/globalsign-root-r6.der"),
    include_bytes!("../roots/globalsign-ecc-root-r5.der"),
];
