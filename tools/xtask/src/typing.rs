// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Typing that follows the guest instead of the clock (incident 0022).
//!
//! QEMU's keyboards hold few events (usb-kbd and the i8042 queue 16 key
//! codes, virtio-input drops events when the guest has no buffer posted)
//! and drop what does not fit. A host that types on a fixed wall-clock
//! rhythm overflows them whenever the guest falls behind, which happens
//! when other processes load the host. When the guest echoes what it reads
//! (the shell, the PS/2 self-test), the runner types one key, waits for its
//! echo on serial and only then sends the next: at most one key is ever
//! queued in the device, whatever the host load.
//!
//! Matching works on the serial capture with a cursor that only moves
//! forward: before the first key of a line the cursor moves past the next
//! prompt, and each key's echo is the first occurrence of its character
//! after the cursor. Waiting for the prompt first keeps the output of the
//! previous command (which contains every letter) from passing for an
//! echo; other output printed between the prompt and the echo can at worst
//! make one key go out early, never late, because the earliest occurrence
//! is taken.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one key may wait for its echo, or a line for its prompt,
/// before typing gives up. A command's output under load (`diag` with
/// other guests running) takes a few seconds; the scenario's own timeout
/// bounds the whole run anyway.
pub const ECHO_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the capture is looked at while waiting.
const POLL: Duration = Duration::from_millis(5);

/// The byte the guest echoes for `key` (a QEMU qcode name, `+` joining
/// modifiers), or `None` when it cannot be known without the guest's
/// layout (combinations, symbols) or the key prints nothing (modifiers):
/// those keys are only paced by the minimum delay.
pub fn key_echo(key: &str) -> Option<u8> {
    match key {
        "ret" | "kp_enter" => Some(b'\n'),
        "spc" => Some(b' '),
        k if k.len() == 1 => {
            let c = k.as_bytes()[0];
            (c.is_ascii_lowercase() || c.is_ascii_digit()).then_some(c)
        }
        _ => None,
    }
}

/// One step of typing a key sequence with echo waits.
#[derive(Debug, PartialEq, Eq)]
pub enum Step<'a> {
    /// Wait for the next prompt: the guest is ready to read a line.
    Prompt,
    /// Press and release this key.
    Key(&'a str),
    /// Wait for the echo of the key just typed.
    Echo(u8),
}

/// The steps for `keys`: a prompt before the first echoed key of each line
/// (the first one, and the first after an Enter), an echo after every key
/// whose echo is known. Keys without a known echo go out unwaited.
pub fn plan(keys: &[String]) -> Vec<Step<'_>> {
    let mut steps = Vec::new();
    let mut line_start = true;
    for key in keys {
        let echo = key_echo(key);
        if echo.is_some() && line_start {
            steps.push(Step::Prompt);
            line_start = false;
        }
        steps.push(Step::Key(key));
        if let Some(c) = echo {
            steps.push(Step::Echo(c));
            if c == b'\n' {
                line_start = true;
            }
        }
    }
    steps
}

/// Position in the serial capture up to which output has been accounted
/// for (prompts and echoes already matched).
#[derive(Debug, Default)]
pub struct Cursor(usize);

impl Cursor {
    /// Moves past the first `needle` at or after the cursor in `serial`;
    /// `false` (cursor unchanged) when it is not there yet.
    pub fn advance_past(&mut self, serial: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() || self.0 >= serial.len() {
            return false;
        }
        match serial[self.0..]
            .windows(needle.len())
            .position(|w| w == needle)
        {
            Some(at) => {
                self.0 += at + needle.len();
                true
            }
            None => false,
        }
    }
}

