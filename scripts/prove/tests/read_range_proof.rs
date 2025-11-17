use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use cargo_metadata::MetadataCommand;
use op_succinct_client_utils::boot::BootInfoStruct;
use sp1_sdk::{SP1ProofWithPublicValues};
use tracing::{info, warn};

// Simple helper to hex-encode a byte slice (lowercase, no 0x prefix).
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[test]
/// Read a range artifact file and print its parsed contents.
/// Usage: FNAME=520300 cargo test -p op-succinct-prove --test read_range_proof -- --nocapture
/// Note, a real proof file is needed (either from prover network or generated locally).
fn read_and_print_range_proof() -> Result<()> {
    // Locate the workspace root to build a stable path to the data folder.
    let metadata = MetadataCommand::new().exec()?;
    let data_dir: PathBuf = metadata.workspace_root.join("scripts/prove/data/range_proofs").into();

    // Artifact name via env
    let artifact_fname = std::env::var("FNAME").ok();
    // Artifact path is data_dir appended with artifact_fname
    let artifact_path: PathBuf = match artifact_fname {
        Some(fname) => data_dir.join(fname),
        None => {
            panic!("FNAME not set. Set it with the file name in the range_proofs folder");
        }
    };

    info!("Range proof path: {}", artifact_path.display());

    // Try parsing as an SP1 proof with public values (range program output).
    // Strategy:
    // 1) Try SP1ProofWithPublicValues::load(path)
    // 2) If that fails, read bytes and try bincode::deserialize (compressed proofs we store in DB)
    let mut parsed: SP1ProofWithPublicValues = match SP1ProofWithPublicValues::load(&artifact_path) {
        Ok(p) => p,
        Err(_) => {
            warn!("Failed to load SP1ProofWithPublicValues. Trying bincode deserialization.");
            let mut file = fs::File::open(&artifact_path)?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            bincode::deserialize(&buf)
                .map_err(|e| anyhow!("Failed to parse artifact as SP1 proof: {e}"))?
        }
    };

    println!("Parsed SP1 proof type: {:?}", parsed.proof);

    // For the range program, public values encode BootInfoStruct
    let boot_info: BootInfoStruct = parsed.public_values.read();
    println!("BootInfoStruct:");
    println!("  l1Head         = 0x{}", to_hex(boot_info.l1Head.as_slice()));
    println!("  l2PreRoot      = 0x{}", to_hex(boot_info.l2PreRoot.as_slice()));
    println!("  l2PostRoot     = 0x{}", to_hex(boot_info.l2PostRoot.as_slice()));
    println!("  l2BlockNumber  = {}", boot_info.l2BlockNumber);
    println!("  rollupConfig   = 0x{}", to_hex(boot_info.rollupConfigHash.as_slice()));
    println!("  mailboxRoot    = 0x{}", to_hex(boot_info.mailboxRoot.as_slice()));

    Ok(())
}
