//! Poly1305 one-time authenticator (RFC 8439 section 2.5), with 26-bit
//! limbs in the style of poly1305-donna. The key must never be reused: SSH
//! derives a fresh one per packet from ChaCha20.

pub const KEY_LEN: usize = 32;
pub const TAG_LEN: usize = 16;

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Tag of `msg` under the one-time `key`.
pub fn tag(key: &[u8; KEY_LEN], msg: &[u8]) -> [u8; TAG_LEN] {
    const M: u32 = 0x3ff_ffff;
    let r0 = le32(&key[0..]) & 0x3ff_ffff;
    let r1 = (le32(&key[3..]) >> 2) & 0x3ff_ff03;
    let r2 = (le32(&key[6..]) >> 4) & 0x3ff_c0ff;
    let r3 = (le32(&key[9..]) >> 6) & 0x3f0_3fff;
    let r4 = (le32(&key[12..]) >> 8) & 0x00f_ffff;
    let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
    let (mut h0, mut h1, mut h2, mut h3, mut h4) = (0u32, 0u32, 0u32, 0u32, 0u32);

    for chunk in msg.chunks(16) {
        let mut block = [0u8; 17];
        block[..chunk.len()].copy_from_slice(chunk);
        // A full block carries the 2^128 bit; a short one is padded with 1.
        let hibit = if chunk.len() == 16 {
            1 << 24
        } else {
            block[chunk.len()] = 1;
            0
        };
        h0 += le32(&block[0..]) & M;
        h1 += (le32(&block[3..]) >> 2) & M;
        h2 += (le32(&block[6..]) >> 4) & M;
        h3 += (le32(&block[9..]) >> 6) & M;
        h4 += (le32(&block[12..]) >> 8) | hibit;

        let m = |a: u32, b: u32| a as u64 * b as u64;
        let d0 = m(h0, r0) + m(h1, s4) + m(h2, s3) + m(h3, s2) + m(h4, s1);
        let mut d1 = m(h0, r1) + m(h1, r0) + m(h2, s4) + m(h3, s3) + m(h4, s2);
        let mut d2 = m(h0, r2) + m(h1, r1) + m(h2, r0) + m(h3, s4) + m(h4, s3);
        let mut d3 = m(h0, r3) + m(h1, r2) + m(h2, r1) + m(h3, r0) + m(h4, s4);
        let mut d4 = m(h0, r4) + m(h1, r3) + m(h2, r2) + m(h3, r1) + m(h4, r0);

        let mut c = d0 >> 26;
        h0 = d0 as u32 & M;
        d1 += c;
        c = d1 >> 26;
        h1 = d1 as u32 & M;
        d2 += c;
        c = d2 >> 26;
        h2 = d2 as u32 & M;
        d3 += c;
        c = d3 >> 26;
        h3 = d3 as u32 & M;
        d4 += c;
        c = d4 >> 26;
        h4 = d4 as u32 & M;
        h0 += c as u32 * 5;
        let c = h0 >> 26;
        h0 &= M;
        h1 += c;
    }

    // Full carry, then h mod p by comparing against h + 5 - 2^130.
    let mut c = h1 >> 26;
    h1 &= M;
    h2 += c;
    c = h2 >> 26;
    h2 &= M;
    h3 += c;
    c = h3 >> 26;
    h3 &= M;
    h4 += c;
    c = h4 >> 26;
    h4 &= M;
    h0 += c * 5;
    c = h0 >> 26;
    h0 &= M;
    h1 += c;

    let mut g0 = h0.wrapping_add(5);
    c = g0 >> 26;
    g0 &= M;
    let mut g1 = h1.wrapping_add(c);
    c = g1 >> 26;
    g1 &= M;
    let mut g2 = h2.wrapping_add(c);
    c = g2 >> 26;
    g2 &= M;
    let mut g3 = h3.wrapping_add(c);
    c = g3 >> 26;
    g3 &= M;
    let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);
    // g4 negative (top bit set): h < p, keep h; otherwise take g.
    let take_g = (g4 >> 31).wrapping_sub(1);
    let sel = |h: u32, g: u32| (h & !take_g) | (g & take_g);
    let (h0, h1, h2, h3, h4) = (
        sel(h0, g0),
        sel(h1, g1),
        sel(h2, g2),
        sel(h3, g3),
        sel(h4, g4),
    );

    let w0 = h0 | (h1 << 26);
    let w1 = (h1 >> 6) | (h2 << 20);
    let w2 = (h2 >> 12) | (h3 << 14);
    let w3 = (h3 >> 18) | (h4 << 8);
    let mut out = [0u8; TAG_LEN];
    let mut f = w0 as u64 + le32(&key[16..]) as u64;
    out[0..4].copy_from_slice(&(f as u32).to_le_bytes());
    f = w1 as u64 + le32(&key[20..]) as u64 + (f >> 32);
    out[4..8].copy_from_slice(&(f as u32).to_le_bytes());
    f = w2 as u64 + le32(&key[24..]) as u64 + (f >> 32);
    out[8..12].copy_from_slice(&(f as u32).to_le_bytes());
    f = w3 as u64 + le32(&key[28..]) as u64 + (f >> 32);
    out[12..16].copy_from_slice(&(f as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> std::vec::Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn rfc8439_vector() {
        // Section 2.5.2.
        let key: [u8; 32] = hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
            .try_into()
            .unwrap();
        assert_eq!(
            tag(&key, b"Cryptographic Forum Research Group").to_vec(),
            hex("a8061dc1305136c6c22b8baf0c0127a9")
        );
    }

    #[test]
    fn edge_cases_from_rfc8439_appendix() {
        // A.3 test vector #1: all-zero key and message give a zero tag.
        assert_eq!(tag(&[0u8; 32], &[0u8; 64]), [0u8; 16]);
        // A.3 #11 style: h close to p exercises the final reduction.
        let mut key = [0u8; 32];
        key[0] = 2;
        let msg = [0xffu8; 16];
        assert_eq!(
            tag(&key, &msg).to_vec(),
            hex("03000000000000000000000000000000")
        );
    }

    extern crate std;
}
