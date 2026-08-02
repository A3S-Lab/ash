# Format evidence: v0.1.0

This directory contains deterministic format-only evidence generated from `benches/corpus/v1.json`.

Regenerate it with:

```sh
cargo run -p a3s-ash-bench --release --locked -- \
  --write benches/reports/v0.1.0/format.json
```

Verify it byte for byte with:

```sh
cargo run -p a3s-ash-bench --release --locked -- \
  --check benches/reports/v0.1.0/format.json
```

The report compares the same typed values encoded as canonical ASON, compact row-object JSON, and compact columnar JSON. It uses the `cl100k_base` and `o200k_base` vocabularies embedded by the pinned `tiktoken-rs` dependency. This is format evidence, not an agent task-success or runtime performance claim.

For a host-local real-operation search and snapshot scaling report, run `cargo run -p a3s-ash-bench --release --locked -- --runtime`. Canonical outputs must match across worker counts; timing output is intentionally not committed as cross-host evidence.
