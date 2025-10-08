use std::sync::Arc;
use tracing::info;
mod common;
// tracing_subscriber is not needed when using custom formatter.

use anyhow::Result;
use cargo_metadata::MetadataCommand;
use op_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher,
    host::OPSuccinctHost,
    stats::ExecutionStats,
    witness_generation::WitnessGenerator,
};
use op_succinct_proof_utils::initialize_host;
use op_succinct_prove::execute_multi;

// Usage example:
// L2_START_BLOCK=517970 L2_RANGE=1 cargo test -p op-succinct-prove --test range_ethereum_env -- --nocapture

/// Integration-style test that executes the Ethereum range zk program using RPC endpoints
/// specified in the workspace `.env` file.
///
/// This test mirrors `multi.rs` but explicitly loads `.env` from the workspace root to make
/// it easy to test with the provided RPC endpoints.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_range_with_workspace_env() -> Result<()> {
    // Initialize logger once
    common::color_logs::init_pretty_color_logs();

    // Locate the workspace root and load `.env` from there.
    let metadata = MetadataCommand::new().exec()?;
    let env_path = metadata.workspace_root.join(".env");
    info!("Loading environment variables from {:?}", env_path);
    dotenv::from_path(&env_path)?;

    let data_fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;
    // Json pretty print the rollup config for visibility.
    info!("Fetched rollup config and stored in file: {:?}. Config:\n{}",
        data_fetcher.rollup_config_path.clone().unwrap(),
        serde_json::to_string_pretty(&data_fetcher.rollup_config)?);

    // Get starting L2 block and a range (minimum 1), where end = start + range.
    // Allow overriding via env vars; fall back to defaults if not provided.
    let l2_start_block: u64 = std::env::var("L2_START_BLOCK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(517970);
    let l2_range: u64 = std::env::var("L2_RANGE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    if l2_range < 1 {
        panic!("L2_RANGE must be at least 1");
    }
    let l2_end_block: u64 = l2_start_block + l2_range;
    info!("Proving range: from {} to {} (range = {})", l2_start_block, l2_end_block, l2_range);


    info!("Fetching host args");
    let host = initialize_host(Arc::new(data_fetcher.clone()));
    let host_args = host.fetch(l2_start_block, l2_end_block, None, true).await?;


    info!("Running host with fetched arguments");
    let witness_data = host.run(&host_args).await?;

    // Prepare SP1 stdin and execute the program natively for cycle/gas report (no proving).
    info!("Preparing SP1 stdin and executing the program...");
    let sp1_stdin = host.witness_generator().get_sp1_stdin(witness_data)?;
    
    info!("Executing the program");
    let (block_data, report, execution_duration) =
        execute_multi(&data_fetcher, sp1_stdin, l2_start_block, l2_end_block).await?;

    let stats = ExecutionStats::new(0, &block_data, &report, 0, execution_duration.as_secs());
    info!("Execution Stats: {}", stats.to_string());

    Ok(())
}
