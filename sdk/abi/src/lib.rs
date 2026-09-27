//! ABI de syscalls de AriCodeOS (sección 22 del documento maestro).
//!
//! Convención x86-64: número en `rax`, argumentos en `rdi, rsi, rdx, r10, r8,
//! r9`, resultado en `rax`. `rcx` y `r11` los destruye la instrucción
//! `syscall`; el resto de registros se conservan. Un resultado negativo en
//! el rango `-4095..=-1` es un [`Error`].
//!
//! Los recursos se nombran por handles (sección 24): enteros opacos de 32
//! bits con derechos asociados; `0` nunca es válido. Todo proceso arranca con
//! los handles de [`handle::SELF_PROCESS`], [`handle::MAIN_THREAD`] y
//! [`handle::CONSOLE`].
//!
//! Durante `0.x` el ABI puede cambiar (sección 74). Versión actual: 2.

#![no_std]

pub const ABI_VERSION: u32 = 3;

/// Handles iniciales de todo proceso.
pub mod handle {
    /// Handle con el que `init` recibe el objeto `SPAWNER` (`MANAGE`).
    pub const INIT_SPAWNER: u32 = 0x0001_0004;
    /// Handle con el que `init` recibe la raíz PCI (`READ|MANAGE`).
    pub const INIT_PCI_ROOT: u32 = 0x0001_0005;
    /// El propio proceso (`MANAGE | READ | DUPLICATE`).
    pub const SELF_PROCESS: u32 = 0x0001_0001;
    /// El hilo principal (`READ | WAIT | DUPLICATE`).
    pub const MAIN_THREAD: u32 = 0x0001_0002;
    /// Consola del kernel (`WRITE | DUPLICATE`).
    pub const CONSOLE: u32 = 0x0001_0003;
    /// Handle con el que `init` recibe la capacidad de hardware (`MANAGE`).
    pub const INIT_HARDWARE: u32 = 0x0001_0006;
    /// Handle con el que `init` recibe el objeto `SYSTEM` (`READ|MANAGE`).
    pub const INIT_SYSTEM: u32 = 0x0001_0007;
    /// Handle con el que `init` recibe la entrada de consola (`READ`).
    pub const INIT_CONSOLE_IN: u32 = 0x0001_0008;
    /// Handle con el que `init` recibe el framebuffer (`MMIO`, `READ|WRITE`).
    pub const INIT_FRAMEBUFFER: u32 = 0x0001_0009;
}

/// Bits del `arg` que el kernel pasa a `init`.
pub mod init_arg {
    /// La cmdline pide un shell interactivo (`shell`).
    pub const SHELL: u64 = 1;
    /// La cmdline pide la interfaz gráfica (`gui`): el kernel no pinta en el
    /// framebuffer y init lanza el compositor.
    pub const GUI: u64 = 2;
}

/// Derechos (bits) tal y como los ve el espacio de usuario.
pub mod rights {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const DUPLICATE: u32 = 1 << 2;
    pub const TRANSFER: u32 = 1 << 3;
    pub const WAIT: u32 = 1 << 4;
    pub const SIGNAL: u32 = 1 << 5;
    pub const MANAGE: u32 = 1 << 6;
}

/// Tipos de objeto devueltos por `object_kind`.
pub mod kind {
    pub const THREAD: u64 = 1;
    pub const PROCESS: u64 = 2;
    pub const CONSOLE: u64 = 3;
    pub const ENDPOINT: u64 = 4;
    /// Capacidad de crear procesos desde el initrd (solo `init` la recibe).
    pub const SPAWNER: u64 = 5;
    /// Línea de interrupción de un dispositivo.
    pub const IRQ: u64 = 6;
    /// Región MMIO de un dispositivo.
    pub const MMIO: u64 = 7;
    /// Buffer DMA (memoria física contigua).
    pub const DMA: u64 = 8;
    /// Capacidad de reservar recursos de hardware (buffers DMA).
    pub const HARDWARE: u64 = 9;
    /// Raíz PCI: enumeración y reclamación de dispositivos (solo `init`).
    pub const PCI_ROOT: u64 = 10;
    /// Estado del sistema (`READ`: información; `MANAGE`: apagar, reiniciar).
    pub const SYSTEM: u64 = 11;
}

/// Límite exclusivo del espacio de usuario (mitad baja canónica).
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

