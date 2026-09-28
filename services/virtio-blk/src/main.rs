//! Driver virtio-blk (Fase 18) y servicio de bloques (Fase 19) en ring 3.
//!
//! Recibe de `init`, en este orden, el endpoint de servicio, el espacio de
//! configuración PCI, la capacidad de hardware, la IRQ MSI-X y los BARs
//! presentes (`arg` es la máscara de BARs). Inicializa el dispositivo por la
//! interfaz moderna, crea la cola 0 en memoria DMA, lee la GPT y atiende por
//! IPC (`aricode-proto::block`) peticiones de otros procesos, que aportan su
//! propio buffer DMA: el dispositivo lee y escribe directamente en él.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};
use aricode_rt::sys::virtio_pci::{DmaRegion, Queue, Transport, IRQ, SERVICE};
use aricode_rt::sys::{self, Error, Handle, TIMEOUT_INFINITE};
use aricode_virtio::blk;

fn say(msg: &[u8]) {
    sys::print(b"virtio-blk: ");
    sys::print(msg);
    sys::print(b"\n");
}

struct Device {
    transport: Transport,
    queue: Queue,
    /// Buffer DMA propio: cabecera (16), un sector de datos (512) y estado (1).
    req: DmaRegion,
    /// `VIRTIO_BLK_F_FLUSH` negociado: `T_FLUSH` hace duraderas las escrituras.
    flush: bool,
}

fn setup(mask: u64) -> Result<Device, u64> {
    let transport = Transport::open(mask, blk::F_FLUSH).map_err(|e| e.code())?;
    let flush = transport.features & blk::F_FLUSH != 0;
    let queue = transport.setup_queue(0, 64).map_err(|e| e.code())?;
    transport.driver_ok();
    let req = DmaRegion::alloc(4096).map_err(|_| 30u64)?;
    Ok(Device {
        transport,
        queue,
        req,
        flush,
    })
}

impl Device {
    fn capacity(&self) -> u64 {
        self.transport.device_cfg.r64(blk::CFG_CAPACITY)
    }

    /// Una peticion sincrona: cabecera y estado en el buffer propio, datos en
    /// `(data_phys, data_len)` si los hay (`T_FLUSH` no lleva); espera la
    /// interrupcion MSI-X y comprueba el estado.
    fn request(
        &mut self,
        header: blk::RequestHeader,
        data: Option<(u64, u32, bool)>,
    ) -> Result<(), u64> {
        self.req
            .slice_mut(0, 16)
            .copy_from_slice(&header.to_bytes());
        self.req.slice_mut(528, 1)[0] = 0xFF;
        let status = (self.req.phys + 528, 1, true);
        let head = match data {
            Some((phys, len, device_writes)) => self.queue.vq.add(&[
                (self.req.phys, 16, false),
                (phys, len, device_writes),
                status,
            ]),
            None => self.queue.vq.add(&[(self.req.phys, 16, false), status]),
        }
        .ok_or(40u64)?;
        self.queue.notify(&self.transport);
        loop {
            if let Some((done, _)) = self.queue.vq.pop_used() {
                if done != head {
                    return Err(41);
                }
                break;
            }
            match sys::irq_wait(IRQ, 2000) {
                Ok(_) => {}
                Err(Error::Timeout) => return Err(42),
                Err(_) => return Err(43),
            }
        }
        match self.req.slice_mut(528, 1)[0] {
            blk::STATUS_OK => Ok(()),
            _ => Err(44),
        }
    }

    /// Lee un sector en el buffer propio (para la GPT).
    fn read_own(&mut self, lba: u64) -> Result<&[u8], u64> {
        self.request(
            blk::RequestHeader::read(lba),
            Some((self.req.phys + 16, 512, true)),
        )?;
        Ok(self.req.slice_mut(16, 512))
    }
}

/// Lee la GPT y anuncia las particiones.
fn report_partitions(dev: &mut Device) {
    let mut buf = [0u8; 20];
    let header = match dev
        .read_own(1)
        .ok()
        .and_then(|s| aricode_gpt::Header::parse(s).ok())
    {
        Some(h) => h,
        None => {
            say(b"sin GPT");
            return;
        }
    };
    // La tabla (16 KiB) va al heap: la pila de usuario es pequeña.
    let mut table = alloc::vec![0u8; 128 * aricode_gpt::ENTRY_LEN];
    let sectors = header.entries_bytes() / 512;
    for i in 0..sectors.min(32) {
        match dev.read_own(header.entries_lba + i as u64) {
            Ok(s) => table[i * 512..(i + 1) * 512].copy_from_slice(s),
            Err(_) => {
                say(b"error leyendo la tabla GPT");
                return;
            }
        }
    }
    let mut parts = [None; 8];
    match aricode_gpt::partitions(&header, &table, &mut parts) {
        Ok(n) => {
            sys::print(b"virtio-blk: GPT con ");
            sys::print(aricode_rt::fmt_u64(n as u64, &mut buf));
            sys::print(b" particion(es)\n");
            for p in parts.iter().flatten() {
                sys::print(b"virtio-blk:   ");
                sys::print(p.name_str().as_bytes());
                sys::print(b" LBA ");
                sys::print(aricode_rt::fmt_u64(p.first_lba, &mut buf));
                sys::print(b"..");
                sys::print(aricode_rt::fmt_u64(p.last_lba, &mut buf));
                sys::print(b"\n");
            }
        }
        Err(_) => say(b"tabla GPT corrupta"),
    }
}

