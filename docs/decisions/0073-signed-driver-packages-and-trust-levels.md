# ADR 0073: Signed driver packages and three trust levels

- **Date:** 2026-11-03
- **Status:** Accepted (design). The owner, 2026-11-03, answering the open
  questions of ADR 0042's "Trusted vendors" section:
  - **Vendor certificates:** a separate vendor-root key, kept offline.
  - **Unofficial drivers:** signed with a key the machine's owner enrolls
    at the physical console.
  - **Revocation:** a revoked vendor or driver stops starting at the next
    boot.
  - **Compositor:** it stays in the base system (ADR 0074).

  This ADR closes the trust part of ADR 0042. Its implementation comes in
  the phases below, each with its own branch, tests and audit.

## Context

Today every driver is in the signed initrd. init finds the device by PCI
vendor and class, claims it and spawns the driver by name
(`services/init/src/main.rs`, `start_driver`). A driver for new hardware
therefore means a new system image built by the project.

Installed packages (ADR 0017) are applications only:
- one key signs them, the release key, or the development key in tests;
- the broker grants them at most `APP_ALLOWED`: system, fs, net, window;
- nothing outside init can claim a PCI device.

ADR 0042 recorded the owner's direction:
- three levels: official, trusted vendor and unofficial;
- delegated public-key signing instead of a shared secret;
- vendor certificates and revocation.

It left four questions open: which key signs the certificates, the
certificate format, the rules for unofficial drivers, and what revocation
does. The owner answered them on 2026-11-03.

### What the veterans learnt (ADR 0063: compare, then reuse or improve)

| System | What it does | What went wrong, or what worked |
|---|---|---|
| Windows kernel-mode signing | A signed driver loads into ring 0. | Stolen vendor certificates signed malware (Stuxnet with Realtek's and JMicron's, 2010). Legitimately signed but vulnerable drivers are still used to enter the kernel ("bring your own vulnerable driver"). That forced a separate blocklist of driver hashes, which was updated rarely and off by default for years. A signature says who built a driver, not that it is safe. |
| Linux module signing and MOK | Each kernel build has its key. Owners add their own key through MOK, confirmed in MokManager at the next boot on the physical console, with a one-time password. | Physical-presence enrolment works: remote malware cannot add a key on its own. Revocation (blacklist keyring, dbx) exists but is rarely used. |
| UEFI Secure Boot dbx | A revocation database of hashes and keys. | Revocation was not planned for. dbx has little room, and revoking a widely used signed boot loader would leave machines unable to boot. BlackLotus (CVE-2022-21894) kept booting for over a year after the fix. Revocation has to be designed in from the start, by key and by hash, with room to grow and a fallback that does not stop the machine. |
| TUF and Uptane (Uptane is the automotive variant) | An offline root delegates narrow roles. Metadata carries version numbers and expiry. | Offline root keys and scoped delegation limit what one stolen key can do. Monotonic version numbers stop rollback and freeze attacks. Uptane was built for vehicles, so it is close to robotics. |
| Apple DriverKit | Third-party drivers run in user space. A vendor's entitlement lets its driver match only that vendor's own devices. | The closest model to ours: trust limits which devices a driver may take, and isolation limits what it can do with them. |

AriCodeOS has the advantage the others lacked: a driver is an ordinary
process with only the handles init gave it (ADR 0005, 0016). Behind an
IOMMU (ADR 0038, Phase 33) its DMA reaches only its own buffers. So a
vulnerable or malicious signed driver can harm its own device, but not the
kernel. The design below keeps it that way: trust decides whether a driver
may be installed and started, and how it is labelled, never what it may
touch.

## Decision

### 1. Three levels, decided by the key that signed the package

| Level | Signed by | Shown as |
|---|---|---|
| official | the release key, or the development-channel key on a dev machine (ADR 0045) | `official` |
| trusted vendor | a vendor key whose certificate verifies with the vendor-root key, is not revoked and covers the device | `vendor:NAME` |
| unofficial | a key the machine's owner enrolled (point 5) | `unofficial:NAME` |

Anything else is refused and the refusal is logged. No unsigned code runs
at any level.

### 2. The vendor-root key

- **A new Ed25519 key, used only to sign vendor certificates and
  revocation lists.**
  - It is generated on an offline machine with `cargo xtask keygen`.
  - Its seed lives on two dedicated USB sticks kept disconnected, with a
    paper copy in a safe place.
  - It is never on the build host and never in the repository.
