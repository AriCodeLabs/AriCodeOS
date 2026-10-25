// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Versioned IPC protocols of the services (section 75). Each message is a
//! fixed-size block of bytes starting with a protocol identifier and
//! version, so a service rejects what it does not understand.

#![no_std]

/// Block service protocol (`BlockServiceProtocol v1`).
pub mod block {
    pub const PROTOCOL: u8 = 1;
    pub const VERSION: u8 = 1;
    /// Maximum sectors per request (8 KiB DMA buffer).
    pub const MAX_SECTORS: u16 = 16;
    pub const SECTOR_SIZE: u64 = 512;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Capacity in sectors in `Reply::value`.
        Info = 1,
        /// Reads `count` sectors from `lba` into the transferred DMA buffer.
        Read = 2,
        /// Writes `count` sectors from the transferred DMA buffer at `lba`.
        Write = 3,
        /// Makes previous writes durable (ADR 0014); carries only
        /// the reply handle. Without `VIRTIO_BLK_F_FLUSH` it returns without doing anything.
        Flush = 4,
    }

    /// Request (16 bytes). Message handles: `[reply (WRITE), DMA buffer (READ|WRITE)]`;
    /// `Info` carries only the reply handle.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub op: Op,
        pub count: u16,
        pub lba: u64,
    }

    impl Request {
        pub const LEN: usize = 16;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.op as u8;
            b[4..6].copy_from_slice(&self.count.to_le_bytes());
            b[8..16].copy_from_slice(&self.lba.to_le_bytes());
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let op = match b[2] {
                1 => Op::Info,
                2 => Op::Read,
                3 => Op::Write,
                4 => Op::Flush,
                _ => return None,
            };
            Some(Self {
                op,
                count: u16::from_le_bytes([b[4], b[5]]),
                lba: u64::from_le_bytes(b[8..16].try_into().ok()?),
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Ok = 0,
        /// Malformed or out-of-bounds request.
        Invalid = 1,
        /// Device error.
        Io = 2,
        /// Handles missing or not of the expected kind.
        BadHandles = 3,
    }

    /// Reply (16 bytes).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        /// Capacity (`Info`) or sectors transferred.
        pub value: u64,
    }

    impl Reply {
        pub const LEN: usize = 16;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.status as u8;
            b[8..16].copy_from_slice(&self.value.to_le_bytes());
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let status = match b[2] {
                0 => Status::Ok,
                1 => Status::Invalid,
                2 => Status::Io,
                3 => Status::BadHandles,
                _ => return None,
            };
            Some(Self {
                status,
                value: u64::from_le_bytes(b[8..16].try_into().ok()?),
            })
        }
    }
}

