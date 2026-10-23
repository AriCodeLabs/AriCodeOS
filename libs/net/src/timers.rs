// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The network service's next deadline (ADR 0044): the time until which
//! its loop may sleep when no frame, request or interrupt comes first.
//! Every timed piece of work of a round has to be here, or it waits for
//! the next unrelated wakeup (at worst the periodic sweep). Pure and
//! host-tested; the service gathers the sources from its tables.

use crate::dhcp;
use crate::tcp::Connection;

/// When the DHCP client has work: the end of the ARP probe of an
/// acknowledged address while probing, its next retry before it is bound,
/// nothing once bound.
pub fn dhcp_due(state: dhcp::State, probe_deadline: u64, retry_at: u64) -> Option<u64> {
    match state {
        dhcp::State::Bound => None,
        dhcp::State::Probing => Some(probe_deadline),
        dhcp::State::Init | dhcp::State::Selecting | dhcp::State::Requesting => Some(retry_at),
    }
}

/// The earliest of: the periodic sweep of the clients (`sweep_due`, always
/// there, so the service wakes at least that often), DHCP (`dhcp_due`),
/// every client wait (`waits`: pings, DNS queries, UDP and TCP receives,
/// accepts and connects, frames parked for ARP), every TCP connection's
/// own timer (retransmission or the end of TIME-WAIT,
/// `Connection::next_timer`) and the end of the window holding faults back
/// (`faults_due`).
pub fn next_deadline<'a>(
    sweep_due: u64,
    dhcp: Option<u64>,
    waits: impl IntoIterator<Item = u64>,
    connections: impl IntoIterator<Item = &'a Connection>,
    faults_due: Option<u64>,
) -> u64 {
    dhcp.into_iter()
        .chain(waits)
        .chain(connections.into_iter().filter_map(Connection::next_timer))
        .chain(faults_due)
        .fold(sweep_due, u64::min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ipv4Addr;

    const SWEEP: u64 = 10_000;
    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

    fn none() -> core::iter::Empty<&'static Connection> {
        core::iter::empty()
    }

    #[test]
    fn the_sweep_alone_bounds_the_wait() {
        assert_eq!(next_deadline(SWEEP, None, [], none(), None), SWEEP);
    }

    #[test]
    fn dhcp_decides_when_it_is_earliest() {
        let retry = dhcp_due(dhcp::State::Selecting, 0, 1_200);
        assert_eq!(retry, Some(1_200));
        assert_eq!(next_deadline(SWEEP, retry, [], none(), None), 1_200);
        let probe = dhcp_due(dhcp::State::Probing, 600, 1_200);
        assert_eq!(probe, Some(600));
        assert_eq!(next_deadline(SWEEP, probe, [], none(), None), 600);
        assert_eq!(dhcp_due(dhcp::State::Bound, 600, 1_200), None);
    }

    #[test]
    fn a_client_wait_decides_when_it_is_earliest() {
        assert_eq!(
            next_deadline(SWEEP, Some(9_000), [7_000, 3_500, 8_000], none(), None),
            3_500
        );
    }

    /// A connection's retransmission timer alone: without it a lost
    /// segment would wait for the sweep instead of its RTO.
    #[test]
    fn a_tcp_timer_decides_when_it_is_earliest() {
        let mut c = Connection::new(A, 40_000, B, 80);
        c.connect(1, 100);
        let rto = c.next_timer().expect("a SYN in flight has a timer");
        assert!(rto > 100 && rto < SWEEP);
        assert_eq!(next_deadline(SWEEP, None, [], [&c], None), rto);
        // A connection without a timer adds nothing.
        let idle = Connection::new(A, 40_001, B, 80);
        assert_eq!(idle.next_timer(), None);
        assert_eq!(next_deadline(SWEEP, None, [], [&idle], None), SWEEP);
    }

    #[test]
    fn held_faults_decide_when_they_are_earliest() {
        assert_eq!(next_deadline(SWEEP, None, [], none(), Some(4_000)), 4_000);
    }
}
