// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A test package named `terminal` (ADR 0074 phase C). It holds the
//! terminal endpoint and checks the rules init applies there: sshd's
//! `Exec` is refused, a request without its handles is dropped, four
//! interactive shells are granted and a fifth is refused. Then it closes
//! the shells' input, so they end, and exits. With `arg` 1 it opens one
//! shell, has it `run cpu_hog`, which never ends, and exits without a
//! word: init must stop `cpu_hog`, and the shell then ends on its closed
//! input (audit of the terminal, M2). With `arg` 2 its shell `spawn`s
//! `cpu_hog` and the test closes the shell's input: the shell ends, and
//! what it spawned goes with it, as a terminal's job would.
//!
//! Handles, in the broker's order: the terminal endpoint (send only).
#![no_std]
#![no_main]

use aricode_proto::session::{self, Kind};
use aricode_rt::sys::{self, kind, permissions, Handle};

fn say(text: &[u8]) {
    sys::print(b"terminal test: ");
    sys::print(text);
    sys::print(b"\n");
}

/// Asks for a session shell of `kind`; the input side it keeps, if
/// granted.
fn ask(terminals: Handle, kind: Kind) -> Option<Handle> {
    let (in_r, input) = sys::endpoint_create_pair().ok()?;
    let (output, out_w) = sys::endpoint_create_pair().ok()?;
    let (reply, reply_w) = sys::endpoint_create_pair().ok()?;
    // The side init watches; the other stays open until this test exits,
    // and its shells end then (a handle is never closed by going out of
    // scope).
    let (alive_r, _alive) = sys::endpoint_create_pair().ok()?;
    let sent = sys::ipc_send(
        terminals,
        &session::Request { kind }.encode(),
        &[reply_w, in_r, out_w, alive_r],
        2000,
    );
    let mut msg = [0u8; 8];
    let mut none = [Handle(0); 1];
    let ok = sent.is_ok()
        && sys::ipc_receive(reply, &mut msg, &mut none, 5000)
            .is_ok_and(|(n, _)| session::reply_ok(&msg[..n]));
    let _ = sys::handle_close(reply);
    // The output is not read: the shell's writes time out, which is fine
    // for a shell that only lives a moment.
    let _ = sys::handle_close(output);
    if ok {
        Some(input)
    } else {
        let _ = sys::handle_close(input);
        None
    }
}

fn main(arg: u64) -> u64 {
    let mut terminals = None;
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) && terminals.is_none() {
            terminals = Some(h);
        }
    });
    let Some(terminals) = terminals else {
        say(b"started without the terminal endpoint");
        return 1;
    };
    if arg == 2 {
        let Some(input) = ask(terminals, Kind::Shell) else {
            say(b"no shell granted");
            return 3;
        };
        let _ = sys::ipc_send(input, b"spawn cpu_hog\n", &[], 2000);
        sys::thread_sleep(1500);
        say(b"closing a shell that spawned cpu_hog");
        let _ = sys::ipc_send(input, &[], &[], 100);
        let _ = sys::handle_close(input);
        sys::thread_sleep(1500);
        return 0;
    }
    if arg == 1 {
        let Some(input) = ask(terminals, Kind::Shell) else {
            say(b"no shell granted");
            return 3;
        };
        let _ = sys::ipc_send(input, b"run cpu_hog\n", &[], 2000);
        sys::thread_sleep(1500);
        say(b"leaving its shell in run");
        return 0;
    }
    let mut kept = true;
    if ask(terminals, Kind::Exec).is_some() {
        say(b"an Exec session was granted");
        kept = false;
    } else {
        say(b"Exec: refused");
    }
    // `Ready` carries no handles: init drops it, saying so once.
    let _ = sys::ipc_send(
        terminals,
        &session::Request { kind: Kind::Ready }.encode(),
        &[],
        2000,
    );
    let mut shells = [None; 5];
    for s in shells.iter_mut() {
        *s = ask(terminals, Kind::Shell);
    }
    let granted = shells.iter().filter(|s| s.is_some()).count();
    let mut b = [0u8; 20];
    sys::print(b"terminal test: ");
    sys::print(aricode_rt::fmt_u64(granted as u64, &mut b));
    sys::print(b" of 5 shells granted\n");
    kept &= granted == 4 && shells[4].is_none();
    for input in shells.into_iter().flatten() {
        let _ = sys::ipc_send(input, &[], &[], 100);
        let _ = sys::handle_close(input);
    }
    say(if kept {
        b"init kept every rule"
    } else {
        b"init broke a rule"
    });
    if kept {
        0
    } else {
        2
    }
}

aricode_rt::entry!(main);
