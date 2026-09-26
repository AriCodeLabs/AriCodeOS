//! Protocolos IPC versionados de los servicios (sección 75). Cada mensaje es
//! un bloque de bytes de tamaño fijo con un identificador de protocolo y
//! versión al principio, de modo que un servicio rechaza lo que no entiende.

#![no_std]

/// Protocolo del servicio de bloques (`BlockServiceProtocol v1`).
pub mod block {
    pub const PROTOCOL: u8 = 1;
    pub const VERSION: u8 = 1;
    /// Sectores por petición como máximo (buffer DMA de 8 KiB).
    pub const MAX_SECTORS: u16 = 16;
    pub const SECTOR_SIZE: u64 = 512;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Capacidad en sectores en `Reply::value`.
        Info = 1,
        /// Lee `count` sectores desde `lba` al buffer DMA transferido.
        Read = 2,
        /// Escribe `count` sectores desde el buffer DMA transferido en `lba`.
        Write = 3,
    }

    /// Petición (16 bytes). Handles del mensaje: `[respuesta (WRITE), buffer DMA (READ|WRITE)]`;
    /// `Info` solo lleva la respuesta.
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
        /// Petición mal formada o fuera de límites.
        Invalid = 1,
        /// Error del dispositivo.
        Io = 2,
        /// Faltan handles o no son del tipo esperado.
        BadHandles = 3,
    }

    /// Respuesta (16 bytes).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Reply {
        pub status: Status,
        /// Capacidad (`Info`) o sectores transferidos.
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

/// Protocolo del servicio de ficheros (`FileServiceProtocol v1`).
///
/// Los datos de ficheros no viajan en el mensaje (256 bytes): el cliente
/// transfiere un buffer DMA y el servicio lee o escribe en él.
pub mod fs {
    pub const PROTOCOL: u8 = 2;
    pub const VERSION: u8 = 1;
    pub const MAX_NAME: usize = 32;
    /// Máximo por petición de lectura/escritura (buffer del cliente).
    pub const MAX_IO: u32 = 4096;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// `arg` = índice; respuesta: `value` = tamaño, nombre en `Reply::name`.
        List = 1,
        /// Nombre en la petición; respuesta: `value` = descriptor.
        Open = 2,
        /// Nombre en la petición; crea vacío; respuesta: `value` = descriptor.
        Create = 3,
        /// `fd`, `offset`, `len`; handles `[respuesta, buffer]`; `value` = leídos.
        Read = 4,
        /// `fd`, `offset`, `len`; handles `[respuesta, buffer]`; `value` = escritos.
        Write = 5,
        /// `fd`.
        Close = 6,
    }

    /// Petición (64 bytes): cabecera, argumentos y nombre.
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
    }

    /// Respuesta (64 bytes): estado, valor y nombre (para `List`).
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

#[cfg(test)]
mod tests {
    use super::block::*;

    #[test]
    fn fs_request_and_reply_round_trip() {
        use super::fs::*;
        let mut r = Request::new(Op::Read).with_name("hola.txt");
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
        let long = Request::new(Op::Open).with_name("abcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(long.name_len as usize, MAX_NAME);
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
        assert_eq!(Request::decode(&bad), None, "version desconocida");
        bad = r.encode();
        bad[2] = 7;
        assert_eq!(Request::decode(&bad), None, "op desconocida");
        assert_eq!(Request::decode(&r.encode()[..8]), None, "truncada");
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
