pub mod executor;
pub mod preimage_store;
mod mailbox;

pub use mailbox::{MailboxStore, compute_mailbox_root};

use std::{fmt::Debug, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use kzg_rs::{Blob, Bytes48};
use preimage_store::PreimageStore;
use serde::{Deserialize, Serialize};

use crate::BlobStore;

#[async_trait]
pub trait WitnessData: Sized + Send {
    /// Creates a new WitnessData from the given preimage store, blob data, and storage data.
    fn from_parts(
        preimage_store: PreimageStore, 
        blob_data: BlobData,
        mailbox_store: MailboxStore,
    ) -> Self;

    /// Consumes the WitnessData to extract its core components.
    fn into_parts(self) -> (PreimageStore, BlobData, MailboxStore);

    /// Gets the oracle and blob provider from the witness data and validates the correctness of the
    /// preimages.
    async fn get_oracle_and_blob_provider(self) -> Result<(Arc<PreimageStore>, BlobStore, MailboxStore)> {
        let (owned_preimage_store, owned_blob_data, mailbox_store) = self.into_parts();

        println!("cycle-tracker-report-start: oracle-verify");
        // Check the preimages in the witness are valid.
        owned_preimage_store.check_preimages().expect("Failed to validate preimages");
        println!("cycle-tracker-report-end: oracle-verify");

        // Create an Arc of the preimage store.
        let oracle = Arc::new(owned_preimage_store);

        // Create a BlobStore from the blobs in the witness and verifies them for correctness.
        println!("cycle-tracker-report-start: blob-verification");
        let beacon = BlobStore::from(owned_blob_data);
        println!("cycle-tracker-report-end: blob-verification");

        Ok((oracle, beacon, mailbox_store))
    }
}

#[derive(Clone, Debug, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct DefaultWitnessData {
    pub preimage_store: PreimageStore,
    pub blob_data: BlobData,
    pub mailbox_store: MailboxStore
}

#[async_trait]
impl WitnessData for DefaultWitnessData {
    fn from_parts(
        preimage_store: PreimageStore, 
        blob_data: BlobData,
        mailbox_store: MailboxStore,
    ) -> Self {
        Self { 
            preimage_store, 
            blob_data,
            mailbox_store
        }
    }

    fn into_parts(self) -> (PreimageStore, BlobData, MailboxStore) {
        (
            self.preimage_store, 
            self.blob_data,
            self.mailbox_store
        )
    }
}

#[derive(
    Clone, Debug, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct BlobData {
    pub blobs: Vec<Blob>,
    pub commitments: Vec<Bytes48>,
    pub proofs: Vec<Bytes48>,
}
