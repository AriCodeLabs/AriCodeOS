// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! The kernel command line kept for user space (`sysinfo::CMDLINE`):
//! programs read boot flags such as `strict` or test knobs directly
//! (ADR 0028).

use crate::sync::IrqLock;
use alloc::string::String;

static ARGS: IrqLock<Option<String>> = IrqLock::new(None);

pub fn set(raw: &str) {
    *ARGS.lock() = Some(String::from(raw));
}

/// A copy of the command line (empty if none).
pub fn get() -> String {
    ARGS.lock().clone().unwrap_or_default()
}
