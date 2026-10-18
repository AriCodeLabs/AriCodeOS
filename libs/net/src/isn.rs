// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! TCP secrets (ADR 0034, incident 0014): initial sequence numbers as RFC
//! 6528 prescribes and ephemeral ports as RFC 6056 section 3.3.3. Both are
//! keyed hashes over the connection's addresses with a [`Secret`] the
//! service draws once per boot from the kernel's entropy, so an off-path
//! host cannot predict either from public inputs (uptime, the peer's own
//! sequence number, the order of the connections). Pure functions: the
//! service supplies the secret and the clock.

use crate::Ipv4Addr;
use aricode_crypto::hmac::hmac_sha256;

/// Length of the per-boot secret (RFC 6528 asks for at least 128 bits).
pub const SECRET_LEN: usize = 32;
pub type Secret = [u8; SECRET_LEN];

/// RFC 6528 section 3: the sequence number clock advances every 4 µs;
/// the service's clock counts milliseconds.
const TICKS_PER_MS: u64 = 250;

/// Ephemeral port range of IANA and RFC 6056 section 3.2.
pub const EPHEMERAL_FIRST: u16 = 49152;
pub const EPHEMERAL_LAST: u16 = 65535;
const EPHEMERAL_COUNT: u32 = (EPHEMERAL_LAST - EPHEMERAL_FIRST) as u32 + 1;

/// Domain separation between the two uses of the same secret.
const TAG_ISN: u8 = 1;
const TAG_PORT: u8 = 2;

/// First 32 bits of HMAC-SHA-256 over `tag || fields`.
fn keyed(secret: &Secret, tag: u8, fields: &[u8]) -> u32 {
    let mac = hmac_sha256(secret, &[&[tag], fields]);
    u32::from_be_bytes([mac[0], mac[1], mac[2], mac[3]])
}

/// Initial sequence number for a connection: `M + F(local, remote, secret)`
/// (RFC 6528 section 3), where `M` is `now_ms` in 4 µs ticks and `F` the
/// keyed hash of the 4-tuple. Used for the active SYN and for the SYN|ACK
/// of a passive open alike; the peer's number plays no part.
pub fn initial_seq(
    secret: &Secret,
    local: Ipv4Addr,
    local_port: u16,
    remote: Ipv4Addr,
    remote_port: u16,
    now_ms: u64,
) -> u32 {
    let mut tuple = [0u8; 12];
    tuple[0..4].copy_from_slice(&local.0);
    tuple[4..6].copy_from_slice(&local_port.to_be_bytes());
    tuple[6..10].copy_from_slice(&remote.0);
    tuple[10..12].copy_from_slice(&remote_port.to_be_bytes());
    let m = now_ms.wrapping_mul(TICKS_PER_MS) as u32;
    m.wrapping_add(keyed(secret, TAG_ISN, &tuple))
}

