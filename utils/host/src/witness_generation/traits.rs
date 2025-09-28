use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use alloy_primitives::{keccak256, Address, U256, B256};
use alloy_sol_types::SolValue;
use kona_preimage::{HintWriter, NativeChannel, OracleReader};
use kona_proof::{
    l1::{OracleBlobProvider, OracleL1ChainProvider},
    l2::OracleL2ChainProvider,
    CachingOracle,
};
use op_succinct_client_utils::witness::{
    executor::{get_inputs_for_pipeline, WitnessExecutor},
    preimage_store::PreimageStore,
    BlobData, WitnessData, MailboxStore,
};
use sp1_sdk::SP1Stdin;
use serde_json::{json, Value};
use reqwest;
use hex;
use kzg_rs::Bytes32;
use crate::witness_generation::{OnlineBlobStore, PreimageWitnessCollector};

pub type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;


/// Converts bytes to an integer, adds a value, and returns as bytes
fn add_to_bytes(data: &[u8], value: u64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[(32 - data.len())..].copy_from_slice(data);
    let int_val = U256::from_be_bytes(bytes);
    let result = int_val + U256::from(value);
    result.to_be_bytes()
}

/// Extracts storage data using eth_getProof RPC calls to external endpoint
async fn extract_contract_storage_data(
    contract_addr: Address,
    block_number: u64,
    rpc_url: &str,
) -> Result<(Vec<Bytes32>, Vec<Bytes32>, Vec<Bytes32>, Vec<Bytes32>)> {
    let client = reqwest::Client::new();

    println!("extract_contract_storage_data for {} block number", block_number);

    // Get chainIDsInbox list at slot 0
    let inbox_chains_u256 = get_list_state(&client, rpc_url, contract_addr, 0, block_number).await?;
    
    // Get chainIDsOutbox list at slot 1
    let outbox_chains_u256 = get_list_state(&client, rpc_url, contract_addr, 1, block_number).await?;
    
    // Get inbox roots map at slot 2 using inbox_chains as keys
    let inbox_roots_b256 = get_map_state(&client, rpc_url, contract_addr, 2, &inbox_chains_u256, block_number).await?;
    
    // Get outbox roots map at slot 3 using outbox_chains as keys
    let outbox_roots_b256 = get_map_state(&client, rpc_url, contract_addr, 3, &outbox_chains_u256, block_number).await?;

    // Convert to Bytes32
    let inbox_chains = inbox_chains_u256.into_iter().map(|u| Bytes32(u.to_be_bytes())).collect();
    let outbox_chains = outbox_chains_u256.into_iter().map(|u| Bytes32(u.to_be_bytes())).collect();
    let inbox_roots = inbox_roots_b256.into_iter().map(|b| Bytes32(b.0)).collect();
    let outbox_roots = outbox_roots_b256.into_iter().map(|b| Bytes32(b.0)).collect();

    Ok((inbox_chains, outbox_chains, inbox_roots, outbox_roots))
}

/// Gets list state from contract storage at specified slot
async fn get_list_state(
    client: &reqwest::Client,
    rpc_url: &str,
    contract_addr: Address,
    slot: u64,
    block_number: u64,
) -> Result<Vec<U256>> {
    println!("Getting list at slot {}", slot);
    
    // First, get the length of the list at this slot
    let length_proof = eth_get_proof(
        client,
        rpc_url,
        contract_addr,
        vec![format!("0x{:x}", slot)],
        block_number,
    ).await?;
    
    let length = if let Some(storage_proof) = length_proof["storageProof"].as_array() {
        if let Some(first_proof) = storage_proof.first() {
            if let Some(value_str) = first_proof["value"].as_str() {
                hex_to_u64(value_str)?
            } else {
                return Ok(Vec::new());
            }
        } else {
            return Ok(Vec::new());
        }
    } else {
        return Ok(Vec::new());
    };
    
    println!("List at slot {} has length {}", slot, length);
    
    if length == 0 {
        return Ok(Vec::new());
    }
    
    // Generate storage keys for list elements
    let mut storage_keys = Vec::new();
    let base = keccak_hash_u256(slot);
    
    for i in 0..length {
        if i == 0 {
            storage_keys.push(format!("0x{}", hex::encode(base)));
        } else {
            let key = increment_hex_string(&format!("0x{}", hex::encode(base)), i);
            storage_keys.push(key);
        }
    }
    
    // Get values for all storage keys
    let content_proof = eth_get_proof(
        client,
        rpc_url,
        contract_addr,
        storage_keys,
        block_number,
    ).await?;
    
    let mut values = Vec::new();
    if let Some(storage_proofs) = content_proof["storageProof"].as_array() {
        for (i, proof) in storage_proofs.iter().enumerate() {
            if let Some(value_str) = proof["value"].as_str() {
                let value = hex_to_u256(value_str)?;
                values.push(value);
                println!("\tItem: {}. Value: {}", i, value);
            }
        }
    }
    
    Ok(values)
}

