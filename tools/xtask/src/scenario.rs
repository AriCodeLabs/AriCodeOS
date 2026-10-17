// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! QEMU test scenarios declared in `tests/qemu/*.toml`.

use crate::{root, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Host unit tests only (seconds).
    Unit,
    /// Unit + the nominal QEMU boot. Default level and minimum for `develop`.
    Smoke,
    /// Every QEMU scenario, including induced failures. Required for `main`.
    Full,
}

impl std::str::FromStr for Level {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "unit" => Ok(Self::Unit),
            "smoke" => Ok(Self::Smoke),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown level '{other}' (unit|smoke|full)")),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inject {
    /// Truncates `kernel.elf` to N bytes before booting.
    pub truncate_kernel: Option<usize>,
    /// Removes `kernel.elf` from the ESP.
    #[serde(default)]
    pub remove_kernel: bool,
    /// Overwrites the kernel's `e_entry` with this value.
    pub patch_entry: Option<u64>,
    /// Adds an empty RW `PT_LOAD` (`memsz = 0`, alignment 1) at `e_entry`.
    #[serde(default)]
    pub add_empty_segment_at_entry: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostTcp {
    pub marker: String,
    pub payload: String,
}

/// SSH from the host (ADR 0025): a client key is generated and installed
/// as `authorized_keys` in AriFS; on seeing `marker` the host's `ssh` runs
/// each command through the forwarded port and its output joins the log
/// as `[host] ssh ...` lines.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSsh {
    pub marker: String,
    pub commands: Vec<String>,
    /// Opens a TCP connection to port 22 first and keeps it idle (never
    /// sends its identification) while the commands run: they must still
    /// be served once sshd drops the idle client.
    #[serde(default)]
    pub idle_client: bool,
}

/// What the scenario's update source offers and how the machine knows it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostUpdate {
    /// System image version offered; no system image when absent.
    pub system: Option<u32>,
    /// App packages offered: `DIR:VERSION`.
    #[serde(default)]
    pub apps: Vec<String>,
    /// The index is altered after signing (the updater must refuse it).
    #[serde(default)]
    pub tamper_index: bool,
    /// The image is altered after the index recorded its SHA-256 (the
    /// updater must refuse the download).
    #[serde(default)]
    pub tamper_image: bool,
    /// The machine already holds a verified copy of this source's index
    /// with a higher serial (the published one must be refused as stale).
    #[serde(default)]
    pub stale_index: bool,
    /// How the machine knows the source: `sources` (registered in
    /// `updater/sources`), `legacy-url` (the ADR 0027 `updater/update_url`,
    /// to be migrated) or `none` (the scenario adds it from the shell).
    #[serde(default = "default_update_source")]
    pub source: String,
}

