//! Puente de consola (etapa 2, ADR 0013): lleva las pulsaciones de los
//! teclados de usuario (virtio-input, USB HID) al anillo de entrada de
//! consola del kernel, para que el shell lea de ellos igual que de COM1 o
//! del teclado PS/2. Handles: 0x10004 = entrada de consola (`MANAGE`),
//! 0x10005.. = endpoints de los servicios de entrada (`arg` = cuántos).
#![no_std]
#![no_main]

use aricode_input::{Keyboard, EV_KEY};
use aricode_proto::input::{Event, MAX_EVENTS};
use aricode_rt::sys::input::InputClient;
use aricode_rt::sys::{self, Handle};

const CONSOLE_IN: Handle = Handle(0x0001_0004);
const FIRST_INPUT: u32 = 0x0001_0005;
const MAX_INPUTS: usize = 4;
/// Espera por dispositivo en cada vuelta: los servicios se recorren por
/// turnos (cada uno admite un solo lector a la espera), así que la latencia
/// máxima de una pulsación es este plazo por cada dispositivo restante.
const POLL_MS: u32 = 20;

fn say(msg: &[u8]) {
    sys::print(b"console-bridge: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn main(count: u64) -> u64 {
    let count = (count as usize).min(MAX_INPUTS);
    if count == 0 {
        say(b"sin dispositivos de entrada");
        return 1;
    }
    let mut clients: [Option<InputClient>; MAX_INPUTS] = [const { None }; MAX_INPUTS];
    for (i, slot) in clients.iter_mut().take(count).enumerate() {
        match InputClient::new(Handle(FIRST_INPUT + i as u32)) {
            Ok(c) => *slot = Some(c),
            Err(e) => {
                aricode_rt::print_error(b"console-bridge: entrada", e);
                return 2;
            }
        }
    }
    // Un estado de teclado por dispositivo: mayúsculas y bloqueo son suyos.
    let mut keyboards = [Keyboard::new(); MAX_INPUTS];
    let mut events = [Event::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    let mut text = [0u8; MAX_EVENTS];
    // Sin anuncio propio: lo hace init antes de lanzar el shell, para que
    // nada se entrelace con el primer prompt en la consola (varias CPUs).
    loop {
        for (i, client) in clients.iter().take(count).enumerate() {
            let Some(client) = client else { continue };
            // Sin eventos (plazo), servicio ocupado o muerto (el supervisor
            // lo relanza y el endpoint sigue valiendo): nada que traducir.
            let n = client.read(POLL_MS, &mut events, &mut times).unwrap_or(0);
            let mut len = 0;
            for e in &events[..n] {
                let ev = aricode_input::Event {
                    kind: e.kind,
                    code: e.code,
                    value: e.value,
                };
                // Los botones del ratón son EV_KEY >= 0x100: no son texto.
                if ev.kind == EV_KEY && ev.code >= 0x100 {
                    continue;
                }
                if let Some(ch) = keyboards[i].feed(ev) {
                    text[len] = ch;
                    len += 1;
                }
            }
            if len > 0 && sys::console_inject(CONSOLE_IN, &text[..len]).is_err() {
                say(b"la consola rechaza la entrada; termina");
                return 3;
            }
        }
    }
}

aricode_rt::entry!(main);
