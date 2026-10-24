// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Launch broker client (`aricode-proto::launch`): asks init to run an
//! initrd program with the capabilities from its manifest.

use crate::{rpc, Error, Handle, TIMEOUT_INFINITE};
pub use aricode_proto::launch::Status;
use aricode_proto::launch::{Reply, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    Ipc(Error),
    Status(Status),
    /// Empty or too long name.
    BadName,
}

/// Runs `name` with `arg` through the broker and returns its exit code.
pub fn run(broker: Handle, name: &str, arg: u64) -> Result<u64, LaunchError> {
    let req = Request::new(name, arg).ok_or(LaunchError::BadName)?;
    wait_for_exit(broker, req, &[])
}

/// Like `run`, with `console` (the send side of an endpoint, with
/// `TRANSFER`) as the program's console instead of the kernel console
/// (ADR 0047). `console` is given up whatever the outcome.
pub fn run_with_console(
    broker: Handle,
    name: &str,
    arg: u64,
    console: Handle,
) -> Result<u64, LaunchError> {
    let Some(req) = Request::new(name, arg) else {
        let _ = crate::handle_close(console);
        return Err(LaunchError::BadName);
    };
    wait_for_exit(broker, req.with_console(), &[console])
}

fn wait_for_exit(broker: Handle, req: Request, handles: &[Handle]) -> Result<u64, LaunchError> {
    // The reply is the program's exit: the only unbounded wait.
    let mut msg = [0u8; Reply::LEN];
    let len = rpc::call(
        broker,
        &req.encode(),
        handles,
        rpc::GRACE_MS,
        TIMEOUT_INFINITE,
        &mut msg,
    )
    .map_err(LaunchError::Ipc)?;
    let reply = Reply::decode(&msg[..len]).ok_or(LaunchError::Ipc(Error::Invalid))?;
    match reply.status {
        Status::Ok => Ok(reply.value),
        s => Err(LaunchError::Status(s)),
    }
}

/// Launches `name` with `arg` in the background: returns as soon as the
/// broker has created the process (ADR 0015). There is no way to wait for it
/// or stop it.
pub fn spawn(broker: Handle, name: &str, arg: u64) -> Result<(), LaunchError> {
    let req = Request::new(name, arg)
        .ok_or(LaunchError::BadName)?
        .detached();
    let mut msg = [0u8; Reply::LEN];
    let len = rpc::call(
        broker,
        &req.encode(),
        &[],
        rpc::GRACE_MS,
        rpc::GRACE_MS,
        &mut msg,
    )
    .map_err(LaunchError::Ipc)?;
    let reply = Reply::decode(&msg[..len]).ok_or(LaunchError::Ipc(Error::Invalid))?;
    match reply.status {
        Status::Started => Ok(()),
        s => Err(LaunchError::Status(s)),
    }
}