/// Números de syscall.
pub mod nr {
    /// `process_exit(code)`: nunca retorna.
    pub const PROCESS_EXIT: u64 = 0;
    /// `console_write(handle, ptr, len) -> bytes escritos`; exige `WRITE`.
    pub const CONSOLE_WRITE: u64 = 1;
    /// `thread_yield()`.
    pub const THREAD_YIELD: u64 = 2;
    /// `thread_sleep(ms)`.
    pub const THREAD_SLEEP: u64 = 3;
    /// `thread_get_tid() -> tid`.
    pub const THREAD_GET_TID: u64 = 4;
    /// `abi_version() -> ABI_VERSION`.
    pub const ABI_VERSION: u64 = 5;
    /// `handle_close(handle)`.
    pub const HANDLE_CLOSE: u64 = 6;
    /// `handle_duplicate(handle, rights) -> handle`; exige `DUPLICATE` y
    /// `rights` subconjunto de los del origen.
    pub const HANDLE_DUPLICATE: u64 = 7;
    /// `object_kind(handle) -> kind::*`.
    pub const OBJECT_KIND: u64 = 8;
    /// `object_id(handle) -> tid | pid`; exige `READ`. La consola devuelve 0.
    pub const OBJECT_ID: u64 = 9;
    /// `object_revoke(handle)`: invalida el objeto para todos sus handles,
    /// en todos los procesos; exige `MANAGE`.
    pub const OBJECT_REVOKE: u64 = 10;
    /// `endpoint_create() -> handle` con `READ|WRITE|DUPLICATE|TRANSFER|MANAGE`.
    pub const ENDPOINT_CREATE: u64 = 11;
    /// `ipc_send(ep, data_ptr, data_len, handles_ptr, handles_len, timeout_ms)`;
    /// exige `WRITE` sobre `ep` y `TRANSFER` sobre cada handle enviado (que
    /// se retira del emisor). `timeout_ms`: 0 = no bloquear,
    /// [`TIMEOUT_INFINITE`] = esperar siempre.
    pub const IPC_SEND: u64 = 12;
    /// `ipc_receive(ep, buf_ptr, buf_len, handles_out_ptr, handles_max, timeout_ms)
    /// -> data_len | (handles_recibidos << 32)`; exige `READ` sobre `ep`.
    pub const IPC_RECEIVE: u64 = 13;
    /// `process_spawn(spawner, name_ptr, name_len, arg, handles_ptr, handles_len)
    /// -> handle de proceso`; exige `MANAGE` sobre un objeto `SPAWNER` y
    /// `TRANSFER` sobre cada handle, que se retira del padre y aparece en el
    /// hijo a partir de `0x0001_0004` en el mismo orden. El hijo recibe `arg`
    /// en `rdi`.
    pub const PROCESS_SPAWN: u64 = 14;
    /// `process_wait(process, timeout_ms) -> código de salida`; exige `WAIT`.
    /// Un proceso terminado por el kernel devuelve `Error::Killed`.
    pub const PROCESS_WAIT: u64 = 15;
    /// `irq_wait(irq, timeout_ms) -> interrupciones acumuladas`; exige `WAIT`.
    pub const IRQ_WAIT: u64 = 16;
    /// `mmio_map(region) -> dirección virtual`; exige `READ|WRITE` sobre un
    /// objeto MMIO o DMA. Cada llamada crea un mapeo nuevo.
    pub const MMIO_MAP: u64 = 17;
    /// `region_len(region) -> bytes`; exige `READ`.
    pub const REGION_LEN: u64 = 18;
    /// `dma_alloc(hardware, len) -> handle DMA`; exige `MANAGE` sobre un objeto
    /// `HARDWARE`. Memoria física contigua, máximo `DMA_MAX_LEN`.
    pub const DMA_ALLOC: u64 = 19;
    /// `dma_phys(dma) -> dirección física`; exige `READ`.
    pub const DMA_PHYS: u64 = 20;
    /// `pci_count(root) -> funciones enumeradas`; exige `READ` sobre `PCI_ROOT`.
    pub const PCI_COUNT: u64 = 21;
    /// `pci_info(root, index) -> PciInfo empaquetado` (ver [`pci_info`]); exige `READ`.
    pub const PCI_INFO: u64 = 22;
    /// `pci_claim(root, index, out_ptr) -> handles escritos`; exige `MANAGE`.
    /// Escribe [`PCI_CLAIM_HANDLES`] `u32` en `out_ptr`: espacio de
    /// configuración (MMIO), `HARDWARE`, IRQ MSI-X (0 si no hay) y los seis
    /// BARs de memoria (0 si ausente o de E/S). El dispositivo queda con
    /// memoria y bus master habilitados y MSI-X en la entrada 0.
    pub const PCI_CLAIM: u64 = 23;
    /// `console_read(console, ptr, len, timeout_ms) -> bytes leídos (>= 1)`;
    /// exige `READ`. Bloquea hasta que haya entrada o venza el plazo (`Timeout`).
    pub const CONSOLE_READ: u64 = 24;
    /// `system_info(system, kind, ptr, len) -> bytes escritos` (ver [`sysinfo`]);
    /// exige `READ`.
    pub const SYSTEM_INFO: u64 = 25;
    /// `system_power(system, action)` (ver [`power`]); exige `MANAGE`. No retorna.
    pub const SYSTEM_POWER: u64 = 26;
    /// `initrd_read(spawner, name_ptr, name_len, buf, len) -> bytes copiados`;
    /// exige `READ` sobre el `SPAWNER`. `TooBig` si el fichero no cabe.
    pub const INITRD_READ: u64 = 27;
}

