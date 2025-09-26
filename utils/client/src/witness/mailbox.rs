use alloy_primitives::map::HashMap;
use kona_preimage::PreimageKey;
use kzg_rs::Bytes32;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Debug, Default, Serialize, Deserialize, rkyv::Serialize, rkyv::Archive, rkyv::Deserialize,
)]
pub struct MailboxStore {
    pub inbox_chains: Vec<Bytes32>,
    pub outbox_chains: Vec<Bytes32>,
    pub inbox_roots: Vec<Bytes32>,
    pub outbox_roots: Vec<Bytes32>,
}

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

