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

        /// `None` si el nombre no cabe: nunca se recorta un identificador.
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

/// Protocolo del servicio de red (`NetServiceProtocol v1`). Los datagramas
/// viajan por el buffer DMA del cliente; el mensaje lleva direcciones,
/// puertos y longitudes.
pub mod net {
    pub const PROTOCOL: u8 = 3;
    pub const VERSION: u8 = 1;
    /// Máximo de un datagrama por petición (buffer del cliente).
    pub const MAX_DATAGRAM: u16 = 1472;
    /// Puerto UDP del respondedor de pruebas del host (eco y DNS), alcanzable
    /// desde el invitado en la dirección del anfitrión virtual (10.0.2.2).
    pub const TEST_RESPONDER_PORT: u16 = 17007;
    /// Puerto TCP del servidor de eco de pruebas del host.
    pub const TEST_TCP_ECHO_PORT: u16 = 17008;
    /// Puerto del host redirigido (`hostfwd`) al puerto TCP 7 del invitado.
    pub const TEST_TCP_FORWARD_PORT: u16 = 17009;
    /// Puerto TCP en el que escucha el programa de prueba del invitado.
    pub const TEST_TCP_LISTEN_PORT: u16 = 7;
    /// Puerto del servidor HTTP de pruebas del host.
    pub const TEST_HTTP_PORT: u16 = 17010;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Configuración: `Reply::data` = ip, máscara, router, dns, mac.
        Info = 1,
        /// Eco ICMP a `addr` con `id` como secuencia; `value` = RTT en ms.
        Ping = 2,
        /// Reserva el puerto UDP `port`; `value` = socket.
        UdpBind = 3,
        /// Envía `len` bytes del buffer desde `sock` a `addr:port`.
        UdpSend = 4,
        /// Espera hasta `timeout_ms` un datagrama en `sock`; `value` = bytes,
        /// remitente en `Reply::data`/`Reply::port`.
        UdpRecv = 5,
        /// Libera `sock`.
        UdpClose = 6,
        /// Resuelve el nombre que viaja en el buffer del cliente (`len` bytes,
        /// hasta 253) en `addr:port` (0 = servidor DHCP).
        Dns = 7,
        /// Conecta con `addr:port`; espera hasta `timeout_ms`; `value` = socket.
        TcpConnect = 8,
        /// Escucha en `port`; `value` = socket de escucha.
        TcpListen = 9,
        /// Espera hasta `timeout_ms` una conexión en `sock`; `value` = socket
        /// nuevo, remitente en `Reply::data`/`Reply::port`.
        TcpAccept = 10,
        /// Encola `len` bytes del buffer; `value` = aceptados (0 si está lleno).
        TcpSend = 11,
        /// Espera hasta `timeout_ms` datos en `sock`; `value` = bytes (0 = el
        /// otro extremo cerró).
        TcpRecv = 12,
        /// Cierre ordenado de `sock`.
        TcpClose = 13,
    }

    /// Petición (64 bytes). Los nombres DNS no viajan en el mensaje: van por
    /// el buffer del cliente, sin límite artificial.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub op: Op,
        pub sock: u16,
        pub port: u16,
        pub len: u16,
        pub addr: [u8; 4],
        pub timeout_ms: u32,
        pub id: u16,
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
            }
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
                _ => return None,
            };
            let mut addr = [0u8; 4];
            addr.copy_from_slice(&b[10..14]);
            Some(Self {
                op,
                sock: u16::from_le_bytes(b[4..6].try_into().ok()?),
                port: u16::from_le_bytes(b[6..8].try_into().ok()?),
                len: u16::from_le_bytes(b[8..10].try_into().ok()?),
                addr,
                timeout_ms: u32::from_le_bytes(b[14..18].try_into().ok()?),
                id: u16::from_le_bytes(b[18..20].try_into().ok()?),
            })
        }
    }

    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Ok = 0,
        Invalid = 1,
        /// Plazo vencido sin respuesta.
        Timeout = 2,
        /// Sin configuración de red todavía (DHCP en curso).
        NoRoute = 3,
        BadHandles = 4,
        /// Demasiadas peticiones pendientes.
        Busy = 5,
        BadSock = 6,
        InUse = 7,
        /// El servidor DNS respondió sin dirección.
        NotFound = 8,
        /// La conexión fue rechazada o reiniciada por el otro extremo.
        Reset = 9,
    }

    /// Respuesta (48 bytes): estado, valor, puerto y datos (direcciones).
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

    /// Configuración de red tal como viaja en `Reply::data` de `Info`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Info {
        pub address: [u8; 4],
        pub mask: [u8; 4],
        pub router: [u8; 4],
        pub dns: [u8; 4],
        pub mac: [u8; 6],
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

#[cfg(test)]
mod tests {
    use super::block::*;

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
            "un nombre largo se rechaza, nunca se recorta"
        );
        let exact = "a".repeat(MAX_NAME);
        assert_eq!(
            Request::new(Op::Open).with_name(&exact).unwrap().name_str(),
            exact
        );
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
