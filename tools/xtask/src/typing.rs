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
//! echo. Other output printed after the prompt (a driver's log line) can
//! satisfy echo waits too, in the worst case every wait of that line, so
//! those keys go out paced only by the minimum delay; the next line's
//! prompt gate stops it there. Typing is never late because of it: the
//! earliest occurrence is taken.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one key may wait for its echo, or a line for its prompt,
/// before typing gives up, counted only while QEMU runs (a thermal pause,
/// SIGSTOP, does not count). A command's output under load (`diag` with
/// other guests running) takes a few seconds; the scenario's own timeout,
/// which ends QEMU and with it the wait, bounds the whole run anyway.
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
/// whose echo is known, except after the last key: nothing follows it to
/// protect, and the guest may end QEMU on it (Enter on `shutdown`) before
/// the echo reaches the capture. Keys without a known echo go out unwaited.
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
    if let Some(Step::Echo(_)) = steps.last() {
        steps.pop();
    }
    steps
}

/// Bytes the i8042 queues for `key` pressed and released: two per part
/// (make and break codes), four for the keys with an E0 prefix.
pub fn ps2_bytes(key: &str) -> usize {
    const E0: &[&str] = &[
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "pgup",
        "pgdn",
        "insert",
        "delete",
        "ctrl_r",
        "alt_r",
        "meta_l",
        "meta_r",
        "menu",
        "kp_enter",
        "kp_divide",
    ];
    key.split('+')
        .map(|p| if E0.contains(&p) { 4 } else { 2 })
        .sum()
}

/// The most key codes the steps can leave queued in a device at once: the
/// longest run of keys between two waits, counted as PS/2 bytes (the
/// largest of the three encodings: usb-kbd queues one code per event).
pub fn longest_unwaited_run(steps: &[Step]) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for s in steps {
        match s {
            Step::Key(k) => {
                run += ps2_bytes(k);
                longest = longest.max(run);
            }
            Step::Prompt | Step::Echo(_) => run = 0,
        }
    }
    longest
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

/// What a wait watches: the serial capture, the flag that QEMU has exited
/// (nothing more will be printed) and whether QEMU is stopped right now.
pub struct Guest<'a> {
    pub capture: &'a Mutex<Vec<u8>>,
    pub gone: &'a AtomicBool,
    pub stopped: &'a dyn Fn() -> bool,
}

