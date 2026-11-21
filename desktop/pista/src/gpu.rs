// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Optional Gen7 compute finishing. Each allocation is owned by this module;
//! a failed/timed-out dispatch is never reused or read while still in flight.
use aricode_pista::{effects, gpu as shader};
use aricode_rt::sys::{
    self,
    abi::{
        gpu::Dispatch,
        sysinfo::{self, GpuReport},
    },
    Handle,
};
use core::sync::atomic::{fence, Ordering};

struct Mapping {
    cpu: u64,
    gpu: Handle,
    address: u64,
    words: usize,
}
impl Mapping {
    fn new(gpu: Handle, hw: Handle, address: u64, words: usize) -> Option<Self> {
        let memory = sys::dma_alloc(hw, (words * 4) as u64).ok()?;
        let cpu = match sys::mmio_map(memory) {
            Ok(v) => v,
            Err(_) => {
                let _ = sys::handle_close(memory);
                return None;
            }
        };
        let mapped = sys::gpu_map(gpu, memory, address);
        let _ = sys::handle_close(memory);
        if mapped.is_err() {
            let _ = sys::mmio_unmap(cpu);
            return None;
        }
        Some(Self {
            cpu,
            gpu,
            address,
            words,
        })
    }
    fn data(&mut self) -> &mut [u32] {
        // SAFETY: exclusively borrowed CPU mapping, owned for the lifetime of
        // self. Callers only access after gpu_wait, or before first submission.
        unsafe { core::slice::from_raw_parts_mut(self.cpu as *mut u32, self.words) }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        // If the engine is still busy, its PPGTT mapping retains the allocation
        // until the process is destroyed. It is never recycled by this module.
        let _ = sys::gpu_unmap(self.gpu, self.address);
        let _ = sys::mmio_unmap(self.cpu);
    }
}

pub struct Finish {
    gpu: Handle,
    input: Mapping,
    output: Mapping,
    state: Mapping,
    dimensions: (usize, usize),
    dispatch: Dispatch,
    threads: u16,
    active: bool,
}
impl Finish {
    pub fn new(gpu: Option<Handle>, hw: Handle, system: Handle) -> Option<Self> {
        let gpu = gpu?;
        let mut b = [0; GpuReport::LEN];
        if sys::system_info(system, sysinfo::GPU, &mut b).ok()? != GpuReport::LEN {
            return None;
        }
        let report = GpuReport::decode(&b)?;
        if report.state != sysinfo::GPU_READY || report.render != 1 || report.threads == 0 {
            return None;
        }
        let mut this = Self {
            gpu,
            input: Mapping::new(gpu, hw, shader::INPUT, (960 + 8) * (540 + 8))?,
            output: Mapping::new(gpu, hw, shader::OUTPUT, 960 * 540)?,
            state: Mapping::new(gpu, hw, shader::STATE, shader::STATE_BYTES / 4)?,
            dimensions: (0, 0),
            dispatch: Dispatch::default(),
            threads: report.threads,
            active: true,
        };
        // Verify the actual hardware path, including readback and every edge.
        let pattern: [u32; 128] = core::array::from_fn(|i| {
            if i % 11 == 0 {
                0xffffff
            } else {
                (i as u32 * 7919) & 0xffffff
            }
        });
        let mut expected = [0; 128];
        let mut actual = [0; 128];
        effects::finish(&pattern, &mut expected, 16, 8);
        if !this.compute(&pattern, &mut actual, 16, 8) || actual != expected {
            sys::print(b"pista: GPU finishing self-check failed; using CPU\n");
            return None;
        }
        sys::print(b"pista: Gen7 GPU bloom enabled (readback verified); geometry on CPU\n");
        Some(this)
    }

    fn compute(&mut self, source: &[u32], out: &mut [u32], width: usize, height: usize) -> bool {
        if !self.active {
            return false;
        }
        if self.dimensions != (width, height) {
            let Some(d) =
                shader::setup(self.state.data(), width as u32, height as u32, self.threads)
            else {
                self.active = false;
                return false;
            };
            self.dispatch = d;
            self.dimensions = (width, height);
        }
        effects::pad(source, self.input.data(), width, height);
        fence(Ordering::SeqCst);
        let result = sys::gpu_dispatch(self.gpu, &self.dispatch)
            .and_then(|n| sys::gpu_wait(self.gpu, n, 1000));
        if result.is_err() {
            // Quarantine all three GPU buffers: no further CPU writes or GPU
            // submissions. The renderer's separate buffers remain safe to use.
            self.active = false;
            sys::print(b"pista: GPU finishing unavailable; continuing on CPU\n");
            return false;
        }
        fence(Ordering::SeqCst);
        out[..width * height].copy_from_slice(&self.output.data()[..width * height]);
        true
    }

    pub fn apply(
        optional: &mut Option<Self>,
        source: &[u32],
        out: &mut [u32],
        width: usize,
        height: usize,
    ) -> bool {
        if let Some(gpu) = optional {
            if gpu.compute(source, out, width, height) {
                return true;
            }
        }
        effects::finish(source, out, width, height);
        false
    }
}
