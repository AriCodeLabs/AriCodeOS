// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Host ports of one scenario run (ADR 0040).
//!
//! Every host-side port a scenario binds that the guest does not have
//! compiled in (QEMU's `hostfwd` ports, the scenario's update server, its
//! HTTPS servers) is chosen per run: the kernel picks a free port
//! (`bind` to port 0), so another suite on the same machine gets other
//! ports, and this process remembers it until the run ends, so two runs of
//! one suite never share one. The ports the guest's test programs have
//! compiled in (`aricode_proto::net::TEST_*`) stay fixed: QEMU's user
//! network sends the guest's traffic to 10.0.2.2:P to the host's
//! 127.0.0.1:P and offers no way to remap it (`guestfwd` refuses the
//! virtual host address, and has no UDP), so those are bound once per
//! suite (`qemu::Responder`).

use crate::Result;
use std::collections::BTreeSet;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::Mutex;

/// Ports handed out by this process and not yet released.
static LEASED: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

/// Attempts to find one unleased free port before giving up.
const ATTEMPTS: usize = 1000;

/// Free host ports on 127.0.0.1, distinct from every other lease of this
/// process while the value lives. The ports are not held open: the program
/// they are passed to (QEMU, `openssl s_server`) binds them, and a program
/// that cannot (another process took the port in between) fails at start
/// with its own error, which the scenario's log shows.
pub struct PortLease(Vec<u16>);

impl PortLease {
    pub fn take(count: usize) -> Result<Self> {
        let mut lease = Self(Vec::with_capacity(count));
        // The listeners stay open until every port is chosen, so the kernel
        // cannot hand the same one twice within a lease.
        let mut held = Vec::with_capacity(count);
        let mut attempts = 0;
        while lease.0.len() < count {
            attempts += 1;
            if attempts > ATTEMPTS {
                return Err(format!(
                    "no free host port after {ATTEMPTS} attempts ({} of {count} found)",
                    lease.0.len()
                ));
            }
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            if LEASED.lock().expect("port leases").insert(port) {
                lease.0.push(port);
            }
            held.push(listener);
        }
        Ok(lease)
    }

    pub fn ports(&self) -> &[u16] {
        &self.0
    }

    /// A listener this process binds itself (the scenario's update server),
    /// on a free port that is registered as a lease under the same lock as
    /// [`PortLease::take`], so it can never be a port another scenario has
    /// leased for QEMU or HTTPS but not bound yet (audit of 2943a1e,
    /// finding 1). The lease lives as long as the returned value.
    pub fn listener() -> Result<(TcpListener, Self)> {
        // Listeners on ports that were already leased stay open until a
        // good one is found, so the kernel does not hand them out again.
        let mut rejected = Vec::new();
        for _ in 0..ATTEMPTS {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            if LEASED.lock().expect("port leases").insert(port) {
                return Ok((listener, Self(vec![port])));
            }
            rejected.push(listener);
        }
        Err(format!(
            "no unleased free host port after {ATTEMPTS} attempts"
        ))
    }
}

impl Drop for PortLease {
    fn drop(&mut self) {
        let mut leased = LEASED.lock().expect("port leases");
        for p in &self.0 {
            leased.remove(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PortLease, LEASED};

    #[test]
    fn leases_are_distinct_free_and_released() {
        let a = PortLease::take(3).unwrap();
        let b = PortLease::take(3).unwrap();
        let mut all: Vec<u16> = a.ports().iter().chain(b.ports()).copied().collect();
        assert!(all.iter().all(|&p| p != 0));
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 6, "two live leases share a port: {all:?}");
        // Not held open: the program the port is for can bind it. Another
        // socket can take a port between the lease and this bind (other
        // tests of this binary bind and connect in parallel, and so does
        // the rest of the host; incident 0049), so a fresh lease is tried
        // when that happens: a lease that held its ports would fail every
        // time, a chance collision almost never twice.
        let bindable = |lease: &PortLease| {
            lease
                .ports()
                .iter()
                .map(|&p| std::net::TcpListener::bind(("127.0.0.1", p)))
                .collect::<Result<Vec<_>, _>>()
                .is_ok()
        };
        assert!(
            bindable(&a) || (0..4).any(|_| bindable(&PortLease::take(3).unwrap())),
            "five leases in a row had a port that could not be bound"
        );
        let gone = a.ports().to_vec();
        drop(a);
        let leased = LEASED.lock().unwrap();
        assert!(gone.iter().all(|p| !leased.contains(p)), "released on drop");
        assert!(
            b.ports().iter().all(|p| leased.contains(p)),
            "b still holds"
        );
    }

    #[test]
    fn listeners_never_take_a_leased_port() {
        // Many reserved-but-unbound ports (as QEMU's hostfwd ports are
        // between `take` and QEMU's start), then many listeners: none may
        // land on a reserved one, and each holds its own lease.
        let reserved = PortLease::take(64).unwrap();
        let listeners: Vec<_> = (0..64).map(|_| PortLease::listener().unwrap()).collect();
        let leased = LEASED.lock().unwrap();
        for (l, lease) in &listeners {
            let p = l.local_addr().unwrap().port();
            assert_eq!(lease.ports(), [p]);
            assert!(
                !reserved.ports().contains(&p),
                "listener took leased port {p}"
            );
            assert!(leased.contains(&p));
        }
    }

    #[test]
    fn concurrent_leases_never_collide() {
        let leases: Vec<PortLease> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| PortLease::take(4).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut all: Vec<u16> = leases.iter().flat_map(|l| l.ports().to_vec()).collect();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n);
    }
}