/// File service protocol (`FileServiceProtocol v1`).
///
/// File data does not travel in the message (256 bytes): the client
/// transfers a DMA buffer and the service reads from or writes to it.
pub mod fs {
    pub const PROTOCOL: u8 = 2;
    pub const VERSION: u8 = 1;
    pub const MAX_NAME: usize = 32;
    /// Maximum per read/write request (client buffer).
    pub const MAX_IO: u32 = 4096;
    /// Maximum size of an atomic replace (the AriFS journal, ADR 0018).
    pub const MAX_REPLACE: u32 = 32 * 1024;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// `arg` = index; reply: `value` = size, name in `Reply::name`.
        List = 1,
        /// Name in the request; reply: `value` = descriptor.
        Open = 2,
        /// Name in the request; creates it empty; reply: `value` = descriptor.
        Create = 3,
        /// `fd`, `offset`, `len`; handles `[reply, buffer]`; `value` = bytes read.
        Read = 4,
        /// `fd`, `offset`, `len`; handles `[reply, buffer]`; `value` = bytes written.
        Write = 5,
        /// `fd`.
        Close = 6,
        /// Main endpoint only (the broker): creates an endpoint bound
        /// to the prefix `name/`; the reply transfers that endpoint (`WRITE`).
        /// Whoever receives it only sees and creates files under its prefix.
        Bind = 7,
        /// Makes previous writes durable (ADR 0014).
        Flush = 8,
        /// `fd`, `len`; handles `[reply, buffer]`: replaces the whole file
        /// content atomically and durably (ADR 0014); `value`
        /// = bytes written.
        Replace = 9,
        /// Main endpoint only (the broker): creates an endpoint bound
        /// to a single file in read-only mode (ADR 0016); the reply
        /// transfers it (`WRITE`). Through it only that file can be seen, opened and read.
        /// `name` is a top-level file (no `/`, ADR 0050). `NotFound` if it
        /// does not exist, unless `len` = `SHARE_MAY_BE_MISSING`: then the
        /// file may appear later (sshd's `authorized_keys`).
        Share = 10,
        /// `fd`, `offset`, `len`; handles `[reply, buffer]`: one chunk of the
        /// new content to the journal, without committing anything (ADR 0018). Only
        /// one replace in progress per service (`Busy` for the others).
        ReplaceChunk = 11,
        /// `fd`, `len`: commits and applies the previous chunks at once;
        /// `value` = bytes of the new content.
        ReplaceCommit = 12,
        /// Main endpoint only (the broker, init): creates a session bound to
        /// the main namespace (every file, full names) and transfers its
        /// send side (`WRITE`) in the reply (ADR 0036). The holder sees what
        /// the main endpoint sees except `Bind`, `Share` and `Attach`; its
        /// descriptors are released when the kernel reports that nobody
        /// holds the send side any more (its process died).
        Attach = 13,
        /// Main endpoint and main-namespace sessions: `fd` = owner slot
        /// (0 = the main endpoint, then the sessions); reply: `value` =
        /// [`OwnerDescriptors`] packed, name = the owner's label (`main`,
        /// `root`, `dir NAME`, `file NAME`; empty for a free slot).
        /// `NotFound` past the last slot; `Denied` from a private
        /// directory or a shared file (ADR 0036).
        Descriptors = 14,
    }

    /// Descriptor table usage as `Op::Descriptors` reports it for one
    /// owner slot (ADR 0036): what that owner holds, what everybody holds,
    /// the table's capacity and how many descriptors the service has
    /// released from dead clients since it started. Four 16-bit counts in
    /// `Reply::value`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct OwnerDescriptors {
        pub owned: u16,
        pub used: u16,
        pub capacity: u16,
        pub reclaimed: u16,
    }

    impl OwnerDescriptors {
        pub fn encode(&self) -> u64 {
            self.owned as u64
                | (self.used as u64) << 16
                | (self.capacity as u64) << 32
                | (self.reclaimed as u64) << 48
        }

        pub fn decode(v: u64) -> Self {
            Self {
                owned: v as u16,
                used: (v >> 16) as u16,
                capacity: (v >> 32) as u16,
                reclaimed: (v >> 48) as u16,
            }
        }
    }

    /// Request (64 bytes): header, arguments and name.
    /// `Request::len` of a `Share` whose file may not exist yet.
    pub const SHARE_MAY_BE_MISSING: u32 = 1;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub op: Op,
        pub fd: u32,
        pub offset: u32,
        pub len: u32,
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
    }

    impl Request {
        pub const LEN: usize = 64;

        pub fn new(op: Op) -> Self {
            Self {
                op,
                fd: 0,
                offset: 0,
                len: 0,
                name: [0; MAX_NAME],
                name_len: 0,
            }
        }

        /// `None` if the name does not fit: an identifier is never truncated.
        pub fn with_name(mut self, name: &str) -> Option<Self> {
            if name.len() > MAX_NAME {
                return None;
            }
            self.name[..name.len()].copy_from_slice(name.as_bytes());
            self.name_len = name.len() as u8;
            Some(self)
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.op as u8;
            b[3] = self.name_len;
            b[4..8].copy_from_slice(&self.fd.to_le_bytes());
            b[8..12].copy_from_slice(&self.offset.to_le_bytes());
            b[12..16].copy_from_slice(&self.len.to_le_bytes());
            b[16..48].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let op = match b[2] {
                1 => Op::List,
                2 => Op::Open,
                3 => Op::Create,
                4 => Op::Read,
                5 => Op::Write,
                6 => Op::Close,
                7 => Op::Bind,
                8 => Op::Flush,
                9 => Op::Replace,
                10 => Op::Share,
                11 => Op::ReplaceChunk,
                12 => Op::ReplaceCommit,
                13 => Op::Attach,
                14 => Op::Descriptors,
                _ => return None,
            };
            let mut name = [0u8; MAX_NAME];
            name.copy_from_slice(&b[16..48]);
            Some(Self {
                op,
                fd: u32::from_le_bytes(b[4..8].try_into().ok()?),
                offset: u32::from_le_bytes(b[8..12].try_into().ok()?),
                len: u32::from_le_bytes(b[12..16].try_into().ok()?),
                name,
                name_len: b[3].min(MAX_NAME as u8),
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Ok = 0,
        Invalid = 1,
        NotFound = 2,
        Exists = 3,
        NoSpace = 4,
        BadHandles = 5,
        Io = 6,
        BadFd = 7,
        OutOfRange = 8,
        /// The endpoint does not allow the operation (shared read-only
        /// file, ADR 0016).
        Denied = 9,
        /// Another chunked replace is in progress (ADR 0018).
        Busy = 10,
    }

    /// Reply (64 bytes): status, value and name (for `List`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        pub value: u64,
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
    }

    impl Reply {
        pub const LEN: usize = 64;

        pub fn status(status: Status) -> Self {
            Self {
                status,
                value: 0,
                name: [0; MAX_NAME],
                name_len: 0,
            }
        }

        pub fn ok(value: u64) -> Self {
            Self {
                status: Status::Ok,
                value,
                name: [0; MAX_NAME],
                name_len: 0,
            }
        }

        pub fn with_name(mut self, name: &str) -> Self {
            let n = name.len().min(MAX_NAME);
            self.name[..n].copy_from_slice(&name.as_bytes()[..n]);
            self.name_len = n as u8;
            self
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.status as u8;
            b[3] = self.name_len;
            b[8..16].copy_from_slice(&self.value.to_le_bytes());
            b[16..48].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let status = match b[2] {
                0 => Status::Ok,
                1 => Status::Invalid,
                2 => Status::NotFound,
                3 => Status::Exists,
                4 => Status::NoSpace,
                5 => Status::BadHandles,
                6 => Status::Io,
                7 => Status::BadFd,
                8 => Status::OutOfRange,
                9 => Status::Denied,
                10 => Status::Busy,
                _ => return None,
            };
            let mut name = [0u8; MAX_NAME];
            name.copy_from_slice(&b[16..48]);
            Some(Self {
                status,
                value: u64::from_le_bytes(b[8..16].try_into().ok()?),
                name,
                name_len: b[3].min(MAX_NAME as u8),
            })
        }
    }
}

/// Network service protocol (`NetServiceProtocol v1`). Datagrams
/// travel through the client's DMA buffer; the message carries addresses,
/// ports and lengths.
pub mod net {
    pub const PROTOCOL: u8 = 3;
    pub const VERSION: u8 = 1;
    /// `Op::Link` replies.
    pub const LINK_UNKNOWN: u64 = 0;
    pub const LINK_DOWN: u64 = 1;
    pub const LINK_UP: u64 = 2;
    /// Maximum datagram size per request (client buffer).
    pub const MAX_DATAGRAM: u16 = 1472;
    /// UDP port of the host test responder (echo and DNS), reachable
    /// from the guest at the virtual host address (10.0.2.2).
    pub const TEST_RESPONDER_PORT: u16 = 17007;
    /// TCP port of the host test echo server.
    pub const TEST_TCP_ECHO_PORT: u16 = 17008;
    /// TCP port the guest test program listens on (the host reaches it
    /// through a `hostfwd` port chosen per test run, ADR 0040).
    pub const TEST_TCP_LISTEN_PORT: u16 = 7;
    /// Port of the host test HTTP server.
    pub const TEST_HTTP_PORT: u16 = 17010;
    /// Longest label of a client session (`Op::Attach`): the bound the
    /// kernel gives process names, which is what the label is.
    pub const MAX_LABEL: usize = 24;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Configuration: `Reply::data` = ip, netmask, router, dns, mac.
        Info = 1,
        /// ICMP echo to `addr` with `id` as sequence; `value` = RTT in ms.
        Ping = 2,
        /// Binds UDP port `port`; `value` = socket.
        UdpBind = 3,
        /// Sends `len` bytes of the buffer from `sock` to `addr:port`.
        UdpSend = 4,
        /// Waits up to `timeout_ms` for a datagram on `sock`; `value` = bytes,
        /// sender in `Reply::data`/`Reply::port`.
        UdpRecv = 5,
        /// Releases `sock`.
        UdpClose = 6,
        /// Resolves the name carried in the client buffer (`len` bytes,
        /// up to 253) at `addr:port` (0 = DHCP server).
        Dns = 7,
        /// Connects to `addr:port`; waits up to `timeout_ms`; `value` = socket.
        TcpConnect = 8,
        /// Listens on `port`; `value` = listening socket.
        TcpListen = 9,
        /// Waits up to `timeout_ms` for a connection on `sock`; `value` = new
        /// socket, peer in `Reply::data`/`Reply::port`.
        TcpAccept = 10,
        /// Queues `len` bytes of the buffer; `value` = accepted (0 if full).
        TcpSend = 11,
        /// Waits up to `timeout_ms` for data on `sock`; `value` = bytes (0 = the
        /// other end closed).
        TcpRecv = 12,
        /// Orderly close of `sock`.
        TcpClose = 13,
        /// Physical link state: `value` = [`LINK_UNKNOWN`], [`LINK_DOWN`]
        /// or [`LINK_UP`]. Works without DHCP configuration.
        Link = 14,
        /// TCP table by state: `Reply::data` = [`TcpStats`] (incident 0013).
        /// Works without DHCP configuration.
        TcpStats = 15,
        /// Main endpoint only (init, the broker): a session for one client
        /// labelled `Request::label` (ADR 0037). The reply carries the send
        /// side of a new endpoint; the sockets opened through it belong to
        /// that client and are released when the kernel reports it gone.
        /// `Invalid` from a session, `Busy` when every session slot is taken.
        /// `Request::port`: the one port below `PRIVILEGED_PORTS` the session
        /// may listen or bind on (0: none).
        Attach = 16,
        /// Socket table usage of owner slot `sock` (0 = main endpoint,
        /// `1..` = sessions): `Reply::value` = [`OwnerSockets`] packed,
        /// `Reply::label` = the owner's label (empty for a free slot).
        /// `NotFound` past the last slot. Works without DHCP configuration.
        Sockets = 17,
    }

    /// Request (64 bytes). DNS names do not travel in the message: they go through
    /// the client buffer, without an artificial limit. The label of an
    /// `Attach` does (bytes 20..45): it is a process name, bounded like one.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub op: Op,
        pub sock: u16,
        pub port: u16,
        pub len: u16,
        pub addr: [u8; 4],
        pub timeout_ms: u32,
        pub id: u16,
        pub label: [u8; MAX_LABEL],
        pub label_len: u8,
    }

    impl Request {
        pub const LEN: usize = 64;

        pub fn new(op: Op) -> Self {
            Self {
                op,
                sock: 0,
                port: 0,
                len: 0,
                addr: [0; 4],
                timeout_ms: 0,
                id: 0,
                label: [0; MAX_LABEL],
                label_len: 0,
            }
        }

        /// With the client label of an `Attach`; `None` if it does not fit
        /// (a label is never cut here: the caller decides what to send).
        pub fn with_label(mut self, label: &str) -> Option<Self> {
            if label.len() > MAX_LABEL {
                return None;
            }
            self.label[..label.len()].copy_from_slice(label.as_bytes());
            self.label_len = label.len() as u8;
            Some(self)
        }

        pub fn label_str(&self) -> &str {
            core::str::from_utf8(&self.label[..self.label_len as usize]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.op as u8;
            b[4..6].copy_from_slice(&self.sock.to_le_bytes());
            b[6..8].copy_from_slice(&self.port.to_le_bytes());
            b[8..10].copy_from_slice(&self.len.to_le_bytes());
            b[10..14].copy_from_slice(&self.addr);
            b[14..18].copy_from_slice(&self.timeout_ms.to_le_bytes());
            b[18..20].copy_from_slice(&self.id.to_le_bytes());
            b[20..20 + MAX_LABEL].copy_from_slice(&self.label);
            b[20 + MAX_LABEL] = self.label_len;
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let op = match b[2] {
                1 => Op::Info,
                2 => Op::Ping,
                3 => Op::UdpBind,
                4 => Op::UdpSend,
                5 => Op::UdpRecv,
                6 => Op::UdpClose,
                7 => Op::Dns,
                8 => Op::TcpConnect,
                9 => Op::TcpListen,
                10 => Op::TcpAccept,
                11 => Op::TcpSend,
                12 => Op::TcpRecv,
                13 => Op::TcpClose,
                14 => Op::Link,
                15 => Op::TcpStats,
                16 => Op::Attach,
                17 => Op::Sockets,
                _ => return None,
            };
            let mut addr = [0u8; 4];
            addr.copy_from_slice(&b[10..14]);
            let mut label = [0u8; MAX_LABEL];
            label.copy_from_slice(&b[20..20 + MAX_LABEL]);
            let label_len = b[20 + MAX_LABEL];
            // A label is a process name: valid UTF-8 or the request is
            // invalid (not silently read as an empty name).
            if label_len as usize > MAX_LABEL
                || core::str::from_utf8(&label[..label_len as usize]).is_err()
            {
                return None;
            }
            Some(Self {
                op,
                sock: u16::from_le_bytes(b[4..6].try_into().ok()?),
                port: u16::from_le_bytes(b[6..8].try_into().ok()?),
                len: u16::from_le_bytes(b[8..10].try_into().ok()?),
                addr,
                timeout_ms: u32::from_le_bytes(b[14..18].try_into().ok()?),
                id: u16::from_le_bytes(b[18..20].try_into().ok()?),
                label,
                label_len,
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Ok = 0,
        Invalid = 1,
        /// Deadline expired without a reply.
        Timeout = 2,
        /// No network configuration yet (DHCP in progress).
        NoRoute = 3,
        BadHandles = 4,
        /// Too many pending requests.
        Busy = 5,
        BadSock = 6,
        InUse = 7,
        /// The DNS server answered without an address.
        NotFound = 8,
        /// The connection was refused or reset by the other end.
        Reset = 9,
        /// A port below `PRIVILEGED_PORTS` the session was not given.
        Denied = 10,
    }

    /// Ports a session may listen or bind on only if `Attach` gave it that
    /// one (audit 2026-10-25 E, L1: any program with `net` could listen on
    /// 22 while sshd was down and pose as it).
    pub const PRIVILEGED_PORTS: u16 = 1024;
    /// The SSH server's port, given to sshd's session.
    pub const SSH_PORT: u16 = 22;

    /// Reply (48 bytes): status, value, port and data (addresses).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        pub port: u16,
        pub value: u64,
        pub data: [u8; 24],
    }

    impl Reply {
        pub const LEN: usize = 48;

        pub fn status(status: Status) -> Self {
            Self {
                status,
                port: 0,
                value: 0,
                data: [0; 24],
            }
        }

        pub fn ok(value: u64) -> Self {
            Self::status(Status::Ok).with_value(value)
        }

        pub fn with_value(mut self, value: u64) -> Self {
            self.value = value;
            self
        }

        pub fn with_addr(mut self, addr: [u8; 4], port: u16) -> Self {
            self.data[..4].copy_from_slice(&addr);
            self.port = port;
            self
        }

        pub fn addr(&self) -> [u8; 4] {
            let mut a = [0u8; 4];
            a.copy_from_slice(&self.data[..4]);
            a
        }

        /// An owner's label (`Op::Sockets`): in `data`, its length in `port`.
        pub fn with_label(mut self, label: &str) -> Self {
            let n = label.len().min(self.data.len());
            self.data[..n].copy_from_slice(&label.as_bytes()[..n]);
            self.port = n as u16;
            self
        }

        pub fn label_str(&self) -> &str {
            let n = (self.port as usize).min(self.data.len());
            core::str::from_utf8(&self.data[..n]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.status as u8;
            b[4..6].copy_from_slice(&self.port.to_le_bytes());
            b[8..16].copy_from_slice(&self.value.to_le_bytes());
            b[16..40].copy_from_slice(&self.data);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let status = match b[2] {
                0 => Status::Ok,
                1 => Status::Invalid,
                2 => Status::Timeout,
                3 => Status::NoRoute,
                4 => Status::BadHandles,
                5 => Status::Busy,
                6 => Status::BadSock,
                7 => Status::InUse,
                8 => Status::NotFound,
                9 => Status::Reset,
                10 => Status::Denied,
                _ => return None,
            };
            let mut data = [0u8; 24];
            data.copy_from_slice(&b[16..40]);
            Some(Self {
                status,
                port: u16::from_le_bytes(b[4..6].try_into().ok()?),
                value: u64::from_le_bytes(b[8..16].try_into().ok()?),
                data,
            })
        }
    }

    /// Network configuration as carried in `Reply::data` of `Info`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Info {
        pub address: [u8; 4],
        pub mask: [u8; 4],
        pub router: [u8; 4],
        pub dns: [u8; 4],
        pub mac: [u8; 6],
    }

    /// The service's TCP table by state, as carried in `Reply::data` of
    /// `TcpStats` (incident 0013, ADR 0035): what `diag` shows so a full
    /// table can be read on the machine. Every slot is in exactly one of the
    /// counts; `slots` is their sum.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct TcpStats {
        /// Size of the table.
        pub slots: u16,
        /// Listening ports.
        pub listen: u16,
        /// Passive connections whose SYN|ACK is not yet acknowledged
        /// (half-open; they count towards their listener's backlog).
        pub syn_received: u16,
        /// Active opens whose SYN is not yet answered.
        pub syn_sent: u16,
        /// Established connections nobody has accepted yet (the listener's
        /// backlog, with `syn_received`).
        pub backlog: u16,
        /// Established connections in use (accepted or actively opened).
        pub established: u16,
        /// Closing: FIN sent or received, and finished connections not yet
        /// reaped.
        pub closing: u16,
        /// TIME-WAIT.
        pub time_wait: u16,
        /// Free slots.
        pub free: u16,
        /// SYNs to a listener answered with a reset since boot: no free
        /// slot or the listener's backlog at its cap.
        pub syn_dropped: u32,
    }

    impl TcpStats {
        pub fn encode(&self) -> [u8; 24] {
            let mut d = [0u8; 24];
            for (i, v) in [
                self.slots,
                self.listen,
                self.syn_received,
                self.syn_sent,
                self.backlog,
                self.established,
                self.closing,
                self.time_wait,
                self.free,
            ]
            .iter()
            .enumerate()
            {
                d[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
            d[18..22].copy_from_slice(&self.syn_dropped.to_le_bytes());
            d
        }

        pub fn decode(d: &[u8; 24]) -> Self {
            let u = |i: usize| u16::from_le_bytes([d[2 * i], d[2 * i + 1]]);
            Self {
                slots: u(0),
                listen: u(1),
                syn_received: u(2),
                syn_sent: u(3),
                backlog: u(4),
                established: u(5),
                closing: u(6),
                time_wait: u(7),
                free: u(8),
                syn_dropped: u32::from_le_bytes([d[18], d[19], d[20], d[21]]),
            }
        }
    }

    /// Socket table usage as `Op::Sockets` reports it for one owner slot
    /// (ADR 0037): the sockets and waits (TCP, UDP, ping, DNS together)
    /// that owner holds, what everybody holds, how many the service has
    /// released from dead clients since it started and how many client
    /// sessions are live. Four 16-bit counts in `Reply::value`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct OwnerSockets {
        pub owned: u16,
        pub used: u16,
        pub reclaimed: u16,
        pub sessions: u16,
    }

    impl OwnerSockets {
        pub fn encode(&self) -> u64 {
            self.owned as u64
                | (self.used as u64) << 16
                | (self.reclaimed as u64) << 32
                | (self.sessions as u64) << 48
        }

        pub fn decode(v: u64) -> Self {
            Self {
                owned: v as u16,
                used: (v >> 16) as u16,
                reclaimed: (v >> 32) as u16,
                sessions: (v >> 48) as u16,
            }
        }
    }

    impl Info {
        pub fn encode(&self) -> [u8; 24] {
            let mut d = [0u8; 24];
            d[..4].copy_from_slice(&self.address);
            d[4..8].copy_from_slice(&self.mask);
            d[8..12].copy_from_slice(&self.router);
            d[12..16].copy_from_slice(&self.dns);
            d[16..22].copy_from_slice(&self.mac);
            d
        }

        pub fn decode(d: &[u8; 24]) -> Self {
            let mut i = Info::default();
            i.address.copy_from_slice(&d[..4]);
            i.mask.copy_from_slice(&d[4..8]);
            i.router.copy_from_slice(&d[8..12]);
            i.dns.copy_from_slice(&d[12..16]);
            i.mac.copy_from_slice(&d[16..22]);
            i
        }
    }
}

/// Input service protocol (`InputServiceProtocol v1`): evdev events
/// from a virtio-input device.
pub mod input {
    pub const PROTOCOL: u8 = 4;
    pub const VERSION: u8 = 1;
    /// Events per reply.
    pub const MAX_EVENTS: usize = 4;
    pub const MAX_NAME: usize = 32;
    /// The longest a read waits: a longer `timeout_ms` is cut to this, so a
    /// reader that died while its read waited holds the service's one
    /// place no longer (audit 2026-10-25 C, M4).
    pub const MAX_READ_MS: u32 = 10_000;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Device name in `Reply::name`.
        Info = 1,
        /// Waits up to `timeout_ms` (at most `MAX_READ_MS`) and returns up
        /// to `MAX_EVENTS` events.
        Read = 2,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub op: Op,
        pub timeout_ms: u32,
    }

    impl Request {
        pub const LEN: usize = 8;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.op as u8;
            b[4..8].copy_from_slice(&self.timeout_ms.to_le_bytes());
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let op = match b[2] {
                1 => Op::Info,
                2 => Op::Read,
                _ => return None,
            };
            Some(Self {
                op,
                timeout_ms: u32::from_le_bytes(b[4..8].try_into().ok()?),
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Ok = 0,
        Invalid = 1,
        Timeout = 2,
        Busy = 3,
    }

    /// Event (8 bytes, evdev format): type, code, value.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Event {
        pub kind: u16,
        pub code: u16,
        pub value: u32,
    }

    /// Reply (104 bytes): status, number of events, events, the timestamp
    /// (ms since boot) at which the service received each one, and name. The
    /// timestamps allow ordering events from several devices.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        pub count: u8,
        pub events: [Event; MAX_EVENTS],
        pub times: [u64; MAX_EVENTS],
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
    }

    impl Reply {
        pub const LEN: usize = 8 + 16 * MAX_EVENTS + MAX_NAME;
        const TIMES: usize = 8 + 8 * MAX_EVENTS;
        const NAME: usize = 8 + 16 * MAX_EVENTS;

        pub fn status(status: Status) -> Self {
            Self {
                status,
                count: 0,
                events: [Event::default(); MAX_EVENTS],
                times: [0; MAX_EVENTS],
                name: [0; MAX_NAME],
                name_len: 0,
            }
        }

        /// Events with their timestamps (`times[i]` goes with `events[i]`).
        pub fn events(events: &[Event], times: &[u64]) -> Self {
            let mut r = Self::status(Status::Ok);
            let n = events.len().min(times.len()).min(MAX_EVENTS);
            r.events[..n].copy_from_slice(&events[..n]);
            r.times[..n].copy_from_slice(&times[..n]);
            r.count = n as u8;
            r
        }

        /// `None` if the name does not fit.
        pub fn with_name(mut self, name: &str) -> Option<Self> {
            if name.len() > MAX_NAME {
                return None;
            }
            self.name[..name.len()].copy_from_slice(name.as_bytes());
            self.name_len = name.len() as u8;
            Some(self)
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.status as u8;
            b[3] = self.count;
            b[4] = self.name_len;
            for (i, e) in self.events.iter().enumerate() {
                let o = 8 + 8 * i;
                b[o..o + 2].copy_from_slice(&e.kind.to_le_bytes());
                b[o + 2..o + 4].copy_from_slice(&e.code.to_le_bytes());
                b[o + 4..o + 8].copy_from_slice(&e.value.to_le_bytes());
            }
            for (i, t) in self.times.iter().enumerate() {
                let o = Self::TIMES + 8 * i;
                b[o..o + 8].copy_from_slice(&t.to_le_bytes());
            }
            b[Self::NAME..].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let status = match b[2] {
                0 => Status::Ok,
                1 => Status::Invalid,
                2 => Status::Timeout,
                3 => Status::Busy,
                _ => return None,
            };
            let mut events = [Event::default(); MAX_EVENTS];
            for (i, e) in events.iter_mut().enumerate() {
                let o = 8 + 8 * i;
                *e = Event {
                    kind: u16::from_le_bytes([b[o], b[o + 1]]),
                    code: u16::from_le_bytes([b[o + 2], b[o + 3]]),
                    value: u32::from_le_bytes(b[o + 4..o + 8].try_into().ok()?),
                };
            }
            let mut times = [0u64; MAX_EVENTS];
            for (i, t) in times.iter_mut().enumerate() {
                let o = Self::TIMES + 8 * i;
                *t = u64::from_le_bytes(b[o..o + 8].try_into().ok()?);
            }
            let mut name = [0u8; MAX_NAME];
            name.copy_from_slice(&b[Self::NAME..Self::LEN]);
            Some(Self {
                status,
                count: b[3].min(MAX_EVENTS as u8),
                events,
                times,
                name,
                name_len: b[4].min(MAX_NAME as u8),
            })
        }
    }
}

/// Compositor protocol (`WindowProtocol v1`): windows whose content
/// lives in a client DMA buffer, and already routed input events.
pub mod window {
    pub const PROTOCOL: u8 = 5;
    pub const VERSION: u8 = 1;
    pub const MAX_TITLE: usize = 24;
    /// Maximum window size (pixels per side).
    pub const MAX_SIDE: u32 = 4096;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Creates a `w`x`h` window with `title`; handles `[reply, buffer]`
        /// (the buffer has `w*h*4` bytes in `0x00RRGGBB`); `value` = id.
        Create = 1,
        /// The buffer content changed: recompose.
        Present = 2,
        /// Waits up to `timeout_ms` for a window event.
        Poll = 3,
        Close = 4,
    }

    /// Request (64 bytes).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub op: Op,
        pub window: u32,
        pub width: u32,
        pub height: u32,
        pub timeout_ms: u32,
        pub title: [u8; MAX_TITLE],
        pub title_len: u8,
    }

    impl Request {
        pub const LEN: usize = 64;

        pub fn new(op: Op) -> Self {
            Self {
                op,
                window: 0,
                width: 0,
                height: 0,
                timeout_ms: 0,
                title: [0; MAX_TITLE],
                title_len: 0,
            }
        }

        /// `None` if the title does not fit.
        pub fn with_title(mut self, title: &str) -> Option<Self> {
            if title.len() > MAX_TITLE {
                return None;
            }
            self.title[..title.len()].copy_from_slice(title.as_bytes());
            self.title_len = title.len() as u8;
            Some(self)
        }

        pub fn title_str(&self) -> &str {
            core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.op as u8;
            b[3] = self.title_len;
            b[4..8].copy_from_slice(&self.window.to_le_bytes());
            b[8..12].copy_from_slice(&self.width.to_le_bytes());
            b[12..16].copy_from_slice(&self.height.to_le_bytes());
            b[16..20].copy_from_slice(&self.timeout_ms.to_le_bytes());
            b[24..48].copy_from_slice(&self.title);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let op = match b[2] {
                1 => Op::Create,
                2 => Op::Present,
                3 => Op::Poll,
                4 => Op::Close,
                _ => return None,
            };
            let mut title = [0u8; MAX_TITLE];
            title.copy_from_slice(&b[24..48]);
            Some(Self {
                op,
                window: u32::from_le_bytes(b[4..8].try_into().ok()?),
                width: u32::from_le_bytes(b[8..12].try_into().ok()?),
                height: u32::from_le_bytes(b[12..16].try_into().ok()?),
                timeout_ms: u32::from_le_bytes(b[16..20].try_into().ok()?),
                title,
                title_len: b[3].min(MAX_TITLE as u8),
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Ok = 0,
        Invalid = 1,
        Busy = 2,
        BadHandles = 3,
        BadWindow = 4,
        Timeout = 5,
    }

    /// Event delivered to a window.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Event {
        /// Character already translated by the compositor.
        Key(u8),
        /// Button pressed, in content coordinates.
        Click { x: i32, y: i32, button: u8 },
        /// The window gained (`true`) or lost focus.
        Focus(bool),
    }

    /// Reply (32 bytes): status, value and optional event.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        pub value: u64,
        pub event: Option<Event>,
    }

    impl Reply {
        pub const LEN: usize = 32;

        pub fn status(status: Status) -> Self {
            Self {
                status,
                value: 0,
                event: None,
            }
        }
        pub fn ok(value: u64) -> Self {
            Self::status(Status::Ok).with_value(value)
        }
        pub fn with_value(mut self, value: u64) -> Self {
            self.value = value;
            self
        }
        pub fn event(event: Event) -> Self {
            Self {
                status: Status::Ok,
                value: 0,
                event: Some(event),
            }
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.status as u8;
            b[8..16].copy_from_slice(&self.value.to_le_bytes());
            match self.event {
                None => b[3] = 0,
                Some(Event::Key(c)) => {
                    b[3] = 1;
                    b[16] = c;
                }
                Some(Event::Click { x, y, button }) => {
                    b[3] = 2;
                    b[16..20].copy_from_slice(&x.to_le_bytes());
                    b[20..24].copy_from_slice(&y.to_le_bytes());
                    b[24] = button;
                }
                Some(Event::Focus(f)) => {
                    b[3] = 3;
                    b[16] = f as u8;
                }
            }
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let status = match b[2] {
                0 => Status::Ok,
                1 => Status::Invalid,
                2 => Status::Busy,
                3 => Status::BadHandles,
                4 => Status::BadWindow,
                5 => Status::Timeout,
                _ => return None,
            };
            let event = match b[3] {
                0 => None,
                1 => Some(Event::Key(b[16])),
                2 => Some(Event::Click {
                    x: i32::from_le_bytes(b[16..20].try_into().ok()?),
                    y: i32::from_le_bytes(b[20..24].try_into().ok()?),
                    button: b[24],
                }),
                3 => Some(Event::Focus(b[16] != 0)),
                _ => return None,
            };
            Some(Self {
                status,
                value: u64::from_le_bytes(b[8..16].try_into().ok()?),
                event,
            })
        }
    }
}

