// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel command line: `key` or `key=value` separated by
//! spaces. No `alloc`: iterates over the original `&str`.
//!
//! Read by AriBoot (to know whether to exit QEMU on failure) and
//! AriKernel (test mode, injected failures...). File on the ESP:
//! `\aricodeos\cmdline.txt`.

#![no_std]

#[derive(Debug, Clone, Copy)]
pub struct CmdLine<'a> {
    raw: &'a str,
}

impl<'a> CmdLine<'a> {
    pub const fn new(raw: &'a str) -> Self {
        Self { raw }
    }

    /// Builds from bytes; non-UTF-8 content yields an empty line.
    pub fn from_bytes(bytes: &'a [u8]) -> Self {
        Self {
            raw: core::str::from_utf8(bytes).unwrap_or(""),
        }
    }

    pub fn raw(&self) -> &'a str {
        self.raw
    }

    pub fn is_empty(&self) -> bool {
        self.entries().next().is_none()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&'a str, Option<&'a str>)> {
        self.raw
            .split_whitespace()
            .map(|tok| match tok.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (tok, None),
            })
    }

    /// `true` if `key` appears (with or without a value).
    pub fn has(&self, key: &str) -> bool {
        self.entries().any(|(k, _)| k == key)
    }

    /// `true` only if `key` appears bare, without `=value`: a switch such as
    /// `selftest` must not be turned on by `selftest=0`.
    pub fn has_flag(&self, key: &str) -> bool {
        self.entries().any(|(k, v)| k == key && v.is_none())
    }

    /// First key on the line that belongs to `set` (with or without a
    /// value), if any: e.g. [`keys::TEST_ONLY`] or [`keys::LOADER_ONLY`],
    /// which a command line for a real machine must not carry.
    pub fn first_of(&self, set: &[&str]) -> Option<&'a str> {
        self.entries().map(|(k, _)| k).find(|k| set.contains(k))
    }

    /// The line's tokens (`key` or `key=value`) whose key is not in `set`,
    /// in order: AriBoot drops [`keys::LOADER_ONLY`] from `cmdline.txt`
    /// with it before adding its own.
    pub fn tokens_without<'s>(&self, set: &'s [&'s str]) -> impl Iterator<Item = &'a str> + 's
    where
        'a: 's,
    {
        self.raw
            .split_whitespace()
            .filter(move |tok| !set.contains(&tok.split('=').next().unwrap_or(tok)))
    }

    /// Value of the last occurrence of `key=value`.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.entries()
            .filter(|(k, _)| *k == key)
            .last()
            .and_then(|(_, v)| v)
    }
}

/// Recognised keys. Centralised so that bootloader and kernel agree.
pub mod keys {
    /// Tests only: exit QEMU with `isa-debug-exit` on completion (success)
    /// or failure (error).
    pub const QEMU_EXIT: &str = "qemu_exit";
    /// Tests only: an injected failure or kernel self-test, `test=panic`.
    pub const TEST: &str = "test";
    /// Maximum log level: `log=trace|debug|info|warn|error|off`.
    pub const LOG: &str = "log";
    /// `shell`: init launches the interactive shell after the boot services.
    pub const SHELL: &str = "shell";
    /// `gui`: the kernel hands the framebuffer to the user-space compositor.
    pub const GUI: &str = "gui";
    /// `kbd=us|es`: PS/2 keyboard layout of the console (US by default).
    pub const KBD: &str = "kbd";
    /// `boot_slot=A|B`: AriBoot adds it when booting from a slot
    /// partition; `boot_pending` comes along if the slot awaits confirmation.
    pub const BOOT_SLOT: &str = "boot_slot";
    /// Added by AriBoot with `boot_slot` (see there).
    pub const BOOT_PENDING: &str = "boot_pending";
    /// `boot_version=N`: added by AriBoot with `boot_slot`, the version (or
    /// development build number) in the booted image's signed manifest.
    pub const BOOT_VERSION: &str = "boot_version";
    /// `boot_channel=stable|dev`: added by AriBoot with `boot_slot`, the
    /// channel of the booted image (ADR 0045).
    pub const BOOT_CHANNEL: &str = "boot_channel";
    /// `boot_commit=HEX`: added by AriBoot with `boot_slot` for a
    /// development build, the commit it was built from (ADR 0045).
    pub const BOOT_COMMIT: &str = "boot_commit";
    /// Tests only: `init_fail_late`, init fails after the services and before
    /// confirming.
    pub const INIT_FAIL_LATE: &str = "init_fail_late";
    /// Tests only: `restart_storm`, init subjects a test service to 100
    /// restarts.
    pub const RESTART_STORM: &str = "restart_storm";
    /// Tests only: `service_hang`, init launches a test service that never
    /// answers and checks that the supervisor kills it when its probe
    /// expires.
    pub const SERVICE_HANG: &str = "service_hang";
    /// Tests only: `input_restart`, init supervises an input driver that
    /// crashes holding a client's read and checks the client keeps the
    /// device (ADR 0030).
    pub const INPUT_RESTART: &str = "input_restart";
    /// Tests only: `shell_fault`, the shell accepts `fault`, which makes it
    /// crash on purpose (a session shell that dies, ADR 0030).
    pub const SHELL_FAULT: &str = "shell_fault";
    /// `smp=off`: do not start the application processors (real hardware).
    pub const SMP: &str = "smp";
    /// `iommu=off`: do not enable DMA translation (real hardware).
    pub const IOMMU: &str = "iommu";
    /// `pci.flr=off`: never reset a PCI function (FLR) when its driver
    /// dies; bus mastering is still turned off (recovery knob for real
    /// hardware, ADR 0038).
    pub const PCI_FLR: &str = "pci.flr";
    /// Failure protocol (ADR 0028): a component halts on its first
    /// unexpected failure and init restarts nothing.
    pub const STRICT: &str = "strict";
    /// Tests only (incident 0005, ADR 0032): init also runs its test-only
    /// boot checks, which write to the disk and AriFS and talk to the QEMU
    /// test hosts at 10.0.2.2. Never set on a real machine: without it init
    /// runs only the read-only health checks. Only the bare key counts
    /// (`CmdLine::has_flag`): `selftest=0` does not enable it. xtask adds it
    /// to every QEMU scenario unless the scenario says `selftest = false`.
    pub const SELFTEST: &str = "selftest";