/// Waits until `needle` appears in `capture` after `cursor` and moves the
/// cursor past it. Fails after `timeout`, or at once when `stop` is set
/// (QEMU has exited: nothing more will be printed).
pub fn wait_past(
    cursor: &mut Cursor,
    capture: &Mutex<Vec<u8>>,
    needle: &[u8],
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if cursor.advance_past(&capture.lock().expect("capture"), needle) {
            return Ok(());
        }
        if stop.load(Ordering::SeqCst) {
            return Err(format!("QEMU exited before {}", describe(needle)));
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "no {} within {} s",
                describe(needle),
                timeout.as_secs_f32()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// `echo of 'd'`, `echo of Enter`, `prompt 'ari> '` for error messages.
fn describe(needle: &[u8]) -> String {
    match needle {
        b"\n" => String::from("echo of Enter"),
        [c] => format!("echo of '{}'", *c as char),
        _ => format!("prompt '{}'", String::from_utf8_lossy(needle)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn echoes_known_only_for_unambiguous_keys() {
        assert_eq!(key_echo("d"), Some(b'd'));
        assert_eq!(key_echo("7"), Some(b'7'));
        assert_eq!(key_echo("ret"), Some(b'\n'));
        assert_eq!(key_echo("spc"), Some(b' '));
        // Layout-dependent or silent: paced only.
        assert_eq!(key_echo("shift"), None);
        assert_eq!(key_echo("shift+dot"), None);
        assert_eq!(key_echo("dot"), None);
        assert_eq!(key_echo("A"), None);
    }

    #[test]
    fn plan_waits_for_a_prompt_per_line_and_an_echo_per_key() {
        let k = keys(&["shift", "d", "i", "ret", "s", "ret"]);
        assert_eq!(
            plan(&k),
            vec![
                // The sacrificial key of usb.hid_fault: no prompt, no echo.
                Step::Key("shift"),
                Step::Prompt,
                Step::Key("d"),
                Step::Echo(b'd'),
                Step::Key("i"),
                Step::Echo(b'i'),
                Step::Key("ret"),
                Step::Echo(b'\n'),
                Step::Prompt,
                Step::Key("s"),
                Step::Echo(b's'),
                Step::Key("ret"),
                Step::Echo(b'\n'),
            ]
        );
    }

    #[test]
    fn cursor_takes_the_earliest_match_and_never_moves_back() {
        let serial = b"ari> dd\nari> ";
        let mut c = Cursor::default();
        assert!(c.advance_past(serial, b"ari> "));
        assert!(c.advance_past(serial, b"d"));
        assert_eq!(c.0, 6);
        assert!(c.advance_past(serial, b"d"));
        assert!(!c.advance_past(serial, b"d"), "no third 'd'");
        assert_eq!(c.0, 7, "a miss leaves the cursor where it was");
        assert!(c.advance_past(serial, b"\n"));
        assert!(c.advance_past(serial, b"ari> "));
        assert!(!c.advance_past(serial, b"ari> "));
    }

    /// The failure of incident 0022: while `diag` prints (every letter in
    /// it), "shutdown" must not be typed. With the prompt step first, the
    /// 's' of `diag`'s output is skipped and only the echo after the next
    /// prompt counts.
    #[test]
    fn output_of_the_previous_command_is_not_an_echo() {
        let k = keys(&["d", "ret", "s", "ret"]);
        let serial = b"ari> d\nservices: shell sshd\n".to_vec();
        let mut c = Cursor::default();
        let mut done = 0;
        for step in plan(&k) {
            let ok = match step {
                Step::Prompt => c.advance_past(&serial, b"ari> "),
                Step::Echo(b) => c.advance_past(&serial, &[b]),
                Step::Key(_) => true,
            };
            if !ok {
                break;
            }
            done += 1;
        }
        // Prompt, d, echo, ret, echo; then the next prompt is missing.
        assert_eq!(done, 5);
    }

    #[test]
    fn wait_follows_a_growing_capture() {
        let capture = Arc::new(Mutex::new(b"ari> ".to_vec()));
        let stop = AtomicBool::new(false);
        let writer = {
            let capture = Arc::clone(&capture);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                capture.lock().unwrap().extend_from_slice(b"s");
            })
        };
        let mut c = Cursor::default();
        wait_past(&mut c, &capture, b"ari> ", ECHO_TIMEOUT, &stop).unwrap();
        wait_past(&mut c, &capture, b"s", ECHO_TIMEOUT, &stop).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn wait_is_bounded_and_names_the_lost_key() {
        let capture = Mutex::new(b"ari> shut".to_vec());
        let stop = AtomicBool::new(false);
        let mut c = Cursor::default();
        for needle in [&b"ari> "[..], b"s", b"h", b"u", b"t"] {
            wait_past(&mut c, &capture, needle, ECHO_TIMEOUT, &stop).unwrap();
        }
        let start = Instant::now();
        let err = wait_past(&mut c, &capture, b"d", Duration::from_millis(50), &stop).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(err, "no echo of 'd' within 0.05 s");
        stop.store(true, Ordering::SeqCst);
        let err = wait_past(&mut c, &capture, b"\n", ECHO_TIMEOUT, &stop).unwrap_err();
        assert_eq!(err, "QEMU exited before echo of Enter");
    }
}
