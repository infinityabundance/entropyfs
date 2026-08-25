//! `entropyfs unmount <mountpoint>`: unmount via fusermount3.

#![forbid(unsafe_code)]

use std::path::PathBuf;

/// Options for unmount.
#[derive(Debug, Clone, clap::Args)]
pub struct UnmountArgs {
    /// Mountpoint.
    #[arg(value_name = "MOUNTPOINT")]
    pub mountpoint: PathBuf,
}

/// Run unmount.
pub fn run(args: &UnmountArgs) -> Result<(), String> {
    crate::fuse::mount::unmount(&args.mountpoint)?;
    println!("unmounted {}", args.mountpoint.display());
    Ok(())
}
