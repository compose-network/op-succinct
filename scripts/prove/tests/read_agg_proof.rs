use std::fs;
use std::path::{PathBuf};

use anyhow::{anyhow, Result};
use cargo_metadata::MetadataCommand;
use op_succinct_client_utils::{types::AggregationOutputs, AGGREGATION_OUTPUTS_SIZE};
use alloy_sol_types::SolValue;
use sp1_sdk::{SP1ProofWithPublicValues};
use tracing::info;

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
/// Read an agg proof file and print its parsed contents.
/// Usage: RUST_LOG=info FNAME=521800_a cargo test -p op-succinct-prove --test read_agg_proof -- --nocapture
/// Note, a real proof file is needed (either from prover network or generated locally).
fn read_and_print_agg_proof() -> Result<()> {
    // Initialize logging for tracing::info! macros in tests.
    let _ = tracing_subscriber::fmt::try_init();
    let metadata = MetadataCommand::new().exec()?;
    let data_dir: PathBuf = metadata.workspace_root.join("scripts/prove/data/agg_proofs").into();

    // Prefer explicit env for aggregation artifact.
    let fname = std::env::var("FNAME").ok();
    // Artifact path is data_fir appended with fname
    let artifact_path:PathBuf = match fname {
        Some(p) => data_dir.join(p),
        None => {
            panic!("FNAME not set. Set it with the file name in the agg_proofs folder");
        }
    };

    info!("Aggregation proof path: {}", artifact_path.display());

    // Try to parse as SP1 proof with public values. First try load(); fallback to bincode.
    let mut parsed: SP1ProofWithPublicValues = match SP1ProofWithPublicValues::load(&artifact_path) {
        Ok(p) => p,
        Err(_) => {
            let buf = fs::read(&artifact_path)?;
            bincode::deserialize(&buf)
                .map_err(|e| anyhow!("Failed to parse aggregation artifact as SP1 proof: {e}"))?
        }
    };

    info!("Parsed SP1 proof type: {:?}", parsed.proof);

    // Parse AggregationOutputs from public values (ABI-encoded, 8 * 32 bytes)
    let mut raw_outputs = [0u8; AGGREGATION_OUTPUTS_SIZE];
    parsed.public_values.read_slice(&mut raw_outputs);
    let agg_outputs = AggregationOutputs::abi_decode(&raw_outputs)
        .map_err(|e| anyhow!("Failed to ABI-decode AggregationOutputs: {e}"))?;

    info!("AggregationOutputs:");
    info!("  l1Head         = 0x{}", to_hex(agg_outputs.l1Head.as_slice()));
    info!("  l2PreRoot      = 0x{}", to_hex(agg_outputs.l2PreRoot.as_slice()));
    info!("  l2PostRoot     = 0x{}", to_hex(agg_outputs.l2PostRoot.as_slice()));
    info!("  l2BlockNumber  = {}", agg_outputs.l2BlockNumber);
    info!("  rollupConfig   = 0x{}", to_hex(agg_outputs.rollupConfigHash.as_slice()));
    info!("  mailboxRoot    = 0x{}", to_hex(agg_outputs.mailboxRoot.as_slice()));
    info!("  multiBlockVKey = 0x{}", to_hex(agg_outputs.multiBlockVKey.as_slice()));
    info!("  proverAddress  = 0x{}", to_hex(agg_outputs.proverAddress.as_slice()));

    Ok(())
}