fn default_update_source() -> String {
    String::from("sources")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostInput {
    pub marker: String,
    #[serde(default)]
    pub keys: Vec<String>,
    pub mouse_rel: Option<(i32, i32)>,
    pub click: Option<String>,
    /// Pause after each key event (ms). QEMU's usb-kbd queues at most 16
    /// scancodes (`QUEUE_LENGTH`, hw/input/hid.c) and drops whole events
    /// beyond that, while the guest drains it only at the interrupt
    /// endpoint's polling rate: more than 8 keys typed at QMP speed lose
    /// keystrokes on the USB keyboard.
    #[serde(default)]
    pub key_delay_ms: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Expected QEMU exit code. `isa-debug-exit` returns
    /// `(value << 1) | 1`: the system writes 0x10 on success (33) and 0x11
    /// on a failure reported by AriBoot or AriKernel (35). A `reboot` ends
    /// QEMU with 0 (`-no-reboot`).
    pub exit: Option<i32>,
    #[serde(default)]
    pub serial_contains: Vec<String>,
    #[serde(default)]
    pub serial_absent: Vec<String>,
    /// The runner had to kill QEMU on seeing `kill_on` (power cut).
    #[serde(default)]
    pub killed: bool,
}

impl Expect {
    fn is_empty(&self) -> bool {
        self.exit.is_none()
            && self.serial_contains.is_empty()
            && self.serial_absent.is_empty()
            && !self.killed
    }
}

/// One boot of a multi-boot scenario (`[[boots]]`): the test disk is kept
/// across boots and each one has its own cmdline, console lines and
/// expectations.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boot {
    #[serde(default = "default_cmdline")]
    pub cmdline: String,
    #[serde(default)]
    pub stdin: Vec<String>,
    /// Kills QEMU (power cut) as soon as the serial output shows this text.
    pub kill_on: Option<String>,
    pub expect: Expect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub level: Level,
    pub areas: Vec<String>,
    /// Contents of `aricodeos/cmdline.txt`. `qemu_exit` makes the system
    /// exit QEMU instead of staying in `hlt`.
    #[serde(default = "default_cmdline")]
    pub cmdline: String,
    /// Adds `selftest` to the cmdline of every boot (default): init runs
    /// its test-only boot checks, which write to the test disk and talk to
    /// the QEMU test hosts (incident 0005, ADR 0032). `false` boots like a
    /// real machine: only the read-only health checks.
    #[serde(default = "default_selftest")]
    pub selftest: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Lines the runner sends over the serial console, one each time the
    /// output shows `prompt` (the first one at the first prompt).
    #[serde(default)]
    pub stdin: Vec<String>,
    /// Text announcing that the system is waiting for a line.
    #[serde(default = "default_prompt")]
    pub prompt: String,
    /// Host TCP probe: on seeing `marker` it connects to the port forwarded
    /// to the guest, sends `payload` and records the echo as `[host] ...`.
    pub host_tcp: Option<HostTcp>,
    /// QMP input injection on seeing `marker`: keys (QEMU qcode names) on
    /// the virtio keyboard, relative motion and a click on the mouse.
    pub host_input: Option<HostInput>,
    #[serde(default)]
    pub host_ssh: Option<HostSsh>,
    /// HTTPS servers on the host (`openssl s_server -www`): one with a
    /// certificate for `10.0.2.2` from a test CA installed in AriFS as
    /// `trusted_ca.der`, one self-signed and untrusted.
    #[serde(default)]
    pub host_https: bool,
    /// An update source published into `build/update-test/` and served by
    /// the test HTTP server at `http://10.0.2.2:17010/update/` (ADR 0029).
    pub host_update: Option<HostUpdate>,
    /// Initial state of the test disk (`image::DiskVariant`): `slots`
    /// (default), `update-empty`, `esp-only`, `pending-b`, `b-exhausted`, `b-corrupt`,
    /// `fs-pending-replace`, `fs-corrupt`, `wasm-restricted`, `wasm-invalid`,
    /// `wasm-bad-import`.
    #[serde(default = "default_disk")]
    pub disk: String,
    /// Presents the test disk as read-only (write failures).
    #[serde(default)]
    pub disk_readonly: bool,
    /// Virtual machine processors (`-smp N`). When undeclared it is 1 and
    /// `cargo xtask test --cpus N` overrides it; an explicit value is always
    /// honoured (the scenario depends on that CPU count).
    pub cpus: Option<u32>,
    /// Adds an xHCI controller with a USB HID keyboard and mouse. QMP events
    /// go to the most recently activated input handler: the USB keyboard
    /// only while no virtio-input driver has started (QEMU activates a
    /// virtio keyboard when its guest driver sets DRIVER_OK, after the
    /// usb-kbd activated itself at realize), so under `shell`, where init
    /// starts the virtio-input drivers, they reach the virtio keyboard
    /// unless `usb_hid_only_input` is set.
    #[serde(default)]
    pub usb_hid: bool,
    /// No virtio keyboard and mouse: the USB HID devices are the only
    /// input devices, so QMP input reaches them whatever the guest starts.
    #[serde(default)]
    pub usb_hid_only_input: bool,
    /// The `usb_hid` xHCI offers MSI only (no MSI-X), like 2012 Intel
    /// controllers: the kernel must program the MSI capability.
    #[serde(default)]
    pub usb_hid_msi: bool,
    /// The test disk is attached over USB (`usb-storage` on the xHCI) and
    /// there is no virtio-blk: usbd serves it as a block service (ADR 0023).
    #[serde(default)]
    pub usb_storage: bool,
    /// Also adds a pre-xHCI USB controller (EHCI): init must neither hand it
    /// to the xHCI driver nor hang on it.
    #[serde(default)]
    pub usb_legacy_controller: bool,
    #[serde(default)]
    pub inject: Inject,
    /// What is expected from the single boot (without `[[boots]]`).
    #[serde(default)]
    pub expect: Expect,
    /// Several boots on the same test disk (see [`Boot`]); with them the
    /// scenario's `cmdline`, `stdin` and `expect` are not used.
    #[serde(default)]
    pub boots: Vec<Boot>,
    /// External applications (directories with `Cargo.toml` and
    /// `NAME.manifest`, relative to the root) that xtask builds, packs as
    /// version 1 and installs into the test disk's AriFS as
    /// `apps/NAME.aripkg` (ADR 0017).
    #[serde(default)]
    pub install: Vec<String>,
    /// Pending packages in the test disk's `pkgs/`: `DIR:VERSION` or
    /// `DIR:VERSION:corrupt` (tampered signature), for the installer (ADR 0018).
    #[serde(default)]
    pub packages: Vec<String>,
    /// Packages already staged by the updater: `DIR:VERSION`, or
    /// `DIR:VERSION:wrong-hash` (the staged list's SHA-256 does not match
    /// the stage file), for `run installer 2` (ADR 0029).
    #[serde(default)]
    pub staged: Vec<String>,
    /// Adds a staged line naming a file outside the stage files.
    #[serde(default)]
    pub staged_bad_path: bool,
    #[serde(skip)]
    pub file: PathBuf,
}

