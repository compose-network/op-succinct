# op-succinct — AGENTS Guide

This repo contains the **zkVM programs and host tooling** that generate validity proofs for Optimism‑style L2 state transitions with Ethereum data‑availability. It is the low‑level proving engine that the superblock prover and Publisher build on top of.

Use this document as your starting point when:
- changing the proving logic for L2 ranges or aggregation,
- debugging proof/public‑value mismatches between zkVM and Solidity,
- wiring new networks or DA backends.


## High‑Level Architecture

Core pieces:

- **Range program (`programs/range/…`)**
  - zkVM program that proves a single L2 range (one or more blocks) is a valid Optimism execution trace against Ethereum DA.
  - Reads a serialized `DefaultWitnessData` from `sp1_zkvm::io`, replays L2 blocks, enforces execution invariants, and commits to a `BootInfoStruct` as public values.
- **Aggregation program (`programs/aggregation`)**
  - zkVM program that verifies multiple range proofs and aggregates them into a single `AggregationOutputs` struct:
    - Chains `BootInfoStruct` entries end‑to‑end, checking `l2PostRoot`/`l2PreRoot` links and consistent `rollupConfigHash`.
    - Verifies each range proof via `sp1_lib::verify::verify_sp1_proof`.
    - Checks an L1 header chain that covers all `l1Head` values referenced by the ranges.
    - Produces final `AggregationOutputs` that compose into the superblock proof pipeline.
- **Fault‑proof / validity harness (`fault-proof`, `validity`)**
  - Rust host binaries and scripts that:
    - fetch L2 and L1 data (headers, outputs, DA) from RPC,
    - construct witnesses (`DefaultWitnessData`, `AggregationInputs`),
    - call `cargo prove` to build/execute zkVM programs and extract public values + proofs.
- **Bindings / utils (`bindings`, `utils`)**
  - Shared types (e.g., `BootInfoStruct`, `AggregationInputs`, `AggregationOutputs`) that must match:
    - the Publisher’s proof pipeline,
    - the superblock prover’s public‑value encoding,
    - Solidity structs in the L1 contracts in this repo (see `contracts/src/lib/Types.sol` and friends).


## Range Program (per‑chain L2 validity)

Key file: `programs/range/ethereum/src/main.rs`

- Entry point:
  - `#![no_main]` with `sp1_zkvm::entrypoint!(main);`
  - Reads zkVM stdin: `witness_rkyv_bytes` encoding `DefaultWitnessData`.
  - Uses `ETHDAWitnessExecutor` to replay the L2 range against Ethereum DA.
  - Delegates to `run_range_program(executor, witness_data)` from `op_succinct_range_utils`.
- Witness contents (high‑level):
  - L2 blocks (headers + transactions) for the range.
  - L1 DA information (blob commitments / calldata) needed to reconstruct the batch.
  - Rollup configuration, genesis state, and any additional context needed by the STF.
- Public values:
  - The range program commits to a `BootInfoStruct` that captures:
    - `l2PreRoot` / `l2PostRoot`
    - `l2BlockNumber` at the end of the range
    - `l1Head` (L1 checkpoint) and `rollupConfigHash`
    - `mailboxRoot` (latest cross‑rollup mailbox root for this range)

These public values are later verified and aggregated by the aggregation program and fed into the superblock prover.


## Aggregation Program (multi‑range aggregation)

Key file: `programs/aggregation/src/main.rs`

- Input:
  - `AggregationInputs`:
    - list of `BootInfoStruct` entries for each proven range,
    - verifier key for the range program (`multi_block_vkey`),
    - latest L1 checkpoint head (`latest_l1_checkpoint_head`),
    - prover address.
  - `headers_bytes`: CBOR‑encoded L1 header chain covering all `l1Head` values.
- Core checks:
  - **Boot info chaining**
    - `prev.l2PostRoot == next.l2PreRoot` for all adjacent boot infos.
    - `prev.rollupConfigHash == next.rollupConfigHash` to ensure all ranges are the same rollup.
  - **Range proof verification**
    - Recomputes the digest of each `BootInfoStruct` and calls `verify_sp1_proof` with the range vkey.
  - **L1 coverage**
    - Walks the header chain backwards from `latest_l1_checkpoint_head`.
    - Ensures each `boot_info.l1Head` appears in the chain (marks them in a `HashMap<B256,bool>`).
  - **Aggregated outputs**
    - Builds a final `BootInfoStruct` that reflects the full range (start pre‑root, end post‑root, final block number, latest L1 head, mailbox root).
    - Packages this plus the range vkey and prover address into `AggregationOutputs`.
- Output:
  - Commits the ABI‑encoded `AggregationOutputs` via `sp1_zkvm::io::commit_slice(&agg_outputs.abi_encode());`
  - These outputs are what the superblock prover and Publisher treat as the per‑rollup proof artifact.


## How This Repo Fits With Superblock Prover & Publisher

- **op-succinct** proves:
  - “Given DA and config, these L2 ranges are valid, and here is an aggregated statement (`AggregationOutputs`) summarizing them.”
- **superblock-prover** uses `AggregationOutputs` as part of its `SuperblockAggOutputs`:
  - It combines per‑rollup `AggregationOutputs` into a superblock‑level statement and generates a Groth16/SP1 proof.
- **Publisher**:
  - Collects `AggregationOutputs` from per‑rollup provers (or directly from op-succinct pipelines),
  - Feeds them into the superblock prover,
  - Submits the resulting proof + public values to the hoodi settlement contracts.

If you modify `BootInfoStruct` or `AggregationOutputs` here, you must also:

- Update the bindings in the Publisher (`x/superblock/proofs/*`) and superblock prover,
- Update Solidity structs / encoders in the L1 contracts that consume these outputs (e.g., `OPSuccinctL2OutputOracle`, `OPSuccinctFaultDisputeGame`, and their shared `Types.AggregationOutputs`),
- Re‑deploy and re‑configure the settlement layer before using the new proofs in stage/prod.


## Local Development & Workflows

Typical workflows:

- Build range and aggregation programs:
  - `cd programs/range/ethereum && cargo prove build`
  - `cd programs/aggregation && cargo prove build`
  - Or from the repo root: `cargo prove build -p range -p aggregation`
- Run the Ethereum range test harness:
  - `cd scripts/prove/tests`
  - `L2_START_BLOCK=… L2_RANGE=… cargo test -p op-succinct-prove --test range_ethereum -- --nocapture`
- Enable detailed guest logging:
  - `cargo prove build -p range --features tracing-subscriber`
  - `RUST_LOG=info,sp1::stdout=info cargo test -p op-succinct-prove --test range_ethereum -- --nocapture`

When debugging:

- Confirm witness construction (host side) before blaming the zkVM programs.
- Inspect `BootInfoStruct` and `AggregationOutputs` values at each stage; mismatches here usually explain prover/Publisher or prover/solidity disagreements.
- If the superblock prover or Publisher reports failed verification, check that:
  - the vkeys used here match the ones wired into the superblock prover,
  - the encoded public values are byte‑for‑byte identical across components.


## When to Touch This Repo

You are in the right place if you are:

- Changing what it means for a rollup range to be “valid” (e.g., new mailbox invariants, extra metadata).
- Extending the aggregated outputs that the superblock prover and Publisher consume.
- Porting the pipeline to a new rollup or DA configuration (new `rollupConfigHash`, new header chains).
- Investigating proof failures that clearly originate from the zkVM side rather than L1 contracts.

Keep this document updated as the range/aggregation semantics evolve so future agents can orient quickly without re‑deriving the architecture from code. 
