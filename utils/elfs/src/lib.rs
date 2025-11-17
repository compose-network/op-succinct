//! The zkvm ELF binaries.

pub const AGGREGATION_ELF: &[u8] = include_bytes!("../../../elf/aggregation-elf");

pub const RANGE_ELF_BUMP: &[u8] = include_bytes!("../../../elf/range-elf-bump");
pub const RANGE_ELF_EMBEDDED: &[u8] = include_bytes!("../../../elf/range-elf-embedded");

// Celestia ELF intentionally not wired here; Celestia support is disabled in this fork.
pub const EIGENDA_RANGE_ELF_EMBEDDED: &[u8] =
    include_bytes!("../../../elf/eigenda-range-elf-embedded");