/// A scenario name is also a directory name under `build/scenarios/`:
/// only `[a-z0-9._-]`, not starting with `.`, no `..`.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid scenario name '{name}': use [a-z0-9._-], no '/', no '..', no leading '.'"
        ))
    }
}

fn default_cmdline() -> String {
    "qemu_exit".into()
}
fn default_selftest() -> bool {
    true
}
fn default_timeout() -> u64 {
    30
}

fn default_prompt() -> String {
    String::from("ari> ")
}

fn default_disk() -> String {
    String::from("slots")
}

pub fn scenarios_dir() -> PathBuf {
    root().join("tests/qemu")
}

/// Loads every scenario in `tests/qemu/`, sorted by name.
pub fn load_all() -> Result<Vec<Scenario>> {
    load_from(&scenarios_dir())
}

/// Loads the scenarios in a directory. Invalid TOML or a disallowed name is
/// an error: a badly written test must not pass silently.
pub fn load_from(dir: &Path) -> Result<Vec<Scenario>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|x| x == "toml") {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut s: Scenario =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if s.level == Level::Unit {
                return Err(format!(
                    "{}: a QEMU scenario cannot be level unit",
                    path.display()
                ));
            }
            validate_name(&s.name).map_err(|e| format!("{}: {e}", path.display()))?;
            if s.boots.is_empty() == s.expect.is_empty() {
                return Err(format!(
                    "{}: a scenario has [expect] or [[boots]], not both and not neither",
                    path.display()
                ));
            }
            if !s.boots.is_empty() && !s.stdin.is_empty() {
                return Err(format!(
                    "{}: with [[boots]] the console lines go in each boot",
                    path.display()
                ));
            }
            s.file = path;
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

impl Scenario {
    /// What a boot of this scenario writes to `aricodeos/cmdline.txt`: its
    /// declared `cmdline` plus `selftest` unless the scenario opts out.
    pub fn boot_cmdline(&self, cmdline: &str) -> String {
        let key = aricode_cmdline::keys::SELFTEST;
        if self.selftest && !aricode_cmdline::CmdLine::new(cmdline).has(key) {
            format!("{} {key}", cmdline.trim_end())
                .trim_start()
                .to_string()
        } else {
            cmdline.to_string()
        }
    }

    /// CPUs the scenario boots with: the declared ones, or the value forced
    /// by `--cpus`, or 1.
    pub fn cpus(&self, forced: Option<u32>) -> u32 {
        self.cpus.or(forced).unwrap_or(1)
    }

    pub fn matches(&self, max_level: Level, areas: &[String]) -> bool {
        self.level <= max_level
            && (areas.is_empty() || self.areas.iter().any(|a| areas.contains(a)))
    }
}

/// Selects scenarios by level and areas. For smoke/full, an empty selection
/// is an error: a mistyped filter must not succeed without booting anything.
pub fn select<'a>(
    all: &'a [Scenario],
    level: Level,
    areas: &[String],
) -> Result<Vec<&'a Scenario>> {
    let selected: Vec<&Scenario> = all.iter().filter(|s| s.matches(level, areas)).collect();
    if selected.is_empty() && level > Level::Unit {
        let mut known: Vec<&str> = all
            .iter()
            .flat_map(|s| s.areas.iter().map(String::as_str))
            .collect();
        known.sort_unstable();
        known.dedup();
        let higher: Vec<&str> = all
            .iter()
            .filter(|s| s.level > level && s.areas.iter().any(|a| areas.contains(a)))
            .map(|s| s.name.as_str())
            .collect();
        let mut msg = format!(
            "no QEMU scenario matches level {level:?} and areas [{}]. Known areas: {}",
            areas.join(","),
            known.join(", ")
        );
        if !higher.is_empty() {
            msg.push_str(&format!(
                ". With --all these would run: {}",
                higher.join(", ")
            ));
        }
        return Err(msg);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_scenarios_have_known_disk_variants() {
        for sc in load_all().expect("scenarios") {
            assert!(
                crate::image::DiskVariant::parse(&sc.disk).is_some(),
                "{}: unknown disk '{}'",
                sc.name,
                sc.disk
            );
        }
    }

    #[test]
    fn repo_scenarios_are_valid_and_unique() {
        let all = load_all().expect("tests/qemu/*.toml must parse");
        assert!(!all.is_empty());
        let mut names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
        names.dedup();
        assert_eq!(names.len(), all.len(), "duplicate names");
        assert!(
            all.iter().any(|s| s.level == Level::Smoke),
            "there must be at least one smoke"
        );
        for s in &all {
            assert!(!s.areas.is_empty(), "{}: no areas", s.name);
        }
    }

    fn scenario(name: &str, level: &str, areas: &str) -> Scenario {
        toml::from_str(&format!(
            "name = \"{name}\"\nlevel = \"{level}\"\nareas = [{areas}]\n[expect]\nexit = 33\n"
        ))
        .unwrap()
    }

    #[test]
    fn empty_selection_is_an_error_for_qemu_levels() {
        let all = vec![
            scenario("boot.ok", "smoke", "\"boot\""),
            scenario("boot.panic", "full", "\"boot\", \"panic\""),
        ];
        // nonexistent area (audit, finding 5)
        let err = select(&all, Level::Smoke, &["booot".into()]).unwrap_err();
        assert!(
            err.contains("booot") && err.contains("boot, panic"),
            "{err}"
        );
        // area whose only scenario is full
        let err = select(&all, Level::Smoke, &["panic".into()]).unwrap_err();
        assert!(
            err.contains("With --all these would run: boot.panic"),
            "{err}"
        );
        // at unit level an empty selection is normal
        assert!(select(&all, Level::Unit, &["booot".into()])
            .unwrap()
            .is_empty());
        assert_eq!(select(&all, Level::Full, &[]).unwrap().len(), 2);
    }

    #[test]
    fn load_rejects_invalid_names() {
        // Audit review, finding 3: validation must happen at load time.
        let tmp = crate::test_util::TempDir::new("scenarios-load");
        std::fs::write(
            tmp.path().join("bad.toml"),
            "name = \"../escape\"\nlevel = \"full\"\nareas = [\"boot\"]\n[expect]\nexit = 33\n",
        )
        .unwrap();
        let err = load_from(tmp.path()).unwrap_err();
        assert!(err.contains("invalid scenario name"), "{err}");
    }

    #[test]
    fn scenario_names_cannot_escape_scenarios_dir() {
        for bad in [
            "/tmp/x",
            "../x",
            "a/b",
            "a\\b",
            ".hidden",
            "",
            "Boot.Ready",
            "a b",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["boot.kernel_ready", "memory.oom-2", "x"] {
            assert!(validate_name(good).is_ok(), "{good:?} should be accepted");
        }
    }

    #[test]
    fn level_filter_is_inclusive() {
        let s: Scenario = toml::from_str(
            r#"
            name = "x"
            level = "full"
            areas = ["boot"]
            [expect]
            exit = 35
            "#,
        )
        .unwrap();
        assert!(s.matches(Level::Full, &[]));
        assert!(!s.matches(Level::Smoke, &[]));
        assert!(s.matches(Level::Full, &["boot".into()]));
        assert!(!s.matches(Level::Full, &["memory".into()]));
        assert_eq!(s.cmdline, "qemu_exit");
        assert_eq!(s.timeout_secs, 30);
    }

    #[test]
    fn selftest_is_added_unless_the_scenario_opts_out() {
        let mut s = scenario("x", "full", "\"init\"");
        assert!(s.selftest);
        assert_eq!(
            s.boot_cmdline("qemu_exit shell"),
            "qemu_exit shell selftest"
        );
        assert_eq!(s.boot_cmdline("qemu_exit selftest"), "qemu_exit selftest");
        assert_eq!(s.boot_cmdline(""), "selftest");
        s.selftest = false;
        assert_eq!(s.boot_cmdline("qemu_exit shell"), "qemu_exit shell");
        let off: Scenario = toml::from_str(
            "name = \"y\"\nlevel = \"full\"\nareas = [\"init\"]\nselftest = false\n[expect]\nexit = 33\n",
        )
        .unwrap();
        assert!(!off.selftest);
    }

    #[test]
    fn repo_has_a_scenario_without_selftest() {
        // At least one scenario boots like a real machine (incident 0005).
        assert!(load_all().expect("scenarios").iter().any(|s| !s.selftest));
    }

    #[test]
    fn nearly_full_arifs_scenarios_boot_without_selftest() {
        // `fill_arifs` leaves exactly the entries the updater needs; the
        // selftest `fs_client` would take one for `nuevo.txt`.
        for s in load_all().expect("scenarios") {
            if matches!(s.disk.as_str(), "fs-nearly-full" | "fs-one-short") {
                assert!(!s.selftest, "{}: needs selftest = false", s.name);
            }
        }
    }
}
