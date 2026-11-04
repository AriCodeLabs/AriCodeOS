// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A test package named `launcher` (ADR 0074 phase B). It holds `launch`:
//! it presents a window, starts `gui_demo` through the broker's launch
//! endpoint, and checks that the broker refuses the rest (an application
//! without a window, a system program, itself, a launch waited for, a
//! console, a list with another flag), and that its own pool holds six
//! applications. Then it waits 4 s, so the demos' windows close, and
//! exits; in the desktop
//! session that ends the desktop and gives the console back its screen and
//! keyboard.
//!
//! Handles, in the broker's order: its compositor session, the launch
//! endpoint (both endpoints, send only) and `HARDWARE`.
#![no_std]
#![no_main]

use aricode_gfx::{rgb, Rect};
use aricode_proto::launch::{Entry, Request};
use aricode_rt::sys::launch::{self, LaunchError, Status};
use aricode_rt::sys::window::Window;
use aricode_rt::sys::{self, kind, permissions};

fn say(text: &[u8]) {
    sys::print(b"launcher: ");
    sys::print(text);
    sys::print(b"\n");
}

fn status_name(r: &Result<(), LaunchError>) -> &'static [u8] {
    match r {
        Ok(()) => b"started",
        Err(LaunchError::Status(Status::Denied)) => b"denied",
        Err(LaunchError::Status(Status::Invalid)) => b"invalid",
        Err(LaunchError::Status(Status::NotFound)) => b"not found",
        Err(LaunchError::Status(Status::Limit)) => b"limit",
        Err(LaunchError::Status(_)) => b"another status",
        Err(LaunchError::Ipc(_)) => b"ipc error",
        Err(LaunchError::BadName) => b"bad name",
    }
}

/// One launch through the endpoint, said with its outcome; whether it was
/// `want`.
fn check(what: &[u8], got: Result<(), LaunchError>, want: &[u8]) -> bool {
    sys::print(b"launcher: ");
    sys::print(what);
    sys::print(b": ");
    sys::print(status_name(&got));
    sys::print(b"\n");
    status_name(&got) == want
}

fn main(_arg: u64) -> u64 {
    let mut endpoints = [None; 2];
    let mut found = 0;
    let mut hw = None;
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) && found < endpoints.len() {
            endpoints[found] = Some(h);
            found += 1;
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let ([Some(session), Some(apps)], Some(hw)) = (endpoints, hw) else {
        say(b"started without a window, the launch endpoint or HARDWARE");
        return 1;
    };
    let Ok(mut win) = Window::create(session, hw, 320, 120, "Launcher") else {
        say(b"could not create its window");
        return 2;
    };
    {
        let mut c = win.canvas();
        c.fill(rgb(0x20, 0x30, 0x40));
        c.fill_rect(&Rect::new(0, 0, 320, 2), rgb(255, 255, 255));
        c.text(
            16,
            16,
            "AriCodeOS launcher (test)",
            rgb(255, 255, 255),
            None,
        );
    }
    if win.present().is_err() {
        return 3;
    }
    say(b"window presented");
    let mut kept = true;
    // hello_ext opens no window: not the launcher's to start (audit of
    // phase C, L2).
    kept &= check(
        b"hello_ext (no window)",
        launch::spawn(apps, "hello_ext", 5),
        b"denied",
    );
    kept &= check(
        b"installer (a system program)",
        launch::spawn(apps, "installer", 0),
        b"denied",
    );
    kept &= check(b"itself", launch::spawn(apps, "launcher", 0), b"denied");
    kept &= check(
        b"hello_ext waited for",
        launch::run(apps, "hello_ext", 6).map(|_| ()),
        b"invalid",
    );
    let console = match sys::endpoint_create_pair() {
        Ok((receive, send)) => {
            let _ = sys::handle_close(receive);
            send
        }
        Err(_) => return 4,
    };
    kept &= check(
        b"hello_ext with a console",
        launch::run_with_console(apps, "hello_ext", 7, console).map(|_| ()),
        b"invalid",
    );
    kept &= check(b"gui_demo", launch::spawn(apps, "gui_demo", 0), b"started");
    // The launcher's own pool (ADR 0074 phase C): six applications at
    // once, whatever the shell runs: five more demos, then `Limit`.
    let mut more = 0;
    let limit = loop {
        match launch::spawn(apps, "gui_demo", 0) {
            Ok(()) if more < 8 => more += 1,
            r => break r,
        }
    };
    let mut b = [0u8; 20];
    sys::print(b"launcher: the broker answered ");
    sys::print(status_name(&limit));
    sys::print(b" after ");
    sys::print(aricode_rt::fmt_u64(more, &mut b));
    sys::print(b" more gui_demo\n");
    kept &= status_name(&limit) == b"limit" && more == 5;
    // A list request with another flag is refused whole (audit of phase
    // C, L3).
    let odd = Request::list(0);
    let mut raw = odd.encode();
    raw[3] |= Request::DETACH;
    let mut reply = [0u8; Entry::LEN];
    let refused = sys::rpc::call(apps, &raw, &[], 2000, 2000, &mut reply)
        .ok()
        .and_then(|n| Entry::decode(&reply[..n]))
        .is_some_and(|e| e.status == Status::Invalid);
    say(if refused {
        b"a list with another flag: invalid"
    } else {
        b"a list with another flag was answered"
    });
    kept &= refused;
    say(if kept {
        b"the broker kept every rule"
    } else {
        b"the broker broke a rule"
    });
    sys::thread_sleep(4000);
    let _ = win.close();
    say(b"leaving");
    if kept {
        0
    } else {
        5
    }
}

aricode_rt::entry!(main);
