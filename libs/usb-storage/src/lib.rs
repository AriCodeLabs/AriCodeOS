// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! USB mass storage (ADR 0023): the bulk-only transport (BOT) and the
//! subset of SCSI commands a block service needs.
//! Only encodes and decodes; the xHCI driver performs the transfers.

#![no_std]

/// Command Block Wrapper (31 bytes, USB Mass Storage Bulk-Only 1.0, 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cbw {
    pub tag: u32,
    /// Bytes in the data phase (0 if there is none).
    pub data_len: u32,
    /// Data from the device to the host.
    pub data_in: bool,
    pub lun: u8,
    pub cdb: Cdb,
}

impl Cbw {
    pub const LEN: usize = 31;
    const SIGNATURE: [u8; 4] = *b"USBC";

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[..4].copy_from_slice(&Self::SIGNATURE);
        b[4..8].copy_from_slice(&self.tag.to_le_bytes());
        b[8..12].copy_from_slice(&self.data_len.to_le_bytes());
        b[12] = if self.data_in { 0x80 } else { 0 };
        b[13] = self.lun & 0xF;
        b[14] = self.cdb.len;
        b[15..31].copy_from_slice(&self.cdb.bytes);
        b
    }
}

/// Command Status Wrapper (13 bytes, 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Csw {
    pub tag: u32,
    /// Bytes of the data phase that were not transferred.
    pub residue: u32,
    pub status: CswStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CswStatus {
    Passed,
    /// The command failed: `REQUEST SENSE` says why.
    Failed,
    /// Phase error: the transport must be reset.
    PhaseError,
}

/// Fault event names of the storage driver (`sys::event_record`, ADR
/// 0028): at most `NAME_LEN` bytes, so an LBA prefix leaves room for the
/// ten digits of a READ(10)/WRITE(10) address.
pub mod fault {
    pub const NAME_LEN: usize = 24;
    /// Prefixes followed by the LBA in decimal.
    pub const READ: &str = "usb r lba ";
    pub const WRITE: &str = "usb w lba ";
    pub const INJECTED_READ: &str = "usb inj r lba ";
    pub const INJECTED_WRITE: &str = "usb inj w lba ";
    pub const LBA_PREFIXES: [&str; 4] = [READ, WRITE, INJECTED_READ, INJECTED_WRITE];
    /// A CSW with a bad signature or status, for another command or with a
    /// phase error (`CswError::event`), code 0.
    pub const CSW_INVALID: &str = "usb csw invalid";
    /// A CSW transfer that moved other than 13 bytes (incident 0007); the
    /// code is the count moved, so it is its own event: `0` here is not
    /// `CSW_INVALID`.
    pub const CSW_LENGTH: &str = "usb csw length";
    /// A passed READ or WRITE whose data stage or CSW residue shows bytes
    /// not moved (`bytes_not_moved`); the code is the count (incident 0010).
    pub const SHORT_DATA: &str = "usb short data";

    /// `prefix` followed by `lba`; `None` if it does not fit `out`.
    pub fn lba_name<'a>(prefix: &str, lba: u64, out: &'a mut [u8; NAME_LEN]) -> Option<&'a str> {
        if prefix.len() > out.len() {
            return None;
        }
        out[..prefix.len()].copy_from_slice(prefix.as_bytes());
        let len = put_decimal(out, prefix.len(), lba)?;
        core::str::from_utf8(&out[..len]).ok()
    }

    /// The device's own reason for a failed command (`REQUEST SENSE`):
    /// `usb sense K A Q` with the key in decimal and ASC / ASCQ as two hex
    /// digits, the SCSI convention (`usb sense 3 11 00`: unrecovered error).
    pub fn sense_name<'a>(
        sense: &super::scsi::Sense,
        out: &'a mut [u8; NAME_LEN],
    ) -> Option<&'a str> {
        const PREFIX: &str = "usb sense ";
        out[..PREFIX.len()].copy_from_slice(PREFIX.as_bytes());
        let mut len = put_decimal(out, PREFIX.len(), sense.key as u64)?;
        for byte in [sense.asc, sense.ascq] {
            if len + 3 > out.len() {
                return None;
            }
            out[len] = b' ';
            out[len + 1] = HEX[(byte >> 4) as usize];
            out[len + 2] = HEX[(byte & 0xF) as usize];
            len += 3;
        }
        core::str::from_utf8(&out[..len]).ok()
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";

    /// Writes `v` in decimal at `out[pos..]`; the new length, or `None` if
    /// it does not fit.
    pub(crate) fn put_decimal(out: &mut [u8], pos: usize, v: u64) -> Option<usize> {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        let mut v = v;
        loop {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        let d = &digits[i..];
        let len = pos + d.len();
        if len > out.len() {
            return None;
        }
        out[pos..len].copy_from_slice(d);
        Some(len)
    }
}

