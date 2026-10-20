// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Receive budget of the network service's loop (incident 0013, ADR 0035).
//! The service is single-threaded: a round receives frames, serves IPC
//! requests, expires deadlines and emits segments. Draining the receive
//! ring without bound would let a line-rate flood (SYNs to a listener, each
//! costing a reset) hold the round for as long as frames keep coming, so
//! the clients' requests and the timers would never get their turn. A round
//! therefore receives at most [`FRAMES_PER_ROUND`] frames; what is left in
//! the ring is reported ([`Round::left_over`]) so the service can record it
//! and start the next round without waiting for an interrupt. Pure and
//! host-tested; the service connects it to the NIC.

/// Frames one round receives at most. The receive rings have 16 slots
/// (virtio-net and rtl-net alike) and refill as frames are consumed, so a
/// drain-until-empty loop is unbounded under a flood; two rings' worth
/// bounds it at about 48 KiB of frames, which is under half a millisecond
/// of a gigabit link and about 4 ms of the laptop's 100 Mbit/s Realtek,
/// while a quiet round costs nothing extra. With the IPC budget of eight
/// requests per round that bounds the latency a client sees under flood to
/// one round: the frames plus eight requests plus the timers.
pub const FRAMES_PER_ROUND: usize = 32;
const _: () = assert!(FRAMES_PER_ROUND >= 1);

/// What a round of reception did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Round {
    /// Frames received and handled.
    pub received: usize,
    /// The budget ran out with frames still in the ring: the next round
    /// should follow at once, and the service records it.
    pub left_over: bool,
}

/// Receives with `recv` into `frame` and handles each frame with `handle`
/// (its length is `recv`'s answer), at most `budget` times; `pending`
/// answers whether the ring still holds frames once the budget is spent.
/// `state` is the service (the NIC and the stack), lent to every closure
/// in turn.
pub fn receive_round<S, B>(
    budget: usize,
    state: &mut S,
    frame: &mut B,
    mut recv: impl FnMut(&mut S, &mut B) -> Option<usize>,
    mut handle: impl FnMut(&mut S, &B, usize),
    pending: impl FnOnce(&S) -> bool,
) -> Round {
    let mut received = 0;
    while received < budget {
        let Some(n) = recv(state, frame) else {
            return Round {
                received,
                left_over: false,
            };
        };
        handle(state, frame, n);
        received += 1;
    }
    Round {
        received,
        left_over: pending(state),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// A ring of frames of one byte each, counting what was handled.
    struct Ring {
        frames: Vec<u8>,
        handled: Vec<u8>,
    }

    fn round(ring: &mut Ring, budget: usize) -> Round {
        let mut frame = [0u8; 4];
        receive_round(
            budget,
            ring,
            &mut frame,
            |r, f| {
                let b = r.frames.pop()?;
                f[0] = b;
                Some(1)
            },
            |r, f, n| r.handled.push(f[..n][0]),
            |r| !r.frames.is_empty(),
        )
    }

    fn ring(frames: usize) -> Ring {
        Ring {
            frames: (0..frames as u8).collect(),
            handled: Vec::new(),
        }
    }

    #[test]
    fn a_quiet_ring_is_drained_and_leaves_nothing() {
        let mut r = ring(10);
        assert_eq!(
            round(&mut r, FRAMES_PER_ROUND),
            Round {
                received: 10,
                left_over: false
            }
        );
        assert_eq!(r.handled.len(), 10, "every frame was handled");
        assert_eq!(
            round(&mut r, FRAMES_PER_ROUND),
            Round {
                received: 0,
                left_over: false
            }
        );
    }

    /// Incident 0013: a flood does not hold the round; the rest waits for
    /// the next one and the round says so.
    #[test]
    fn a_flood_stops_at_the_budget_and_reports_the_rest() {
        let mut r = ring(100);
        assert_eq!(
            round(&mut r, FRAMES_PER_ROUND),
            Round {
                received: FRAMES_PER_ROUND,
                left_over: true
            }
        );
        assert_eq!(r.frames.len(), 100 - FRAMES_PER_ROUND);
        assert_eq!(r.handled.len(), FRAMES_PER_ROUND);
        // Three more rounds finish it; only the ones that left frames say so.
        assert!(round(&mut r, FRAMES_PER_ROUND).left_over);
        assert!(round(&mut r, FRAMES_PER_ROUND).left_over);
        let last = round(&mut r, FRAMES_PER_ROUND);
        assert_eq!(last.received, 100 - 3 * FRAMES_PER_ROUND);
        assert!(!last.left_over);
    }

    /// Exactly the budget: the ring is empty, so nothing is reported even
    /// though the budget was spent.
    #[test]
    fn exactly_the_budget_is_not_a_hit() {
        let mut r = ring(FRAMES_PER_ROUND);
        assert_eq!(
            round(&mut r, FRAMES_PER_ROUND),
            Round {
                received: FRAMES_PER_ROUND,
                left_over: false
            }
        );
        let mut r = ring(FRAMES_PER_ROUND + 1);
        assert!(round(&mut r, FRAMES_PER_ROUND).left_over);
    }

    #[test]
    fn frames_are_handled_in_order_with_their_length() {
        let mut r = ring(3);
        round(&mut r, 2);
        assert_eq!(r.handled, [2, 1]);
    }
}
