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
    /// Exit QEMU with `isa-debug-exit` on completion (success) or failure (error).
    pub const QEMU_EXIT: &str = "qemu_exit";
    /// Injected failure for tests: `test=panic`.
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
    pub const BOOT_PENDING: &str = "boot_pending";
    /// `init_fail_late`: init fails after the services and before confirming.
    pub const INIT_FAIL_LATE: &str = "init_fail_late";
    /// `restart_storm`: init subjects a test service to 100 restarts.
    pub const RESTART_STORM: &str = "restart_storm";
    /// `service_hang`: init launches a test service that never answers and
    /// checks that the supervisor kills it when its probe expires.
    pub const SERVICE_HANG: &str = "service_hang";
    /// `smp=off`: do not start the application processors (real hardware).
    pub const SMP: &str = "smp";
    /// `iommu=off`: do not enable DMA translation (real hardware).
    pub const IOMMU: &str = "iommu";
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
