//! The libublk target glue (Phase 7, ADR-0020).
//!
//! Registers a Linux ublk block device whose IO is served by the same
//! entropy store (`BlockStore`). Requires root and `CONFIG_BLK_DEV_UBLK`;
//! the IO path is the normal representation engine (materialize on read,
//! guided search on write, durability barrier on flush, hole punch on
//! discard). The engine is single-writer (a `Mutex<BlockStore>`), the same
//! concurrency model as the FUSE frontend.
//!
//! This module is compiled always (no cfg gate) but is inert unless the
//! `ublk run` CLI is invoked.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use libublk::UblkFlags;
use libublk::ctrl::UblkCtrlBuilder;
use libublk::helpers::IoBuf;
use libublk::io::{BufDesc, UblkDev, UblkQueue};

use super::block::BlockStore;
use crate::store::{StoreConfig, StoreError};

/// The shared device backing the kernel target (the target callbacks carry
/// no user data, so the device lives here).
static DEV: OnceLock<Arc<Mutex<BlockStore>>> = OnceLock::new();

fn dev() -> &'static Arc<Mutex<BlockStore>> {
    DEV.get().expect("ublk device not initialized")
}

/// Negative errno for the kernel IO reply (no libc dependency; rustix
/// carries the constants).
fn errno(e: rustix::io::Errno) -> i32 {
    -e.raw_os_error()
}

/// Handle one IO command. Returns the bytes processed (or a negative
/// errno). Never panics on malformed IO.
fn handle_io_cmd(q: &UblkQueue<'_>, tag: u16, buf: &mut [u8]) -> i32 {
    let iod = q.get_iod(tag);
    let off = iod.start_sector << 9;
    let bytes = (iod.nr_sectors << 9) as u64;
    let op = iod.op_flags & 0xff;

    if bytes > buf.len() as u64 {
        return errno(rustix::io::Errno::INVAL);
    }
    if off.checked_add(bytes).is_none() {
        return errno(rustix::io::Errno::INVAL);
    }

    let mut guard = match dev().lock() {
        Ok(g) => g,
        Err(_) => return errno(rustix::io::Errno::IO),
    };
    match op {
        libublk::sys::UBLK_IO_OP_READ => match guard.read(off, bytes) {
            Ok(data) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                n as i32
            }
            Err(_) => errno(rustix::io::Errno::IO),
        },
        libublk::sys::UBLK_IO_OP_WRITE => match guard.write(off, &buf[..bytes as usize]) {
            Ok(n) => n as i32,
            Err(StoreError::Full(_)) => errno(rustix::io::Errno::NOSPC),
            Err(_) => errno(rustix::io::Errno::IO),
        },
        libublk::sys::UBLK_IO_OP_FLUSH => match guard.flush() {
            Ok(()) => 0,
            Err(_) => errno(rustix::io::Errno::IO),
        },
        libublk::sys::UBLK_IO_OP_DISCARD => match guard.discard(off, bytes) {
            Ok(()) => 0,
            Err(_) => errno(rustix::io::Errno::IO),
        },
        _ => errno(rustix::io::Errno::OPNOTSUPP),
    }
}

/// One queue's IO loop (one task per tag, smol-driven; the ramdisk
/// example's structure).
async fn io_task(q: &UblkQueue<'_>, tag: u16) -> Result<(), libublk::UblkError> {
    let buf_bytes = q.dev.dev_info.max_io_buf_bytes as usize;
    let mut buffer = IoBuf::<u8>::new(buf_bytes);
    q.submit_io_prep_cmd(tag, BufDesc::Slice(buffer.as_slice()), 0, Some(&buffer))
        .await?;
    loop {
        let io_slice = buffer.as_mut_slice();
        let res = handle_io_cmd(q, tag, io_slice);
        q.submit_io_commit_cmd(tag, BufDesc::Slice(buffer.as_slice()), res)
            .await?;
    }
}

/// The queue function passed to `run_target`.
fn q_fn(qid: u16, dev: &UblkDev) {
    let q_rc = std::rc::Rc::new(UblkQueue::new(qid, dev).expect("ublk queue"));
    let exe_rc = std::rc::Rc::new(smol::LocalExecutor::new());
    let exe = exe_rc.clone();
    let mut tasks = Vec::new();
    for tag in 0..dev.dev_info.queue_depth {
        let q = q_rc.clone();
        tasks.push(exe.spawn(async move { io_task(&q, tag).await }));
    }
    smol::block_on(exe_rc.run(async move {
        let run_ops = || while exe.try_tick() {};
        let done = || tasks.iter().all(|t| t.is_finished());
        if let Err(e) = libublk::wait_and_handle_io_events(&q_rc, Some(20), run_ops, done).await {
            log::error!("handle_uring_events failed: {e}");
        }
    }));
}

/// Run a ublk device: open the entropy-backed block device, register it
/// with the kernel, and serve IO until terminated. Requires root and the
/// `ublk_drv` kernel module (CachyOS ships it as a module).
pub fn run(
    store_dir: &Path,
    name: &str,
    capacity_bytes: u64,
    nr_queues: u16,
) -> Result<(), String> {
    let block =
        BlockStore::open_or_create(store_dir, &StoreConfig::default(), name, capacity_bytes)
            .map_err(|e| e.to_string())?;
    DEV.set(Arc::new(Mutex::new(block)))
        .map_err(|_| "ublk device already running".to_string())?;

    let ctrl = std::sync::Arc::new(
        UblkCtrlBuilder::default()
            .name(name)
            .nr_queues(nr_queues)
            .dev_flags(UblkFlags::UBLK_DEV_F_ADD_DEV)
            .build()
            .map_err(|e| format!("ublk control: {e}"))?,
    );
    let ctrl_sig = std::sync::Arc::clone(&ctrl);
    ctrlc::set_handler(move || {
        let _ = ctrl_sig.kill_dev();
    })
    .map_err(|e| format!("signal handler: {e}"))?;

    println!("ublk: registering '{name}' ({capacity_bytes} bytes, {nr_queues} queues)");
    ctrl.run_target(
        |dev| {
            dev.set_default_params(capacity_bytes);
            Ok(())
        },
        q_fn,
        |dev| dev.dump(),
    )
    .map_err(|e| format!("ublk target: {e}"))?;
    ctrl.del_dev().map_err(|e| format!("ublk del: {e}"))?;
    Ok(())
}
