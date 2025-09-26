use std::io::Bytes;
use std::sync::{Arc, Once};

use alloy_primitives::{address, keccak256, Address, Sealable, B256}; // for seal_ref_slow
use kzg_rs::Bytes32;

use kona_executor::{TrieDB, TrieDBProvider};
use kona_proof::{l1::OracleL1ChainProvider, l2::OracleL2ChainProvider, BootInfo};
use kona_protocol::BatchValidationProvider; // enables block_by_number on the provider
use op_succinct_client_utils::{
    boot::{hash_rollup_config, BootInfoStruct, MailboxInfoStruct},
    witness::{
        executor::{get_inputs_for_pipeline, WitnessExecutor},
        preimage_store::PreimageStore,
        WitnessData,
        MailboxStore,
    },
    BlobStore,
};
use tracing::{debug, error, info, warn};

macro_rules! log_info {
    ($($arg:tt)*) => {{
        info!($($arg)*);
        #[cfg(target_os = "zkvm")]
        println!($($arg)*);
    }};
}

macro_rules! log_debug {
    ($($arg:tt)*) => {{
        debug!($($arg)*);
        #[cfg(target_os = "zkvm")]
        println!($($arg)*);
    }};
}

macro_rules! log_warn {
    ($($arg:tt)*) => {{
        warn!($($arg)*);
        #[cfg(target_os = "zkvm")]
        println!($($arg)*);
    }};
}

macro_rules! log_error {
    ($($arg:tt)*) => {{
        error!($($arg)*);
        #[cfg(target_os = "zkvm")]
        println!($($arg)*);
    }};
}

/// Sets up tracing for the range program
pub fn setup_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        #[cfg(feature = "tracing-subscriber")]
        {
            use anyhow::anyhow;
            use tracing::Level;

            let subscriber = tracing_subscriber::fmt().with_max_level(Level::INFO).finish();
            tracing::subscriber::set_global_default(subscriber).map_err(|e| anyhow!(e)).unwrap();
        }

        #[cfg(not(feature = "tracing-subscriber"))]
        {
            // no op
        }
    });
}

pub async fn run_range_program<E, W>(executor: E, witness_data: W)
where
    E: WitnessExecutor<
            O = PreimageStore,
            B = BlobStore,
            L1 = OracleL1ChainProvider<PreimageStore>,
            L2 = OracleL2ChainProvider<PreimageStore>,
        > + Send
        + Sync,
    W: WitnessData + Send + Sync,
{
    ////////////////////////////////////////////////////////////////
    //                          PROLOGUE                          //
    ////////////////////////////////////////////////////////////////

    log_info!("Starting blocks verification...");

    let (oracle, beacon, mailbox_store) = witness_data.get_oracle_and_blob_provider().await.unwrap();


    let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await.unwrap();
    let mut l2_provider_for_mailbox: Option<OracleL2ChainProvider<PreimageStore>> = None;
    let boot_info = match input {
        // Some((_, _, l2_provider)) => {
        Some((cursor, l1_provider, l2_provider)) => {
        let rollup_config = Arc::new(boot_info.rollup_config.clone());

        let pipeline = executor
            .create_pipeline(
                rollup_config,
                cursor.clone(),
                oracle,
                beacon,
                l1_provider,
                l2_provider.clone(),
            )
            .await
            .unwrap();
        // Save for mailbox computation (stubbed for now)
        l2_provider_for_mailbox = Some(l2_provider.clone());
        executor.run(boot_info, pipeline, cursor, l2_provider).await.unwrap()
        // boot_info
        }
        None => boot_info,
    };

    log_info!("Finished blocks verification. Now computing mailbox root...");

    let mailbox_root = compute_mailbox_root(mailbox_store.clone());
    log_info!("Mailbox root hash computed: {:?}", mailbox_root);

    let mailbox_info = MailboxInfoStruct {
        inbox_chains: mailbox_store.inbox_chains.iter().map(|b| B256::from_slice(&b.0)).collect(),
        outbox_chains: mailbox_store.outbox_chains.iter().map(|b| B256::from_slice(&b.0)).collect(),
        inbox_roots: mailbox_store.inbox_roots.iter().map(|b| B256::from_slice(&b.0)).collect(),
        outbox_roots: mailbox_store.outbox_roots.iter().map(|b| B256::from_slice(&b.0)).collect(),
    };

    let boot_info_struct = BootInfoStruct {
        l1Head: boot_info.l1_head,
        l2PreRoot: boot_info.agreed_l2_output_root,
        l2PostRoot: boot_info.claimed_l2_output_root,
        l2BlockNumber: boot_info.claimed_l2_block_number,
        rollupConfigHash: hash_rollup_config(&boot_info.rollup_config),
        mailboxRoot: mailbox_root,
        mailboxInfo: mailbox_info,
    };

    sp1_zkvm::io::commit(&boot_info_struct);
}

pub fn compute_mailbox_root(mailbox_store: MailboxStore) -> B256 {
    let mut bytes = Vec::new();

    let mut chain_ids: Vec<u64> = mailbox_store.decode_inbox_chains();
    chain_ids.extend(mailbox_store.decode_outbox_chains());
    chain_ids.sort_unstable();
    chain_ids.dedup();

    bytes.extend_from_slice(b"MAILBOX");

    bytes.extend_from_slice(&(chain_ids.len() as u64).to_be_bytes());


    for chain_id in chain_ids {
        // Write the chain ID first
        bytes.extend_from_slice(&chain_id.to_be_bytes());

        let inbox_index = mailbox_store
            .decode_inbox_chains()
            .iter()
            .position(|&inbox_chain_id| chain_id == inbox_chain_id);

        if let Some(index) = inbox_index {
            let inbox_root = mailbox_store.decode_inbox_roots()[index];
            bytes.extend_from_slice(&inbox_root);
        } else {
            // Write 32 zero bytes for missing inbox root
            bytes.extend_from_slice(&[0u8; 32]);
        }

        let outbox_index = mailbox_store
            .decode_outbox_chains()
            .iter()
            .position(|&outbox_chain_id| chain_id == outbox_chain_id);

        if let Some(index) = outbox_index {
            let outbox_root = mailbox_store.decode_outbox_roots()[index];
            bytes.extend_from_slice(&outbox_root);
        } else {
            // Write 32 zero bytes for missing outbox root
            bytes.extend_from_slice(&[0u8; 32]);
        }
    }

    B256::from(keccak256(&bytes))
}
