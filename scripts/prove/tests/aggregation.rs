use std::{fs, path::PathBuf};

use anyhow::{anyhow, Result};
use alloy_primitives::Address;
use cargo_metadata::MetadataCommand;
use op_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher,
    get_agg_proof_stdin,
};
use op_succinct_proof_utils::get_range_elf_embedded;
use op_succinct_elfs::AGGREGATION_ELF;
use sp1_sdk::{
    Prover, ProverClient, SP1ProofWithPublicValues,
};
use tracing::info;
use op_succinct_client_utils::boot::BootInfoStruct;
use op_succinct_host_utils::fetcher::BlockInfo;
use op_succinct_host_utils::stats::ExecutionStats;

mod common;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
/// Execute aggregation using range proofs
/// Usage: AGG_PROOFS=521200,521500,521800 cargo test -p op-succinct-prove --test aggregation -- --nocapture
async fn execute_aggregation_with_workspace_env() -> Result<()> {
    common::color_logs::init_pretty_color_logs();

    // Load workspace .env for RPCs
    let metadata = MetadataCommand::new().exec()?;
    let env_path = metadata.workspace_root.join(".env");
    info!("Loading environment variables from {:?}", env_path);
    dotenv::from_path(&env_path)?;

    // Candidate directories for range proofs
    let scripts_data: PathBuf = metadata.workspace_root.join("scripts/prove/data/range_proofs/").into();

    // Get l2_block_numbers list of strings provided by env var AGG_PROOFS
    let l2_block_numbers: Option<Vec<String>> = std::env::var("AGG_PROOFS")
        .ok()
        .map(|s| s.split(',').map(|s| s.trim().to_string()).collect());
    

    let mut proofs: Vec<SP1ProofWithPublicValues> = Vec::new();
    if let Some(nums) = l2_block_numbers {
        for num in nums {
            // [num] indicates the artifact in /scripts/prove/data/artifact_[num]
            let path: PathBuf = scripts_data.join(num);
            match SP1ProofWithPublicValues::load(&path) {
                Ok(pp) => proofs.push(pp),
                Err(_) => {
                    let buf = fs::read(&path)?;
                    proofs.push(bincode::deserialize(&buf)?);
                }
            }
        }
    } else {
        // Raise error if none was provided
        panic!("No override paths provided!");
    }

    // Require at least 2 range proofs to aggregate; skip gracefully otherwise.
    if proofs.len() < 2 {
        println!("Found {} range proof(s). Need >=2 for aggregation. Set AGG_PROOFS to override.", proofs.len());
        return Ok(());
    }

    info!("Got {} range proofs to aggregate", proofs.len());

    info!("Setting up range prover to get verification key");
    // Initialize prover and range vkey, verify proofs, and collect data
    let prover = ProverClient::builder().cpu().build();
    let (_, vkey) = prover.setup(get_range_elf_embedded());

    info!("Iterating through range proofs");
    let mut boot_infos = Vec::with_capacity(proofs.len());
    let mut inner_proofs = Vec::with_capacity(proofs.len());
    let mut block_info_list: Vec<BlockInfo>  = vec![];
    for mut p in proofs {
        // Verify against range vkey
        prover.verify(&p, &vkey).map_err(|e| anyhow!("range proof verification failed: {e}"))?;
        // Extract public values (BootInfoStruct) and inner proof
        let boot_info: BootInfoStruct = p.public_values.read();
        let l2_block_number = boot_info.l2BlockNumber;
        boot_infos.push(boot_info);
        inner_proofs.push(p.proof);
        block_info_list.push(BlockInfo{
            block_number: l2_block_number,
            transaction_count: 1,
            gas_used: 1,
            total_l1_fees: 1,
            total_tx_fees: 1,
        });
    }

    // Build aggregation stdin
    info!("Building stdin for aggregation program");
    let fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    let latest_header = fetcher.get_latest_l1_head_in_batch(&boot_infos).await?;
    let headers = fetcher.get_header_preimages(&boot_infos, latest_header.hash_slow()).await?;

    // Prover address for outputs; default to zero if not provided
    let prover_address = std::env::var("PROVER_ADDRESS")
        .ok()
        .and_then(|s| s.parse::<Address>().ok())
        .unwrap_or(Address::ZERO);

    let stdin = get_agg_proof_stdin(
        inner_proofs,
        boot_infos,
        headers,
        &vkey,
        latest_header.hash_slow(),
        prover_address,
    )?;

    // Execute aggregation program to get report
    info!("Setting up aggregation program prover");
    let (_pk, _vk) = prover.setup(AGGREGATION_ELF);
    let (_out, report) = prover
        .execute(AGGREGATION_ELF, &stdin)
        .calculate_gas(true)
        .run()
        .unwrap();

    let stats = ExecutionStats::new(0, &block_info_list, &report, 0, 0);
    info!("Aggregation execute report: {}", stats.to_string());

    Ok(())
}