    /// Tests only: `usb_fault=WHEN[:KIND]` injects one failure into the USB
    /// driver. `WHEN` is `N` (the N-th block request, or the N-th HID
    /// report for `hid`) or `idle` (the first one after two idle seconds).
    /// `KIND` is `cbw` (the default: a transaction error reported on the
    /// CBW), `csw` (a CSW read with no command outstanding, so the bulk IN
    /// endpoint really halts with a STALL and the halted-endpoint recovery
    /// runs), `short-csw` (the CSW read of the request is made to look as
    /// if the device moved only 8 of the 13 bytes, the previous command's
    /// status still in the buffer: an invalid CSW per BOT 5.2 and 6.3,
    /// followed by a real Reset Recovery; incident 0007), `short-data` (the
    /// data stage of the request is reported one sector short although the
    /// command passed: the request answers Io with the bytes not
    /// transferred, no recovery is needed; incident 0010), `reenum` (the
    /// storage endpoint is given up: Disable Slot or Reset Device and the
    /// whole re-enumeration run), `unreleased` (like `reenum`, but the
    /// Disable Slot completion is read as Context State Error: the slot
    /// was not released, so the request's buffer must be kept; incident
    /// 0017) or `hid` (a HID report is treated as a
    /// transaction error: the interrupt endpoint is stopped and recovered
    /// from the idle loop, its report TRBs queued again).
    /// The EHCI driver (ADR 0031) understands `N` and `idle` with `cbw`
    /// (followed by a real Reset Recovery), `csw`, `short-csw` and
    /// `short-data`.
    pub const USB_FAULT: &str = "usb_fault";
    /// Tests only: `blk_fault=LBA` makes the virtio-blk driver leave the
    /// device unnotified of the first client request that covers `LBA` (a
    /// lost doorbell): the request times out for real while its descriptor
    /// chain stays in the ring, and the device completes it on the next
    /// notification; init then runs the `blk_late` test client on that
    /// LBA (incident 0017).
    pub const BLK_FAULT: &str = "blk_fault";
    /// Tests only: `gui_trace`, the compositor logs every frame it puts on
    /// screen over serial (frame number, windows, cursor, pixels flushed)
    /// with one pixel sample per window, which the GUI scenarios check.
    /// Without it a frame prints nothing: the serial port busy-waits.
    pub const GUI_TRACE: &str = "gui_trace";

    /// Every key documented "Tests only": they exit QEMU, inject failures or
    /// run test programs. `cargo xtask usb-image` refuses them (a unit test
    /// keeps this list and the documentation in step).
    pub const TEST_ONLY: &[&str] = &[
        QEMU_EXIT,
        TEST,
        INIT_FAIL_LATE,
        RESTART_STORM,
        SERVICE_HANG,
        INPUT_RESTART,
        SHELL_FAULT,
        SELFTEST,
        USB_FAULT,
        BLK_FAULT,
        GUI_TRACE,
    ];

