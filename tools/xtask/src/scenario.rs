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
    /// Installs a host key in the AriFS root, where releases before ADR
    /// 0050 kept it (`OLD_HOST_KEY_SEED`, public, tests only): once init
    /// has moved it into `sshd/`, sshd must still listen with it (the
    /// runner checks its fingerprint).
    #[serde(default)]
    pub old_host_key: bool,
    /// A command whose connection fails (ssh exit 255) is tried again,
    /// once a second, for up to this many seconds: for scenarios where
    /// sshd is relaunched between commands. 0 (default): no retry.
    #[serde(default)]
    pub retry_connect_secs: u64,
}

/// Connection flood from the host (incident 0013): on seeing `marker` the
/// runner opens `connections` plain TCP connections to the guest's `port`
/// through its `hostfwd`, sends nothing and holds them until QEMU exits;
/// the console lines are typed only once they are open. QEMU completes the
/// handshake itself, so the guest sees established connections nobody
/// accepts (a listener's backlog), not half-open ones. Port 22 installs the
/// scenario's SSH key so sshd listens.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFlood {
    pub marker: String,
    #[serde(default = "default_flood_port")]
    pub port: u16,
    pub connections: usize,
}

fn default_flood_port() -> u16 {
    22
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
    /// A development source (ADR 0045) offering this build number,
    /// published into the scenario's `dev/` directory and served at
    /// `http://10.0.2.2:{update_port}/dev/`.
    pub dev_build: Option<u32>,
    /// The commit that development build names.
    #[serde(default = "default_dev_commit")]
    pub dev_commit: String,
    /// The development source is signed with the stable key (the updater
    /// must refuse it).
    #[serde(default)]
    pub dev_wrong_key: bool,
    /// The development source is registered in `updater/sources` (`dev
    /// URL`); without it the scenario adds it from the shell, as the
    /// channel is closed by default.
    #[serde(default)]
    pub dev_source: bool,
    /// The development source's index offers a stable image under its
    /// build number (the updater must refuse to install it).
    #[serde(default)]
    pub dev_stable_image: bool,
    /// The machine starts with this stable floor in `updater/state` (the
    /// newest stable release it ran, ADR 0045 audit M1).
    pub stable_floor: Option<u32>,
}

fn default_update_source() -> String {
    String::from("sources")
}

fn default_dev_commit() -> String {
    String::from("0123abc")
}

/// `[host_hotplug]`: QEMU `device_add` of `driver` on `bus` with `id`, once
/// `marker` was seen and `delay_ms` more passed (letting the guest settle
/// first, e.g. a driver into its idle polling).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostHotplug {
    pub marker: String,
    #[serde(default)]
    pub delay_ms: u64,
    pub driver: String,
    pub bus: String,
    pub id: String,
}

