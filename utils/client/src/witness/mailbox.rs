use alloy_primitives::{keccak256, B256};
use kzg_rs::Bytes32;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(
    Clone, Debug, Default, Serialize, Deserialize, rkyv::Serialize, rkyv::Archive, rkyv::Deserialize,
)]
pub struct MailboxStore {
    pub inbox_chains: Vec<Bytes32>,
    pub outbox_chains: Vec<Bytes32>,
    pub inbox_roots: Vec<Bytes32>,
    pub outbox_roots: Vec<Bytes32>,
}

impl fmt::Display for MailboxStore {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MailboxStore error: {:?}", self)
    }
}

impl std::error::Error for MailboxStore {}

impl MailboxStore {
    pub fn new(
        inbox_chains: Vec<Bytes32>,
        outbox_chains: Vec<Bytes32>,
        inbox_roots: Vec<Bytes32>,
        outbox_roots: Vec<Bytes32>,
    ) -> Self {
        Self {
            inbox_chains,
            outbox_chains,
            inbox_roots,
            outbox_roots,
        }
    }

    pub fn decode_inbox_chains(&self) -> Vec<u64> {
        self.inbox_chains
            .iter()
            .map(|bytes| {
                let mut array = [0u8; 8];
                array.copy_from_slice(&bytes.0[24..32]); // Take last 8 bytes for u64
                u64::from_be_bytes(array)
            })
            .collect()
    }

    pub fn decode_outbox_chains(&self) -> Vec<u64> {
        self.outbox_chains
            .iter()
            .map(|bytes| {
                let mut array = [0u8; 8];
                array.copy_from_slice(&bytes.0[24..32]); // Take last 8 bytes for u64
                u64::from_be_bytes(array)
            })
            .collect()
    }

    pub fn decode_inbox_roots(&self) -> Vec<[u8; 32]> {
        self.inbox_roots
            .iter()
            .map(|bytes| bytes.0)
            .collect()
    }

    pub fn decode_outbox_roots(&self) -> Vec<[u8; 32]> {
        self.outbox_roots
            .iter()
            .map(|bytes| bytes.0)
            .collect()
    }
}

/// Compute the mailbox root from mailbox store data.
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

