use std::sync::Arc;
use tracing_subscriber;

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

/// Integration-style test that executes the Ethereum range zk program using RPC endpoints
/// specified in the workspace `.env` file.
///
/// This test mirrors `multi.rs` but explicitly loads `.env` from the workspace root to make
/// it easy to test with the provided RPC endpoints.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_range_with_workspace_env() -> Result<()> {
    // Initialize logger once
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer() // ensures compatibility with `cargo test`
        .try_init();

    // Locate the workspace root and load `env.env` from there.
    let metadata = MetadataCommand::new().exec().unwrap();
    let env_path = metadata.workspace_root.join(".env");
    dotenv::from_path(&env_path)?;

    let data_fetcher = OPSuccinctDataFetcher::new_with_rollup_config().await?;

    // Fixed L2 block range as requested.
    let l2_start_block: u64 = 245341;
    let l2_end_block: u64 = 245342;

    let host = initialize_host(Arc::new(data_fetcher.clone()));
    let host_args = host.fetch(l2_start_block, l2_end_block, None, false).await?;

    let witness_data = host.run(&host_args).await?;

    // Prepare SP1 stdin and execute the program natively for cycle/gas report (no proving).
    let sp1_stdin = host.witness_generator().get_sp1_stdin(witness_data)?;
    let (block_data, report, execution_duration) =
        execute_multi(&data_fetcher, sp1_stdin, l2_start_block, l2_end_block).await?;

    let stats = ExecutionStats::new(0, &block_data, &report, 0, execution_duration.as_secs());
    println!("Execution Stats:\n{:?}", stats.to_string());

    Ok(())
}

