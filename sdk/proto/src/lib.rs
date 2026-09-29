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
        /// Hace duraderas las escrituras anteriores (ADR 0014); solo lleva
        /// la respuesta. Sin `VIRTIO_BLK_F_FLUSH` vuelve sin hacer nada.
        Flush = 4,
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
        /// Solo por el endpoint principal (el broker): crea un endpoint ligado
        /// al prefijo `name/`; la respuesta transfiere ese endpoint (`WRITE`).
        /// Quien lo reciba solo ve y crea ficheros bajo su prefijo.
        Bind = 7,
        /// Hace duraderas las escrituras anteriores (ADR 0014).
        Flush = 8,
        /// `fd`, `len`; handles `[respuesta, buffer]`: sustituye el contenido
        /// entero del fichero de forma atómica y duradera (ADR 0014); `value`
        /// = bytes escritos.
        Replace = 9,
        /// Solo por el endpoint principal (el broker): crea un endpoint ligado
        /// a un único fichero en modo lectura (ADR 0016); la respuesta lo
        /// transfiere (`WRITE`). Desde él solo se ve, abre y lee ese fichero.
        Share = 10,
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
                7 => Op::Bind,
                8 => Op::Flush,
                9 => Op::Replace,
                10 => Op::Share,
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
        /// El endpoint no permite la operación (fichero compartido de solo
        /// lectura, ADR 0016).
        Denied = 9,
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
                9 => Status::Denied,
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

/// Protocolo del servicio de entrada (`InputServiceProtocol v1`): eventos
/// evdev de un dispositivo virtio-input.
pub mod input {
    pub const PROTOCOL: u8 = 4;
    pub const VERSION: u8 = 1;
    /// Eventos por respuesta.
    pub const MAX_EVENTS: usize = 4;
    pub const MAX_NAME: usize = 32;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Nombre del dispositivo en `Reply::name`.
        Info = 1,
        /// Espera hasta `timeout_ms` y devuelve hasta `MAX_EVENTS` eventos.
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

    /// Evento (8 bytes, formato evdev): tipo, código, valor.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Event {
        pub kind: u16,
        pub code: u16,
        pub value: u32,
    }

    /// Respuesta (104 bytes): estado, número de eventos, eventos, el instante
    /// (ms de arranque) en que el servicio recibió cada uno, y nombre. Los
    /// instantes permiten ordenar eventos de varios dispositivos.
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

        /// Eventos con sus instantes (`times[i]` acompaña a `events[i]`).
        pub fn events(events: &[Event], times: &[u64]) -> Self {
            let mut r = Self::status(Status::Ok);
            let n = events.len().min(times.len()).min(MAX_EVENTS);
            r.events[..n].copy_from_slice(&events[..n]);
            r.times[..n].copy_from_slice(&times[..n]);
            r.count = n as u8;
            r
        }

        /// `None` si el nombre no cabe.
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

/// Protocolo del compositor (`WindowProtocol v1`): ventanas cuyo contenido
/// vive en un buffer DMA del cliente, y eventos de entrada ya enrutados.
pub mod window {
    pub const PROTOCOL: u8 = 5;
    pub const VERSION: u8 = 1;
    pub const MAX_TITLE: usize = 24;
    /// Ventana máxima (píxeles por lado).
    pub const MAX_SIDE: u32 = 4096;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// Crea una ventana `w`x`h` con `title`; handles `[respuesta, buffer]`
        /// (el buffer tiene `w*h*4` bytes en `0x00RRGGBB`); `value` = id.
        Create = 1,
        /// El contenido del buffer cambió: recomponer.
        Present = 2,
        /// Espera hasta `timeout_ms` un evento de la ventana.
        Poll = 3,
        Close = 4,
    }

    /// Petición (64 bytes).
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

        /// `None` si el título no cabe.
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

    /// Evento entregado a una ventana.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Event {
        /// Carácter ya traducido por el compositor.
        Key(u8),
        /// Botón pulsado en coordenadas del contenido.
        Click { x: i32, y: i32, button: u8 },
        /// La ventana ganó (`true`) o perdió el foco.
        Focus(bool),
    }

    /// Respuesta (32 bytes): estado, valor y evento opcional.
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

/// Protocolo del broker de lanzamiento (`LaunchProtocol v1`): un programa
/// pide a init que ejecute otro; init lee su manifiesto, decide qué
/// capabilities entrega y devuelve el código de salida.
pub mod launch {
    pub const PROTOCOL: u8 = 6;
    pub const VERSION: u8 = 1;
    pub const MAX_NAME: usize = 32;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        pub arg: u64,
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
        /// `DETACH`: responder `Started` al crear el proceso, sin esperar su
        /// salida (ADR 0015).
        pub flags: u8,
    }

    impl Request {
        pub const LEN: usize = 48;
        /// Lanzamiento en segundo plano.
        pub const DETACH: u8 = 1;

        /// `None` si el nombre no cabe.
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
        /// `value` = código de salida del programa.
        Ok = 0,
        Invalid = 1,
        /// No hay tal programa en el initrd.
        NotFound = 2,
        /// El manifiesto pide capacidades que el broker no concede.
        Denied = 3,
        /// El programa murió por un fallo.
        Killed = 4,
        /// Manifiesto ilegible.
        BadManifest = 5,
        /// El broker no pudo lanzarlo (recursos).
        Failed = 6,
        /// Lanzado en segundo plano (`DETACH`); `value` = 0.
        Started = 7,
        /// Límite de programas en marcha del broker (ADR 0015).
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

/// Estado de los servicios supervisados por init (etapa 1 del plan posterior
/// al roadmap): el shell y las herramientas de diagnóstico lo consultan por
/// el endpoint del broker; init distingue las peticiones por el byte de
/// protocolo. Una petición por servicio (`index` desde 0) hasta `NotFound`.
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

    /// Estado de vida (`aricode_supervisor::State`) en el cable.
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
                State::Stopped => "parado",
                State::Starting => "arrancando",
                State::Ready => "listo",
                State::Degraded => "degradado",
                State::Failed => "fallido",
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

    /// Última salida del servicio en el cable.
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
        /// Milisegundos desde el último `listo` (0 si no lo está).
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
                "otro protocolo"
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
            assert_eq!(d.state.name(), "degradado");
            assert!(!Reply::decode(&Reply::not_found().encode()).unwrap().found);
            let mut bad = reply.encode();
            bad[3] = 9;
            assert_eq!(Reply::decode(&bad), None, "estado desconocido");
        }
    }
}