/// Sexto argumento de `process_spawn`: número de handles (bits 0..7) y
/// límite de memoria DMA del hijo en KiB (bits 8..). 0 KiB = sin límite.
pub mod spawn_limits {
    pub const fn pack(handles: usize, memory_kib: u32) -> u64 {
        (handles as u64 & 0xFF) | (memory_kib as u64) << 8
    }
    pub const fn handles(raw: u64) -> usize {
        (raw & 0xFF) as usize
    }
    pub const fn memory_kib(raw: u64) -> u32 {
        (raw >> 8) as u32
    }
}

/// Consultas de `system_info` y formato de sus respuestas.
pub mod sysinfo {
    /// Un `u64`: milisegundos desde el arranque.
    pub const UPTIME: u64 = 0;
    /// Dos `u64`: frames de RAM utilizable y frames libres.
    pub const MEMORY: u64 = 1;
    /// Tantas [`ProcessEntry`] como quepan en el buffer.
    pub const PROCESSES: u64 = 2;
    /// Un [`Framebuffer`] (40 bytes): geometría y formato de la pantalla GOP.
    pub const FRAMEBUFFER: u64 = 3;
    /// Hasta `RANDOM_MAX` bytes aleatorios del generador del kernel (la
    /// longitud pedida es la del buffer).
    pub const RANDOM: u64 = 4;
    pub const RANDOM_MAX: u64 = 256;

    /// Formato de píxel del framebuffer (32 bpp).
    pub const FB_FORMAT_RGB: u8 = 0;
    pub const FB_FORMAT_BGR: u8 = 1;
    pub const FB_FORMAT_BITMASK: u8 = 2;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Framebuffer {
        pub width: u32,
        pub height: u32,
        /// Píxeles por fila.
        pub stride: u32,
        pub format: u8,
        pub size: u64,
        pub red_mask: u32,
        pub green_mask: u32,
        pub blue_mask: u32,
    }

    impl Framebuffer {
        pub const LEN: usize = 40;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0..4].copy_from_slice(&self.width.to_le_bytes());
            b[4..8].copy_from_slice(&self.height.to_le_bytes());
            b[8..12].copy_from_slice(&self.stride.to_le_bytes());
            b[12] = self.format;
            b[16..24].copy_from_slice(&self.size.to_le_bytes());
            b[24..28].copy_from_slice(&self.red_mask.to_le_bytes());
            b[28..32].copy_from_slice(&self.green_mask.to_le_bytes());
            b[32..36].copy_from_slice(&self.blue_mask.to_le_bytes());
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN {
                return None;
            }
            Some(Self {
                width: u32::from_le_bytes(b[0..4].try_into().ok()?),
                height: u32::from_le_bytes(b[4..8].try_into().ok()?),
                stride: u32::from_le_bytes(b[8..12].try_into().ok()?),
                format: b[12],
                size: u64::from_le_bytes(b[16..24].try_into().ok()?),
                red_mask: u32::from_le_bytes(b[24..28].try_into().ok()?),
                green_mask: u32::from_le_bytes(b[28..32].try_into().ok()?),
                blue_mask: u32::from_le_bytes(b[32..36].try_into().ok()?),
            })
        }
    }

    pub const PROCESS_NAME_LEN: usize = 24;

    /// Estado de un proceso en [`ProcessEntry`].
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProcessState {
        Running = 0,
        Exited = 1,
        Killed = 2,
    }

    /// Entrada de la lista de procesos (32 bytes): pid, estado, código de
    /// salida (si terminó) y nombre.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessEntry {
        pub pid: u32,
        pub state: ProcessState,
        pub exit_code: u16,
        pub name: [u8; PROCESS_NAME_LEN],
        pub name_len: u8,
    }

    impl ProcessEntry {
        pub const LEN: usize = 32;

        pub fn new(pid: u32, state: ProcessState, exit_code: u16, name: &str) -> Self {
            let n = name.len().min(PROCESS_NAME_LEN);
            let mut buf = [0u8; PROCESS_NAME_LEN];
            buf[..n].copy_from_slice(&name.as_bytes()[..n]);
            Self {
                pid,
                state,
                exit_code,
                name: buf,
                name_len: n as u8,
            }
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[..4].copy_from_slice(&self.pid.to_le_bytes());
            b[4] = self.state as u8;
            b[5] = self.name_len;
            b[6..8].copy_from_slice(&self.exit_code.to_le_bytes());
            b[8..].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN {
                return None;
            }
            let state = match b[4] {
                0 => ProcessState::Running,
                1 => ProcessState::Exited,
                2 => ProcessState::Killed,
                _ => return None,
            };
            let mut name = [0u8; PROCESS_NAME_LEN];
            name.copy_from_slice(&b[8..Self::LEN]);
            Some(Self {
                pid: u32::from_le_bytes(b[..4].try_into().ok()?),
                state,
                exit_code: u16::from_le_bytes(b[6..8].try_into().ok()?),
                name,
                name_len: b[5].min(PROCESS_NAME_LEN as u8),
            })
        }
    }
}

