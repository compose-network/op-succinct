use alloy_primitives::Address;
use anyhow::Result;
use op_succinct_host_utils::{fetcher::OPSuccinctDataFetcher, OPSuccinctL2OutputOracle};
use crate::DriverDBClient;

/// Get the latest proposed block number from the database.
/// 
/// SSV: Instead of querying the L2OutputOracle contract, we query the database
/// for the latest aggregation proof that has been relayed (status = Relayed).
/// This allows us to track the latest proposed block number via the shared publisher.
pub async fn get_latest_proposed_block_number(
    address: Address,
    fetcher: &OPSuccinctDataFetcher,
) -> Result<u64> {
    // Query the database for the latest relayed aggregation proof
    let db_client = DriverDBClient::new(&std::env::var("DATABASE_URL")?).await?;
    
    let end_block = db_client.get_latest_relayed_block_number().await?
        .unwrap_or(0);
    
    Ok(end_block as u64)

    // LEGACY: Old implementation that queried the L2OutputOracle contract directly
    // This has been replaced with database queries to support the shared publisher model.
    // 
    // let l2_output_oracle = OPSuccinctL2OutputOracle::new(address, fetcher.l1_provider.clone());
    // let block_number = l2_output_oracle.latestBlockNumber().call().await?;
    // let block_number = block_number.to::<u64>();
    // Ok(block_number)
}