/// Mapeos de buffers DMA de clientes, reutilizados por direccion fisica
/// (cada `mmio_map` crea un mapeo nuevo que no se libera hasta salir).
struct ClientBuffers {
    entries: [(u64, u64); 8], // (phys, len)
    count: usize,
}

impl ClientBuffers {
    fn map(&mut self, dma: Handle) -> Option<(u64, u64)> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        if let Some(e) = self.entries[..self.count].iter().find(|e| e.0 == phys) {
            return Some(*e);
        }
        sys::mmio_map(dma).ok()?;
        if self.count < self.entries.len() {
            self.entries[self.count] = (phys, len);
            self.count += 1;
        }
        Some((phys, len))
    }
}

/// Bucle de servicio: una peticion por mensaje, respuesta por el endpoint
/// transferido en el mensaje.
fn serve(dev: &mut Device) -> ! {
    let capacity = dev.capacity();
    let mut buffers = ClientBuffers {
        entries: [(0, 0); 8],
        count: 0,
    };
    let mut msg = [0u8; 32];
    let mut handles = [Handle(0); 2];
    say(b"sirviendo peticiones");
    loop {
        let Ok((n, nh)) = sys::ipc_receive(SERVICE, &mut msg, &mut handles, TIMEOUT_INFINITE)
        else {
            continue;
        };
        if nh == 0 {
            continue; // sin endpoint de respuesta no hay a quien contestar
        }
        let reply_to = handles[0];
        let reply = match Request::decode(&msg[..n]) {
            None => Reply {
                status: Status::Invalid,
                value: 0,
            },
            Some(Request { op: Op::Info, .. }) => Reply {
                status: Status::Ok,
                value: capacity,
            },
            Some(Request { op: Op::Flush, .. }) => {
                let done = if dev.flush {
                    dev.request(blk::RequestHeader::flush(), None)
                } else {
                    Ok(())
                };
                Reply {
                    status: if done.is_ok() { Status::Ok } else { Status::Io },
                    value: 0,
                }
            }
            Some(Request { op, count, lba }) => {
                let count = count.min(MAX_SECTORS) as u64;
                if nh < 2 {
                    Reply {
                        status: Status::BadHandles,
                        value: 0,
                    }
                } else if count == 0 || lba.checked_add(count).is_none_or(|end| end > capacity) {
                    Reply {
                        status: Status::Invalid,
                        value: 0,
                    }
                } else {
                    match buffers.map(handles[1]) {
                        Some((phys, len)) if len >= count * 512 => {
                            let bytes = (count * 512) as u32;
                            let r = match op {
                                Op::Read => dev.request(
                                    blk::RequestHeader::read(lba),
                                    Some((phys, bytes, true)),
                                ),
                                Op::Write => dev.request(
                                    blk::RequestHeader::write(lba),
                                    Some((phys, bytes, false)),
                                ),
                                Op::Info | Op::Flush => unreachable!(),
                            };
                            match r {
                                Ok(()) => Reply {
                                    status: Status::Ok,
                                    value: count,
                                },
                                Err(_) => {
                                    if dev.transport.needs_reset() {
                                        say(b"el dispositivo pide reset");
                                    }
                                    Reply {
                                        status: Status::Io,
                                        value: 0,
                                    }
                                }
                            }
                        }
                        _ => Reply {
                            status: Status::BadHandles,
                            value: 0,
                        },
                    }
                }
            }
        };
        if nh >= 2 {
            let _ = sys::handle_close(handles[1]);
        }
        let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
        let _ = sys::handle_close(reply_to);
    }
}

fn main(mask: u64) -> u64 {
    let mut dev = match setup(mask) {
        Ok(d) => d,
        Err(step) => {
            say(b"fallo en la inicializacion");
            return step;
        }
    };
    let mut buf = [0u8; 20];
    say(b"dispositivo listo");
    say(if dev.flush {
        b"flush negociado (VIRTIO_BLK_F_FLUSH)"
    } else {
        b"sin flush: el dispositivo no lo ofrece"
    });
    sys::print(b"virtio-blk: capacidad ");
    sys::print(aricode_rt::fmt_u64(dev.capacity(), &mut buf));
    sys::print(b" sectores\n");
    report_partitions(&mut dev);
    serve(&mut dev)
}

aricode_rt::entry!(main);