/// Gets map state from contract storage at specified slot using provided keys
async fn get_map_state(
    client: &reqwest::Client,
    rpc_url: &str,
    contract_addr: Address,
    slot: u64,
    keys: &[U256],
    block_number: u64,
) -> Result<Vec<B256>> {
    println!("Getting map at slot {} with keys {:?}", slot, keys);
    
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    
    // Generate storage keys for map values
    let mut storage_keys = Vec::new();
    for &key in keys {
        let map_key = keccak_hash_map_key(key, slot);
        storage_keys.push(format!("0x{}", hex::encode(map_key)));
    }
    
    // Get values for all storage keys
    let proof = eth_get_proof(
        client,
        rpc_url,
        contract_addr,
        storage_keys,
        block_number,
    ).await?;
    
    let mut values = Vec::new();
    if let Some(storage_proofs) = proof["storageProof"].as_array() {
        for (i, proof) in storage_proofs.iter().enumerate() {
            if let Some(value_str) = proof["value"].as_str() {
                let value = hex_to_b256(value_str)?;
                values.push(value);
                println!("\tItem: {}. Key: {}. Value: {}", i, keys[i], value);
            }
        }
    }
    
    Ok(values)
}

/// Makes eth_getProof RPC call
async fn eth_get_proof(
    client: &reqwest::Client,
    rpc_url: &str,
    address: Address,
    storage_keys: Vec<String>,
    block_number: u64,
) -> Result<Value> {
    let payload = json!({
        "jsonrpc": "2.0",
        "method": "eth_getProof",
        "params": [
            format!("0x{}", hex::encode(address.as_slice())),
            storage_keys,
            format!("0x{:x}", block_number)
        ],
        "id": 1
    });
    
    let response = client
        .post(rpc_url)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await?;
    
    let json: Value = response.json().await?;
    
    if let Some(result) = json.get("result") {
        Ok(result.clone())
    } else if let Some(error) = json.get("error") {
        Err(anyhow::anyhow!("RPC error: {}", error))
    } else {
        Err(anyhow::anyhow!("Invalid RPC response"))
    }
}

fn hex_to_b256(hex_str: &str) -> Result<B256> {
    let clean_hex = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if clean_hex.is_empty() || clean_hex == "0" {
        return Ok(B256::ZERO);
    }

    let bytes = hex::decode(clean_hex)
        .map_err(|e| anyhow::anyhow!("Failed to decode hex: {}", e))?;
        
    if bytes.len() == 32 {
        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);
        Ok(B256::from(array))
    } else {
        Err(anyhow::anyhow!("Invalid byte length: expected 32, got {}", bytes.len()))
    }
}

fn hex_to_u256(hex_str: &str) -> Result<U256> {
    let clean_hex = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if clean_hex.is_empty() || clean_hex == "0" {
        return Ok(U256::from(0));
    }
    U256::from_str_radix(clean_hex, 16).map_err(|e| anyhow::anyhow!("Failed to parse hex: {}", e))
}

fn hex_to_u64(hex_str: &str) -> Result<u64> {
    let clean_hex = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if clean_hex.is_empty() || clean_hex == "0" {
        return Ok(0);
    }
    u64::from_str_radix(clean_hex, 16).map_err(|e| anyhow::anyhow!("Failed to parse hex: {}", e))
}

/// Computes keccak hash for a U256 value (for list base key)
fn keccak_hash_u256(slot: u64) -> [u8; 32] {
    let slot_bytes = U256::from(slot).to_be_bytes::<32>();
    keccak256(slot_bytes).into()
}

