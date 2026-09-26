//! Programas de usuario embebidos: ELF estáticos generados por la crate
//! `user-testprogs` (los construye `cargo xtask` y expone su directorio en
//! `ARICODE_USER_BIN`).

macro_rules! programs {
    ($($name:ident),* $(,)?) => {
        /// ELF de un programa por nombre de test, o `None`.
        pub fn by_name(name: &str) -> Option<&'static [u8]> {
            match name {
                $( stringify!($name) => Some(include_bytes!(concat!(env!("ARICODE_USER_BIN"), "/", stringify!($name)))), )*
                _ => None,
            }
        }
    };
}

programs!(
    hello,
    kernel_read,
    kernel_write,
    privileged,
    bad_syscall,
    bad_pointer,
    sleep_yield,
    handles,
    caps,
    ipc_server,
    ipc_client
);