/// Launch broker protocol (`LaunchProtocol v1`): a program
/// asks init to run another; init reads its manifest, decides which
/// capabilities to hand over and returns the exit code.
pub mod launch {
    pub const PROTOCOL: u8 = 6;
    pub const VERSION: u8 = 1;
    pub const MAX_NAME: usize = 32;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub arg: u64,
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
        /// `DETACH`: reply `Started` when the process is created, without waiting
        /// for it to exit (ADR 0015). `CONSOLE`: a second handle follows the
        /// reply endpoint and becomes the program's console (ADR 0047).
        pub flags: u8,
    }

    impl Request {
        pub const LEN: usize = 48;
        /// Background launch.
        pub const DETACH: u8 = 1;
        /// The request carries the program's console (the send side of an
        /// endpoint); never with `DETACH`.
        pub const CONSOLE: u8 = 2;

        /// `None` if the name does not fit.
        pub fn new(name: &str, arg: u64) -> Option<Self> {
            if name.is_empty() || name.len() > MAX_NAME {
                return None;
            }
            let mut n = [0u8; MAX_NAME];
            n[..name.len()].copy_from_slice(name.as_bytes());
            Some(Self {
                arg,
                name: n,
                name_len: name.len() as u8,
                flags: 0,
            })
        }

        pub fn detached(mut self) -> Self {
            self.flags |= Self::DETACH;
            self
        }

        pub fn is_detached(&self) -> bool {
            self.flags & Self::DETACH != 0
        }

        pub fn with_console(mut self) -> Self {
            self.flags |= Self::CONSOLE;
            self
        }

        pub fn has_console(&self) -> bool {
            self.flags & Self::CONSOLE != 0
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.name_len;
            b[3] = self.flags;
            b[8..16].copy_from_slice(&self.arg.to_le_bytes());
            b[16..48].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let mut name = [0u8; MAX_NAME];
            name.copy_from_slice(&b[16..48]);
            Some(Self {
                arg: u64::from_le_bytes(b[8..16].try_into().ok()?),
                name,
                name_len: b[2].min(MAX_NAME as u8),
                flags: b[3],
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        /// `value` = exit code of the program.
        Ok = 0,
        Invalid = 1,
        /// No such program in the initrd.
        NotFound = 2,
        /// The manifest asks for capabilities the broker does not grant.
        Denied = 3,
        /// The program died from a fault.
        Killed = 4,
        /// Unreadable manifest.
        BadManifest = 5,
        /// The broker could not launch it (resources).
        Failed = 6,
        /// Launched in the background (`DETACH`); `value` = 0.
        Started = 7,
        /// Broker limit of running programs (ADR 0015).
        Limit = 8,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        pub value: u64,
    }

    impl Reply {
        pub const LEN: usize = 16;

        pub fn status(status: Status) -> Self {
            Self { status, value: 0 }
        }
        pub fn exited(code: u64) -> Self {
            Self {
                status: Status::Ok,
                value: code,
            }
        }
        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.status as u8;
            b[8..16].copy_from_slice(&self.value.to_le_bytes());
            b
        }
        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let status = match b[2] {
                0 => Status::Ok,
                1 => Status::Invalid,
                2 => Status::NotFound,
                3 => Status::Denied,
                4 => Status::Killed,
                5 => Status::BadManifest,
                6 => Status::Failed,
                7 => Status::Started,
                8 => Status::Limit,
                _ => return None,
            };
            Some(Self {
                status,
                value: u64::from_le_bytes(b[8..16].try_into().ok()?),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::block::*;

    #[test]
    fn launch_round_trip() {
        use super::launch::*;
        let r = Request::new("hello_app", 7).unwrap();
        let d = Request::decode(&r.encode()).unwrap();
        assert_eq!(d, r);
        assert_eq!(d.name_str(), "hello_app");
        assert!(!d.has_console() && !d.is_detached());
        let c = Request::decode(&r.with_console().encode()).unwrap();
        assert!(c.has_console() && !c.is_detached());
        assert!(Request::new("", 0).is_none());
        assert!(Request::new(&"x".repeat(33), 0).is_none());
        assert_eq!(
            Reply::decode(&Reply::exited(42).encode()),
            Some(Reply::exited(42))
        );
        assert_eq!(
            Reply::decode(&Reply::status(Status::Denied).encode())
                .unwrap()
                .status,
            Status::Denied
        );
    }

    #[test]
    fn window_round_trip() {
        use super::window::*;
        let mut r = Request::new(Op::Create).with_title("Demo A").unwrap();
        r.width = 400;
        r.height = 300;
        let d = Request::decode(&r.encode()).unwrap();
        assert_eq!(d, r);
        assert_eq!(d.title_str(), "Demo A");
        assert!(Request::new(Op::Create)
            .with_title(&"t".repeat(25))
            .is_none());
        for ev in [
            Event::Key(b'h'),
            Event::Click {
                x: -3,
                y: 280,
                button: 1,
            },
            Event::Focus(true),
        ] {
            let reply = Reply::event(ev);
            assert_eq!(Reply::decode(&reply.encode()), Some(reply));
        }
        let ok = Reply::ok(7);
        assert_eq!(Reply::decode(&ok.encode()), Some(ok));
        assert_eq!(
            Reply::decode(&Reply::status(Status::BadWindow).encode())
                .unwrap()
                .status,
            Status::BadWindow
        );
    }

    #[test]
    fn input_round_trip() {
        use super::input::*;
        let r = Request {
            op: Op::Read,
            timeout_ms: 1500,
        };
        assert_eq!(Request::decode(&r.encode()), Some(r));
        let evs = [
            Event {
                kind: 1,
                code: 35,
                value: 1,
            },
            Event {
                kind: 2,
                code: 1,
                value: (-5i32) as u32,
            },
        ];
        let reply = Reply::events(&evs, &[1000, 1003])
            .with_name("QEMU Virtio Keyboard")
            .unwrap();
        let d = Reply::decode(&reply.encode()).unwrap();
        assert_eq!(d, reply);
        assert_eq!(d.count, 2);
        assert_eq!(d.name_str(), "QEMU Virtio Keyboard");
        assert!(Reply::status(Status::Ok)
            .with_name(&"x".repeat(33))
            .is_none());
        assert_eq!(
            Reply::decode(&Reply::status(Status::Timeout).encode())
                .unwrap()
                .status,
            Status::Timeout
        );
    }

    #[test]
    fn net_request_reply_round_trip() {
        use super::net::*;
        let mut r = Request::new(Op::UdpSend);
        r.sock = 3;
        r.port = 17007;
        r.len = 14;
        r.addr = [10, 0, 2, 2];
        r.timeout_ms = 2000;
        r.id = 7;
        let d = Request::decode(&r.encode()).unwrap();
        assert_eq!(d, r);
        let reply = Reply::ok(14).with_addr([10, 0, 2, 2], 17007);
        let back = Reply::decode(&reply.encode()).unwrap();
        assert_eq!(back, reply);
        assert_eq!(back.addr(), [10, 0, 2, 2]);
        let info = Info {
            address: [10, 0, 2, 15],
            mask: [255, 255, 255, 0],
            router: [10, 0, 2, 2],
            dns: [10, 0, 2, 3],
            mac: [0x52, 0x54, 0, 0x12, 0x34, 0x56],
        };
        assert_eq!(Info::decode(&info.encode()), info);
        let link = Request::new(Op::Link);
        assert_eq!(Request::decode(&link.encode()).unwrap().op, Op::Link);
        let stats = Request::new(Op::TcpStats);
        assert_eq!(Request::decode(&stats.encode()).unwrap().op, Op::TcpStats);
        let table = TcpStats {
            slots: 8,
            listen: 1,
            syn_received: 2,
            syn_sent: 1,
            backlog: 1,
            established: 1,
            closing: 1,
            time_wait: 1,
            free: 0,
            syn_dropped: 70_000,
        };
        assert_eq!(TcpStats::decode(&table.encode()), table);
        assert_eq!(TcpStats::decode(&[0; 24]), TcpStats::default());
        // ADR 0037: a session for a labelled client and the table per owner.
        let attach = Request::new(Op::Attach).with_label("sock_leaker").unwrap();
        let d = Request::decode(&attach.encode()).unwrap();
        assert_eq!(d, attach);
        assert_eq!(d.label_str(), "sock_leaker");
        assert_eq!(
            Request::new(Op::Attach).with_label("a name longer than the process name bound"),
            None
        );
        let mut bad_label = attach.encode();
        bad_label[20 + MAX_LABEL] = MAX_LABEL as u8 + 1;
        assert!(Request::decode(&bad_label).is_none());
        let mut not_utf8 = attach.encode();
        not_utf8[20] = 0xff;
        assert!(Request::decode(&not_utf8).is_none());
        // Bytes past the length are not part of the label.
        let mut tail = Request::new(Op::Attach).with_label("sh").unwrap().encode();
        tail[22] = 0xff;
        assert_eq!(Request::decode(&tail).unwrap().label_str(), "sh");
        assert_eq!(
            Request::decode(&Request::new(Op::Sockets).encode())
                .unwrap()
                .op,
            Op::Sockets
        );
        let usage = OwnerSockets {
            owned: 3,
            used: 16,
            reclaimed: 65535,
            sessions: 15,
        };
        assert_eq!(OwnerSockets::decode(usage.encode()), usage);
        assert_eq!(OwnerSockets::decode(0), OwnerSockets::default());
        let labelled = Reply::ok(usage.encode()).with_label("shell");
        let back = Reply::decode(&labelled.encode()).unwrap();
        assert_eq!(OwnerSockets::decode(back.value), usage);
        assert_eq!(back.label_str(), "shell");
        assert_eq!(Reply::ok(0).label_str(), "");
        let mut bad = r.encode();
        bad[2] = 99;
        assert!(Request::decode(&bad).is_none());
        assert_eq!(
            Reply::decode(&Reply::status(Status::InUse).encode())
                .unwrap()
                .status,
            Status::InUse
        );
        let t = Request::new(Op::TcpAccept);
        assert_eq!(Request::decode(&t.encode()).unwrap().op, Op::TcpAccept);
        assert_eq!(
            Reply::decode(&Reply::status(Status::Reset).encode())
                .unwrap()
                .status,
            Status::Reset
        );
    }

    #[test]
    fn fs_request_and_reply_round_trip() {
        use super::fs::*;
        let mut r = Request::new(Op::Read).with_name("hola.txt").unwrap();
        r.fd = 3;
        r.offset = 512;
        r.len = 100;
        let d = Request::decode(&r.encode()).unwrap();
        assert_eq!(d, r);
        assert_eq!(d.name_str(), "hola.txt");
        let reply = Reply::ok(17).with_name("hola.txt");
        assert_eq!(Reply::decode(&reply.encode()), Some(reply));
        assert_eq!(
            Reply::decode(&Reply::status(Status::BadFd).encode())
                .unwrap()
                .status,
            Status::BadFd
        );
        let mut bad = r.encode();
        bad[2] = 99;
        assert!(Request::decode(&bad).is_none());
        assert!(
            Request::new(Op::Open)
                .with_name("abcdefghijklmnopqrstuvwxyz0123456789")
                .is_none(),
            "a long name is rejected, never truncated"
        );
        let exact = "a".repeat(MAX_NAME);
        assert_eq!(
            Request::new(Op::Open).with_name(&exact).unwrap().name_str(),
            exact
        );
    }

    /// ADR 0036: the additive operations decode, and the descriptor usage
    /// packs four counts without one overwriting another.
    #[test]
    fn fs_session_operations_and_descriptor_usage() {
        use super::fs::*;
        for op in [Op::Attach, Op::Descriptors] {
            let mut r = Request::new(op);
            r.fd = 7;
            assert_eq!(Request::decode(&r.encode()), Some(r));
        }
        let usage = OwnerDescriptors {
            owned: 3,
            used: 16,
            capacity: 16,
            reclaimed: 65535,
        };
        assert_eq!(OwnerDescriptors::decode(usage.encode()), usage);
        let reply = Reply::ok(usage.encode()).with_name("dir hello_ext");
        let d = Reply::decode(&reply.encode()).unwrap();
        assert_eq!(OwnerDescriptors::decode(d.value), usage);
        assert_eq!(d.name_str(), "dir hello_ext");
        assert_eq!(OwnerDescriptors::decode(0), OwnerDescriptors::default());
    }

    #[test]
    fn request_round_trip_and_rejections() {
        let r = Request {
            op: Op::Read,
            count: 4,
            lba: 0x1234_5678_9ABC,
        };
        assert_eq!(Request::decode(&r.encode()), Some(r));
        let mut bad = r.encode();
        bad[1] = 9;
        assert_eq!(Request::decode(&bad), None, "unknown version");
        bad = r.encode();
        bad[2] = 7;
        assert_eq!(Request::decode(&bad), None, "unknown op");
        assert_eq!(Request::decode(&r.encode()[..8]), None, "truncated");
    }

    #[test]
    fn reply_round_trip() {
        for status in [Status::Ok, Status::Invalid, Status::Io, Status::BadHandles] {
            let r = Reply {
                status,
                value: 8192,
            };
            assert_eq!(Reply::decode(&r.encode()), Some(r));
        }
        let mut bad = Reply {
            status: Status::Ok,
            value: 0,
        }
        .encode();
        bad[2] = 200;
        assert_eq!(Reply::decode(&bad), None);
    }
}

/// Status of the services supervised by init (stage 1 of the post-roadmap
/// plan): the shell and the diagnostic tools query it through
/// the broker endpoint; init tells requests apart by the protocol
/// byte. One request per service (`index` from 0) until `NotFound`.
/// Remote sessions (ADR 0025): `sshd` asks init for a shell or a single
/// command over a dedicated endpoint. The request carries three handles:
/// `[reply (WRITE), shell input (READ|TRANSFER), shell output
/// (WRITE|TRANSFER)]`. init launches `arish` in session mode on them: the
/// shell reads what the client types from the input endpoint and writes
/// its output to the output endpoint, as raw bytes of up to
/// `IPC_MAX_DATA` per message. An empty message in either direction means
/// end of stream (hang-up to the shell, exit to sshd).
pub mod session {
    pub const PROTOCOL: u8 = 8;
    pub const VERSION: u8 = 1;
    /// Shell argument bit: I/O through the session endpoints.
    pub const SHELL_SESSION: u64 = 1 << 16;
    /// Shell argument bit: no banner, prompt or echo (single command).
    pub const SHELL_QUIET: u64 = 1 << 17;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        /// Interactive shell (prompt, echo, line editing).
        Shell = 0,
        /// Commands fed by sshd, no prompt or echo.
        Exec = 1,
        /// sshd is listening (no handles): init starts the console shell
        /// only after this, so the two never interleave on the console.
        Ready = 2,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub kind: Kind,
    }

    impl Request {
        pub const LEN: usize = 8;
        pub fn encode(&self) -> [u8; Self::LEN] {
            [PROTOCOL, VERSION, self.kind as u8, 0, 0, 0, 0, 0]
        }
        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let kind = match b[2] {
                0 => Kind::Shell,
                1 => Kind::Exec,
                2 => Kind::Ready,
                _ => return None,
            };
            Some(Self { kind })
        }
    }

    /// Reply: `[PROTOCOL, VERSION, ok]`.
    pub fn reply(ok: bool) -> [u8; 4] {
        [PROTOCOL, VERSION, ok as u8, 0]
    }

    pub fn reply_ok(b: &[u8]) -> bool {
        b.len() >= 3 && b[0] == PROTOCOL && b[1] == VERSION && b[2] == 1
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trip() {
            for kind in [Kind::Shell, Kind::Exec, Kind::Ready] {
                assert_eq!(
                    Request::decode(&Request { kind }.encode()),
                    Some(Request { kind })
                );
            }
            assert_eq!(
                Request::decode(&[PROTOCOL, VERSION, 9, 0, 0, 0, 0, 0]),
                None
            );
            assert_eq!(Request::decode(&[7, VERSION, 0, 0, 0, 0, 0, 0]), None);
            assert!(reply_ok(&reply(true)));
            assert!(!reply_ok(&reply(false)));
        }
    }
}

pub mod status {
    pub const PROTOCOL: u8 = 7;
    pub const VERSION: u8 = 1;
    pub const MAX_NAME: usize = 16;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub index: u8,
    }

    impl Request {
        pub const LEN: usize = 8;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.index;
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            Some(Self { index: b[2] })
        }
    }

    /// Lifecycle state (`aricode_supervisor::State`) on the wire.
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum State {
        Stopped = 0,
        Starting = 1,
        Ready = 2,
        Degraded = 3,
        Failed = 4,
    }

    impl State {
        pub fn name(self) -> &'static str {
            match self {
                State::Stopped => "stopped",
                State::Starting => "starting",
                State::Ready => "ready",
                State::Degraded => "degraded",
                State::Failed => "failed",
            }
        }
        fn from_u8(v: u8) -> Option<Self> {
            Some(match v {
                0 => State::Stopped,
                1 => State::Starting,
                2 => State::Ready,
                3 => State::Degraded,
                4 => State::Failed,
                _ => return None,
            })
        }
    }

    /// Last exit of the service on the wire.
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LastExit {
        None = 0,
        Code = 1,
        Killed = 2,
        StartTimeout = 3,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub found: bool,
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
        pub state: State,
        pub optional: bool,
        pub generation: u32,
        pub restarts: u32,
        pub last_exit: LastExit,
        pub last_code: u64,
        /// Milliseconds since the last `ready` (0 if it is not ready).
        pub ready_for_ms: u64,
    }

    impl Reply {
        pub const LEN: usize = 48;

        pub fn not_found() -> Self {
            Self {
                found: false,
                name: [0; MAX_NAME],
                name_len: 0,
                state: State::Stopped,
                optional: false,
                generation: 0,
                restarts: 0,
                last_exit: LastExit::None,
                last_code: 0,
                ready_for_ms: 0,
            }
        }

        pub fn with_name(mut self, name: &str) -> Self {
            let n = name.len().min(MAX_NAME);
            self.name = [0; MAX_NAME];
            self.name[..n].copy_from_slice(&name.as_bytes()[..n]);
            self.name_len = n as u8;
            self
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = PROTOCOL;
            b[1] = VERSION;
            b[2] = self.found as u8;
            b[3] = self.state as u8;
            b[4] = self.optional as u8;
            b[5] = self.last_exit as u8;
            b[6] = self.name_len;
            b[8..12].copy_from_slice(&self.generation.to_le_bytes());
            b[12..16].copy_from_slice(&self.restarts.to_le_bytes());
            b[16..24].copy_from_slice(&self.last_code.to_le_bytes());
            b[24..32].copy_from_slice(&self.ready_for_ms.to_le_bytes());
            b[32..48].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN || b[0] != PROTOCOL || b[1] != VERSION {
                return None;
            }
            let mut name = [0u8; MAX_NAME];
            name.copy_from_slice(&b[32..48]);
            Some(Self {
                found: b[2] != 0,
                state: State::from_u8(b[3])?,
                optional: b[4] != 0,
                last_exit: match b[5] {
                    0 => LastExit::None,
                    1 => LastExit::Code,
                    2 => LastExit::Killed,
                    3 => LastExit::StartTimeout,
                    _ => return None,
                },
                name_len: b[6].min(MAX_NAME as u8),
                generation: u32::from_le_bytes(b[8..12].try_into().ok()?),
                restarts: u32::from_le_bytes(b[12..16].try_into().ok()?),
                last_code: u64::from_le_bytes(b[16..24].try_into().ok()?),
                ready_for_ms: u64::from_le_bytes(b[24..32].try_into().ok()?),
                name,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn status_round_trip() {
            let r = Request { index: 3 };
            assert_eq!(Request::decode(&r.encode()), Some(r));
            assert_eq!(
                Request::decode(&[6, 1, 0, 0, 0, 0, 0, 0]),
                None,
                "another protocol"
            );
            let reply = Reply {
                found: true,
                state: State::Degraded,
                optional: true,
                generation: 7,
                restarts: 2,
                last_exit: LastExit::Code,
                last_code: 42,
                ready_for_ms: 1234,
                ..Reply::not_found()
            }
            .with_name("virtio_input_00");
            let d = Reply::decode(&reply.encode()).unwrap();
            assert_eq!(d, reply);
            assert_eq!(d.name_str(), "virtio_input_00");
            assert_eq!(d.state.name(), "degraded");
            assert!(!Reply::decode(&Reply::not_found().encode()).unwrap().found);
            let mut bad = reply.encode();
            bad[3] = 9;
            assert_eq!(Reply::decode(&bad), None, "unknown state");
        }
    }
}
