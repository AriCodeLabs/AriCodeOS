// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's terminal (ADR 0074 phase C). It asks init for a session
//! shell, as sshd does for a remote one (ADR 0025): the shell reads the
//! keys the terminal forwards and writes to an endpoint the terminal
//! reads, and draws that text in its window with the console's text grid
//! (`aricode-fb-console`), which understands what the shell's line editor
//! sends. The shell is the owner's own; the terminal only carries bytes.
//! When the shell ends (`exit`), the terminal closes.
//!
//! Handles, in the broker's order: its compositor session, the terminal
//! endpoint (both endpoints, send only) and `HARDWARE`.
#![no_std]
#![no_main]

use aricode_fb_console::{Console, Rgb, Surface, Utf8Stream};
use aricode_proto::session;
use aricode_rt::sys::window::{self, Event, Window};
use aricode_rt::sys::{self, abi, kind, permissions, Error, Handle};
use aricode_sync::SpinLock;
use core::fmt::Write;

/// 80 columns and 25 rows of the 8x16 font.
const W: u32 = 640;
const H: u32 = 400;
/// How long a window poll waits; also the cursor's blink.
const POLL_MS: u32 = 500;

fn say(text: &[u8]) {
    sys::print(b"terminal: ");
    sys::print(text);
    sys::print(b"\n");
}

/// The window as the console's surface.
struct Screen(Window);

impl Surface for Screen {
    fn width(&self) -> usize {
        W as usize
    }

    fn height(&self) -> usize {
        H as usize
    }

    fn put(&mut self, x: usize, y: usize, color: Rgb) {
        self.0.canvas().set(x as i32, y as i32, encode(color));
    }

    fn glyph(&mut self, x: usize, y: usize, rows: &[u8], fg: Rgb, bg: Rgb) {
        let (fg, bg) = (encode(fg), encode(bg));
        let mut c = self.0.canvas();
        for (dy, bits) in rows.iter().enumerate() {
            for dx in 0..8 {
                let color = if bits & (0x80 >> dx) != 0 { fg } else { bg };
                c.set((x + dx) as i32, (y + dy) as i32, color);
            }
        }
    }

    fn fill_rows(&mut self, y: usize, rows: usize, color: Rgb) {
        let mut c = self.0.canvas();
        c.fill_rect(
            &aricode_gfx::Rect::new(0, y as i32, W, rows as u32),
            encode(color),
        );
    }
}

fn encode(c: Rgb) -> u32 {
    aricode_gfx::rgb(c.0, c.1, c.2)
}

/// The text grid (about 64 KiB): too large for the stack, so it lives
/// here, built empty, and the window is attached once it exists.
static CONSOLE: SpinLock<Console<Screen>> = SpinLock::new(Console::empty());

/// Bytes of the shell's text shown, said when the terminal closes.
static SHOWN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The shell's text as it arrives in messages: a character one message
/// cuts is kept for the next (audit, L2).
static STREAM: SpinLock<Utf8Stream> = SpinLock::new(Utf8Stream::new());

/// Shows the shell's bytes: text, newlines, carriage returns, backspaces
/// and tabs, as the console does at boot; anything else is a `?`.
fn show(bytes: &[u8]) {
    SHOWN.fetch_add(bytes.len() as u64, core::sync::atomic::Ordering::Relaxed);
    let mut console = CONSOLE.lock();
    STREAM.lock().feed(bytes, |text| {
        let _ = console.write_str(text);
    });
}

fn present() {
    if let Some(screen) = CONSOLE.lock().surface() {
        let _ = screen.0.present();
    }
}

/// The session shell's two endpoints, the terminal's sides: where it sends
/// the keys and where the shell's text arrives.
struct Shell {
    input: Handle,
    output: Handle,
    /// The send side of the endpoint whose other side init keeps: it
    /// closes with the terminal, whatever ends it, and init then stops the
    /// shell (audit of the terminal, M2).
    alive: Handle,
}

/// Asks init for an interactive session shell. Every endpoint is a pair:
/// the shell gets the receive side of the input and the send side of the
/// output, the terminal keeps the other two (as sshd, ADR 0025). A fourth
/// tells init when the terminal is gone.
fn open_shell(terminals: Handle) -> Result<Shell, Error> {
    let (in_r, input) = sys::endpoint_create_pair()?;
    let (output, out_w) = match sys::endpoint_create_pair() {
        Ok(p) => p,
        Err(e) => {
            let _ = sys::handle_close(in_r);
            let _ = sys::handle_close(input);
            return Err(e);
        }
    };
    let (alive_r, alive) = match sys::endpoint_create_pair() {
        Ok(p) => p,
        Err(e) => {
            for h in [in_r, input, output, out_w] {
                let _ = sys::handle_close(h);
            }
            return Err(e);
        }
    };
    let (reply, reply_w) = match sys::endpoint_create_pair() {
        Ok(p) => p,
        Err(e) => {
            for h in [in_r, input, output, out_w, alive_r, alive] {
                let _ = sys::handle_close(h);
            }
            return Err(e);
        }
    };
    let req = session::Request {
        kind: session::Kind::Shell,
    };
    let handles = [reply_w, in_r, out_w, alive_r];
    let sent = sys::ipc_send(terminals, &req.encode(), &handles, 2000);
    if sent.is_err() {
        // Not transferred: still ours.
        for h in handles {
            let _ = sys::handle_close(h);
        }
    }
    let mut msg = [0u8; 8];
    let mut none = [Handle(0); 1];
    let ok = sent.is_ok()
        && sys::ipc_receive(reply, &mut msg, &mut none, 5000)
            .is_ok_and(|(n, _)| session::reply_ok(&msg[..n]));
    let _ = sys::handle_close(reply);
    if !ok {
        let _ = sys::handle_close(input);
        let _ = sys::handle_close(output);
        let _ = sys::handle_close(alive);
        return Err(Error::Denied);
    }
    Ok(Shell {
        input,
        output,
        alive,
    })
}

