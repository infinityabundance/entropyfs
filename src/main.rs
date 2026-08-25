//! EntropyFS — entropy-native Linux filesystem.
//!
//! Persist irreducible state. Materialize structure. Preserve exact
//! bytes. Measure everything.

#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};

mod cli;

// The binary re-exports the library modules the CLI addresses as
// `crate::…` (bin + lib share the crate name `entropyfs`).
#[allow(unused_imports)]
use entropyfs::{core, dsfb, entropy, format, fsck, fuse, optimizer, platform, rans, store};

/// The entropy-native Linux filesystem.
#[derive(Parser)]
#[command(
    name = "entropyfs",
    version,
    about = "Entropy-native Linux filesystem: persist irreducible state, materialize structure, preserve exact bytes",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new filesystem store.
    Mkfs(cli::mkfs::MkfsArgs),
    /// Mount a store (FUSE daemon; runs in the foreground).
    Mount(cli::mount::MountArgs),
    /// Unmount a mountpoint.
    Unmount(cli::unmount::UnmountArgs),
    /// Store status and accounting.
    Status(cli::status::StatusArgs),
    /// Per-extent representation detail for a file.
    Inspect(cli::inspect::InspectArgs),
    /// Full representation breakdown of a file.
    Explain(cli::explain::ExplainArgs),
    /// Create a snapshot.
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,
    },
    /// List snapshots.
    Snapshots(cli::snapshot::SnapshotsArgs),
    /// Independent filesystem check.
    Fsck(cli::fsck::FsckArgs),
    /// Deep scrub (full materialized verification).
    Scrub(cli::fsck::ScrubArgs),
    /// Reachability garbage collection.
    Gc(cli::gc::GcArgs),
    /// Foreground re-encoding pass.
    Optimize(cli::optimize::OptimizeArgs),
    /// Reproducible write/read benchmark.
    Benchmark(cli::benchmark::BenchmarkArgs),
    /// Compiled-in capabilities and environment.
    Capabilities,
}

#[derive(Subcommand)]
enum SnapshotAction {
    /// Create a snapshot of the current root.
    Create(cli::snapshot::SnapshotCreateArgs),
    /// Delete a snapshot.
    Delete(cli::snapshot::SnapshotDeleteArgs),
    /// Restore (roll back to) a snapshot.
    Restore(cli::snapshot::SnapshotRestoreArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Mkfs(a) => cli::mkfs::run(a),
        Command::Mount(a) => cli::mount::run(a),
        Command::Unmount(a) => cli::unmount::run(a),
        Command::Status(a) => cli::status::run(a),
        Command::Inspect(a) => cli::inspect::run(a),
        Command::Explain(a) => cli::explain::run(a),
        Command::Snapshot { action } => match action {
            SnapshotAction::Create(a) => cli::snapshot::run_create(a),
            SnapshotAction::Delete(a) => cli::snapshot::run_delete(a),
            SnapshotAction::Restore(a) => cli::snapshot::run_restore(a),
        },
        Command::Snapshots(a) => cli::snapshot::run_list(a),
        Command::Fsck(a) => cli::fsck::run_fsck(a),
        Command::Scrub(a) => cli::fsck::run_scrub(a),
        Command::Gc(a) => cli::gc::run(a),
        Command::Optimize(a) => cli::optimize::run(a),
        Command::Benchmark(a) => cli::benchmark::run(a),
        Command::Capabilities => cli::capabilities::run(),
    };
    if let Err(e) = result {
        eprintln!("entropyfs: {e}");
        std::process::exit(1);
    }
}
