//! fsd (Fase 20): servicio de ficheros AriFS en ring 3 sobre el servicio de
//! bloques. Handles: 0x10004 = endpoint propio (READ|WRITE), 0x10005 =
//! endpoint del servicio de bloques (WRITE), 0x10006 = HARDWARE (DMA).
//! `arg` = LBA absoluto de la partición AriFS.
#![no_std]
#![no_main]

use aricode_arifs::{AriFs, BlockDevice, FsError, Recovery, SECTOR};
use aricode_proto::fs::{Op, Reply, Request, Status, MAX_IO};
use aricode_rt::sys::{self, block::BlockClient, Handle};

const SERVICE: Handle = Handle(0x0001_0004);
const BLOCK: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);

fn say(msg: &[u8]) {
    sys::print(b"fsd: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// El servicio de bloques visto como dispositivo de la partición.
struct PartitionDevice {
    blk: BlockClient,
    base_lba: u64,
}

impl BlockDevice for PartitionDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), FsError> {
        self.read_sectors(lba, buf)
    }
    fn write_sector(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<(), FsError> {
        self.write_sectors(lba, buf)
    }
    fn flush(&mut self) -> Result<(), FsError> {
        self.blk.flush().map_err(|_| FsError::Io)
    }
    /// Un lote entero en una peticion al servicio de bloques (etapa 5).
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        let count = (buf.len() / SECTOR) as u16;
        self.blk
            .read(self.base_lba + lba, count)
            .map_err(|_| FsError::Io)?;
        buf.copy_from_slice(&self.blk.buffer()[..buf.len()]);
        Ok(())
    }
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        let count = (buf.len() / SECTOR) as u16;
        self.blk.buffer()[..buf.len()].copy_from_slice(buf);
        self.blk
            .write(self.base_lba + lba, count)
            .map_err(|_| FsError::Io)
    }
}

/// Descriptores abiertos: índice de fichero por fd (global al servicio).
/// Descriptores abiertos: (índice de fichero, endpoint propietario). Un
/// descriptor solo responde al endpoint (directorio privado) que lo abrió:
/// sin esto, una aplicación podía leer, escribir o cerrar los ficheros de
/// otra probando números de descriptor.
struct Fds([Option<(usize, usize)>; 16]);

impl Fds {
    fn open(&mut self, index: usize, owner: usize) -> Option<u32> {
        let slot = self.0.iter().position(Option::is_none)?;
        self.0[slot] = Some((index, owner));
        Some(slot as u32)
    }
    fn get(&self, fd: u32, owner: usize) -> Option<usize> {
        match self.0.get(fd as usize).copied().flatten() {
            Some((index, o)) if o == owner => Some(index),
            _ => None,
        }
    }
    fn close(&mut self, fd: u32, owner: usize) -> bool {
        match self.0.get_mut(fd as usize) {
            Some(s) if s.is_some_and(|(_, o)| o == owner) => {
                *s = None;
                true
            }
            _ => false,
        }
    }
}

fn status_of(e: FsError) -> Status {
    match e {
        FsError::Io | FsError::BadMagic | FsError::Corrupt => Status::Io,
        FsError::NotFound => Status::NotFound,
        FsError::Exists => Status::Exists,
        FsError::NoSpace => Status::NoSpace,
        FsError::BadName => Status::Invalid,
        FsError::OutOfRange => Status::OutOfRange,
    }
}

/// Mapeos de buffers de clientes reutilizados por dirección física.
struct ClientBuffers {
    entries: [(u64, u64, u64); 8], // (phys, virt, len)
    count: usize,
}

impl ClientBuffers {
    fn map(&mut self, dma: Handle) -> Option<&mut [u8]> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        let virt = match self.entries[..self.count].iter().find(|e| e.0 == phys) {
            Some(e) => e.1,
            None => {
                let v = sys::mmio_map(dma).ok()?;
                if self.count < self.entries.len() {
                    self.entries[self.count] = (phys, v, len);
                    self.count += 1;
                }
                v
            }
        };
        // SAFETY: buffer del cliente mapeado RW en este proceso.
        Some(unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, len as usize) })
    }
}

/// Lo que un endpoint concedido por el broker deja ver (ADR 0016).
#[derive(Clone, Copy, PartialEq, Eq)]
enum NsKind {
    /// Directorio privado: los ficheros bajo `nombre/`.
    Dir,
    /// Un solo fichero (nombre completo) en modo lectura.
    ReadOnlyFile,
}