impl HostHotplug {
    /// The names go into hand-written QMP JSON: plain names only.
    fn check(&self) -> Result {
        for (what, v) in [
            ("driver", &self.driver),
            ("bus", &self.bus),
            ("id", &self.id),
        ] {
            if v.is_empty()
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err(format!(
                    "[host_hotplug] {what} {v:?}: letters, digits, '.', '_' and '-' only"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostInput {
    pub marker: String,
    #[serde(default)]
    pub keys: Vec<String>,
    pub mouse_rel: Option<(i32, i32)>,
    pub click: Option<String>,
    /// Minimum pause after each key event (ms). QEMU's usb-kbd queues at
    /// most 16 scancodes (`QUEUE_LENGTH`, hw/input/hid.c) and drops whole
    /// events beyond that, while the guest drains it only at the interrupt
    /// endpoint's polling rate: more than 8 keys typed at QMP speed lose
    /// keystrokes on the USB keyboard. A wall-clock pause alone does not
    /// hold when the guest falls behind (incident 0022): see `echo_prompt`.
    #[serde(default)]
    pub key_delay_ms: u64,
    /// The guest prints this text before reading each line and echoes
    /// every typed character on serial (the shell: `ari> `). The runner
    /// then waits for it before the first key of each line and for each
    /// key's echo before sending the next (`typing`, incident 0022), so
    /// at most one key is queued in the device whatever the host load.
    pub echo_prompt: Option<String>,
}

/// Key codes QEMU's keyboards queue: usb-kbd (`QUEUE_LENGTH`,
/// hw/input/hid.c) and the i8042 (`PS2_QUEUE_SIZE`, hw/input/ps2.c).
const DEVICE_QUEUE: usize = 16;

impl HostInput {
    /// Incident 0022: the keys typed between two waits for the guest
    /// (`typing::plan` with `echo_prompt`, all of them without it) must fit
    /// in a device queue, counted in PS/2 bytes; a wall-clock delay does
    /// not drain the queue when the guest falls behind.
    fn check_queue(&self) -> std::result::Result<(), String> {
        use crate::typing::{longest_unwaited_run, plan, Step};
        let steps = match self.echo_prompt {
            Some(_) => plan(&self.keys),
            None => self.keys.iter().map(|k| Step::Key(k)).collect(),
        };
        let run = longest_unwaited_run(&steps);
        if run > DEVICE_QUEUE {
            return Err(format!(
                "[host_input] types {run} key codes without waiting for the guest, more than the {DEVICE_QUEUE} QEMU's keyboards queue: set echo_prompt (incident 0022)"
            ));
        }
        Ok(())
    }
}

/// The kernel seed store in the test disk's `ARIBOOT` after a boot (ADR
/// 0043), as the runner reads it from the disk image (`image::seed_record`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeedStore {
    /// No intact seed copy.
    Absent,
    /// An intact seed different from the one the disk held before this
    /// boot (a fresh disk holds none).
    Changed,
}

impl SeedStore {
    /// `None` if `after` (the record after the boot) matches, given
    /// `before` (the record before it); otherwise what went wrong. The
    /// seed itself is never part of the message.
    pub fn check(
        self,
        before: Option<&aricode_ab::seed::Record>,
        after: Option<&aricode_ab::seed::Record>,
    ) -> Option<String> {
        match (self, after) {
            (Self::Absent, None) => None,
            (Self::Absent, Some(r)) => Some(format!(
                "seed_store: the disk holds a seed (generation {}), expected none",
                r.generation
            )),
            (Self::Changed, None) => Some(String::from(
                "seed_store: the disk holds no seed after the boot",
            )),
            (Self::Changed, Some(r)) if before == Some(r) => Some(format!(
                "seed_store: the seed did not change in this boot (generation {})",
                r.generation
            )),
            (Self::Changed, Some(r)) if before.is_some_and(|b| b.seed == r.seed) => Some(format!(
                "seed_store: the same seed under a new generation ({})",
                r.generation
            )),
            (Self::Changed, Some(_)) => None,
        }
    }
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
    /// Texts that must appear in this order (each one after the end of the
    /// previous one's first match from there on).
    #[serde(default)]
    pub serial_order: Vec<String>,
    /// The runner had to kill QEMU on seeing `kill_on` (power cut).
    #[serde(default)]
    pub killed: bool,
    /// The kernel seed store after this boot (ADR 0043).
    pub seed_store: Option<SeedStore>,
}

impl Expect {
    fn is_empty(&self) -> bool {
        self.exit.is_none()
            && self.serial_contains.is_empty()
            && self.serial_absent.is_empty()
            && self.serial_order.is_empty()
            && !self.killed
            && self.seed_store.is_none()
    }
}

/// Index in `order` of the first text not found after the previous one in
/// `serial` (`Expect::serial_order`), or `None` if they all appear in order.
pub fn first_out_of_order(serial: &str, order: &[String]) -> Option<usize> {
    let mut from = 0;
    for (i, needle) in order.iter().enumerate() {
        match serial[from..].find(needle.as_str()) {
            Some(at) => from += at + needle.len(),
            None => return Some(i),
        }
    }
    None
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
    /// Runs with no other scenario at the same time (`--jobs`, ADR 0040):
    /// for a scenario whose host-side timing depends on the guest keeping
    /// up in wall-clock time, which other runs on the host slow down. The
    /// scenario file says why.
    #[serde(default)]
    pub serial: bool,
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
    /// A device plugged in over QMP (`device_add`) `delay_ms` after the
    /// guest prints `marker`: hot-plug detection.
    pub host_hotplug: Option<HostHotplug>,
    #[serde(default)]
    pub host_ssh: Option<HostSsh>,
    /// Plain TCP connections held open against a guest listener (incident
    /// 0013), with the console lines typed once they are in place.
    pub host_flood: Option<HostFlood>,
    /// HTTPS servers on the host (`openssl s_server -www`): one with a
    /// certificate for `10.0.2.2` from a test CA installed in AriFS as
    /// `trusted_ca.der`, one self-signed and untrusted, on ports chosen per
    /// run: [`HTTPS_PORT`] and [`HTTPS_UNTRUSTED_PORT`].
    #[serde(default)]
    pub host_https: bool,
    /// An update source (ADR 0029) published into the scenario's directory
    /// and served by its own HTTP server at
    /// `http://10.0.2.2:{update_port}/update/` ([`UPDATE_PORT`]).
    pub host_update: Option<HostUpdate>,
    /// Initial state of the test disk (`image::DiskVariant`): `slots`
    /// (default), `update-empty`, `esp-only`, `pending-b`, `b-exhausted`, `b-corrupt`,
    /// `fs-pending-replace`, `fs-corrupt`, `wasm-restricted`, `wasm-invalid`,
    /// `wasm-bad-import`, `wasm-many-blocks`.
    #[serde(default = "default_disk")]
    pub disk: String,
    /// Presents the test disk as read-only (write failures).
    #[serde(default)]
    pub disk_readonly: bool,
    /// Virtual machine processors (`-smp N`). When undeclared it is 1 and
    /// `cargo xtask test --cpus N` overrides it; an explicit value is always
    /// honoured (the scenario depends on that CPU count).
    pub cpus: Option<u32>,
    /// The CPU model exposes `rdrand` (`-cpu qemu64,+rdrand`; TCG
    /// implements it from the host's randomness), so the kernel generator
    /// is seeded from a hardware source (ADR 0039). Off, `qemu64` has none.
    #[serde(default)]
    pub cpu_rdrand: bool,
    /// Adds an xHCI controller with a USB HID keyboard and mouse. QMP events
    /// go to the most recently activated input handler: the USB keyboard
    /// only while no virtio-input driver has started (QEMU activates a
    /// virtio keyboard when its guest driver sets DRIVER_OK, after the
    /// usb-kbd activated itself at realize), so under `shell`, where init
    /// starts the virtio-input drivers, they reach the virtio keyboard
    /// unless `usb_hid_only_input` is set.
    #[serde(default)]
    pub usb_hid: bool,
    /// No virtio keyboard and mouse: the USB HID devices (`usb_hid` on the
    /// xHCI or `usb_ehci_hid` on the EHCI) are the only input devices, so
    /// QMP input reaches them whatever the guest starts.
    #[serde(default)]
    pub usb_hid_only_input: bool,
    /// The `usb_hid` xHCI offers MSI only (no MSI-X), like 2012 Intel
    /// controllers: the kernel must program the MSI capability.
    #[serde(default)]
    pub usb_hid_msi: bool,
    /// The `usb_hid` xHCI offers both MSI and MSI-X: a function whose
    /// MSI-X the kernel refuses falls back to MSI (ADR 0051).
    #[serde(default)]
    pub usb_hid_msi_and_msix: bool,
    /// The test disk is attached over USB (`usb-storage` on the xHCI) and
    /// there is no virtio-blk: usbd serves it as a block service (ADR 0023).
    #[serde(default)]
    pub usb_storage: bool,
    /// Also adds an EHCI with no devices: init launches the EHCI driver
    /// for it next to the xHCI driver (ADR 0031).
    #[serde(default)]
    pub usb_legacy_controller: bool,
    /// An EHCI with a USB keyboard and mouse; QMP events then go to them
    /// (the most recent handlers). ADR 0031.
    #[serde(default)]
    pub usb_ehci_hid: bool,
    /// The test disk is attached to an EHCI as `usb-storage` and there is
    /// no virtio-blk: the EHCI driver serves it as a block service.
    #[serde(default)]
    pub usb_ehci_storage: bool,
    /// An ICH9 EHCI with a UHCI companion and a full-speed USB keyboard:
    /// the EHCI driver hands the port to the companion (ADR 0031).
    #[serde(default)]
    pub usb_ehci_companion_kbd: bool,
    /// No network card: init finds neither virtio-net nor a Realtek and
    /// runs without a network service.
    #[serde(default)]
    pub no_nic: bool,
    /// Adds a PCI function that advertises Function Level Reset (an
    /// unused Intel 82576 `igb` NIC with no backend) for `test=pci_reap`
    /// (incident 0004).
    #[serde(default)]
    pub flr_device: bool,
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

/// Placeholders for ports chosen per run (ADR 0040). The guest reaches a
/// host port P at 10.0.2.2:P, so a scenario whose host server runs on a
/// port picked at run time writes the port as a placeholder in its console
/// lines, `kill_on` and expectations, and the runner fills it in. Each one
/// needs the field that starts its server.
pub const UPDATE_PORT: &str = "{update_port}";
pub const HTTPS_PORT: &str = "{https_port}";
pub const HTTPS_UNTRUSTED_PORT: &str = "{https_untrusted_port}";

/// `text` with each placeholder of `values` replaced by its port.
pub fn fill(text: &str, values: &[(&str, u16)]) -> String {
    values
        .iter()
        .fold(text.to_string(), |t, (k, v)| t.replace(k, &v.to_string()))
}

impl Expect {
    /// The expectations with the run's ports filled in ([`fill`]).
    pub fn filled(&self, values: &[(&str, u16)]) -> Self {
        let all = |v: &[String]| v.iter().map(|t| fill(t, values)).collect();
        Self {
            exit: self.exit,
            serial_contains: all(&self.serial_contains),
            serial_absent: all(&self.serial_absent),
            serial_order: all(&self.serial_order),
            killed: self.killed,
            seed_store: self.seed_store,
        }
    }

    fn texts(&self) -> impl Iterator<Item = &String> {
        self.serial_contains
            .iter()
            .chain(&self.serial_absent)
            .chain(&self.serial_order)
    }
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
            s.check_placeholders()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(h) = &s.host_input {
                h.check_queue()
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            }
            if let Some(h) = &s.host_hotplug {
                h.check().map_err(|e| format!("{}: {e}", path.display()))?;
                // QEMU's QMP socket serves one client at a time, and the
                // input probe holds its connection through all its keys.
                if s.host_input.is_some() {
                    return Err(format!(
                        "{}: [host_hotplug] and [host_input] share QEMU's single QMP connection: use one",
                        path.display()
                    ));
                }
            }
            s.file = path;
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

impl Scenario {
    /// Every text the runner fills placeholders in: console lines,
    /// `kill_on` and expectations, of the scenario and of its boots.
    fn texts(&self) -> impl Iterator<Item = &String> {
        self.stdin
            .iter()
            .chain(self.expect.texts())
            .chain(self.boots.iter().flat_map(|b| {
                b.stdin
                    .iter()
                    .chain(b.kill_on.iter())
                    .chain(b.expect.texts())
            }))
    }

    /// A placeholder without the server that gives it a port would reach
    /// the guest as literal text: refused at load time.
    fn check_placeholders(&self) -> Result<()> {
        for (placeholder, provided, field) in [
            (UPDATE_PORT, self.host_update.is_some(), "[host_update]"),
            (HTTPS_PORT, self.host_https, "host_https = true"),
            (HTTPS_UNTRUSTED_PORT, self.host_https, "host_https = true"),
        ] {
            if !provided && self.texts().any(|t| t.contains(placeholder)) {
                return Err(format!("{placeholder} needs {field}"));
            }
        }
        Ok(())
    }

    /// What a boot of this scenario writes to `aricodeos/cmdline.txt`: its
    /// declared `cmdline` plus `selftest` unless the scenario opts out.
    pub fn boot_cmdline(&self, cmdline: &str) -> String {
        let key = aricode_cmdline::keys::SELFTEST;
        if self.selftest && !aricode_cmdline::CmdLine::new(cmdline).has_flag(key) {
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
    fn hotplug_names_are_plain() {
        let h = |driver: &str| HostHotplug {
            marker: String::from("m"),
            delay_ms: 0,
            driver: driver.to_string(),
            bus: String::from("ehci.0"),
            id: String::from("hot-1"),
        };
        assert!(h("usb-kbd").check().is_ok());
        for bad in ["", "usb\"kbd", "a b", "x}{"] {
            assert!(h(bad).check().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn serial_order_checks_the_sequence() {
        let order = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let log = "a stop\nb free\nc stop\n";
        assert_eq!(first_out_of_order(log, &order(&["stop", "free"])), None);
        assert_eq!(
            first_out_of_order(log, &order(&["free", "stop"])),
            None,
            "the second stop"
        );
        assert_eq!(first_out_of_order(log, &order(&["free", "free"])), Some(1));
        assert_eq!(
            first_out_of_order(log, &order(&["c stop", "b free"])),
            Some(1)
        );
        assert_eq!(first_out_of_order(log, &order(&["missing"])), Some(0));
        assert_eq!(first_out_of_order(log, &[]), None);
    }

    #[test]
    fn seed_store_check() {
        use aricode_ab::seed::Record;
        let a = Record {
            generation: 1,
            seed: [1; 32],
        };
        let b = Record {
            generation: 2,
            seed: [2; 32],
        };
        let same_seed = Record { generation: 3, ..a };
        assert_eq!(SeedStore::Absent.check(None, None), None);
        assert!(SeedStore::Absent.check(None, Some(&a)).is_some());
        assert_eq!(SeedStore::Changed.check(None, Some(&a)), None);
        assert_eq!(SeedStore::Changed.check(Some(&a), Some(&b)), None);
        assert!(SeedStore::Changed.check(Some(&a), None).is_some());
        assert!(SeedStore::Changed.check(Some(&a), Some(&a)).is_some());
        let err = SeedStore::Changed
            .check(Some(&a), Some(&same_seed))
            .unwrap();
        assert!(err.contains("same seed"), "{err}");
        let e: Expect = toml::from_str("seed_store = \"changed\"\n").unwrap();
        assert_eq!(e.seed_store, Some(SeedStore::Changed));
        assert!(!e.is_empty());
        assert!(toml::from_str::<Expect>("seed_store = \"kept\"\n").is_err());
    }

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

    /// Incident 0022: a scenario that would leave more key codes queued in
    /// a device than QEMU's keyboards hold is refused at load (the repo's
    /// scenarios all load: `repo_scenarios_are_valid_and_unique`).
    #[test]
    fn typing_more_than_a_device_queue_without_echo_waits_is_refused() {
        let input = |extra: &str| -> HostInput {
            toml::from_str(&format!(
                "marker = \"M\"\nkeys = [\"s\", \"h\", \"u\", \"t\", \"d\", \"o\", \"w\", \"n\", \"ret\"]\n{extra}"
            ))
            .unwrap()
        };
        let err = input("").check_queue().unwrap_err();
        assert!(err.contains("types 18 key codes"), "{err}");
        input("echo_prompt = \"ari> \"").check_queue().unwrap();
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
    fn placeholders_are_filled_and_need_their_server() {
        let values = [(UPDATE_PORT, 40123), (HTTPS_PORT, 40124)];
        assert_eq!(
            fill(
                "http://10.0.2.2:{update_port}/update/ {https_port} {x}",
                &values
            ),
            "http://10.0.2.2:40123/update/ 40124 {x}"
        );
        let parse = |extra: &str| -> Result<Scenario> {
            let tmp = crate::test_util::TempDir::new("placeholders");
            std::fs::write(
                tmp.path().join("s.toml"),
                format!("name = \"x\"\nlevel = \"full\"\nareas = [\"update\"]\n{extra}"),
            )
            .unwrap();
            load_from(tmp.path()).map(|mut v| v.remove(0))
        };
        let err =
            parse("stdin = [\"update add 10.0.2.2:{update_port}/update/\"]\n[expect]\nexit = 33\n")
                .unwrap_err();
        assert!(err.contains("{update_port} needs [host_update]"), "{err}");
        let err =
            parse("[[boots]]\nkill_on = \"{https_untrusted_port}\"\n[boots.expect]\nexit = 0\n")
                .unwrap_err();
        assert!(err.contains("needs host_https = true"), "{err}");
        let s = parse(
            "[host_update]\nsource = \"none\"\n[expect]\nserial_contains = [\"from http://10.0.2.2:{update_port}/update/\"]\n",
        )
        .unwrap();
        assert_eq!(
            s.expect.filled(&values).serial_contains,
            ["from http://10.0.2.2:40123/update/"]
        );
    }

    /// The fixed test HTTP server has no update channel and the HTTPS
    /// servers have no fixed ports: a scenario reaching them through a
    /// literal port would talk to nothing (or to another run).
    #[test]
    fn repo_scenarios_reach_per_run_servers_through_placeholders() {
        let http = aricode_proto::net::TEST_HTTP_PORT;
        for s in load_all().expect("scenarios") {
            for t in s.texts() {
                assert!(
                    !t.contains(&format!(":{http}/update")),
                    "{}: '{t}' must use {UPDATE_PORT}",
                    s.name
                );
            }
        }
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
