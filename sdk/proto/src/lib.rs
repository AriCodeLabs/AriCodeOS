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

#[cfg(test)]
mod tests {
    use super::block::*;

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