- **Its public half ships in every system** as a third trust anchor, next
  to the release and development-channel keys (`aricode_crypto::trust`).
  As with the existing anchors, the build stops if it equals another
  anchor or a public test key.
- **What a stolen key allows.** With the vendor-root key, someone can
  create vendors but not sign systems. With the release key, someone can
  sign systems but not create vendors. Each key can be rotated without
  the other.
- **Rotation.** A system image may carry two vendor-root keys during a
  rotation. Every certificate names the root it was signed with (`root`,
  the key's SHA-256 prefix).

### 3. Vendor certificates: a minimal signed record, not X.509

- **Format.** A certificate is a short text record, parsed as strictly as
  the package manifest (`aricode-pkg`): an unknown or repeated key is
  refused. It travels with an Ed25519 signature by the vendor-root key.
- **Why not X.509.** X.509 parsing is a large attack surface. The records
  are read by init, which is in the trusted base. Our needs are a key, a
  name and a scope. The `x509` crate stays for TLS only.

```text
format = 1
root = 3f2a91c0                # vendor-root key that signed it (SHA-256 prefix)
serial = 7                     # unique, never reused; revocation names it
vendor = acme                  # [a-z0-9-], at most 16; shown as vendor:acme
display = ACME Robotics S.L.
key = <64 hex digits>          # the vendor's Ed25519 public key
kinds = driver                 # what it may sign; only "driver" for now
pci = 1d6b, 8086:15b8          # PCI vendor ids, or vendor:device pairs
serves = net                   # protocols its drivers may serve
issued = 2026-11-03
expires = 2028-11-03
```

- **The vendor keeps its own private key.** The project never sees it,
  and no secret is shared (ADR 0042).
- **How a vendor joins.** It applies in an `AriCodeLabs/vendors`
  repository and accepts the driver policy (ADR 0042 option 4: tests, a
  capability manifest, behaviour on failure). The project decides, then
  signs the certificate offline.
- **Expiry.** It is checked when a driver is installed, against the newest
  date the machine trusts: its system's build date or the date of the last
  signed update index it verified. The RTC alone is never used, because a
  robot's clock may be wrong or set back.
  - An installed driver keeps running after its certificate expires.
  - Expiry stops new versions; revocation stops running ones (point 6).

### 4. Driver packages

A driver is an `aricode-pkg` package with extra lines in its signed
manifest and, at the vendor level, its certificate inside the container.

```text
kind = driver
match = pci 1d6b:0104          # or: pci-class 10ec 02.00
serves = net
```

| File in the package | official | trusted vendor | unofficial |
|---|---|---|---|
| `manifest`, `manifest.sig`, `program`, `program.manifest` | yes | yes | yes |
| `signer.cert`, `signer.cert.sig` | no | yes | no |

- **Older systems refuse driver packages.** The manifest parser of
  0.17 and earlier refuses unknown keys, so an older system never mistakes
  a driver for an application.
- **The installer puts drivers under `drivers/NAME.aripkg`**, with the
  same streamed verification and atomic `replace` as applications (ADR
  0018, 0072 phase B). It shows the level and the vendor before
  installing.
- **The scope is checked twice, at install time and at every boot.**
  - Every `match` must fall inside the certificate's `pci` list.
  - `serves` must be inside the certificate's `serves`.
  - A driver outside its scope is refused and the refusal is logged.

### 5. Unofficial drivers: owner keys enrolled at the physical console

- **The owner adds a key with `drivers enroll NAME FILE`.** It works from
  the shell or over SSH, but it only writes a request.
- **init confirms the request at the next boot, on the physical
  console.**
  - It shows the key's fingerprint and name, and a six-digit code drawn
    from the kernel's entropy (ADR 0039).
  - The owner types the code on a keyboard served by the input services
    (PS/2, USB HID; virtio-input in QEMU).
  - Serial and the SSH console bridge are not accepted for this, because
    they may be remote.
  - Without the code within 60 s, the request is discarded and init says
    so. As with MOK, someone who only has remote access can ask, but
    cannot approve.
- **Where the enrolled keys live.** They are kept in `system/owner-keys`.
  `system/` becomes a directory that fsd lets only init write; every
  other session, the shell's whole-namespace session included, can only
  read it (a new rule in fsd).
- **Removing a key needs no confirmation:** `drivers forget NAME` works
  from anywhere, because closing a door is always allowed.
- **DMA without an IOMMU.** On a machine where no IOMMU unit covers the
  device (no VT-d, AMD-Vi not yet supported, or `iommu=off`), its DMA is
  not confined.
  - An unofficial driver whose manifest asks for DMA is then not started
    until the owner confirms that device, the same way at the physical
    console.
  - A trusted-vendor driver starts, and `diag` shows "DMA not confined".

### 6. Revocation

- **A signed list, `revoked`.**
  - It is signed by the vendor-root key or by the release key. The
    release key can only take trust away, never add it, so an urgent
    revocation does not wait for the offline key.
  - It holds a sequence number, the certificate serials that are revoked
    and the SHA-256 of individual packages that are revoked. Hashes cover
    the "vulnerable but signed" case at every level, official included.
- **Every system image carries it in its initrd**, and every update index
  carries the latest one. A machine that never goes online still receives
  revocations with its next system update.
- **No rollback.** init keeps the highest sequence it has verified in
  `system/revocation-seq` and refuses any list with a lower sequence, as
  TUF and Uptane do.
- **Effect.**
  - A revoked driver cannot be installed.
  - An installed one is not started at the next boot. init logs it and
    records a fault event, and `diag` shows it.
  - The device is left unclaimed and the rest of the system boots
    normally. This is the fallback that does not stop the machine, which
    dbx lacked.

### 7. How init starts a packaged driver

1. **Built-in drivers claim their devices first.** A packaged driver is
   only for a device that no initrd driver takes.
2. **init verifies each package under `drivers/`:**
   - the signature, the level, the scope and the revocation list;
   - the package is read in pieces into a per-launch DMA buffer, as the
     broker already does for applications (ADR 0072 phase B).
3. **It spawns the driver with `start_driver`'s handles:** the matched
   device's configuration space, BARs and IRQ, and nothing else.
4. **The driver runs under the resident-driver supervisor** (ADR 0011),
   so a crash restarts it with the same limits.
5. **It serves only the protocol it declared.** init wires it as it
   wires today's driver of that class.
6. **`services` and `diag` show the level of every driver**, built-in
   drivers included: those are always `official`.

### 8. Tools

- **`cargo xtask vendor-cert`** issues a certificate from the offline
  vendor-root seed. It takes `--root DIR --serial --vendor --display --key
  --pci --serves --expires`, and refuses to reuse a serial.
- **`cargo xtask revoke`** writes and signs a new `revoked` list with the
  next sequence number.
- **`cargo xtask app-pack --kind driver`** builds a driver package. It
  takes `--signing-key` and, at the vendor level, `--cert`.
- **The test keys** for the vendor root, a test vendor and a test owner
  live in `keys/dev/`, as public test material like the existing
  development key.

## Implementation phases

| Phase | Content |
|---|---|
| A | Certificates, the `revoked` list, the driver manifest lines and the level rules in host-tested libraries; the xtask tools; the third trust anchor. |
| B | init starts packaged drivers, with scope, revocation and the sequence check. The `system/` write rule in fsd. The installer handles `drivers/`. Levels shown in `services` and `diag`. |
| C | Owner key enrolment at the physical console, and the DMA-without-IOMMU confirmation. |
| D | The `AriCodeLabs/vendors` repository: the policy text, the application template and the review checklist (CONTRIBUTING.md, ADR 0042 option 4). |

Phases A to C touch the trusted base (init, fsd, crypto), so each one gets
an independent audit (skill `aricodeos-test-levels`). The QEMU scenarios
use QEMU's `edu` PCI device (vendor 1234, with DMA and an IRQ) driven by a
small test driver. They cover:
- each level accepted;
- a driver outside its scope;
- an expired certificate;
- a revoked serial and a revoked hash;
- a revocation list with a lower sequence;
- an unofficial driver without enrolment;
- enrolment confirmed and enrolment timed out;
- the DMA confirmation with `iommu=off`.

## Consequences

- **Hardware support without a new system image.** A vendor can ship a
  driver for its own devices without the project rebuilding the system,
  and the owner always sees who built every driver.
- **One more key to protect,** but it is used rarely and offline. The
  release key's handling does not change.
- **No new privilege for applications.** `APP_ALLOWED` is unchanged and
  the broker still never grants `pci`. Only init starts drivers.
- **What revocation costs.** Revoking a vendor can leave a device without
  a driver after the next boot. That is the owner's choice, and the
  machine still boots.
- **Licensing.** Closed-source vendor drivers are possible under
  Apache-2.0. The owner set the licence question aside (ADR 0063).
- **Not covered.** Vendor-signed applications and services are left out
  (`kinds` only allows `driver`). Opening them needs its own ADR.
