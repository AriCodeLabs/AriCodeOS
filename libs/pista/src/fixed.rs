// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Numbers in Q16.16, and angles in 1/65536 of a turn.

pub type F = i32;
pub const ONE: F = 1 << 16;

pub const fn f(n: i32) -> F {
    n << 16
}

/// `n / d` as a fixed-point number.
pub const fn ratio(n: i32, d: i32) -> F {
    ((n as i64 * ONE as i64) / d as i64) as F
}

pub const fn mul(a: F, b: F) -> F {
    ((a as i64 * b as i64) >> 16) as F
}

const SINES: usize = 1024;

/// A half-turn of sines by Bhaskara's approximation, which needs only integers
/// and is within 0.2% of the real curve: more than a screen can show.
const fn sine_table() -> [F; SINES] {
    let half = SINES as i64 / 2;
    let mut table = [0; SINES];
    let mut i = 0;
    while i < SINES {
        let x = i as i64 % half;
        let p = x * (half - x);
        let s = (16 * p * ONE as i64) / (5 * half * half - 4 * p);
        table[i] = if i < SINES / 2 { s as F } else { -s as F };
        i += 1;
    }
    table
}

static SINE: [F; SINES] = sine_table();

pub fn sin(angle: u16) -> F {
    SINE[(angle >> 6) as usize]
}

pub fn cos(angle: u16) -> F {
    sin(angle.wrapping_add(0x4000))
}

/// The integer square root.
pub fn isqrt(v: u64) -> u64 {
    if v == 0 {
        return 0;
    }
    let mut x = 1u64 << ((64 - v.leading_zeros()).div_ceil(2));
    loop {
        let y = (x + v / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// The length of (x, y).
pub fn length(x: F, y: F) -> F {
    let (x, y) = (x as i64, y as i64);
    isqrt((x * x + y * y) as u64) as F
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sine_has_its_landmarks() {
        assert_eq!(sin(0), 0);
        assert!((sin(0x4000) - ONE).abs() < 64);
        assert_eq!(sin(0x8000), 0);
        assert!((sin(0xC000) + ONE).abs() < 64);
        assert!((cos(0) - ONE).abs() < 64);
        assert_eq!(cos(0x4000), 0);
        // sin 30 degrees = 1/2, within the approximation.
        assert!((sin(0x1555) - ONE / 2).abs() < ONE / 200);
    }

    #[test]
    fn sine_and_cosine_stay_on_the_circle() {
        for a in (0..=u16::MAX).step_by(97) {
            let r = length(sin(a), cos(a));
            assert!((r - ONE).abs() < ONE / 100, "angle {a}: radius {r}");
        }
    }

    #[test]
    fn square_roots_round_down() {
        for (v, root) in [
            (0, 0),
            (1, 1),
            (3, 1),
            (4, 2),
            (99, 9),
            (100, 10),
            (u64::MAX, u32::MAX as u64),
        ] {
            assert_eq!(isqrt(v), root, "{v}");
        }
    }

    #[test]
    fn products_and_ratios() {
        assert_eq!(mul(f(3), f(-4)), f(-12));
        assert_eq!(mul(ONE / 2, ONE / 2), ONE / 4);
        assert_eq!(ratio(3, 2), ONE + ONE / 2);
        assert_eq!(length(f(3), f(4)), f(5));
    }
}