/// Stage of a Bulk-Only command (BOT 5.3): the three transfers of the
/// transport state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Cbw,
    Data,
    Csw,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cbw => "CBW",
            Self::Data => "DATA",
            Self::Csw => "CSW",
        }
    }
}

/// What the host does once a transfer at some stage failed and the xHCI
/// endpoint that reported it is back in service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Read the CSW once (BOT 6.7.2 and 6.7.3): a device that completed
    /// the command delivers it and the transport is Ready again.
    ReadCsw,
    /// Reset Recovery (BOT 5.3.4) before any new CBW: whether the device
    /// holds a command is unknown.
    ResetRecovery,
}

/// BOT 5.3 and 6.7: after the CBW was sent the transport returns to Ready
/// only through a CSW or a Reset Recovery, so a data or CSW failure is
/// followed by one CSW read; a CBW that failed may or may not have reached
/// the device (BOT 6.6.1 for a STALL), so only a Reset Recovery leaves it
/// in a known state.
pub fn after_transfer_failure(stage: Stage) -> Next {
    match stage {
        Stage::Cbw => Next::ResetRecovery,
        Stage::Data | Stage::Csw => Next::ReadCsw,
    }
}

/// Why a received CSW does not close the command (BOT 6.3): all of them
/// require a Reset Recovery before the next CBW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CswError {
    /// Signature or status byte not valid, or fewer bytes than a CSW to
    /// look at (BOT 6.3.1).
    Invalid,
    /// The transfer moved this many bytes, not the 13 of a CSW (BOT 5.2,
    /// 6.3.1). Where the device wrote nothing the buffer holds whatever the
    /// previous command left, so no field is read: a status that did not
    /// arrive is not a status (incident 0007).
    Length(usize),
    /// A valid CSW for another command (dCSWTag, BOT 6.3.1 and 6.4).
    TagMismatch(Csw),
    /// bCSWStatus 2 (BOT 6.3.1 and 6.7, phase error).
    PhaseError(Csw),
}

impl CswError {
    /// The fault event that records the error (ADR 0028): `fault::CSW_LENGTH`
    /// with the bytes moved as the code for a wrong length, its own event
    /// so that a transfer that moved nothing is told apart from a bad
    /// signature; `fault::CSW_INVALID` with code 0 otherwise.
    pub fn event(self) -> (&'static str, u64) {
        match self {
            Self::Length(moved) => (fault::CSW_LENGTH, moved as u64),
            Self::Invalid | Self::TagMismatch(_) | Self::PhaseError(_) => (fault::CSW_INVALID, 0),
        }
    }

    /// The console line for the error on the command with `tag`, without
    /// the driver's prefix, written into `out` (both drivers print the same
    /// words).
    pub fn describe(self, tag: u32, out: &mut [u8; CSW_TEXT_LEN]) -> &str {
        let mut t = Text { out, len: 0 };
        match self {
            Self::Invalid => t.push("invalid CSW (signature or status)"),
            Self::Length(moved) => {
                t.push("CSW of ");
                t.decimal(moved as u64);
                t.push(" bytes, not 13 (BOT 5.2): invalid, no status for tag ");
                t.decimal(tag as u64);
            }
            Self::TagMismatch(csw) => {
                t.push("CSW tag mismatch: expected ");
                t.decimal(tag as u64);
                t.push(", received ");
                t.decimal(csw.tag as u64);
                t.push(" with status ");
                t.decimal(csw.status as u64);
                t.push(" residue ");
                t.decimal(csw.residue as u64);
            }
            Self::PhaseError(csw) => {
                t.push("CSW phase error, tag ");
                t.decimal(csw.tag as u64);
                t.push(" residue ");
                t.decimal(csw.residue as u64);
            }
        }
        t.finish()
    }
}