/// Computes keccak hash for map key-slot combination using ABI encoding
fn keccak_hash_map_key(key: U256, slot: u64) -> [u8; 32] {
    let encoded = (key, U256::from(slot)).abi_encode();
    keccak256(encoded).into()
}

/// Increments a hex string by specified amount (similar to Python script)
fn increment_hex_string(hex_string: &str, increment: u64) -> String {
    let clean_hex = hex_string.strip_prefix("0x").unwrap_or(hex_string);
    let bytes = hex::decode(clean_hex).unwrap_or_default();
    let incremented = add_to_bytes(&bytes, increment);
    format!("0x{}", hex::encode(incremented))
}

#[async_trait]
pub trait WitnessGenerator {
    type WitnessData: WitnessData;
    type WitnessExecutor: WitnessExecutor<
            O = PreimageWitnessCollector<DefaultOracleBase>,
            B = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
            L1 = OracleL1ChainProvider<PreimageWitnessCollector<DefaultOracleBase>>,
            L2 = OracleL2ChainProvider<PreimageWitnessCollector<DefaultOracleBase>>,
        > + Sync
        + Send;

    fn get_executor(&self) -> &Self::WitnessExecutor;

    async fn run(
        &self,
        preimage_chan: NativeChannel,
        hint_chan: NativeChannel,
    ) -> Result<(Self::WitnessData, MailboxStore)> {
        let preimage_witness_store = Arc::new(Mutex::new(PreimageStore::default()));
        let blob_data = Arc::new(Mutex::new(BlobData::default()));

        let preimage_oracle = Arc::new(CachingOracle::new(
            2048,
            OracleReader::new(preimage_chan),
            HintWriter::new(hint_chan),
        ));
        let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());

        let oracle = Arc::new(PreimageWitnessCollector {
            preimage_oracle: preimage_oracle.clone(),
            preimage_witness_store: preimage_witness_store.clone(),
        });
        let beacon = OnlineBlobStore { provider: blob_provider.clone(), store: blob_data.clone() };

        let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await.unwrap();
        
        if let Some((cursor, l1_provider, l2_provider)) = input {
            let rollup_config = Arc::new(boot_info.rollup_config.clone());

            let pipeline = self
                .get_executor()
                .create_pipeline(
                    rollup_config,
                    cursor.clone(),
                    oracle.clone(),
                    beacon,
                    l1_provider.clone(),
                    l2_provider.clone(),
                )
                .await
                .unwrap();
            self.get_executor().run(boot_info.clone(), pipeline, cursor, l2_provider.clone()).await.unwrap();
        }
        let contract_addr_str = std::env::var("MAILBOX_ADDRESS").expect("MAILBOX_ADDRESS environment variable must be set");
        let contract_addr = contract_addr_str.parse::<Address>().unwrap();
        let l2_rpc_url = std::env::var("L2_RPC").expect("L2_RPC environment variable must be set");

        println!("Query mailbox info from {} contract addr, {} L2_RPC", contract_addr_str, l2_rpc_url);
        let mailbox_store = match extract_contract_storage_data(contract_addr, boot_info.claimed_l2_block_number, &l2_rpc_url).await {
            Ok((ic, oc, ir, or)) => {
                Arc::new(Mutex::new(MailboxStore::new(ic, oc, ir, or)))
            }
            Err(e) => {
                // Log error but continue with empty data
                eprintln!("Failed to extract contract storage data: {:?}", e);
                Arc::new(Mutex::new(MailboxStore::default()))
            }
        };

        // println!("Created MailboxStore with {} inbox chains, {} outbox chains",
        //          mailbox_store.inbox_chains.len(),
        //          mailbox_store.outbox_chains.len());

        let witness = Self::WitnessData::from_parts(
            preimage_witness_store.lock().unwrap().clone(),
            blob_data.lock().unwrap().clone(),
            mailbox_store.lock().unwrap().clone(),
        );
        let mailbox_store_value = mailbox_store.lock().unwrap().clone();
        Ok((witness, mailbox_store_value))
    }

    fn get_sp1_stdin(&self, witness: Self::WitnessData) -> Result<SP1Stdin>;
}