/// Why the terminal stops.
enum End {
    /// The shell ended (`exit`, or it is gone).
    Shell,
    /// The window or the compositor is gone, or the compositor stopped
    /// answering.
    Window,
}

/// Messages of the shell's text taken before painting once: a shell that
/// writes faster than the window paints still lets keys through (audit,
/// M3).
const DRAIN: usize = 8;

/// Takes up to `DRAIN` messages of the shell's text into the window, then
/// paints. `Some(End::Shell)` when the shell ended (an empty message, or
/// its side closed).
fn take_output(shell: &Shell, msg: &mut [u8], first: Option<usize>) -> Option<End> {
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    let mut end = None;
    let mut next = first;
    for _ in 0..DRAIN {
        let n = match next.take() {
            Some(n) => n,
            None => match sys::ipc_receive(shell.output, msg, &mut handles, 0) {
                Ok((n, nh)) => {
                    // Text only: a handle sent along is not kept.
                    for h in &handles[..nh] {
                        let _ = sys::handle_close(*h);
                    }
                    n
                }
                Err(e) if e.endpoint_gone() => {
                    end = Some(End::Shell);
                    break;
                }
                Err(_) => break,
            },
        };
        if n == 0 {
            end = Some(End::Shell);
            break;
        }
        show(&msg[..n]);
    }
    present();
    end
}

/// Sends one key to the shell without waiting: while the shell is busy
/// (in `run`) and its input is full, the key is dropped, and said once,
/// rather than freezing the window (audit, L3).
fn send_key(shell: &Shell, b: u8, said_full: &mut bool) {
    if sys::ipc_send(shell.input, &[b], &[], 0).is_err() && !core::mem::replace(said_full, true) {
        say(b"the shell is not reading its keys; dropping them");
    }
}

fn run(shell: &Shell) -> End {
    let start = || CONSOLE.lock().surface().map(|s| s.0.start_poll(POLL_MS));
    let Some(Ok(mut pending)) = start() else {
        return End::Window;
    };
    let mut msg = [0u8; abi::IPC_MAX_DATA];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    let mut said_full = false;
    let end = loop {
        let waited = sys::ipc_receive_any(
            &[pending, shell.output],
            &mut msg,
            &mut handles,
            2 * POLL_MS as u64,
        );
        if let Ok((_, _, nh)) = waited {
            for h in &handles[..nh] {
                let _ = sys::handle_close(*h);
            }
        }
        match waited {
            Ok((1, n, _)) => {
                if let Some(end) = take_output(shell, &mut msg, Some(n)) {
                    break end;
                }
            }
            Ok((_, n, _)) => {
                let _ = sys::handle_close(pending);
                match window::poll_reply(&msg[..n]) {
                    Ok(Some(Event::Key(b))) => send_key(shell, b, &mut said_full),
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        // The poll's deadline: blink, and take whatever
                        // the shell wrote meanwhile (never dropped; audit,
                        // H1), which also tells a shell that went without
                        // saying so.
                        CONSOLE.lock().blink();
                        if let Some(end) = take_output(shell, &mut msg, None) {
                            return end;
                        }
                    }
                    Err(_) => return End::Window,
                }
                match start() {
                    Some(Ok(p)) => pending = p,
                    _ => return End::Window,
                }
            }
            // The compositor answers every poll within `POLL_MS`: silence
            // for twice that means it is gone or hung (audit, M1).
            Err(Error::Timeout) => break End::Window,
            // Both sides closed: the shell's end decides.
            Err(_) => break End::Shell,
        }
    };
    let _ = sys::handle_close(pending);
    end
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
    let ([Some(session), Some(terminals)], Some(hw)) = (endpoints, hw) else {
        say(b"started without a window, the terminal endpoint or HARDWARE");
        return 1;
    };
    let Ok(win) = Window::create(session, hw, W, H, "Terminal") else {
        say(b"could not create its window");
        return 2;
    };
    CONSOLE.lock().attach(Screen(win));
    let shell = match open_shell(terminals) {
        Ok(s) => s,
        Err(_) => {
            show(b"init refused a shell: four terminals at most, or no file or network session left\n");
            present();
            say(b"init refused a shell");
            sys::thread_sleep(3000);
            return 3;
        }
    };
    present();
    say(b"shell started");
    let end = run(&shell);
    // Closing the input tells the shell to end, if it has not.
    let _ = sys::ipc_send(shell.input, &[], &[], 100);
    let _ = sys::handle_close(shell.input);
    let _ = sys::handle_close(shell.output);
    let _ = sys::handle_close(shell.alive);
    sys::print(match end {
        End::Shell => b"terminal: the shell ended; closing (",
        End::Window => b"terminal: the window is gone; closing (",
    });
    let mut b = [0u8; 20];
    let shown = SHOWN.load(core::sync::atomic::Ordering::Relaxed);
    sys::print(aricode_rt::fmt_u64(shown, &mut b));
    sys::print(b" bytes of the shell's text shown)\n");
    if let Some(Screen(win)) = CONSOLE.lock().detach() {
        let _ = win.close();
    }
    0
}

aricode_rt::entry!(main);