/// Bytes `CswError::describe` needs: its longest line with every number at
/// ten digits is 91 bytes.
pub const CSW_TEXT_LEN: usize = 96;

/// ASCII text built piecewise into a fixed buffer; a piece that does not
/// fit is cut (`CSW_TEXT_LEN` leaves room for every line).
struct Text<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> Text<'a> {
    fn push(&mut self, s: &str) {
        let n = s.len().min(self.out.len() - self.len);
        self.out[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
    }

    fn decimal(&mut self, v: u64) {
        if let Some(len) = fault::put_decimal(self.out, self.len, v) {
            self.len = len;
        }
    }

    fn finish(self) -> &'a str {
        let out: &'a [u8] = self.out;
        core::str::from_utf8(&out[..self.len]).unwrap_or("")
    }
}

/// Bytes of a passed command's data stage that were not moved: what
/// `dCSWDataResidue` says (BOT 6.7.2, 6.7.3: the device's count) or what
/// the host's data transfer fell short of (`data_len` requested, `moved`
/// transferred), whichever is more. With any of them a READ would leave
/// stale bytes in the client's buffer and a WRITE did not reach the medium
/// whole, so the drivers answer such a command with an error, never with a
/// success (incident 0010); 0 means complete.
pub fn bytes_not_moved(residue: u32, data_len: u32, moved: u32) -> u32 {
    residue.max(data_len.saturating_sub(moved))
}

/// Checks the CSW that the bulk IN transfer for the command with `tag`
/// put in `raw`, having moved `moved` bytes. A CSW is exactly `Csw::LEN`
/// bytes (BOT 5.2); a transfer that moved any other count is an invalid
/// CSW before a byte of `raw` is looked at, so the residue and status a
/// previous command left in the buffer never pass as this one's (incident
/// 0007). The drivers check every CSW they read through this function.
pub fn check_csw_transfer(moved: usize, raw: &[u8; Csw::LEN], tag: u32) -> Result<Csw, CswError> {
    if moved != Csw::LEN {
        return Err(CswError::Length(moved));
    }
    check_csw(raw, tag)
}

/// Checks the bytes of a CSW received for the command with `tag`; the
/// caller has made sure they are bytes the device delivered
/// (`check_csw_transfer`).
pub fn check_csw(raw: &[u8], tag: u32) -> Result<Csw, CswError> {
    let csw = Csw::parse(raw).ok_or(CswError::Invalid)?;
    if csw.tag != tag {
        return Err(CswError::TagMismatch(csw));
    }
    if csw.status == CswStatus::PhaseError {
        return Err(CswError::PhaseError(csw));
    }
    Ok(csw)
}

impl Csw {
    pub const LEN: usize = 13;
    const SIGNATURE: [u8; 4] = *b"USBS";

    /// `None` if the signature, length or status are not valid.
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::LEN || b[..4] != Self::SIGNATURE {
            return None;
        }
        let status = match b[12] {
            0 => CswStatus::Passed,
            1 => CswStatus::Failed,
            2 => CswStatus::PhaseError,
            _ => return None,
        };
        Some(Self {
            tag: u32::from_le_bytes(b[4..8].try_into().ok()?),
            residue: u32::from_le_bytes(b[8..12].try_into().ok()?),
            status,
        })
    }
}

/// SCSI Command Descriptor Block (up to 16 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cdb {
    pub bytes: [u8; 16],
    pub len: u8,
}

impl Cdb {
    fn new(b: &[u8]) -> Self {
        let mut bytes = [0u8; 16];
        bytes[..b.len()].copy_from_slice(b);
        Self {
            bytes,
            len: b.len() as u8,
        }
    }
}

