# My op-succinct

To compile the range program:

```bash
cd programs/range/ethereum
cargo prove build
```

To compile the aggregation program:

```bash
cd programs/aggregation
cargo prove build
```

To compile both programs at once from the workspace root:

```bash
cargo prove build -p range -p aggregation
```

To run my test:

```bash
cd scripts/prove/tests
RUST_LOG=info cargo test -p op-succinct-prove --test range_ethereum_env -- --nocapture
```

Logging for the range programs now uses `tracing`. Guest log lines are bridged back into the host logger under the
`sp1::stdout`/`sp1::stderr` targets, so configure `RUST_LOG` accordingly. Rebuild the zkVM binary with the
`tracing-subscriber` feature so the guest installs its subscriber, then run the harness:

```bash
cargo prove build -p range --features tracing-subscriber
RUST_LOG=info,sp1::stdout=info cargo test -p op-succinct-prove --test range_ethereum_env -- --nocapture
```

Ask me for the `.env` file if needed.