/// Acciones de `system_power`.
pub mod power {
    pub const SHUTDOWN: u64 = 0;
    pub const REBOOT: u64 = 1;
}

/// Número de handles que escribe `pci_claim`.
pub const PCI_CLAIM_HANDLES: usize = 9;

/// Empaquetado de `pci_info`: `vendor << 48 | device << 32 | class << 24 |
/// subclass << 16 | bus << 8 | dev << 3 | func`.
pub mod pci_info {
    pub const fn pack(
        vendor: u16,
        device: u16,
        class: u8,
        subclass: u8,
        bus: u8,
        dev: u8,
        func: u8,
    ) -> u64 {
        (vendor as u64) << 48
            | (device as u64) << 32
            | (class as u64) << 24
            | (subclass as u64) << 16
            | (bus as u64) << 8
            | (dev as u64) << 3
            | func as u64
    }
    pub const fn vendor(v: u64) -> u16 {
        (v >> 48) as u16
    }
    pub const fn device(v: u64) -> u16 {
        (v >> 32) as u16
    }
    pub const fn class(v: u64) -> u8 {
        (v >> 24) as u8
    }
    pub const fn subclass(v: u64) -> u8 {
        (v >> 16) as u8
    }
    pub const fn bus(v: u64) -> u8 {
        (v >> 8) as u8
    }
    pub const fn dev(v: u64) -> u8 {
        ((v >> 3) & 0x1F) as u8
    }
    pub const fn func(v: u64) -> u8 {
        (v & 7) as u8
    }
}

/// Máximo de un buffer DMA por llamada (sección 52).
pub const DMA_MAX_LEN: u64 = 8 * 1024 * 1024;

/// Límites de un mensaje IPC (sección 23: mensajes pequeños y rápidos).
pub const IPC_MAX_DATA: usize = 256;
pub const IPC_MAX_HANDLES: usize = 4;
/// Capacidad de la cola de un endpoint (protección frente a agotamiento).
pub const IPC_QUEUE_CAPACITY: usize = 16;
/// Timeout que significa "esperar indefinidamente".
pub const TIMEOUT_INFINITE: u64 = u64::MAX;

/// Descompone el resultado de `ipc_receive`.
pub const fn receive_result(raw: u64) -> (usize, usize) {
    ((raw & 0xFFFF_FFFF) as usize, (raw >> 32) as usize)
}

/// Errores devueltos por el kernel (negados en `rax`).
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Número de syscall desconocido.
    NoSys = 1,
    /// Puntero o rango fuera del espacio de usuario o no mapeado.
    Fault = 2,
    /// Argumento inválido.
    Invalid = 3,
    /// Handle inexistente, cerrado o inventado.
    BadHandle = 4,
    /// El handle no tiene los derechos necesarios.
    Denied = 5,
    /// Intento de obtener más derechos de los que se tienen.
    Escalation = 6,
    /// Límite de recursos del proceso.
    Limit = 7,
    /// El objeto fue revocado: el handle existe pero ya no da acceso.
    Revoked = 8,
    /// Se agotó el plazo (o `timeout_ms == 0` y la operación bloquearía).
    Timeout = 9,
    /// Mensaje o buffer por encima de los límites.
    TooBig = 10,
    /// El proceso esperado fue terminado por el kernel (excepción en ring 3).
    Killed = 11,
    /// No existe un fichero con ese nombre en el initrd.
    NotFound = 12,
}

