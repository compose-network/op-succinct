use alloy_primitives::{Address, U256};

/// Default placeholder mailbox address until configured.
pub const DEFAULT_MAILBOX_ADDRESS: Address = Address::new([0x42; 20]);

/// Local-oracle key to provide the mailbox address in the witness.
/// Keys 1..6 are used by boot; 7 is free for mailbox address.
pub const L2_MAILBOX_ADDRESS_KEY: U256 = U256::from_be_slice(&[7]);

// Storage slots as per Solidity layout in your Mailbox contract.
// 0: chainIDsInbox (dynamic array length), elements at keccak256(slot0) + i
// 1: chainIDsOutbox (dynamic array length), elements at keccak256(slot1) + i
// 2: inboxRootPerChain mapping(uint256=>bytes32) -> slot = keccak256(abi.encode(key, 2))
// 3: outboxRootPerChain mapping(uint256=>bytes32) -> slot = keccak256(abi.encode(key, 3))
pub const SLOT_CHAIN_IDS_INBOX: U256 = U256::from_limbs([0, 0, 0, 0]);
pub const SLOT_CHAIN_IDS_OUTBOX: U256 = U256::from_limbs([1, 0, 0, 0]);
pub const SLOT_INBOX_ROOT_PER_CHAIN: U256 = U256::from_limbs([2, 0, 0, 0]);
pub const SLOT_OUTBOX_ROOT_PER_CHAIN: U256 = U256::from_limbs([3, 0, 0, 0]);

/// Helper to encode an `Address` into a 32-byte word (right-aligned, left-padded with zeros).
pub fn address_to_word(addr: Address) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..32].copy_from_slice(addr.as_slice());
    out
}

/// Helper to decode a 32-byte word into an `Address` (last 20 bytes).
pub fn word_to_address(word: &[u8]) -> Option<Address> {
    if word.len() < 32 {
        return None;
    }
    Some(Address::from_slice(&word[12..32]))
}

// no-op
