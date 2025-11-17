use alloy_primitives::{hex::FromHex, B256};
use kzg_rs::Bytes32;
use tracing::{info, Level};
use tracing_subscriber;
use op_succinct_client_utils::witness::{compute_mailbox_root, MailboxStore};

pub const CHAIN_ID_ROLLUP_A: u32 = 77_777;
pub const CHAIN_ID_ROLLUP_B: u32 = 88_888;
#[allow(dead_code)]
pub const CHAIN_B_CONFIG_HASH: &str =
    "0xed51bbbacc916f42db704327a6a19183b9784a1791d115d3c34e4ab17f225ae9";

#[allow(dead_code)]
const MAILBOX_ROOT_CHAIN_A: &str =
    "0x4c0d5f11c3eb6c2035b9f56b854e84cc90880c5fa5031a057aed92f83f2f0c8f";
#[allow(dead_code)]
const MAILBOX_ROOT_CHAIN_B: &str =
    "0x486b3a08a8f1b91e86e606e90006878003a4ac213395ed501847d59c53765450";

fn bytes32(hex_str: &str) -> Bytes32 {
    let bytes = <[u8; 32]>::from_hex(hex_str).expect("invalid hex string");
    // Convert [u8; 32] to Bytes32
    Bytes32::from_slice(&bytes).unwrap()
}

fn bytes32_vec(hex_list: &[&str]) -> Vec<Bytes32> {
    hex_list.iter().map(|hex| bytes32(hex)).collect()
}

pub fn mailbox_info_for_chain(chain_id: u32) -> MailboxStore {
    match chain_id {
        CHAIN_ID_ROLLUP_B => MailboxStore {
            inbox_chains: bytes32_vec(&["0x0000000000000000000000000000000000000000000000000000000000012fd1"]),
            outbox_chains: bytes32_vec(&["0x0000000000000000000000000000000000000000000000000000000000012fd1"]),
            inbox_roots: bytes32_vec(&["0x32bfb484aba4999be46bc4f0af7e9f4e544d54fd6763edb7a0e67228156bd0bd"]),
            outbox_roots: bytes32_vec(&["0xd30a428fe2342ca1634f7301d78f10fec84ddabfe44b89ca89f02df48d267f47"]),
        },
        CHAIN_ID_ROLLUP_A => MailboxStore {
            inbox_chains: vec![],
            outbox_chains: bytes32_vec(&["0x0000000000000000000000000000000000000000000000000000000000015b38"]),
            inbox_roots: vec![],
            outbox_roots: bytes32_vec(&["0x98b663dcb761a1ccd645d7c7a6cc0a0b4ca942d5ed2831b8be45f12eeda3ae9d"]),
        },
        _ => panic!("unknown chain id {}", chain_id),
    }
}

#[allow(dead_code)]
pub fn expected_mailbox_root(chain_id: u32) -> Option<B256> {
    match chain_id {
        CHAIN_ID_ROLLUP_A => Some(B256::from_hex(MAILBOX_ROOT_CHAIN_A).unwrap()),
        CHAIN_ID_ROLLUP_B => Some(B256::from_hex(MAILBOX_ROOT_CHAIN_B).unwrap()),
        _ => None,
    }
}

// Usage: RUST_LOG=info cargo test -p op-succinct-client-utils --test mailbox_root -- --no-capture

#[test]
fn mailbox_root_chain_b_matches_expected() {
    init_tracing();
    let mailbox_store = mailbox_info_for_chain(CHAIN_ID_ROLLUP_B);
    let root = compute_mailbox_root(mailbox_store);
    let expected =
        expected_mailbox_root(CHAIN_ID_ROLLUP_B).expect("missing expected root for chain B");
    assert_eq!(root, expected);
    info!("Mailbox root (chain B): {:?}", root);
}

#[test]
fn mailbox_root_chain_a_matches_expected() {
    init_tracing();
    let mailbox_store = mailbox_info_for_chain(CHAIN_ID_ROLLUP_A);
    let root = compute_mailbox_root(mailbox_store);
    let expected =
        expected_mailbox_root(CHAIN_ID_ROLLUP_A).expect("missing expected root for chain B");
    assert_eq!(root, expected);
    info!("Mailbox root (chain A): {:?}", root);
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_test_writer()
        .try_init();
}
