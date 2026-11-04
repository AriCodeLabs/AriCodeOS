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

use aricode_fb_console::{Console, Rgb, Surface};
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
        let mut c = self.0.canvas();
        let r = aricode_gfx::Rect::new(x as i32, y as i32, 1, 1);
        c.fill_rect(&r, encode(color));
    }

    fn glyph(&mut self, x: usize, y: usize, rows: &[u8], fg: Rgb, bg: Rgb) {
        let (fg, bg) = (encode(fg), encode(bg));
        let mut c = self.0.canvas();
        for (dy, bits) in rows.iter().enumerate() {
            for dx in 0..8 {
                let color = if bits & (0x80 >> dx) != 0 { fg } else { bg };
                let r = aricode_gfx::Rect::new((x + dx) as i32, (y + dy) as i32, 1, 1);
                c.fill_rect(&r, color);
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

/// Shows the shell's bytes: text, newlines, carriage returns, backspaces
/// and tabs, as the console does at boot; anything else is a `?`.
fn show(bytes: &[u8]) {
    SHOWN.fetch_add(bytes.len() as u64, core::sync::atomic::Ordering::Relaxed);
    let mut console = CONSOLE.lock();
    for chunk in bytes.utf8_chunks() {
        let _ = console.write_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            let _ = console.write_str("?");
        }
    }
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
}

/// Asks init for an interactive session shell. Every endpoint is a pair:
/// the shell gets the receive side of the input and the send side of the
/// output, the terminal keeps the other two (as sshd, ADR 0025).
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
    let (reply, reply_w) = match sys::endpoint_create_pair() {
        Ok(p) => p,
        Err(e) => {
            for h in [in_r, input, output, out_w] {
                let _ = sys::handle_close(h);
            }
            return Err(e);
        }
    };
    let req = session::Request {
        kind: session::Kind::Shell,
    };
    let handles = [reply_w, in_r, out_w];
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
        return Err(Error::Denied);
    }
    Ok(Shell { input, output })
}

/// Why the terminal stops.
enum End {
    /// The shell ended (`exit`, or it is gone).
    Shell,
    /// The window or the compositor is gone.
    Window,
}

fn run(shell: &Shell) -> End {
    let start = |c: &Console<Screen>| c.surface().map(|s| s.0.start_poll(POLL_MS));
    let Some(Ok(mut pending)) = start(&CONSOLE.lock()) else {
        return End::Window;
    };
    let mut msg = [0u8; abi::IPC_MAX_DATA];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    loop {
        let waited = sys::ipc_receive_any(
            &[pending, shell.output],
            &mut msg,
            &mut handles,
            2 * POLL_MS as u64,
        );
        for h in handles.iter_mut().filter(|h| h.0 != 0) {
            let _ = sys::handle_close(*h);
            *h = Handle(0);
        }
        match waited {
            // The shell's text; an empty message is its end of stream.
            Ok((1, 0, _)) => return End::Shell,
            Ok((1, n, _)) => {
                show(&msg[..n]);
                // Whatever else it wrote, before painting once.
                while let Ok((n, _)) = sys::ipc_receive(shell.output, &mut msg, &mut handles, 0) {
                    if n == 0 {
                        present();
                        return End::Shell;
                    }
                    show(&msg[..n]);
                }
                present();
            }
            Ok((_, n, _)) => {
                let _ = sys::handle_close(pending);
                match window::poll_reply(&msg[..n]) {
                    Ok(Some(Event::Key(b))) => {
                        let _ = sys::ipc_send(shell.input, &[b], &[], 1000);
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        // The poll's deadline: blink, and look whether the
                        // shell went without saying so.
                        CONSOLE.lock().blink();
                        present();
                        if matches!(
                            sys::ipc_receive(shell.output, &mut msg, &mut handles, 0),
                            Err(e) if e.endpoint_gone()
                        ) {
                            return End::Shell;
                        }
                    }
                    Err(_) => return End::Window,
                }
                match start(&CONSOLE.lock()) {
                    Some(Ok(p)) => pending = p,
                    _ => return End::Window,
                }
            }
            Err(Error::Timeout) => {}
            Err(_) => return End::Window,
        }
    }
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
            show(b"init refused a shell (four terminals at most)\n");
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
