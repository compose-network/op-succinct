use std::{collections::HashMap, env, str::FromStr, sync::Arc, time::Duration};

use alloy_eips::BlockId;
use alloy_primitives::{B256, U256};
use alloy_provider::{Provider};
use anyhow::{anyhow, Context, Result};
use futures_util::{stream, StreamExt, TryStreamExt};
use op_succinct_client_utils::{boot::hash_rollup_config, types::u32_to_u8};

use op_succinct_elfs::AGGREGATION_ELF;
use op_succinct_host_utils::{
    fetcher::OPSuccinctDataFetcher, host::OPSuccinctHost, metrics::MetricsGauge,
    DisputeGameFactory::DisputeGameFactoryInstance as DisputeGameFactoryContract,
    OPSuccinctL2OutputOracle::OPSuccinctL2OutputOracleInstance as OPSuccinctL2OOContract,
};
use op_succinct_proof_utils::get_range_elf_embedded;
use op_succinct_signer_utils::Signer;
use sp1_sdk::{
    network::proto::types::{ExecutionStatus, FulfillmentStatus},
    HashableKey, NetworkProver, NetworkSigner, Prover, ProverClient, SP1Proof,
    SP1ProofWithPublicValues,
};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::{
    db::{DriverDBClient, OPSuccinctRequest, RequestMode, RequestStatus, RequestType},
    find_gaps, get_latest_proposed_block_number, get_ranges_to_prove, CommitmentConfig,
    ContractConfig, OPSuccinctProofRequester, ProgramConfig, RequesterConfig, ValidityGauge,
};
use crate::publisher::{build_aggregation_outputs, submit_to_publisher};
use op_succinct_client_utils::boot::MailboxInfoStruct;

/// Configuration for the driver.
pub struct DriverConfig {
    pub network_prover: Arc<NetworkProver>,
    pub fetcher: Arc<OPSuccinctDataFetcher>,
    pub driver_db_client: Arc<DriverDBClient>,
    pub signer: Signer,
    pub loop_interval: u64,
    pub publisher_url: Option<reqwest::Url>,
}
/// Type alias for a map of task IDs to their join handles and associated requests
pub type TaskMap = HashMap<i64, (tokio::task::JoinHandle<Result<()>>, OPSuccinctRequest)>;

pub struct Proposer<P, H: OPSuccinctHost>
where
    P: Provider + 'static,
{
    driver_config: DriverConfig,
    contract_config: ContractConfig<P>,
    program_config: ProgramConfig,
    requester_config: RequesterConfig,
    proof_requester: Arc<OPSuccinctProofRequester<H>>,
    tasks: Arc<Mutex<TaskMap>>,
    // Simple counters to limit how many requests we send when SINGLE_SHOT is enabled
    range_requests_sent: usize,
    agg_requests_sent: usize,
}

