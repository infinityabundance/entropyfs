//! `entropyfs mkfs <store-dir>`: create a new filesystem store.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::store::{Store, StoreConfig};

/// Options for mkfs.
#[derive(Debug, Clone, clap::Args)]
pub struct MkfsArgs {
    /// Store directory (must be empty or nonexistent).
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// Segment size in bytes.
    #[arg(long, default_value_t = 128 * 1024 * 1024)]
    pub segment_size: u64,
    /// Explicit filesystem UUID (16 hex bytes; default: random).
    #[arg(long)]
    pub uuid: Option<String>,
    /// Phase-10F storage transport (sync reference path | uring).
    #[arg(long, value_name = "BACKEND", default_value = "sync")]
    pub io_backend: String,
    /// io_uring submission queue capacity (UringIo only).
    #[arg(long, default_value_t = 256)]
    pub io_uring_entries: u32,
}

/// Run mkfs.
pub fn run(args: &MkfsArgs) -> Result<(), String> {
    let uuid = match &args.uuid {
        Some(s) => {
            let bytes = hex_decode(s)?;
            if bytes.len() != 16 {
                return Err("uuid must be exactly 16 bytes (32 hex digits)".into());
            }
            let mut u = [0u8; 16];
            u.copy_from_slice(&bytes);
            u
        }
        None => {
            // Random UUID from the system RNG (std; metadata only).
            let mut u = [0u8; 16];
            getrandom_fill(&mut u);
            u
        }
    };
    let config = StoreConfig {
        segment_size: args.segment_size,
        io_backend: crate::store::io::IoBackendKind::parse(&args.io_backend)?,
        io_uring_entries: args.io_uring_entries,
        ..Default::default()
    };
    Store::create(&args.store, &config, uuid)
        .map_err(|e| crate::cli::errors::transport(config.io_backend, &e))?;
    println!(
        "created entropyfs store at {} (uuid {})",
        args.store.display(),
        hex_encode(&uuid)
    );
    Ok(())
}

/// Hex-decode (accepts optional 0x prefix).
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err("hex string must have even length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi =
            hex_val(bytes[i]).ok_or_else(|| format!("invalid hex digit {}", bytes[i] as char))?;
        let lo = hex_val(bytes[i + 1])
            .ok_or_else(|| format!("invalid hex digit {}", bytes[i + 1] as char))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Hex-encode.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Fill bytes from the OS entropy source (std `getrandom` via `/dev/urandom`).
fn getrandom_fill(buf: &mut [u8]) {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").expect("urandom");
    f.read_exact(buf).expect("urandom read");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let s = "0x00112233445566778899aabbccddeeff";
        let bytes = hex_decode(s).unwrap();
        assert_eq!(bytes.len(), 16);
        assert_eq!(hex_encode(&bytes), "00112233445566778899aabbccddeeff");
        assert!(hex_decode("xyz").is_err());
        assert!(hex_decode("abc").is_err());
    }
}