/// Ephemeral port for the `count`-th connection of this boot (RFC 6056
/// section 3.3.3): a per-destination offset from the keyed hash plus the
/// counter, inside `[EPHEMERAL_FIRST, EPHEMERAL_LAST]`. Consecutive counts
/// walk consecutive ports towards one destination, so the caller skips a
/// port in use by trying the next count; destinations get unrelated
/// walks, and every boot starts somewhere else.
pub fn ephemeral_port(
    secret: &Secret,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    remote_port: u16,
    count: u32,
) -> u16 {
    let mut fields = [0u8; 10];
    fields[0..4].copy_from_slice(&local.0);
    fields[4..8].copy_from_slice(&remote.0);
    fields[8..10].copy_from_slice(&remote_port.to_be_bytes());
    let offset = keyed(secret, TAG_PORT, &fields);
    EPHEMERAL_FIRST + (offset.wrapping_add(count) % EPHEMERAL_COUNT) as u16
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::collections::BTreeSet;

    const LOCAL: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const REMOTE: Ipv4Addr = Ipv4Addr::new(140, 82, 121, 4);

    fn secret(n: u8) -> Secret {
        let mut s = [n; SECRET_LEN];
        s[0] = n.wrapping_mul(7);
        s
    }

    /// `a` and `b` are at least 2^16 apart on the sequence circle.
    fn far_apart(a: u32, b: u32) -> bool {
        a.wrapping_sub(b) > 1 << 16 && b.wrapping_sub(a) > 1 << 16
    }

    #[test]
    fn isn_differs_across_secrets_for_the_same_tuple() {
        let isns: BTreeSet<u32> = (0..64u8)
            .map(|n| initial_seq(&secret(n), LOCAL, 50000, REMOTE, 443, 1000))
            .collect();
        assert_eq!(isns.len(), 64, "every secret gives another ISN");
        // Not a function of the clock alone: nothing near `uptime * 64`.
        assert!(isns.iter().all(|&i| far_apart(i, 1000 * 64)));
    }

    #[test]
    fn isn_differs_across_tuples_for_the_same_secret() {
        let s = secret(3);
        let base = initial_seq(&s, LOCAL, 50000, REMOTE, 443, 1000);
        let variants = [
            initial_seq(&s, LOCAL, 50001, REMOTE, 443, 1000),
            initial_seq(&s, LOCAL, 50000, REMOTE, 22, 1000),
            initial_seq(&s, LOCAL, 50000, Ipv4Addr::new(140, 82, 121, 5), 443, 1000),
            initial_seq(&s, Ipv4Addr::new(10, 0, 2, 16), 50000, REMOTE, 443, 1000),
        ];
        for v in variants {
            assert!(
                far_apart(v, base),
                "neighbouring tuples are not neighbouring ISNs"
            );
        }
    }

    #[test]
    fn isn_follows_the_4us_clock_for_the_same_tuple_and_secret() {
        let s = secret(9);
        let at = |ms| initial_seq(&s, LOCAL, 50000, REMOTE, 443, ms);
        assert_eq!(
            at(1001).wrapping_sub(at(1000)),
            250,
            "1 ms = 250 ticks of 4 µs"
        );
        assert_eq!(at(61_000).wrapping_sub(at(1000)), 60_000 * 250);
        // Wraps like a 32-bit clock (period 2^31 ms: gcd(250, 2^32) = 2),
        // never panics.
        let _ = at(u64::MAX);
        assert_eq!(at(1000 + (1 << 31)), at(1000));
        assert_ne!(at(1000 + (1 << 30)), at(1000));
    }

    #[test]
    fn ephemeral_ports_stay_in_range_walk_by_one_and_start_per_secret() {
        let mut starts = BTreeSet::new();
        for n in 0..32u8 {
            let s = secret(n);
            starts.insert(ephemeral_port(&s, LOCAL, REMOTE, 443, 0));
            for count in 0..EPHEMERAL_COUNT + 5 {
                let p = ephemeral_port(&s, LOCAL, REMOTE, 443, count);
                assert!((EPHEMERAL_FIRST..=EPHEMERAL_LAST).contains(&p));
                let next = ephemeral_port(&s, LOCAL, REMOTE, 443, count + 1);
                let expected = if p == EPHEMERAL_LAST {
                    EPHEMERAL_FIRST
                } else {
                    p + 1
                };
                assert_eq!(next, expected, "consecutive counts, consecutive ports");
            }
        }
        assert!(
            starts.len() >= 30,
            "boots start at unrelated ports: {starts:?}"
        );
        assert!(
            !starts.contains(&50000),
            "never the fixed start of incident 0014"
        );
    }

    #[test]
    fn ephemeral_ports_differ_per_destination() {
        let s = secret(5);
        let a = ephemeral_port(&s, LOCAL, REMOTE, 443, 0);
        let b = ephemeral_port(&s, LOCAL, REMOTE, 22, 0);
        let c = ephemeral_port(&s, LOCAL, Ipv4Addr::new(10, 0, 2, 2), 443, 0);
        assert!(a != b && a != c && b != c);
    }
}
