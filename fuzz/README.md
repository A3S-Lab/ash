# Fuzzing

ASH keeps four bounded libFuzzer targets in a separate locked Cargo workspace.

| Target | Boundary |
| --- | --- |
| `ason_decode` | UTF-8 ASON parse, canonical round trip, handshake, and typed request decoding |
| `frame_decode` | Declared frame length, truncation, canonical payload consumption, and typed decoding |
| `update_metadata` | Arbitrary manifest/signature bytes at the cryptographic verification boundary |
| `signed_update_metadata` | Validly signed, input-driven version, rollback, sequence, protocol, target, and artifact semantics |

The signed target deliberately crosses the signature gate. Successful samples additionally prove deterministic decisions, same-sequence/same-digest acceptance, sequence rollback and equivocation rejection, and post-signing tamper rejection. Its shared property module has an ordinary 448-case test matrix, so stable CI executes the oracle even where a local sanitizer runtime is unavailable.

## Local gates

```sh
cargo check --manifest-path fuzz/Cargo.toml --bins --locked
cargo clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path fuzz/Cargo.toml --locked
cargo +nightly-2026-07-31 fuzz run signed_update_metadata \
  fuzz/corpus/signed_update_metadata -- \
  -runs=1000 -max_len=65536 -rss_limit_mb=2048 -timeout=5
```

The repository pins `cargo-fuzz 0.13.2` and nightly `2026-07-31`. A local libFuzzer run also needs the platform's sanitizer runtime. The authoritative sustained job runs on Ubuntu with AddressSanitizer.

## Sustained evidence

`.github/workflows/fuzz.yml` runs every Monday and Thursday. Each target receives 600 seconds by default. A manually dispatched run may select 60, 600, or 1,800 seconds.

Only scheduled or explicitly dispatched repository workflows can update the evolving corpus cache. Every run restores the newest target-specific corpus, merges checked-in regression seeds, and rejects links, inputs above 64 KiB, more than 8,192 files, or more than 128 MiB before fuzzing. A successful run stages its evolved corpus under a unique cache key for the next run.

The cache is an accelerator, not evidence. Each run independently uploads, for 90 days:

- the exact final corpus with per-file SHA-256;
- every libFuzzer crash, timeout, or OOM artifact;
- the raw log and its SHA-256;
- `summary.json`, bound to the source commit, toolchain, sanitizer, random seed, limits, timestamps, exit status, and final statistics.

GitHub records a SHA-256 for the uploaded evidence bundle in the job summary. The summary contract is [evidence-schema.json](./evidence-schema.json). `scripts/run-fuzz-evidence.ps1` refuses to overwrite an evidence directory and can reproduce the same layout locally.

A passing bounded run means only that its recorded executions found no failure. Release readiness still requires accumulated duration, review of every retained finding, and the separate deterministic release fuzz gate.