    /// Keys AriBoot adds itself; `cargo xtask usb-image` refuses them too.
    /// AriBoot drops them from `cmdline.txt` before adding its own, so what
    /// the system reads about its slot and image always comes from AriBoot.
    pub const LOADER_ONLY: &[&str] = &[
        BOOT_SLOT,
        BOOT_PENDING,
        BOOT_VERSION,
        BOOT_CHANNEL,
        BOOT_COMMIT,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_values() {
        let c = CmdLine::new("  qemu_exit test=panic  log=debug ");
        assert!(c.has("qemu_exit"));
        assert!(c.has("test"));
        assert_eq!(c.get("test"), Some("panic"));
        assert_eq!(c.get("log"), Some("debug"));
        assert_eq!(c.get("qemu_exit"), None);
        assert!(!c.has("missing"));
        assert!(!c.is_empty());
    }

    #[test]
    fn selftest_is_a_bare_whole_key() {
        assert!(CmdLine::new("shell selftest").has_flag(keys::SELFTEST));
        assert!(!CmdLine::new("shell kbd=es").has_flag(keys::SELFTEST));
        assert!(!CmdLine::new("shell selftests no_selftest").has_flag(keys::SELFTEST));
        for off in ["selftest=0", "selftest=", "selftest=1"] {
            assert!(!CmdLine::new(off).has_flag(keys::SELFTEST), "{off}");
        }
    }

    #[test]
    fn test_only_keys_match_their_documentation() {
        // Each `pub const NAME: &str = "key";` of `keys` with the doc
        // comment right above it: "Tests only" there <=> in TEST_ONLY.
        let source = include_str!("lib.rs");
        let module = &source[source.find("pub mod keys {").unwrap()..];
        let mut documented = false;
        let mut checked = 0;
        for line in module.lines().map(str::trim) {
            if let Some(text) = line.strip_prefix("///") {
                // Case-insensitive: "tests only" in mid-sentence counts too.
                documented |= text
                    .as_bytes()
                    .windows(10)
                    .any(|w| w.eq_ignore_ascii_case(b"tests only"));
            } else if let Some(rest) = line.strip_prefix("pub const ") {
                if let Some((_, value)) = rest.split_once(": &str = \"") {
                    let key = value.trim_end_matches("\";");
                    assert_eq!(
                        documented,
                        keys::TEST_ONLY.contains(&key),
                        "`{key}`: documented \"Tests only\" = {documented}, TEST_ONLY disagrees"
                    );
                    checked += 1;
                }
                documented = false;
            } else {
                documented = false;
            }
        }
        assert!(checked >= 18, "only {checked} keys parsed");
        assert!(keys::LOADER_ONLY
            .iter()
            .all(|k| !keys::TEST_ONLY.contains(k)));
    }

    #[test]
    fn test_only_keys_are_found() {
        let first = |s| CmdLine::new(s).first_of(keys::TEST_ONLY);
        assert_eq!(first("shell kbd=es strict log=debug"), None);
        assert_eq!(first("shell selftest"), Some("selftest"));
        assert_eq!(first("qemu_exit shell"), Some("qemu_exit"));
        assert_eq!(first("shell test=panic"), Some("test"));
        assert_eq!(first("usb_fault=idle shell"), Some("usb_fault"));
        assert_eq!(first("shell usb_fault=idle:csw"), Some("usb_fault"));
        assert_eq!(first("shell usb_fault=idle:short-csw"), Some("usb_fault"));
        assert_eq!(first("shell usb_fault=idle:short-data"), Some("usb_fault"));
        assert_eq!(first("shell usb_fault=3:hid"), Some("usb_fault"));
        assert_eq!(first("shell blk_fault=4300"), Some("blk_fault"));
        assert_eq!(first("gui gui_trace"), Some("gui_trace"));
        assert_eq!(first("selftest=0"), Some("selftest"));
        let loader = |s| CmdLine::new(s).first_of(keys::LOADER_ONLY);
        assert_eq!(loader("shell boot_slot=B"), Some("boot_slot"));
        assert_eq!(loader("shell boot_pending"), Some("boot_pending"));
        assert_eq!(loader("shell boot_channel=dev"), Some("boot_channel"));
        assert_eq!(loader("boot_commit=0123abc"), Some("boot_commit"));
        assert_eq!(loader("boot_version=7 shell"), Some("boot_version"));
        assert_eq!(loader("shell kbd=es"), None);
    }

    #[test]
    fn loader_keys_are_dropped_and_the_rest_kept_in_order() {
        let line = CmdLine::new("  shell boot_slot=A kbd=es boot_channel=dev boot_pending strict ");
        let kept: [&str; 3] = {
            let mut it = line.tokens_without(keys::LOADER_ONLY);
            [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
        };
        assert_eq!(kept, ["shell", "kbd=es", "strict"]);
        assert_eq!(line.tokens_without(keys::LOADER_ONLY).count(), 3);
        // A key that only starts like a loader key is not one.
        let line = CmdLine::new("boot_slots=x boot_commitment");
        assert_eq!(line.tokens_without(keys::LOADER_ONLY).count(), 2);
    }

    #[test]
    fn last_value_wins() {
        let c = CmdLine::new("a=1 a=2");
        assert_eq!(c.get("a"), Some("2"));
    }

    #[test]
    fn empty_and_invalid_utf8() {
        assert!(CmdLine::new("").is_empty());
        assert!(CmdLine::from_bytes(&[0xFF, 0xFE]).is_empty());
        assert_eq!(CmdLine::from_bytes(b"x=y\n").get("x"), Some("y"));
    }
}