impl Error {
    pub const fn to_raw(self) -> i64 {
        -(self as i64)
    }

    pub const fn from_raw(raw: i64) -> Option<Error> {
        match -raw {
            1 => Some(Error::NoSys),
            2 => Some(Error::Fault),
            3 => Some(Error::Invalid),
            4 => Some(Error::BadHandle),
            5 => Some(Error::Denied),
            6 => Some(Error::Escalation),
            7 => Some(Error::Limit),
            8 => Some(Error::Revoked),
            9 => Some(Error::Timeout),
            10 => Some(Error::TooBig),
            11 => Some(Error::Killed),
            12 => Some(Error::NotFound),
            _ => None,
        }
    }
}

/// Convierte el `rax` devuelto en `Result`.
pub const fn decode(raw: i64) -> Result<u64, Error> {
    if raw < 0 && raw >= -4095 {
        match Error::from_raw(raw) {
            Some(e) => Err(e),
            None => Err(Error::Invalid),
        }
    } else {
        Ok(raw as u64)
    }
}

/// `true` si `[ptr, ptr+len)` cae íntegramente en el espacio de usuario.
pub const fn user_range_ok(ptr: u64, len: u64) -> bool {
    match ptr.checked_add(len) {
        Some(end) => end <= USER_SPACE_END,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn spawn_limits_pack() {
        use super::spawn_limits::*;
        let raw = pack(6, 128);
        assert_eq!(handles(raw), 6);
        assert_eq!(memory_kib(raw), 128);
        assert_eq!(memory_kib(pack(3, 0)), 0);
    }

    #[test]
    fn framebuffer_info_round_trip() {
        use super::sysinfo::*;
        let f = Framebuffer {
            width: 1280,
            height: 800,
            stride: 1280,
            format: FB_FORMAT_BGR,
            size: 1280 * 800 * 4,
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
        };
        assert_eq!(Framebuffer::decode(&f.encode()), Some(f));
        assert!(Framebuffer::decode(&[0u8; 10]).is_none());
    }

    #[test]
    fn process_entry_round_trip() {
        use super::sysinfo::*;
        let e = ProcessEntry::new(7, ProcessState::Exited, 42, "virtio_blk");
        let d = ProcessEntry::decode(&e.encode()).unwrap();
        assert_eq!(d, e);
        assert_eq!(d.name_str(), "virtio_blk");
        let long = ProcessEntry::new(
            1,
            ProcessState::Running,
            0,
            "un-nombre-demasiado-largo-para-la-entrada",
        );
        assert_eq!(long.name_len as usize, PROCESS_NAME_LEN);
        let mut bad = e.encode();
        bad[4] = 9;
        assert!(ProcessEntry::decode(&bad).is_none());
        assert!(ProcessEntry::decode(&bad[..10]).is_none());
    }

    use super::*;

    #[test]
    fn errors_round_trip_and_decode() {
        assert_eq!(Error::Fault.to_raw(), -2);
        assert_eq!(Error::from_raw(-2), Some(Error::Fault));
        assert_eq!(Error::from_raw(-99), None);
        assert_eq!(decode(-1), Err(Error::NoSys));
        assert_eq!(
            decode(-4096),
            Ok((-4096i64) as u64),
            "fuera del rango de error es un valor"
        );
        assert_eq!(decode(42), Ok(42));
        assert_eq!(
            decode(-77),
            Err(Error::Invalid),
            "codigo desconocido se normaliza"
        );
    }

    #[test]
    fn pci_info_round_trip() {
        let v = pci_info::pack(0x1AF4, 0x1042, 0x01, 0x00, 0, 3, 0);
        assert_eq!(pci_info::vendor(v), 0x1AF4);
        assert_eq!(pci_info::device(v), 0x1042);
        assert_eq!((pci_info::class(v), pci_info::subclass(v)), (1, 0));
        assert_eq!(
            (pci_info::bus(v), pci_info::dev(v), pci_info::func(v)),
            (0, 3, 0)
        );
    }

    #[test]
    fn receive_result_packs_len_and_handles() {
        assert_eq!(receive_result(4 | (2 << 32)), (4, 2));
        assert_eq!(receive_result(0), (0, 0));
    }

    #[test]
    fn user_range_checks() {
        assert!(user_range_ok(0x40_0000, 4096));
        assert!(user_range_ok(USER_SPACE_END - 16, 16));
        assert!(!user_range_ok(USER_SPACE_END - 16, 17));
        assert!(!user_range_ok(0xFFFF_FFFF_8000_0000, 1));
        assert!(!user_range_ok(u64::MAX, 1));
    }
}