/// Espacio de nombres ligado a un endpoint: lado de lectura, nombre
/// (prefijo o fichero) y clase.
#[derive(Clone, Copy)]
struct Namespace {
    ep: Handle,
    name: [u8; 32],
    len: usize,
    kind: NsKind,
}

impl Namespace {
    fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.len]).unwrap_or("")
    }
    /// Nombre con el que un fichero compartido se muestra y se abre: sin
    /// el directorio que lo contiene.
    fn bare(&self) -> &str {
        let n = self.name_str();
        n.rsplit_once('/').map_or(n, |(_, b)| b)
    }
}

/// Nombre completo en la tabla: `prefijo/nombre` dentro de un directorio
/// privado, o el nombre tal cual desde el endpoint principal. `None` si no
/// cabe o intenta salir del prefijo.
fn qualified<'a>(prefix: &str, name: &str, buf: &'a mut [u8; 64]) -> Option<&'a str> {
    if name.is_empty() {
        return None;
    }
    if prefix.is_empty() {
        // Espacio principal (init): nombres completos, con directorios
        // (`apps/x.aripkg`); la libreria valida la forma.
        if name.len() > buf.len() {
            return None;
        }
        buf[..name.len()].copy_from_slice(name.as_bytes());
        return core::str::from_utf8(&buf[..name.len()]).ok();
    }
    if name.contains('/') {
        return None;
    }
    let total = prefix.len() + 1 + name.len();
    if total > buf.len() {
        return None;
    }
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    buf[prefix.len()] = b'/';
    buf[prefix.len() + 1..total].copy_from_slice(name.as_bytes());
    core::str::from_utf8(&buf[..total]).ok()
}

/// Reemplazo por trozos a medias (ADR 0018): fichero y endpoint dueño. Solo
/// uno a la vez, porque el journal es único; lo libera el compromiso, un
/// error o el cierre del descriptor.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Staging {
    index: usize,
    owner: usize,
}

/// Estado del servicio compartido por todas las peticiones.
struct Server {
    fds: Fds,
    buffers: ClientBuffers,
    staging: Option<Staging>,
}