impl<P, H: OPSuccinctHost> Proposer<P, H>
where
    P: Provider + 'static + Clone,
{
    pub async fn new(
        provider: P,
        db_client: Arc<DriverDBClient>,
        fetcher: Arc<OPSuccinctDataFetcher>,
        requester_config: RequesterConfig,
        signer: Signer,
        loop_interval: u64,
        host: Arc<H>,
    ) -> Result<Self> {
        // This check prevents users from running multiple proposers for the same chain at the same
        // time.
        let is_locked = db_client
            .is_chain_locked(
                requester_config.l1_chain_id,
                requester_config.l2_chain_id,
                Duration::from_secs(loop_interval),
            )
            .await?;
        if is_locked {
            return Err(anyhow!(
                "There is another proposer for the same chain connected to the database. Only one proposer can be connected to the database for a chain at a time."
            ));
        }

        // Add the chain lock to the database.
        db_client
            .add_chain_lock(requester_config.l1_chain_id, requester_config.l2_chain_id)
            .await?;

        // Set up the network prover.
        let network_prover = if requester_config.use_kms_requester {
            // If using KMS, NETWORK_PRIVATE_KEY should be a KMS key ARN.
            let kms_key_arn = env::var("NETWORK_PRIVATE_KEY")
                .context("NETWORK_PRIVATE_KEY must be set when USE_KMS_REQUESTER is true")?;
            let signer = NetworkSigner::aws_kms(&kms_key_arn).await?;
            tracing::info!("Using KMS requester with address: {:?}", signer.address());
            Arc::new(ProverClient::builder().network().signer(signer).build())
        } else {
            // Otherwise, use a private key with a default value to avoid errors in mock mode.
            let private_key = env::var("NETWORK_PRIVATE_KEY").unwrap_or_else(|_| {
                tracing::warn!(
                    "Using default NETWORK_PRIVATE_KEY of 0x01. This is only valid in mock mode."
                );
                "0x0000000000000000000000000000000000000000000000000000000000000001".to_string()
            });
            let signer = NetworkSigner::local(&private_key)?;
            tracing::info!("Using local requester with address: {:?}", signer.address());
            Arc::new(ProverClient::builder().network().signer(signer).build())
        };

        let (range_pk, range_vk) = network_prover.setup(get_range_elf_embedded());

        let (agg_pk, agg_vk) = network_prover.setup(AGGREGATION_ELF);
        let multi_block_vkey_u8 = u32_to_u8(range_vk.vk.hash_u32());
        let range_vkey_commitment = B256::from(multi_block_vkey_u8);
        let agg_vkey_hash = B256::from_str(&agg_vk.bytes32()).unwrap();

        // Initialize fetcher
        let rollup_config_hash = hash_rollup_config(fetcher.rollup_config.as_ref().unwrap());

        let program_config = ProgramConfig {
            range_vk: Arc::new(range_vk),
            range_pk: Arc::new(range_pk),
            agg_vk: Arc::new(agg_vk),
            agg_pk: Arc::new(agg_pk),
            commitments: CommitmentConfig {
                range_vkey_commitment,
                agg_vkey_hash,
                rollup_config_hash,
            },
        };

        // Initialize the proof requester.
        let proof_requester = Arc::new(OPSuccinctProofRequester::new(
            host,
            network_prover.clone(),
            fetcher.clone(),
            db_client.clone(),
            program_config.clone(),
            requester_config.mock,
            requester_config.range_proof_strategy,
            requester_config.agg_proof_strategy,
            requester_config.agg_proof_mode,
            requester_config.safe_db_fallback,
            requester_config.max_price_per_pgu,
            requester_config.timeout,
            requester_config.range_cycle_limit,
            requester_config.range_gas_limit,
            requester_config.agg_cycle_limit,
            requester_config.agg_gas_limit,
        ));

        let l2oo_contract =
            OPSuccinctL2OOContract::new(requester_config.l2oo_address, provider.clone());

        let dgf_contract =
            DisputeGameFactoryContract::new(requester_config.dgf_address, provider.clone());

        // Read optional publisher URL directly from env to keep API stable.
        let publisher_url = std::env::var("SHARED_PUBLISHER_URL").ok().and_then(|s| reqwest::Url::parse(&s).ok());

        let proposer = Proposer {
            driver_config: DriverConfig {
                network_prover,
                fetcher,
                driver_db_client: db_client,
                signer,
                loop_interval,
                publisher_url,
            },
            contract_config: ContractConfig {
                l2oo_address: requester_config.l2oo_address,
                dgf_address: requester_config.dgf_address,
                l2oo_contract,
                dgf_contract,
            },
            program_config,
            requester_config,
            proof_requester,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            range_requests_sent: 0,
            agg_requests_sent: 0,
        };
        Ok(proposer)
    }

    /// Use the in-memory index of the highest block number to add new ranges to the database.
    #[tracing::instrument(name = "proposer.add_new_ranges", skip(self))]
    pub async fn add_new_ranges(&self) -> Result<()> {
        // Get the latest proposed block number on the contract.
        let mut latest_proposed_block_number = get_latest_proposed_block_number(
            self.contract_config.l2oo_address,
            self.driver_config.fetcher.as_ref(),
        )
        .await?;

        // Enforce minimum start block if configured.
        latest_proposed_block_number = latest_proposed_block_number.max(self.requester_config.min_l2_block);

        let finalized_block_number = match self
            .proof_requester
            .host
            .get_finalized_l2_block_number(
                self.driver_config.fetcher.as_ref(),
                latest_proposed_block_number,
            )
            .await?
        {
            Some(block_number) => {
                info!("Found finalized block number: {}", block_number);
                block_number
            }
            None => {
                info!("No new finalized block number found since last proposed block. No new range proof requests will be added.");
                return Ok(());
            }
        };

        // Get all active (non-failed) requests with the same commitment config and start block >=
        // latest_proposed_block_number. These requests are non-overlapping.
        let mut requests = self
            .driver_config
            .driver_db_client
            .fetch_ranges_after_block(
                &[
                    RequestStatus::Unrequested,
                    RequestStatus::WitnessGeneration,
                    RequestStatus::Execution,
                    RequestStatus::Prove,
                    RequestStatus::Complete,
                ],
                latest_proposed_block_number as i64,
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        // Sort the requests by start block.
        requests.sort_by_key(|r| r.0);

        let disjoint_ranges = find_gaps(
            latest_proposed_block_number as i64,
            finalized_block_number as i64,
            &requests,
        );

        let ranges_to_prove = get_ranges_to_prove(
            &disjoint_ranges,
            self.requester_config.range_proof_interval as i64,
        );

        if !ranges_to_prove.is_empty() {
            info!("Inserting {} range proof requests into the database.", ranges_to_prove.len());

            // Create range proof requests for the ranges to prove in parallel
            let new_range_requests = stream::iter(ranges_to_prove)
                .map(|range| {
                    let mode = if self.requester_config.mock {
                        RequestMode::Mock
                    } else {
                        RequestMode::Real
                    };
                    OPSuccinctRequest::create_range_request(
                        mode,
                        range.0,
                        range.1,
                        self.program_config.commitments.range_vkey_commitment,
                        self.program_config.commitments.rollup_config_hash,
                        self.requester_config.l1_chain_id,
                        self.requester_config.l2_chain_id,
                        self.driver_config.fetcher.clone(),
                    )
                })
                .buffered(10) // Do 10 at a time, otherwise it's too slow when fetching the block range data.
                .try_collect::<Vec<OPSuccinctRequest>>()
                .await?;

            // Insert the new range proof requests into the database.
            self.driver_config.driver_db_client.insert_requests(&new_range_requests).await?;

            // Log details for each created range proof request.
            for request in &new_range_requests {
                info!(
                    start_block = request.start_block,
                    end_block = request.end_block,
                    "Range proof request created and inserted into database"
                );
            }
        }

        Ok(())
    }

    /// Handle all proof requests in the Prove state.
    #[tracing::instrument(name = "proposer.handle_proving_requests", skip(self))]
    pub async fn handle_proving_requests(&self) -> Result<()> {
        // Get all requests from the database.
        let prove_requests = self
            .driver_config
            .driver_db_client
            .fetch_requests_by_status(
                RequestStatus::Prove,
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        // Get the proof status of all of the requests.
        for request in prove_requests {
            self.process_proof_request_status(request).await?;
        }

        Ok(())
    }

    /// Process a single OP Succinct request's proof status.
    #[tracing::instrument(name = "proposer.process_proof_request_status", skip(self, request))]
    pub async fn process_proof_request_status(&self, request: OPSuccinctRequest) -> Result<()> {
        if let Some(proof_request_id) = request.proof_request_id.as_ref() {
            let (status, proof) = self
                .driver_config
                .network_prover
                .get_proof_status(B256::from_slice(proof_request_id))
                .await?;

            // Check if current time exceeds deadline. If so, the proof has timed out.
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if current_time > status.deadline {
                match self
                    .proof_requester
                    .handle_failed_request(request.clone(), status.execution_status())
                    .await
                {
                    Ok(_) => ValidityGauge::ProofRequestRetryCount.increment(1.0),
                    Err(e) => {
                        ValidityGauge::RetryErrorCount.increment(1.0);
                        return Err(e);
                    }
                }

                ValidityGauge::ProofRequestTimeoutErrorCount.increment(1.0);

                // Log timeout of range proof
                match request.req_type {
                    RequestType::Range => {
                        warn!(
                            proof_id = request.id,
                            start_block = request.start_block,
                            end_block = request.end_block,
                            deadline = status.deadline,
                            current_time = current_time,
                            "Range proof request timed out"
                        );
                    }
                    RequestType::Aggregation => {
                        warn!(
                            proof_id = request.id,
                            start_block = request.start_block,
                            end_block = request.end_block,
                            deadline = status.deadline,
                            current_time = current_time,
                            "Aggregation proof request timed out"
                        );
                    }
                }

                return Ok(());
            }

            // If the proof request has been fulfilled, update the request to status Complete and
            // add the proof bytes to the database.
            if status.fulfillment_status() == FulfillmentStatus::Fulfilled {
                let proof: SP1ProofWithPublicValues = proof.unwrap();

                let proof_bytes = match proof.proof {
                    // If it's a compressed proof, serialize with bincode.
                    SP1Proof::Compressed(_) => bincode::serialize(&proof).unwrap(),
                    // If it's Groth16 or PLONK, get the on-chain proof bytes.
                    SP1Proof::Groth16(_) | SP1Proof::Plonk(_) => proof.bytes(),
                    SP1Proof::Core(_) => return Err(anyhow!("Core proofs are not supported.")),
                };

                // Add the completed proof to the database.
                self.driver_config
                    .driver_db_client
                    .update_proof_to_complete(request.id, &proof_bytes)
                    .await?;
                // Update the prove_duration based on the current time and the proof_request_time.
                self.driver_config.driver_db_client.update_prove_duration(request.id).await?;

                // Log completion of range and aggregation proofs.
                match request.req_type {
                    RequestType::Range => {
                        info!(
                            proof_id = request.id,
                            start_block = request.start_block,
                            end_block = request.end_block,
                            proof_request_time = ?request.proof_request_time,
                            total_tx_fees = %request.total_tx_fees,
                            total_transactions = request.total_nb_transactions,
                            witnessgen_duration_s = request.witnessgen_duration,
                            prove_duration_s = request.prove_duration,
                            total_eth_gas_used = request.total_eth_gas_used,
                            total_l1_fees = %request.total_l1_fees,
                            "Range proof completed successfully"
                        );
                    }
                    RequestType::Aggregation => {
                        info!(
                            proof_id = request.id,
                            start_block = request.start_block,
                            end_block = request.end_block,
                            witnessgen_duration_s = request.witnessgen_duration,
                            prove_duration_s = request.prove_duration,
                            "Aggregation proof completed successfully"
                        );
                    }
                }
            } else if status.fulfillment_status() == FulfillmentStatus::Unfulfillable {
                // Log failure of range and aggregation proofs.
                match request.req_type {
                    RequestType::Range => {
                        warn!(
                            proof_id = request.id,
                            start_block = request.start_block,
                            end_block = request.end_block,
                            proof_request_time = ?request.proof_request_time,
                            total_tx_fees = %request.total_tx_fees,
                            total_transactions = request.total_nb_transactions,
                            witnessgen_duration_s = request.witnessgen_duration,
                            total_eth_gas_used = request.total_eth_gas_used,
                            total_l1_fees = %request.total_l1_fees,
                            execution_status = ?status.execution_status(),
                            "Range proof request failed - unfulfillable"
                        );
                    }
                    RequestType::Aggregation => {
                        warn!(
                            proof_id = request.id,
                            start_block = request.start_block,
                            end_block = request.end_block,
                            witnessgen_duration_s = request.witnessgen_duration,
                            execution_status = ?status.execution_status(),
                            "Aggregation proof request failed - unfulfillable"
                        );
                    }
                }

                self.proof_requester
                    .handle_failed_request(request, status.execution_status())
                    .await?;
                ValidityGauge::ProofRequestRetryCount.increment(1.0);
            }
        } else {
            // There should never be a proof request in Prove status without a proof request id.
            tracing::warn!(id = request.id, start_block = request.start_block, end_block = request.end_block, req_type = ?request.req_type, "Request has no proof request id");
        }

        Ok(())
    }

    /// Create aggregation proofs based on the completed range proofs. The range proofs must be
    /// contiguous and have the same range vkey commitment. Assumes that the range proof retry
    /// logic guarantees that there is not two potential contiguous chains of range proofs.
    ///
    /// Only creates an Aggregation proof if there's not an Aggregation proof in progress with the
    /// same start block.
    #[tracing::instrument(name = "proposer.create_aggregation_proofs", skip(self))]
    pub async fn create_aggregation_proofs(&self) -> Result<()> {
        // Check if there's an Aggregation proof with the same start block AND range verification
        // key commitment AND aggregation vkey. If so, return.
        let mut latest_proposed_block_number = get_latest_proposed_block_number(
            self.contract_config.l2oo_address,
            self.driver_config.fetcher.as_ref(),
        )
        .await? as i64;

        // Apply minimum L2 block for aggregation flows
        latest_proposed_block_number = latest_proposed_block_number
            .max(self.requester_config.min_l2_block as i64);


        // Get all active Aggregation proofs with the same start block, range vkey commitment, and
        // aggregation vkey.
        let active_agg_proofs_count = self
            .driver_config
            .driver_db_client
            .fetch_active_agg_proofs_count(
                latest_proposed_block_number as i64,
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        if active_agg_proofs_count > 0 {
            info!("There is already an Aggregation proof queued with the same start block, range vkey commitment, and aggregation vkey.");
            return Ok(());
        }

        // Get the completed range proofs with a start block greater than the latest proposed block
        // number. These blocks are sorted.
        let mut completed_range_proofs = self
            .driver_config
            .driver_db_client
            .fetch_completed_ranges(
                &self.program_config.commitments,
                latest_proposed_block_number as i64,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        // Sort the completed range proofs by start block.
        completed_range_proofs.sort_by_key(|(start_block, _)| *start_block);

        // Get the highest block number of the completed range proofs.
        let highest_proven_contiguous_block_number = match self
            .get_highest_proven_contiguous_block(completed_range_proofs)?
        {
            Some(block) => block,
            None => return Ok(()), /* No completed range proofs contiguous to the latest proposed
                                    * block number, so no need to create an aggregation proof. */
        };

        // Get the submission interval from the contract.
        // let contract_submission_interval: u64 =
        //     self.contract_config.l2oo_contract.submissionInterval().call().await?.to::<u64>();

        // Use the submission interval from the contract if it's greater than the one in the
        // proposer config.
        let submission_interval = (self.requester_config.submission_interval) as i64;

        info!("Submission interval for aggregation proof: {}.", submission_interval);

        // If the highest proven contiguous block number is greater than the latest proposed block
        // number plus the submission interval, create an aggregation proof.
        if (highest_proven_contiguous_block_number - latest_proposed_block_number) >=
            submission_interval
        {
            // If an aggregation request with the same start block and end block and commitment
            // config exists, there's no need to checkpoint the L1 block hash.
            // Use the existing L1 block hash from the existing request.
            let existing_request = self
                .driver_config
                .driver_db_client
                .fetch_failed_agg_request_with_checkpointed_block_hash(
                    latest_proposed_block_number,
                    highest_proven_contiguous_block_number,
                    &self.program_config.commitments,
                    self.requester_config.l1_chain_id,
                    self.requester_config.l2_chain_id,
                )
                .await?;

            // If there's an existing aggregation request with the same start block, end block, and
            // commitment config that has a checkpointed block hash, use the existing L1 block hash
            // and number. This is likely caused by an error generating the aggregation
            // proof, but there's no need to checkpoint the L1 block hash again.
            let (checkpointed_l1_block_hash, checkpointed_l1_block_number) = if let Some(
                existing_request,
            ) = existing_request
            {
                tracing::debug!("Found existing aggregation request with the same start block, end block, and commitment config that has a checkpointed block hash.");
                (B256::from_slice(&existing_request.0), existing_request.1)
            } else {
                // Checkpoint an L1 block hash that will be used to create the aggregation proof.
                let latest_header =
                    self.driver_config.fetcher.get_l1_header(BlockId::latest()).await?;

                // Checkpoint the L1 block hash.
                let transaction_request = self
                    .contract_config
                    .l2oo_contract
                    .checkpointBlockHash(U256::from(latest_header.number))
                    .into_transaction_request();

                let receipt = self
                    .driver_config
                    .signer
                    .send_transaction_request(
                        self.driver_config.fetcher.as_ref().rpc_config.l1_rpc.clone(),
                        transaction_request,
                    )
                    .await?;

                // If transaction reverted, log the error.
                if !receipt.status() {
                    return Err(anyhow!("Checkpoint block transaction reverted: {:?}", receipt));
                }

                tracing::info!("Checkpointed L1 block number: {:?}.", latest_header.number);

                (latest_header.hash_slow(), latest_header.number as i64)
            };

            // Create an aggregation proof request to cover the range with the checkpointed L1 block
            // hash.
            let agg_request = OPSuccinctRequest::new_agg_request(
                if self.requester_config.mock { RequestMode::Mock } else { RequestMode::Real },
                latest_proposed_block_number,
                highest_proven_contiguous_block_number,
                self.program_config.commitments.range_vkey_commitment,
                self.program_config.commitments.agg_vkey_hash,
                self.program_config.commitments.rollup_config_hash,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
                checkpointed_l1_block_number,
                checkpointed_l1_block_hash,
                self.requester_config.prover_address,
            );

            self.driver_config.driver_db_client.insert_request(&agg_request).await?;

            info!(
                start_block = agg_request.start_block,
                end_block = agg_request.end_block,
                "Aggregation proof request created and inserted into database"
            );
        }

        Ok(())
    }

    /// Request all unrequested proofs up to MAX_CONCURRENT_PROOF_REQUESTS. If there are already
    /// MAX_CONCURRENT_PROOF_REQUESTS proofs in WitnessGeneration, Execute, and Prove status,
    /// return. If there are already MAX_CONCURRENT_WITNESS_GEN proofs in WitnessGeneration or
    /// Execute status, return.
    ///
    /// Note: In the future, submit up to MAX_CONCURRENT_PROOF_REQUESTS at a time. Don't do one per
    /// loop.
    #[tracing::instrument(name = "proposer.request_queued_proofs", skip(self))]
    async fn request_queued_proofs(&mut self) -> Result<()> {
        let commitments = self.program_config.commitments.clone();
        let l1_chain_id = self.requester_config.l1_chain_id;
        let l2_chain_id = self.requester_config.l2_chain_id;

        let witness_gen_count = self
            .driver_config
            .driver_db_client
            .fetch_request_count(
                RequestStatus::WitnessGeneration,
                &commitments,
                l1_chain_id,
                l2_chain_id,
            )
            .await?;

        let execution_count = self
            .driver_config
            .driver_db_client
            .fetch_request_count(RequestStatus::Execution, &commitments, l1_chain_id, l2_chain_id)
            .await?;

        let prove_count = self
            .driver_config
            .driver_db_client
            .fetch_request_count(RequestStatus::Prove, &commitments, l1_chain_id, l2_chain_id)
            .await?;

        // If there are already MAX_CONCURRENT_PROOF_REQUESTS proofs in WitnessGeneration, Execute,
        // and Prove status, return.
        if witness_gen_count + execution_count + prove_count >=
            self.requester_config.max_concurrent_proof_requests as i64
        {
            info!("There are already MAX_CONCURRENT_PROOF_REQUESTS proofs in WitnessGeneration, Execute, and Prove status.");
            return Ok(());
        }

        // If there are already MAX_CONCURRENT_WITNESS_GEN proofs in WitnessGeneration status,
        // return.
        if witness_gen_count >= self.requester_config.max_concurrent_witness_gen as i64 {
            info!(
                "There are already MAX_CONCURRENT_WITNESS_GEN proofs in WitnessGeneration status."
            );
            return Ok(());
        }

        if let Some(request) = self.get_next_unrequested_proof().await? {
            info!(
                request_id = request.id,
                request_type = ?request.req_type,
                start_block = request.start_block,
                end_block = request.end_block,
                "Making proof request"
            );
            // In single-shot mode, increment counters to limit subsequent sends.
            if self.requester_config.single_shot {
                match request.req_type {
                    RequestType::Range => {
                        self.range_requests_sent = self.range_requests_sent.saturating_add(1)
                    }
                    RequestType::Aggregation => {
                        self.agg_requests_sent = self.agg_requests_sent.saturating_add(1)
                    }
                }
            }
            let request_clone = request.clone();
            let proof_requester = self.proof_requester.clone();
            let handle =
                tokio::spawn(
                    async move { proof_requester.make_proof_request(request_clone).await },
                );
            self.tasks.lock().await.insert(request.id, (handle, request));
        }

        Ok(())
    }

    /// Get the next unrequested proof from the database.
    ///
    /// If there is an Aggregation proof with the same start block, range vkey commitment, and
    /// aggregation vkey, return that. Otherwise, return a range proof with the lowest start
    /// block.
    async fn get_next_unrequested_proof(&self) -> Result<Option<OPSuccinctRequest>> {
        let mut latest_proposed_block_number = get_latest_proposed_block_number(
            self.contract_config.l2oo_address,
            self.driver_config.fetcher.as_ref(),
        )
        .await?;

        // Respect minimum start block filter when selecting next proof to request.
        latest_proposed_block_number = latest_proposed_block_number
            .max(self.requester_config.min_l2_block);

        // If aggregation is enabled and allowed by single-shot counter, prefer an unrequested aggregation proof.
        let consider_agg = self.requester_config.enable_aggregation
            && (!self.requester_config.single_shot || self.agg_requests_sent < 1);

        let unreq_agg_request = if consider_agg {
            self
                .driver_config
                .driver_db_client
                .fetch_unrequested_agg_proof(
                    latest_proposed_block_number as i64,
                    &self.program_config.commitments,
                    self.requester_config.l1_chain_id,
                    self.requester_config.l2_chain_id,
                )
                .await?
        } else {
            None
        };

        if let Some(unreq_agg_request) = unreq_agg_request {
            // Fetch consecutive range proofs from the database associated with the aggregation
            // proof request.
            let range_proofs = self
                .proof_requester
                .db_client
                .get_consecutive_complete_range_proofs(
                    unreq_agg_request.start_block,
                    unreq_agg_request.end_block,
                    &self.program_config.commitments,
                    self.requester_config.l1_chain_id,
                    self.requester_config.l2_chain_id,
                )
                .await?;

            // Validate the aggregation proof request
            match self.validate_aggregation_request(&range_proofs, &unreq_agg_request).await {
                true => {
                    debug!(
                        "Aggregation request validated successfully: start_block={}, end_block={}",
                        unreq_agg_request.start_block, unreq_agg_request.end_block
                    );
                    return Ok(Some(unreq_agg_request));
                }
                false => {
                    debug!(
                        "Aggregation request validation failed, moving to range proofs: start_block={}, end_block={}",
                        unreq_agg_request.start_block, unreq_agg_request.end_block
                    );
                    ValidityGauge::AggProofValidationErrorCount.increment(1.0);
                    // Validation failed, continue to try fetching range proofs
                }
            }
        }

        // Respect single-shot counter for range requests as well
        let consider_range = !self.requester_config.single_shot || self.range_requests_sent < 1;

        let unreq_range_request = if consider_range {
            self
                .driver_config
                .driver_db_client
                .fetch_first_unrequested_range_proof(
                    latest_proposed_block_number as i64,
                    &self.program_config.commitments,
                    self.requester_config.l1_chain_id,
                    self.requester_config.l2_chain_id,
                )
                .await?
        } else {
            None
        };

        if let Some(unreq_range_request) = unreq_range_request {
            return Ok(Some(unreq_range_request));
        }

        Ok(None)
    }

    /// Validates an aggregation proof request by checking that:
    /// 1. There are no gaps between consecutive range proofs
    /// 2. There are no duplicate/overlapping range proofs
    /// 3. The range proofs cover the entire block range
    pub async fn validate_aggregation_request(
        &self,
        range_proofs: &[OPSuccinctRequest],
        agg_request: &OPSuccinctRequest,
    ) -> bool {
        debug!(
            "Validating aggregation proof request: start_block={}, end_block={}",
            agg_request.start_block, agg_request.end_block
        );

        // Log all constituent range proofs
        for (i, proof) in range_proofs.iter().enumerate() {
            debug!(
                "Range proof {}: start_block={}, end_block={}",
                i, proof.start_block, proof.end_block
            );
        }

        // If no range proofs found, validation fails
        if range_proofs.is_empty() {
            warn!(
                start_block = ?agg_request.start_block,
                end_block = ?agg_request.end_block,
                commitments = ?self.program_config.commitments,
                "No consecutive span proof range found for request"
            );
            return false;
        }

        let first_range_proof_request =
            range_proofs.first().expect("Range proofs should not be empty");

        let last_range_proof_request =
            range_proofs.last().expect("Range proofs should not be empty");

        if first_range_proof_request.start_block != agg_request.start_block {
            warn!(
                expected_start_block = ?agg_request.start_block,
                actual_start_block = ?first_range_proof_request.start_block,
                commitments = ?self.program_config.commitments,
                "Range proofs start block does not match aggregation request"
            );

            return false;
        }

        if last_range_proof_request.end_block != agg_request.end_block {
            warn!(
                expected_end_block = ?agg_request.end_block,
                actual_end_block = ?last_range_proof_request.end_block,
                commitments = ?self.program_config.commitments,
                "Range proofs end block does not match aggregation request"
            );
            return false;
        }

        // Check for gaps and duplicates / overlaps between consecutive proofs
        for i in 1..range_proofs.len() {
            let prev_proof = &range_proofs[i - 1];
            let curr_proof = &range_proofs[i];

            // Check for gap
            if prev_proof.end_block != curr_proof.start_block {
                debug!(
                    "Gap detected: proof {} ends at {} but proof {} starts at {}",
                    i - 1,
                    prev_proof.end_block,
                    i,
                    curr_proof.start_block
                );
                return false;
            }

            // Check for overlap (duplicate blocks)
            if prev_proof.end_block > curr_proof.start_block {
                debug!(
                    "Overlap detected: proof {} ends at {} but proof {} starts at {}",
                    i - 1,
                    prev_proof.end_block,
                    i,
                    curr_proof.start_block
                );
                return false;
            }
        }

        // All validation checks passed
        debug!(
            "Aggregation request validated successfully with {} consecutive range proofs",
            range_proofs.len()
        );
        true
    }

    /// Relay all completed aggregation proofs to the contract.
    #[tracing::instrument(name = "proposer.submit_agg_proofs", skip(self))]
    async fn submit_agg_proofs(&self) -> Result<()> {
        let mut latest_proposed_block_number = get_latest_proposed_block_number(
            self.contract_config.l2oo_address,
            self.driver_config.fetcher.as_ref(),
        )
        .await?;

        // Respect minimum L2 block for aggregation flows
        latest_proposed_block_number = latest_proposed_block_number
            .max(self.requester_config.min_l2_block);

        // See if there is an aggregation proof that is complete for this start block. NOTE: There
        // should only be one "pending" aggregation proof at a time for a specific start block.
        let completed_agg_proof = self
            .driver_config
            .driver_db_client
            .fetch_completed_agg_proof_after_block(
                latest_proposed_block_number as i64,
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        // If there are no completed aggregation proofs, do nothing.
        let completed_agg_proof = match completed_agg_proof {
            Some(proof) => proof,
            None => return Ok(()),
        };

        // Relay the aggregation proof.
        let transaction_hash = match self.relay_aggregation_proof(&completed_agg_proof).await {
            Ok(transaction_hash) => transaction_hash,
            Err(e) => {
                ValidityGauge::RelayAggProofErrorCount.increment(1.0);
                return Err(e);
            }
        };

        info!("Relayed aggregation proof. Transaction hash: {:?}", transaction_hash);

        // Update the request to status RELAYED.
        self.driver_config
            .driver_db_client
            .update_request_to_relayed(
                completed_agg_proof.id,
                transaction_hash,
                self.contract_config.l2oo_address,
            )
            .await?;

        Ok(())
    }

    /// Submit the transaction to create a validity dispute game.
    ///
    /// If the DGF address is set, use it to create a new validity dispute game that will resolve
    /// with the proof. Otherwise, propose the L2 output.
    /// SSV: we do not submit on-chain; only to the Shared Publisher!!!
    ///
    async fn relay_aggregation_proof(
        &self,
        completed_agg_proof: &OPSuccinctRequest,
    ) -> Result<B256> {
        // Get the output at the end block of the last completed aggregation proof.
        let output = self
            .driver_config
            .fetcher
            .get_l2_output_at_block(completed_agg_proof.end_block as u64)
            .await?;

        if let Some(publisher_url) = &self.driver_config.publisher_url {
            let start_block = completed_agg_proof.start_block as u64;
            let end_block = completed_agg_proof.end_block as u64;

            let pre_output = self
                .driver_config
                .fetcher
                .get_l2_output_at_block(start_block)
                .await?;
            let post_root_b256: B256 = output.output_root.0.into();

            let l1_head = B256::from_slice(
                completed_agg_proof
                    .checkpointed_l1_block_hash
                    .as_ref()
                    .expect("agg proof must have checkpointed l1 block hash"),
            );

            let proof = completed_agg_proof.proof.as_ref()
                .expect("No proof available for deserialization");

            println!("Relay completed aggregation proof: length={}", proof.len());

            // Fetch mailbox data from database
            let (mailbox_root, mailbox_info) = match self.fetch_mailbox_data(completed_agg_proof.end_block, completed_agg_proof.l2_chain_id).await {
                Ok((root, info)) => (root, info),
                Err(e) => {
                    tracing::warn!("Failed to fetch mailbox data for end_block {} l2_chain_id {}: {}. Using zero values.", completed_agg_proof.end_block, completed_agg_proof.l2_chain_id, e);
                    (B256::ZERO, MailboxInfoStruct {
                        inbox_chains: vec![],
                        outbox_chains: vec![],
                        inbox_roots: vec![],
                        outbox_roots: vec![],
                    })
                }
            };

            println!("Fetched mailbox data for request {}:", completed_agg_proof.id);
            println!("  Mailbox root: {:?}", mailbox_root);
            println!("  Mailbox info:");
            println!("    Inbox chains: {:?}", mailbox_info.inbox_chains);
            println!("    Outbox chains: {:?}", mailbox_info.outbox_chains);
            println!("    Inbox roots: {:?}", mailbox_info.inbox_roots);
            println!("    Outbox roots: {:?}", mailbox_info.outbox_roots);

            let agg_outputs = build_aggregation_outputs(
                l1_head,
                pre_output.output_root.0.into(),
                post_root_b256,
                end_block,
                self.program_config.commitments.rollup_config_hash,
                mailbox_root,
                self.program_config.commitments.range_vkey_commitment,
                self.requester_config.prover_address,
            );

            let superblock_hash = post_root_b256;
            let l2_chain_id = self.requester_config.l2_chain_id as u32;

            match submit_to_publisher(
                publisher_url,
                end_block,
                superblock_hash,
                l2_chain_id,
                self.requester_config.prover_address,
                l1_head,
                agg_outputs,
                start_block,
                &self.program_config.agg_vk,
                completed_agg_proof.proof.as_deref(),
                mailbox_info,
            )
            .await
            {
                Ok(_) => info!(
                    start_block,
                    end_block,
                    publisher = %publisher_url,
                    "Published aggregation outputs to shared publisher"
                ),
                Err(e) => warn!(
                    start_block,
                    end_block,
                    error = %e,
                    "Failed to publish aggregation outputs; continuing"
                ),
            }
        }
        
        Ok(B256::ZERO) // Placeholder return value since we're not submitting on-chain
    }

    /// Validate the requester config matches the contract.
    async fn validate_contract_config(&self) -> Result<()> {
        let config_name = self.requester_config.op_succinct_config_name_hash;

        let contract_config =
            self.contract_config.l2oo_contract.opSuccinctConfigs(config_name).call().await?;

        // Extract the OpSuccinctConfig fields with meaningful names.
        let contract_agg_vkey_hash = contract_config.aggregation_vkey();
        let contract_range_vkey_commitment = contract_config.range_vkey_commitment();
        let contract_rollup_config_hash = contract_config.rollup_config_hash();

        let rollup_config_hash_match =
            contract_rollup_config_hash == self.program_config.commitments.rollup_config_hash;
        let agg_vkey_hash_match =
            contract_agg_vkey_hash == self.program_config.commitments.agg_vkey_hash;
        let range_vkey_commitment_match =
            contract_range_vkey_commitment == self.program_config.commitments.range_vkey_commitment;

        /*
        if !rollup_config_hash_match || !agg_vkey_hash_match || !range_vkey_commitment_match {
            tracing::error!(
                rollup_config_hash_match = rollup_config_hash_match,
                agg_vkey_hash_match = agg_vkey_hash_match,
                range_vkey_commitment_match = range_vkey_commitment_match,
                "Config mismatches detected."
            );

            if !rollup_config_hash_match {
                tracing::error!(
                    received = ?contract_rollup_config_hash,
                    expected = ?self.program_config.commitments.rollup_config_hash,
                    "Rollup config hash mismatch"
                );
            }

            if !agg_vkey_hash_match {
                tracing::error!(
                    received = ?contract_agg_vkey_hash,
                    expected = ?self.program_config.commitments.agg_vkey_hash,
                    "Aggregation vkey hash mismatch"
                );
            }

            if !range_vkey_commitment_match {
                tracing::error!(
                    received = ?contract_range_vkey_commitment,
                    expected = ?self.program_config.commitments.range_vkey_commitment,
                    "Range vkey commitment mismatch"
                );
            }

            return Err(anyhow::anyhow!("Config mismatches detected. Please run {{cargo run --bin config --release -- --env-file ENV_FILE}} to get the expected config for your contract."));

        }
        */

        Ok(())
    }

    /// Set orphaned tasks to status FAILED. If a task is in the database in status Execution or
    /// WitnessGeneration but not in the tasks map, set it to status FAILED.
    async fn set_orphaned_tasks_to_failed(&self) -> Result<()> {
        let witnessgen_requests = self
            .driver_config
            .driver_db_client
            .fetch_requests_by_status(
                RequestStatus::WitnessGeneration,
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        let execution_requests = self
            .driver_config
            .driver_db_client
            .fetch_requests_by_status(
                RequestStatus::Execution,
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        let requests = [witnessgen_requests, execution_requests].concat();

        // If a task is in the database in status Execution or WitnessGeneration but not in the
        // tasks map, set it to status FAILED.
        for request in requests {
            if !self.tasks.lock().await.contains_key(&request.id) {
                tracing::warn!(
                    request_id = request.id,
                    request_type = ?request.req_type,
                    "Task is in the database in status Execution or WitnessGeneration but not in the tasks map, setting to status FAILED."
                );
                self.driver_config
                    .driver_db_client
                    .update_request_status(request.id, RequestStatus::Failed)
                    .await?;
            }
        }

        Ok(())
    }

    /// Handle the ongoing witness generation and execution tasks.
    async fn handle_ongoing_tasks(&self) -> Result<()> {
        let mut tasks = self.tasks.lock().await;
        let mut completed = Vec::new();

        // Check and process completed tasks
        for (id, (handle, _)) in tasks.iter() {
            if handle.is_finished() {
                completed.push(*id);
            }
        }

        // Process completed tasks - this will properly await and drop them
        for id in completed {
            if let Some((handle, request)) = tasks.remove(&id) {
                // First await the handle to properly clean up the task.
                match handle.await {
                    Ok(result) => {
                        if let Err(e) = result {
                            warn!(
                                request_id = request.id,
                                request_type = ?request.req_type,
                                error = ?e,
                                "Task failed with error"
                            );
                            // Now safe to retry as original task is cleaned up
                            match self
                                .proof_requester
                                .handle_failed_request(
                                    request,
                                    ExecutionStatus::UnspecifiedExecutionStatus,
                                )
                                .await
                            {
                                Ok(_) => {
                                    ValidityGauge::ProofRequestRetryCount.increment(1.0);
                                }
                                Err(retry_err) => {
                                    warn!(error = ?retry_err, "Failed to retry request");
                                    ValidityGauge::RetryErrorCount.increment(1.0);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            request_id = request.id,
                            request_type = ?request.req_type,
                            error = ?e,
                            "Task panicked"
                        );
                        // Now safe to retry as original task is cleaned up
                        match self
                            .proof_requester
                            .handle_failed_request(
                                request,
                                ExecutionStatus::UnspecifiedExecutionStatus,
                            )
                            .await
                        {
                            Ok(_) => {
                                ValidityGauge::ProofRequestRetryCount.increment(1.0);
                            }
                            Err(retry_err) => {
                                warn!(error = ?retry_err, "Failed to retry request after panic");
                                ValidityGauge::RetryErrorCount.increment(1.0);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Initialize the proposer by cleaning up stale requests and creating new range proof requests
    /// for the proposer with the given chain ID.
    ///
    /// This function performs several key tasks:
    /// 1. Validates that the proposer's config matches the contract
    /// 2. Deletes unrecoverable requests (UNREQUESTED, EXECUTION, WITNESS_GENERATION)
    /// 3. Cancels PROVE requests with mismatched commitment configs
    /// 4. Identifies gaps between the latest proposed block and finalized block
    /// 5. Creates new range proof requests to cover those gaps
    ///
    /// The goal is to ensure the database is in a clean state and all block ranges
    /// between the latest proposed block and finalized block have corresponding requests.
    #[tracing::instrument(name = "proposer.initialize_proposer", skip(self))]
    async fn initialize_proposer(&self) -> Result<()> {
        // Validate the requester config matches the contract.
        // self.validate_contract_config()
        //     .await
        //     .context("Failed to validate the requester config matches the contract.")?;

        // Delete all requests for the same chain ID that are of status UNREQUESTED, EXECUTION or
        // WITNESS_GENERATION as they're unrecoverable.
        self.driver_config
            .driver_db_client
            .delete_all_requests_with_statuses(
                &[
                    RequestStatus::Unrequested,
                    RequestStatus::Execution,
                    RequestStatus::WitnessGeneration,
                ],
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        // Cancel all requests in PROVE state for the same chain id's that have a different
        // commitment config.
        self.driver_config
            .driver_db_client
            .cancel_prove_requests_with_different_commitment_config(
                &self.program_config.commitments,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        info!("Deleted all unrequested, execution, and witness generation requests and canceled all prove requests with different commitment configs.");

        Ok(())
    }

    /// Fetch and log the proposer metrics.
    async fn log_proposer_metrics(&self) -> Result<()> {
        // Get the latest proposed block number on the contract.
        let mut latest_proposed_block_number = get_latest_proposed_block_number(
            self.contract_config.l2oo_address,
            self.driver_config.fetcher.as_ref(),
        )
        .await?;

        // Get all completed range proofs from the database.
        // Apply min block for metrics to reflect the active aggregation start
        let agg_start_block = latest_proposed_block_number
            .max(self.requester_config.min_l2_block) as i64;

        let completed_range_proofs = self
            .driver_config
            .driver_db_client
            .fetch_completed_ranges(
                &self.program_config.commitments,
                agg_start_block,
                self.requester_config.l1_chain_id,
                self.requester_config.l2_chain_id,
            )
            .await?;

        // Get the highest proven contiguous block.
        let highest_block_number = self
            .get_highest_proven_contiguous_block(completed_range_proofs)?
            .map_or(latest_proposed_block_number, |block| block as u64);

        // Fetch request counts for different statuses
        let commitments = &self.program_config.commitments;
        let l1_chain_id = self.requester_config.l1_chain_id;
        let l2_chain_id = self.requester_config.l2_chain_id;
        let db_client = &self.driver_config.driver_db_client;

        // Define statuses and their corresponding variable names
        let (
            num_unrequested_requests,
            num_prove_requests,
            num_execution_requests,
            num_witness_generation_requests,
        ) = (
            db_client
                .fetch_request_count(
                    RequestStatus::Unrequested,
                    commitments,
                    l1_chain_id,
                    l2_chain_id,
                )
                .await?,
            db_client
                .fetch_request_count(RequestStatus::Prove, commitments, l1_chain_id, l2_chain_id)
                .await?,
            db_client
                .fetch_request_count(
                    RequestStatus::Execution,
                    commitments,
                    l1_chain_id,
                    l2_chain_id,
                )
                .await?,
            db_client
                .fetch_request_count(
                    RequestStatus::WitnessGeneration,
                    commitments,
                    l1_chain_id,
                    l2_chain_id,
                )
                .await?,
        );

        // Log metrics
        info!(
            target: "proposer_metrics",
            "unrequested={num_unrequested_requests} prove={num_prove_requests} execution={num_execution_requests} witness_generation={num_witness_generation_requests} highest_contiguous_proven_block={highest_block_number} latest_proposed_block={latest_proposed_block_number}"
        );

        // Update gauges for proof counts
        ValidityGauge::CurrentUnrequestedProofs.set(num_unrequested_requests as f64);
        ValidityGauge::CurrentProvingProofs.set(num_prove_requests as f64);
        ValidityGauge::CurrentWitnessgenProofs.set(num_witness_generation_requests as f64);
        ValidityGauge::CurrentExecuteProofs.set(num_execution_requests as f64);
        ValidityGauge::HighestProvenContiguousBlock.set(highest_block_number as f64);
        ValidityGauge::LatestContractL2Block.set(latest_proposed_block_number as f64);

        // Get and set L2 block metrics
        let fetcher = &self.proof_requester.fetcher;
        ValidityGauge::L2UnsafeHeadBlock
            .set(fetcher.get_l2_header(BlockId::latest()).await?.number as f64);
        ValidityGauge::L2FinalizedBlock
            .set(fetcher.get_l2_header(BlockId::finalized()).await?.number as f64);

        // Get submission interval from contract and set gauge
        let contract_submission_interval: u64 = self
            .contract_config
            .l2oo_contract
            .submissionInterval()
            .call()
            .await?
            .try_into()
            .unwrap();

        let submission_interval =
            contract_submission_interval.max(self.requester_config.submission_interval);
        let min_agg_start = latest_proposed_block_number.max(self.requester_config.min_l2_block);
        ValidityGauge::MinBlockToProveToAgg
            .set((min_agg_start + submission_interval) as f64);

        Ok(())
    }

    #[tracing::instrument(name = "proposer.run", skip(self))]
    pub async fn run(&mut self) -> Result<()> {
        // Handle the case where the proposer is being re-started and the proposer state needs to be
        // updated.
        self.initialize_proposer().await?;

        // Initialize the metrics gauges.
        ValidityGauge::init_all();

        // Loop interval in seconds.
        loop {
            // Wrap the entire loop body in a match to handle errors
            match self.run_loop_iteration().await {
                Ok(_) => {
                    // Normal sleep between iterations
                    tokio::time::sleep(Duration::from_secs(self.driver_config.loop_interval)).await;
                }
                Err(e) => {
                    // Log the error
                    tracing::error!("Error in proposer loop: {:?}", e);
                    // Update the error gauge
                    ValidityGauge::TotalErrorCount.increment(1.0);
                    // Pause for 10 seconds before restarting
                    tracing::debug!("Pausing for 10 seconds before restarting the process");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }

    // Run a single loop of the validity proposer.
    async fn run_loop_iteration(&mut self) -> Result<()> {
        // Validate the requester config matches the contract.
        // self.validate_contract_config().await?;

        // Log the proposer metrics.
        self.log_proposer_metrics().await?;

        // Handle the ongoing tasks.
        self.handle_ongoing_tasks().await?;

        // Set orphaned WitnessGeneration and Execution tasks to status Failed.
        self.set_orphaned_tasks_to_failed().await?;

        // Get all proof statuses of all requests in the proving state.
        self.handle_proving_requests().await?;

        // Add new range requests to the database.
        self.add_new_ranges().await?;

        // Create aggregation proofs based on the completed range proofs. Checkpoints the block hash
        // associated with the aggregation proof in advance.
        if self.requester_config.enable_aggregation {
            self.create_aggregation_proofs().await?;
        }

        // Request all unrequested proofs from the prover network.
        self.request_queued_proofs().await?;

        // Submit any aggregation proofs that are complete.
        if self.requester_config.enable_aggregation {
            self.submit_agg_proofs().await?;
        }

        // Update the chain lock.
        self.proof_requester
            .db_client
            .update_chain_lock(self.requester_config.l1_chain_id, self.requester_config.l2_chain_id)
            .await?;

        Ok(())
    }

    /// Get the highest block number at the end of the largest contiguous range of completed range
    /// proofs. Returns None if there are no completed range proofs.
    fn get_highest_proven_contiguous_block(
        &self,
        completed_range_proofs: Vec<(i64, i64)>,
    ) -> Result<Option<i64>> {
        if completed_range_proofs.is_empty() {
            return Ok(None);
        }

        let mut current_end = completed_range_proofs[0].1;

        for proof in completed_range_proofs.iter().skip(1) {
            if proof.0 != current_end {
                break;
            }
            current_end = proof.1;
        }

        Ok(Some(current_end))
    }

    /// Fetch mailbox root and mailbox info from database.
    async fn fetch_mailbox_data(&self, end_block: i64, l2_chain_id: i64) -> Result<(B256, MailboxInfoStruct)> {
        // Fetch mailbox data from database
        let mailbox_data = self
            .driver_config
            .driver_db_client
            .fetch_mailbox_store(end_block, l2_chain_id, RequestType::Range)
            .await?;

        match mailbox_data {
            Some((inbox_chains, outbox_chains, inbox_roots, outbox_roots, mailbox_root)) => {
                // Parse mailbox root
                let root = match mailbox_root {
                    Some(root_bytes) => {
                        if root_bytes.len() == 32 {
                            B256::from_slice(&root_bytes)
                        } else {
                            tracing::warn!("Invalid mailbox root length for end_block {} l2_chain_id {}: expected 32 bytes, got {}", end_block, l2_chain_id, root_bytes.len());
                            B256::ZERO
                        }
                    }
                    None => {
                        tracing::warn!("No mailbox root found for end_block {} l2_chain_id {}", end_block, l2_chain_id);
                        B256::ZERO
                    }
                };

                // Convert database format to MailboxInfoStruct
                let mailbox_info = MailboxInfoStruct {
                    inbox_chains: inbox_chains
                        .unwrap_or_default()
                        .into_iter()
                        .map(|bytes| {
                            if bytes.len() == 32 {
                                B256::from_slice(&bytes)
                            } else {
                                B256::ZERO
                            }
                        })
                        .collect(),
                    outbox_chains: outbox_chains
                        .unwrap_or_default()
                        .into_iter()
                        .map(|bytes| {
                            if bytes.len() == 32 {
                                B256::from_slice(&bytes)
                            } else {
                                B256::ZERO
                            }
                        })
                        .collect(),
                    inbox_roots: inbox_roots
                        .unwrap_or_default()
                        .into_iter()
                        .map(|bytes| {
                            if bytes.len() == 32 {
                                B256::from_slice(&bytes)
                            } else {
                                B256::ZERO
                            }
                        })
                        .collect(),
                    outbox_roots: outbox_roots
                        .unwrap_or_default()
                        .into_iter()
                        .map(|bytes| {
                            if bytes.len() == 32 {
                                B256::from_slice(&bytes)
                            } else {
                                B256::ZERO
                            }
                        })
                        .collect(),
                };

                Ok((root, mailbox_info))
            }
            None => {
                tracing::warn!("No mailbox data found for end_block {} l2_chain_id {}", end_block, l2_chain_id);
                Ok((B256::ZERO, MailboxInfoStruct {
                    inbox_chains: vec![],
                    outbox_chains: vec![],
                    inbox_roots: vec![],
                    outbox_roots: vec![],
                }))
            }
        }
    }
}
