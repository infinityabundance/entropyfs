#![forbid(unsafe_code)]
use crate::platform::io_uring::Uring;
use io_uring::opcode;
use io_uring::types::Fd;
use std::io::Write;
use std::time::Instant;

fn ns_per(mut f: impl FnMut()) -> f64 {
    let iters = 20000u32;
    // warmup
    for _ in 0..1000 {
        f();
    }
    let t0 = Instant::now();
    for _ in 0..iters {
        f();
    }

    t0.elapsed().as_nanos() as f64 / iters as f64
}

#[test]
fn bench_ring_vs_pread() {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("f");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let payload: Vec<u8> = vec![0xAB; 4096];
    file.write_all(&payload).unwrap();
    let ring = Uring::new(256).unwrap();

    // single pread via std
    let mut buf = vec![0u8; 4096];
    let pread_ns = ns_per(|| {
        let _ = rustix::io::pread(&file, &mut buf, 0).unwrap();
    });

    // single read via ring
    let ring_read_ns = ns_per(|| {
        let mut b = vec![0u8; 4096];
        let _ = ring
            .submit_and_wait(&[(
                1,
                opcode::Read::new(Fd(file.as_raw_fd()), b.as_mut_ptr(), 4096)
                    .offset(0)
                    .build(),
            )])
            .unwrap();
    });

    // single pwrite via std
    let pwrite_ns = ns_per(|| {
        let _ = rustix::io::pwrite(&file, &payload, 0).unwrap();
    });

    // single write via ring
    let ring_write_ns = ns_per(|| {
        let owned = payload.clone();
        let _ = ring
            .submit_and_wait(&[(
                1,
                opcode::Write::new(Fd(file.as_raw_fd()), owned.as_ptr(), 4096)
                    .offset(0)
                    .build(),
            )])
            .unwrap();
    });

    // batch of 32 reads via ring vs 32 preads
    let mut bufs: Vec<Vec<u8>> = vec![vec![0u8; 4096]; 32];
    let batch_ns = ns_per(|| {
        let ops: Vec<(u64, io_uring::squeue::Entry)> = (0..32)
            .map(|i| {
                (
                    i as u64,
                    opcode::Read::new(Fd(file.as_raw_fd()), bufs[i as usize].as_mut_ptr(), 4096)
                        .offset(0)
                        .build(),
                )
            })
            .collect();
        let _ = ring.submit_and_wait(&ops).unwrap();
    });
    let pread32_ns = ns_per(|| {
        for buf in bufs.iter_mut() {
            let _ = rustix::io::pread(&file, buf, 0).unwrap();
        }
    });

    println!("pread 1x4096:      {pread_ns:.1} ns");
    println!("ring read 1x4096:  {ring_read_ns:.1} ns");
    println!("pwrite 1x4096:     {pwrite_ns:.1} ns");
    println!("ring write 1x4096: {ring_write_ns:.1} ns");
    println!(
        "32 preads:         {pread32_ns:.1} ns ({:.2}/op)",
        pread32_ns / 32.0
    );
    println!(
        "ring batch 32:     {batch_ns:.1} ns ({:.2}/op)",
        batch_ns / 32.0
    );
    let _ = &mut buf;
}