fn handle(
    fs: &mut AriFs<PartitionDevice>,
    srv: &mut Server,
    ns: Option<&Namespace>,
    owner: usize,
    req: Request,
    data: Option<Handle>,
) -> Reply {
    let mut qbuf = [0u8; 64];
    // Fichero compartido de solo lectura: un solo nombre visible y ninguna
    // operación que escriba (ADR 0016).
    if let Some(shared) = ns.filter(|n| n.kind == NsKind::ReadOnlyFile) {
        match req.op {
            Op::List => {
                return match (req.fd, fs.find(shared.name_str())) {
                    (0, Some(i)) => {
                        let size = fs.entry(i).map_or(0, |e| e.size);
                        Reply::ok(size as u64).with_name(shared.bare())
                    }
                    _ => Reply::status(Status::NotFound),
                };
            }
            Op::Open => {
                return match fs.find(shared.name_str()) {
                    Some(i) if req.name_str() == shared.bare() => srv
                        .fds
                        .open(i, owner)
                        .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
                    _ => Reply::status(Status::NotFound),
                };
            }
            Op::Read | Op::Close => {}
            _ => return Reply::status(Status::Denied),
        }
    }
    let prefix = ns.map_or("", Namespace::name_str);
    match req.op {
        // Se atienden fuera, solo en el endpoint principal.
        Op::Bind | Op::Share => Reply::status(Status::Invalid),
        Op::List => {
            // Desde un directorio privado solo se ven sus ficheros, sin prefijo;
            // desde el principal, todos con su nombre completo.
            let mut visible = fs.entries().filter(|(_, e)| {
                prefix.is_empty()
                    || e.name_str()
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            match visible.nth(req.fd as usize) {
                Some((_, e)) => {
                    let shown = if prefix.is_empty() {
                        e.name_str()
                    } else {
                        &e.name_str()[prefix.len() + 1..]
                    };
                    Reply::ok(e.size as u64).with_name(shown)
                }
                None => Reply::status(Status::NotFound),
            }
        }
        Op::Open => match qualified(prefix, req.name_str(), &mut qbuf).and_then(|q| fs.find(q)) {
            Some(i) => srv
                .fds
                .open(i, owner)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            None => Reply::status(Status::NotFound),
        },
        Op::Create => match qualified(prefix, req.name_str(), &mut qbuf)
            .ok_or(FsError::BadName)
            .and_then(|q| fs.create(q))
        {
            Ok(i) => srv
                .fds
                .open(i, owner)
                .map_or(Reply::status(Status::NoSpace), |fd| Reply::ok(fd as u64)),
            Err(e) => Reply::status(status_of(e)),
        },
        Op::Close => {
            let index = srv.fds.get(req.fd, owner);
            if srv.fds.close(req.fd, owner) {
                if index.is_some_and(|i| srv.staging == Some(Staging { index: i, owner })) {
                    srv.staging = None;
                }
                Reply::ok(0)
            } else {
                Reply::status(Status::BadFd)
            }
        }
        Op::ReplaceCommit => {
            let Some(index) = srv.fds.get(req.fd, owner) else {
                return Reply::status(Status::BadFd);
            };
            if srv.staging != Some(Staging { index, owner }) {
                return Reply::status(Status::Busy);
            }
            srv.staging = None;
            match fs.replace_commit(index, req.len) {
                Ok(()) => Reply::ok(req.len as u64),
                Err(e) => Reply::status(status_of(e)),
            }
        }
        Op::Flush => match fs.flush() {
            Ok(()) => Reply::ok(0),
            Err(e) => Reply::status(status_of(e)),
        },
        Op::Read | Op::Write | Op::Replace | Op::ReplaceChunk => {
            let Some(index) = srv.fds.get(req.fd, owner) else {
                return Reply::status(Status::BadFd);
            };
            let Some(dma) = data else {
                return Reply::status(Status::BadHandles);
            };
            let len = req.len.min(MAX_IO) as usize;
            let Some(buf) = srv.buffers.map(dma) else {
                return Reply::status(Status::BadHandles);
            };
            if buf.len() < len {
                return Reply::status(Status::Invalid);
            }
            let mine = Staging { index, owner };
            if matches!(req.op, Op::Replace | Op::ReplaceChunk)
                && srv.staging.is_some_and(|s| s != mine)
            {
                return Reply::status(Status::Busy);
            }
            let result = match req.op {
                Op::Read => fs.read(index, req.offset, &mut buf[..len]),
                Op::Write => fs.write(index, req.offset, &buf[..len]),
                Op::Replace => {
                    srv.staging = None;
                    fs.replace(index, &buf[..len]).map(|()| len)
                }
                _ => {
                    let r = fs.replace_chunk(index, req.offset, &buf[..len]);
                    srv.staging = r.is_ok().then_some(mine);
                    r.map(|()| len)
                }
            };
            match result {
                Ok(n) => Reply::ok(n as u64),
                Err(e) => Reply::status(status_of(e)),
            }
        }
    }
}

fn main(partition_lba: u64) -> u64 {
    let Ok(blk) = BlockClient::new(BLOCK, HW, aricode_arifs::MAX_BATCH_SECTORS as u16) else {
        return 1;
    };
    let dev = PartitionDevice {
        blk,
        base_lba: partition_lba,
    };
    let mut fs = match AriFs::open(dev) {
        Ok(fs) => fs,
        Err(FsError::Corrupt) => {
            say(b"AriFS corrupto: no se monta");
            return 3;
        }
        Err(_) => {
            say(b"la particion no contiene AriFS");
            return 2;
        }
    };
    match fs.recovery() {
        Recovery::Clean => {}
        Recovery::Replayed { index } => {
            sys::print(b"fsd: reemplazo pendiente de '");
            sys::print(fs.entry(index).map_or("?", |e| e.name_str()).as_bytes());
            sys::print(b"' rehecho al montar\n");
        }
        Recovery::Discarded => say(b"journal roto descartado al montar"),
    }
    let mut buf = [0u8; 20];
    sys::print(b"fsd: AriFS montado, ");
    sys::print(aricode_rt::fmt_u64(fs.entries().count() as u64, &mut buf));
    sys::print(b" ficheros\n");
    let mut srv = Server {
        fds: Fds([None; 16]),
        buffers: ClientBuffers {
            entries: [(0, 0, 0); 8],
            count: 0,
        },
        staging: None,
    };
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 2];
    // Espacios de nombres concedidos: directorios privados y ficheros
    // compartidos, cada uno con su endpoint (lado de lectura).
    let mut namespaces: [Option<Namespace>; 8] = [None; 8];
    say(b"sirviendo peticiones");
    loop {
        // Espera en el endpoint principal y en todos los concedidos a la
        // vez (ADR 0020): el indice dice por cual llego la peticion.
        let mut eps = [SERVICE; 9];
        let mut slots = [0usize; 9];
        let mut count = 1;
        for (i, ns) in namespaces.iter().enumerate() {
            if let Some(ns) = ns {
                eps[count] = ns.ep;
                slots[count] = i + 1;
                count += 1;
            }
        }
        let mut received = [Handle(0); sys::abi::IPC_MAX_HANDLES];
        let Ok((which, n, nh)) = sys::ipc_receive_any(&eps[..count], &mut msg, &mut received, 1000)
        else {
            continue;
        };
        handles[..2].copy_from_slice(&received[..2]);
        {
            let slot = slots[which];
            let ns = (slot > 0).then(|| namespaces[slot - 1]).flatten();
            if nh == 0 {
                continue;
            }
            let reply_to = handles[0];
            let data = (nh >= 2).then_some(handles[1]);
            let request = Request::decode(&msg[..n]);
            if let Some(
                req @ Request {
                    op: Op::Bind | Op::Share,
                    ..
                },
            ) = request
            {
                if slot != 0 {
                    answer(reply_to, Reply::status(Status::Invalid), None);
                } else {
                    let kind = if req.op == Op::Bind {
                        NsKind::Dir
                    } else {
                        NsKind::ReadOnlyFile
                    };
                    match grant(&fs, &mut namespaces, req.name_str(), kind) {
                        Ok(client_ep) => answer(reply_to, Reply::ok(0), Some(client_ep)),
                        Err(status) => answer(reply_to, Reply::status(status), None),
                    }
                }
                if let Some(d) = data {
                    let _ = sys::handle_close(d);
                }
                continue;
            }
            let reply = match request {
                Some(req) => handle(&mut fs, &mut srv, ns.as_ref(), slot, req, data),
                None => Reply::status(Status::Invalid),
            };
            if let Some(d) = data {
                let _ = sys::handle_close(d);
            }
            answer(reply_to, reply, None);
        }
    }
}

/// Responde por el endpoint transferido (con un handle opcional) y lo cierra.
fn answer(reply_to: Handle, reply: Reply, handle: Option<Handle>) {
    let sent = match handle {
        Some(h) => sys::ipc_send(reply_to, &reply.encode(), &[h], 0),
        None => sys::ipc_send(reply_to, &reply.encode(), &[], 0),
    };
    if sent.is_err() {
        if let Some(h) = handle {
            let _ = sys::handle_close(h);
        }
    }
    let _ = sys::handle_close(reply_to);
}

/// Concede un espacio de nombres: un directorio privado (`Bind`, prefijo
/// nuevo o existente) o un fichero compartido de solo lectura (`Share`, que
/// debe existir). Endpoint nuevo cuyo lado de escritura se transfiere al
/// cliente; fsd conserva el de lectura.
fn grant(
    fs: &AriFs<PartitionDevice>,
    namespaces: &mut [Option<Namespace>; 8],
    name: &str,
    kind: NsKind,
) -> Result<Handle, Status> {
    let valid = match kind {
        NsKind::Dir => {
            !name.is_empty()
                && name.len() <= 32
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        }
        NsKind::ReadOnlyFile => name.len() <= 32,
    };
    if !valid {
        return Err(Status::Invalid);
    }
    if kind == NsKind::ReadOnlyFile && fs.find(name).is_none() {
        return Err(Status::NotFound);
    }
    // El mismo nombre y clase comparten endpoint: cada lanzamiento de una
    // aplicacion recibe otro handle al mismo espacio.
    if let Some(ns) = namespaces
        .iter()
        .flatten()
        .find(|ns| ns.kind == kind && ns.name_str() == name)
    {
        return sys::handle_duplicate(ns.ep, sys::rights::WRITE | sys::rights::TRANSFER)
            .map_err(|_| Status::NoSpace);
    }
    let slot = namespaces
        .iter()
        .position(Option::is_none)
        .ok_or(Status::NoSpace)?;
    let ep = sys::endpoint_create().map_err(|_| Status::NoSpace)?;
    let client = sys::handle_duplicate(ep, sys::rights::WRITE | sys::rights::TRANSFER)
        .map_err(|_| Status::NoSpace)?;
    let mut n = [0u8; 32];
    n[..name.len()].copy_from_slice(name.as_bytes());
    namespaces[slot] = Some(Namespace {
        ep,
        name: n,
        len: name.len(),
        kind,
    });
    sys::print(match kind {
        NsKind::Dir => b"fsd: directorio privado '".as_slice(),
        NsKind::ReadOnlyFile => b"fsd: fichero compartido '".as_slice(),
    });
    sys::print(name.as_bytes());
    sys::print(match kind {
        NsKind::Dir => b"'\n".as_slice(),
        NsKind::ReadOnlyFile => b"' (solo lectura)\n".as_slice(),
    });
    Ok(client)
}

aricode_rt::entry!(main);