/// Waits until `needle` appears in the capture after `cursor` and moves
/// the cursor past it. Fails once QEMU has run for `timeout` without it
/// (time with QEMU stopped is not counted), or at once when QEMU exits.
pub fn wait_past(
    cursor: &mut Cursor,
    guest: &Guest,
    needle: &[u8],
    timeout: Duration,
) -> Result<(), String> {
    let mut ran = Duration::ZERO;
    let mut last = Instant::now();
    loop {
        if cursor.advance_past(&guest.capture.lock().expect("capture"), needle) {
            return Ok(());
        }
        if guest.gone.load(Ordering::SeqCst) {
            return Err(format!("QEMU exited before {}", describe(needle)));
        }
        let now = Instant::now();
        if !(guest.stopped)() {
            ran += now - last;
        }
        last = now;
        if ran >= timeout {
            return Err(format!(
                "no {} within {} s of QEMU running; check /home/serverbig/aricode-thermal.log",
                describe(needle),
                timeout.as_secs_f32()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// `true` while process `pid` is stopped (SIGSTOP, as the thermal guard
/// does, or a tracer): state `T` or `t` in `/proc/<pid>/stat`.
pub fn process_stopped(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| stat_state(&s))
        .is_some_and(|c| c == 'T' || c == 't')
}

/// The state field of a `/proc/<pid>/stat` line: the first field after the
/// command name, which is in parentheses and may itself contain `)`.
fn stat_state(stat: &str) -> Option<char> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().next()?.chars().next()
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

    fn running() -> bool {
        false
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
                // The last Enter: no echo wait, QEMU may end on it.
                Step::Key("ret"),
            ]
        );
    }

    #[test]
    fn unwaited_runs_are_counted_in_ps2_bytes() {
        assert_eq!(ps2_bytes("a"), 2);
        assert_eq!(ps2_bytes("shift+dot"), 4);
        assert_eq!(ps2_bytes("up"), 4);
        assert_eq!(ps2_bytes("ctrl_r+delete"), 8);
        let k = keys(&["h", "o", "shift+dot", "shift+7", "ret"]);
        // h and o are waited for; the two shifted keys and Enter are not.
        assert_eq!(longest_unwaited_run(&plan(&k)), 4 + 4 + 2);
        let all: Vec<Step> = k.iter().map(|k| Step::Key(k)).collect();
        assert_eq!(longest_unwaited_run(&all), 2 + 2 + 4 + 4 + 2);
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

    /// A log line printed after the prompt can satisfy a whole line of
    /// echo waits (module comment); the next prompt still gates the next
    /// line, and the cursor lands right after each matched byte.
    #[test]
    fn log_output_after_the_prompt_is_capped_at_one_line() {
        let serial = b"ari> usb: ready ok\nusb: id 2\n";
        let mut c = Cursor::default();
        assert!(c.advance_past(serial, b"ari> "));
        assert_eq!(c.0, 5);
        // 'd' of "ready", 'i' of "id", the line end after "id 2".
        assert!(c.advance_past(serial, b"d"));
        assert_eq!(c.0, 14);
        assert!(c.advance_past(serial, b"i"));
        assert_eq!(c.0, 25);
        assert!(c.advance_past(serial, b"\n"));
        assert_eq!(c.0, serial.len());
        // No second prompt: the next line waits.
        assert!(!c.advance_past(serial, b"ari> "));
    }

    #[test]
    fn wait_follows_a_growing_capture() {
        let capture = Arc::new(Mutex::new(b"ari> ".to_vec()));
        let gone = AtomicBool::new(false);
        let guest = Guest {
            capture: &capture,
            gone: &gone,
            stopped: &running,
        };
        let writer = {
            let capture = Arc::clone(&capture);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                capture.lock().unwrap().extend_from_slice(b"s");
            })
        };
        let mut c = Cursor::default();
        wait_past(&mut c, &guest, b"ari> ", ECHO_TIMEOUT).unwrap();
        wait_past(&mut c, &guest, b"s", ECHO_TIMEOUT).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn wait_is_bounded_and_names_the_lost_key() {
        let capture = Mutex::new(b"ari> shut".to_vec());
        let gone = AtomicBool::new(false);
        let guest = Guest {
            capture: &capture,
            gone: &gone,
            stopped: &running,
        };
        let mut c = Cursor::default();
        for needle in [&b"ari> "[..], b"s", b"h", b"u", b"t"] {
            wait_past(&mut c, &guest, needle, ECHO_TIMEOUT).unwrap();
        }
        let start = Instant::now();
        let err = wait_past(&mut c, &guest, b"d", Duration::from_millis(50)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(
            err,
            "no echo of 'd' within 0.05 s of QEMU running; check /home/serverbig/aricode-thermal.log"
        );
        gone.store(true, Ordering::SeqCst);
        let err = wait_past(&mut c, &guest, b"\n", ECHO_TIMEOUT).unwrap_err();
        assert_eq!(err, "QEMU exited before echo of Enter");
    }

    /// A thermal pause (SIGSTOP) must not use up the wait: while the guest
    /// is stopped the clock does not run.
    #[test]
    fn time_with_qemu_stopped_is_not_counted() {
        let capture = Mutex::new(Vec::new());
        let gone = AtomicBool::new(false);
        let paused = AtomicBool::new(true);
        let stopped = || paused.load(Ordering::SeqCst);
        let guest = Guest {
            capture: &capture,
            gone: &gone,
            stopped: &stopped,
        };
        std::thread::scope(|s| {
            s.spawn(|| {
                // Stopped for four times the timeout, then running (one
                // poll of the pause may be counted at the transition).
                std::thread::sleep(Duration::from_millis(200));
                paused.store(false, Ordering::SeqCst);
            });
            let start = Instant::now();
            let err = wait_past(
                &mut Cursor::default(),
                &guest,
                b"x",
                Duration::from_millis(50),
            )
            .unwrap_err();
            assert!(err.starts_with("no echo of 'x'"), "{err}");
            assert!(
                start.elapsed() >= Duration::from_millis(240),
                "gave up after {:?}, during the pause",
                start.elapsed()
            );
        });
    }

    #[test]
    fn a_stopped_process_is_seen_as_stopped() {
        assert_eq!(stat_state("42 (qemu (x) y) T 1 42"), Some('T'));
        assert_eq!(stat_state("42 (sleep) S 1 42"), Some('S'));
        assert!(!process_stopped(std::process::id()));
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("sleep");
        let pid = child.id();
        let signal = |sig: &str| {
            let ok = std::process::Command::new("kill")
                .args([sig, &pid.to_string()])
                .status()
                .expect("kill")
                .success();
            assert!(ok, "kill {sig}");
        };
        signal("-STOP");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process_stopped(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(process_stopped(pid));
        signal("-CONT");
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_stopped(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!process_stopped(pid));
        let _ = child.kill();
        let _ = child.wait();
    }
}
