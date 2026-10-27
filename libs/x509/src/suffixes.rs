// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Public suffixes a wildcard may not stand on (ADR 0062): `*.co.uk` would
//! cover every company registered under `co.uk`. The rules are the ICANN
//! section of the Public Suffix List that can name a wildcard's base (two
//! labels or more, wildcard and exception rules), extracted by
//! `psl/update.sh` into `psl/icann-wildcard-bases.dat` (MPL-2.0). The
//! private section (`github.io`, cloud hosting names) is left out, as
//! Chrome leaves it out for certificates: its operators hold wildcard
//! certificates for their own suffixes.

const RULES: &str = include_str!("../psl/icann-wildcard-bases.dat");

fn rules() -> impl Iterator<Item = &'static str> {
    RULES
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
}

/// True if `name` (labels in ASCII, no trailing dot, any case) is a public
/// suffix of the ICANN section: a rule names it, or a wildcard rule names
/// its parent and no exception rule names it. Single-label names are not
/// in the extract: callers refuse them before asking.
pub(crate) fn is_public_suffix(name: &str) -> bool {
    let parent = name.split_once('.').map(|(_, parent)| parent);
    let (mut listed, mut excepted) = (false, false);
    for rule in rules() {
        if let Some(exception) = rule.strip_prefix('!') {
            excepted |= exception.eq_ignore_ascii_case(name);
        } else if let Some(base) = rule.strip_prefix("*.") {
            listed |= parent.is_some_and(|p| p.eq_ignore_ascii_case(base));
        } else {
            listed |= rule.eq_ignore_ascii_case(name);
        }
    }
    listed && !excepted
}