/// SCSI block commands (SBC-3 / SPC-4) used by the block service.
pub mod scsi {
    use super::Cdb;

    pub const INQUIRY_LEN: u16 = 36;
    pub const SENSE_LEN: u16 = 18;
    pub const READ_CAPACITY_LEN: u16 = 8;

    pub fn test_unit_ready() -> Cdb {
        Cdb::new(&[0x00, 0, 0, 0, 0, 0])
    }
    pub fn request_sense() -> Cdb {
        Cdb::new(&[0x03, 0, 0, 0, SENSE_LEN as u8, 0])
    }
    pub fn inquiry() -> Cdb {
        Cdb::new(&[0x12, 0, 0, 0, INQUIRY_LEN as u8, 0])
    }
    pub fn read_capacity10() -> Cdb {
        Cdb::new(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }
    pub fn read10(lba: u32, blocks: u16) -> Cdb {
        let l = lba.to_be_bytes();
        let n = blocks.to_be_bytes();
        Cdb::new(&[0x28, 0, l[0], l[1], l[2], l[3], 0, n[0], n[1], 0])
    }
    pub fn write10(lba: u32, blocks: u16) -> Cdb {
        let l = lba.to_be_bytes();
        let n = blocks.to_be_bytes();
        Cdb::new(&[0x2A, 0, l[0], l[1], l[2], l[3], 0, n[0], n[1], 0])
    }
    /// `SYNCHRONIZE CACHE(10)` of the whole medium: makes writes durable.
    pub fn synchronize_cache10() -> Cdb {
        Cdb::new(&[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    /// `INQUIRY` response (36 bytes): peripheral type and names.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Inquiry {
        /// 0 = direct access (disk); 5 = CD/DVD.
        pub peripheral_type: u8,
        pub removable: bool,
        pub vendor: [u8; 8],
        pub product: [u8; 16],
    }

    impl Inquiry {
        pub fn parse(b: &[u8]) -> Option<Self> {
            if b.len() < INQUIRY_LEN as usize {
                return None;
            }
            Some(Self {
                peripheral_type: b[0] & 0x1F,
                removable: b[1] & 0x80 != 0,
                vendor: b[8..16].try_into().ok()?,
                product: b[16..32].try_into().ok()?,
            })
        }
    }

    /// `READ CAPACITY(10)` response: last LBA and bytes per block.
    /// `None` if the device does not fit in 32 bits (it returns `u32::MAX`).
    pub fn parse_read_capacity10(b: &[u8]) -> Option<(u32, u32)> {
        if b.len() < READ_CAPACITY_LEN as usize {
            return None;
        }
        let last_lba = u32::from_be_bytes(b[..4].try_into().ok()?);
        let block_len = u32::from_be_bytes(b[4..8].try_into().ok()?);
        (last_lba != u32::MAX && block_len != 0).then_some((last_lba, block_len))
    }

    /// Fixed-format sense data (`REQUEST SENSE`): key, code and qualifier.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Sense {
        pub key: u8,
        pub asc: u8,
        pub ascq: u8,
    }

    impl Sense {
        /// Key 6: the device has just powered on or the medium changed; the
        /// command is retried.
        pub const UNIT_ATTENTION: u8 = 6;
        /// Key 2: not ready (no medium or starting up).
        pub const NOT_READY: u8 = 2;
        /// Key 5: command or field not valid for this device.
        pub const ILLEGAL_REQUEST: u8 = 5;

        /// The device does not implement the command (illegal request due
        /// to an invalid operation code or field): many USB sticks reject
        /// `SYNCHRONIZE CACHE` this way, and then there is no cache to
        /// write back.
        pub fn command_unsupported(&self) -> bool {
            self.key == Self::ILLEGAL_REQUEST && matches!(self.asc, 0x20 | 0x24)
        }

        pub fn parse(b: &[u8]) -> Option<Self> {
            if b.len() < 14 || b[0] & 0x7F != 0x70 && b[0] & 0x7F != 0x71 {
                return None;
            }
            Some(Self {
                key: b[2] & 0xF,
                asc: b[12],
                ascq: b[13],
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbw_and_csw_round_trip() {
        let cbw = Cbw {
            tag: 0x1234_5678,
            data_len: 512,
            data_in: true,
            lun: 0,
            cdb: scsi::read10(7, 1),
        };
        let b = cbw.encode();
        assert_eq!(&b[..4], b"USBC");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 0x1234_5678);
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 512);
        assert_eq!((b[12], b[13], b[14]), (0x80, 0, 10));
        assert_eq!(&b[15..25], &[0x28, 0, 0, 0, 0, 7, 0, 0, 1, 0]);
        let mut csw = [0u8; 13];
        csw[..4].copy_from_slice(b"USBS");
        csw[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        csw[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            Csw::parse(&csw),
            Some(Csw {
                tag: 0x1234_5678,
                residue: 0,
                status: CswStatus::Passed
            })
        );
        csw[12] = 1;
        assert_eq!(Csw::parse(&csw).unwrap().status, CswStatus::Failed);
        csw[12] = 2;
        assert_eq!(Csw::parse(&csw).unwrap().status, CswStatus::PhaseError);
        csw[12] = 3;
        assert!(Csw::parse(&csw).is_none());
        csw[0] = b'X';
        assert!(Csw::parse(&csw).is_none());
        assert!(Csw::parse(&csw[..12]).is_none());
    }

    #[test]
    fn fault_names_fit_any_lba() {
        let mut out = [0u8; fault::NAME_LEN];
        for prefix in fault::LBA_PREFIXES {
            let name = fault::lba_name(prefix, u32::MAX as u64, &mut out).expect("fits");
            assert!(
                name.starts_with(prefix) && name.ends_with("4294967295"),
                "{name}"
            );
            assert!(name.len() <= fault::NAME_LEN);
        }
        assert_eq!(
            fault::lba_name(fault::READ, 0, &mut out),
            Some("usb r lba 0")
        );
        assert_eq!(
            fault::lba_name(fault::INJECTED_WRITE, u64::MAX, &mut out),
            None
        );
        // The device's reason: key decimal, ASC / ASCQ hex (SCSI convention).
        let unrecovered = scsi::Sense {
            key: 3,
            asc: 0x11,
            ascq: 0x00,
        };
        assert_eq!(
            fault::sense_name(&unrecovered, &mut out),
            Some("usb sense 3 11 00")
        );
        let worst = scsi::Sense {
            key: 15,
            asc: 0xff,
            ascq: 0xff,
        };
        let name = fault::sense_name(&worst, &mut out).expect("fits");
        assert_eq!(name, "usb sense 15 ff ff");
        assert!(name.len() <= fault::NAME_LEN);
    }

    #[test]
    fn transport_rules_after_a_failure() {
        assert_eq!(after_transfer_failure(Stage::Cbw), Next::ResetRecovery);
        assert_eq!(after_transfer_failure(Stage::Data), Next::ReadCsw);
        assert_eq!(after_transfer_failure(Stage::Csw), Next::ReadCsw);
        assert_eq!(Stage::Data.name(), "DATA");
        let mut csw = [0u8; 13];
        csw[..4].copy_from_slice(b"USBS");
        csw[4..8].copy_from_slice(&7u32.to_le_bytes());
        csw[8..12].copy_from_slice(&512u32.to_le_bytes());
        assert_eq!(
            check_csw(&csw, 7),
            Ok(Csw {
                tag: 7,
                residue: 512,
                status: CswStatus::Passed
            })
        );
        assert!(matches!(
            check_csw(&csw, 8),
            Err(CswError::TagMismatch(Csw { tag: 7, .. }))
        ));
        csw[12] = 2;
        assert!(matches!(check_csw(&csw, 7), Err(CswError::PhaseError(_))));
        csw[0] = b'X';
        assert_eq!(check_csw(&csw, 7), Err(CswError::Invalid));
        assert_eq!(check_csw(&csw[..12], 7), Err(CswError::Invalid));
    }

    /// The CSW a device delivers: `tag`, `residue` and the status byte.
    fn csw_bytes(tag: u32, residue: u32, status: u8) -> [u8; Csw::LEN] {
        let mut b = [0u8; Csw::LEN];
        b[..4].copy_from_slice(b"USBS");
        b[4..8].copy_from_slice(&tag.to_le_bytes());
        b[8..12].copy_from_slice(&residue.to_le_bytes());
        b[12] = status;
        b
    }

    #[test]
    fn csw_transfer_must_move_exactly_thirteen_bytes() {
        // Incident 0007: the buffer still holds the previous command's CSW
        // (tag 6, Passed or Failed) and the device answers the command with
        // tag 7 with the signature and the tag only (8 bytes), or less.
        for previous_status in [0u8, 1] {
            let mut buf = csw_bytes(6, 0, previous_status);
            for moved in [0usize, 8, 12] {
                let reply = csw_bytes(7, 0, 0);
                buf[..moved].copy_from_slice(&reply[..moved]);
                assert_eq!(
                    check_csw_transfer(moved, &buf, 7),
                    Err(CswError::Length(moved)),
                    "{moved} bytes over a previous status {previous_status}"
                );
                assert_eq!(
                    CswError::Length(moved).event(),
                    (fault::CSW_LENGTH, moved as u64)
                );
            }
            // The trap the count closes: once the first 8 bytes are
            // rewritten the buffer alone decodes as a status for tag 7.
            assert!(check_csw(&buf, 7).is_ok());
            // More than a CSW is not a CSW either.
            assert_eq!(
                check_csw_transfer(Csw::LEN + 1, &buf, 7),
                Err(CswError::Length(Csw::LEN + 1))
            );
        }
        // A complete CSW is checked field by field, as before.
        let passed = csw_bytes(7, 0, 0);
        assert_eq!(
            check_csw_transfer(Csw::LEN, &passed, 7),
            Ok(Csw {
                tag: 7,
                residue: 0,
                status: CswStatus::Passed
            })
        );
        let failed = csw_bytes(7, 512, 1);
        assert_eq!(
            check_csw_transfer(Csw::LEN, &failed, 7).map(|c| c.status),
            Ok(CswStatus::Failed)
        );
        assert!(matches!(
            check_csw_transfer(Csw::LEN, &passed, 8),
            Err(CswError::TagMismatch(Csw { tag: 7, .. }))
        ));
        let phase = csw_bytes(7, 0, 2);
        assert!(matches!(
            check_csw_transfer(Csw::LEN, &phase, 7),
            Err(CswError::PhaseError(Csw { tag: 7, .. }))
        ));
        let mut bad = passed;
        bad[0] = b'X';
        assert_eq!(
            check_csw_transfer(Csw::LEN, &bad, 7),
            Err(CswError::Invalid)
        );
        assert_eq!(CswError::Invalid.event(), (fault::CSW_INVALID, 0));
        // A CSW that moved nothing is not the same event as a bad signature.
        assert_ne!(CswError::Length(0).event(), CswError::Invalid.event());
        assert!(fault::CSW_LENGTH.len() <= fault::NAME_LEN);
        assert!(fault::CSW_INVALID.len() <= fault::NAME_LEN);
        // 0xFF, the drivers' fill before every CSW read, is no status.
        assert_eq!(
            check_csw_transfer(Csw::LEN, &[0xFF; Csw::LEN], 7),
            Err(CswError::Invalid)
        );
    }

    #[test]
    fn csw_error_lines() {
        let mut out = [0u8; CSW_TEXT_LEN];
        assert_eq!(
            CswError::Invalid.describe(7, &mut out),
            "invalid CSW (signature or status)"
        );
        assert_eq!(
            CswError::Length(8).describe(7, &mut out),
            "CSW of 8 bytes, not 13 (BOT 5.2): invalid, no status for tag 7"
        );
        let other = Csw {
            tag: 6,
            residue: 512,
            status: CswStatus::Failed,
        };
        assert_eq!(
            CswError::TagMismatch(other).describe(7, &mut out),
            "CSW tag mismatch: expected 7, received 6 with status 1 residue 512"
        );
        let phase = Csw {
            tag: 7,
            residue: 0,
            status: CswStatus::PhaseError,
        };
        assert_eq!(
            CswError::PhaseError(phase).describe(7, &mut out),
            "CSW phase error, tag 7 residue 0"
        );
        // The longest line fits whole.
        let worst = Csw {
            tag: u32::MAX,
            residue: u32::MAX,
            status: CswStatus::PhaseError,
        };
        let line = CswError::TagMismatch(worst).describe(u32::MAX, &mut out);
        assert_eq!(
            line,
            "CSW tag mismatch: expected 4294967295, received 4294967295 with status 2 residue 4294967295"
        );
        assert!(line.len() <= CSW_TEXT_LEN);
    }

    #[test]
    fn data_residue_decision() {
        // Incident 0010: a passed command is complete only when the device
        // reports no residue and the host moved every byte it asked for.
        assert_eq!(bytes_not_moved(0, 512, 512), 0);
        assert_eq!(bytes_not_moved(0, 0, 0), 0);
        // The host's data stage fell short (xHCI residual, EHCI qTD count).
        assert_eq!(bytes_not_moved(0, 512, 0), 512);
        assert_eq!(bytes_not_moved(0, 1024, 512), 512);
        // The device's dCSWDataResidue, even when the host saw every byte.
        assert_eq!(bytes_not_moved(512, 512, 512), 512);
        // Whichever is more.
        assert_eq!(bytes_not_moved(100, 1024, 512), 512);
        assert_eq!(bytes_not_moved(1024, 1024, 512), 1024);
        // Never negative when more arrived than asked for.
        assert_eq!(bytes_not_moved(0, 512, 600), 0);
        assert!(fault::SHORT_DATA.len() <= fault::NAME_LEN);
    }

    #[test]
    fn scsi_cdbs_and_replies() {
        assert_eq!(scsi::test_unit_ready().len, 6);
        assert_eq!(scsi::inquiry().bytes[4], 36);
        assert_eq!(scsi::request_sense().bytes[4], 18);
        let w = scsi::write10(0x0102_0304, 16);
        assert_eq!(&w.bytes[..10], &[0x2A, 0, 1, 2, 3, 4, 0, 0, 16, 0]);
        assert_eq!(scsi::synchronize_cache10().bytes[0], 0x35);
        assert_eq!(scsi::read_capacity10().len, 10);

        let mut inq = [0u8; 36];
        inq[0] = 0x00;
        inq[1] = 0x80;
        inq[8..16].copy_from_slice(b"AriCode ");
        inq[16..32].copy_from_slice(b"Pendrive        ");
        let i = scsi::Inquiry::parse(&inq).unwrap();
        assert_eq!((i.peripheral_type, i.removable), (0, true));
        assert_eq!(&i.vendor, b"AriCode ");
        assert!(scsi::Inquiry::parse(&inq[..35]).is_none());

        let cap = [0, 0, 0x20, 0x00, 0, 0, 2, 0];
        assert_eq!(scsi::parse_read_capacity10(&cap), Some((0x2000, 512)));
        assert!(scsi::parse_read_capacity10(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 2, 0]).is_none());
        assert!(scsi::parse_read_capacity10(&[0, 0, 0x20, 0, 0, 0, 0, 0]).is_none());

        let mut sense = [0u8; 18];
        sense[0] = 0x70;
        sense[2] = 6;
        sense[12] = 0x29;
        let s = scsi::Sense::parse(&sense).unwrap();
        assert_eq!(
            (s.key, s.asc, s.ascq),
            (scsi::Sense::UNIT_ATTENTION, 0x29, 0)
        );
        assert!(!s.command_unsupported());
        sense[2] = 5;
        sense[12] = 0x20;
        assert!(scsi::Sense::parse(&sense).unwrap().command_unsupported());
        sense[12] = 0x24;
        assert!(scsi::Sense::parse(&sense).unwrap().command_unsupported());
        sense[12] = 0x21; // LBA out of range: not "unsupported"
        assert!(!scsi::Sense::parse(&sense).unwrap().command_unsupported());
        sense[0] = 0x00;
        assert!(scsi::Sense::parse(&sense).is_none());
    }
}
