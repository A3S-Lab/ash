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

The report compares the same typed values encoded as canonical ASON, compact row-object JSON, and compact columnar JSON. It also measures the six retained-result formulas as a former ASCII wrapper, direct Greek glyphs, direct ASCII letters, and the canonical `/ # ? - | >` operator algebra. Independent deterministic gates run the production `×N` line reducer over 8,192 retained lines, the `×N#K` block reducer over 12,288 retained lines, and the failure-only `⋯N` diagnostic-window reducer over 8,192 retained lines; all projections must remain at or below 5% of source bytes and tokens under both pinned vocabularies. It uses the `cl100k_base` and `o200k_base` vocabularies embedded by the pinned `tiktoken-rs` dependency. This is format and projection evidence, not an agent task-success or runtime performance claim.

For the host-local eighteen-scenario runtime report, build `ash` and run `cargo run -p a3s-ash-bench --release --locked -- --runtime`. Canonical evidence must match across applicable worker counts and repeated single-caller primitive samples; timing output is intentionally not committed as cross-host evidence.

The separate `benches/tasks/v1` seed binds three cross-platform native-shell task definitions and workspace states. Verify its generated lock with `cargo run -p a3s-ash-bench --locked -- --check-task-lock benches/tasks/v1/lock.json`; run the current platform baseline with `--tasks`. It is baseline infrastructure, not a model or `ash` task result.
