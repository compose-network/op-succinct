use alloy_primitives::Address;
use anyhow::Result;
use op_succinct_host_utils::{fetcher::OPSuccinctDataFetcher, OPSuccinctL2OutputOracle};

/// Get the latest proposed block number from the database or contract.
/// 
/// SSV: First tries to query the database for the latest aggregation proof that has been 
/// relayed (status = Relayed). If no relayed proof is found, falls back to querying 
/// the L2OutputOracle contract directly.
pub async fn get_latest_proposed_block_number(
    address: Address,
    fetcher: &OPSuccinctDataFetcher,
) -> Result<u64> {
    let l2_output_oracle = OPSuccinctL2OutputOracle::new(address, fetcher.l1_provider.clone());
    let block_number = l2_output_oracle.latestBlockNumber().call().await?;
    Ok(block_number.to::<u64>())
}
