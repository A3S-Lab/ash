# Paired Coding Agent trace v1

This directory defines the provider-neutral input accepted by the benchmark
runner. A trace is evidence captured by an external model adapter; it is not a
prompt template, a simulated model, or a checked-in score.

`schema.json` describes the serial shape. The Rust validator is authoritative
for cross-field rules that JSON Schema cannot express.

## Experiment contract

One trace binds the exact task manifest and lock, driver binary or source,
provider, model revision, sampling configuration, shared primer, arm-specific
primer, platform, architecture, and raw provider-usage records. Each repetition
contains an `ash` run followed by a `native-shell` run with the same seed and
the same randomized task order. Every task appears exactly once in each arm.

An adapter records only model-visible text:

- `model_output` is the exact request or script emitted by the model;
- `tool_result_sha256` binds the exact result returned to the model;
- `final_stdout` and `final_stderr` are the model's final semantic answer;
- provider usage separates input, cached input, visible output, and optional
  hidden reasoning tokens, and binds the provider's raw usage object by SHA-256.

For the ASH arm, a `request` attempt must be one canonical ASH/1 ASON request.
The runner decodes it, enforces the task's declared operations and budgets, and
executes it through `ExecutionSession`. A malformed output is recorded as
`invalid-request` and receives exactly:

```text
e:invalid-request
```

A valid request outside the exposed task policy is `policy-rejected` and
receives exactly:

```text
e:policy-rejected
```

For the native arm, every attempt is a `request` whose `model_output` is a
script for `sh` on Linux/macOS or non-interactive PowerShell on Windows. The
runner clears the child environment, restores only the platform variables
needed to find and start tools (including Windows system-module discovery),
and returns this exact normalized envelope:

```text
exit:<code|signal|timeout|output-limit>
stdout:<UTF-8-byte-count>
<stdout with LF newlines>stderr:<UTF-8-byte-count>
<stderr with LF newlines>
```

Each stream is captured only up to the task ceiling while the rest is drained;
the raw captured prefixes are hashed separately in the report. The command can
still access host resources allowed by the operating system. Native replay is
therefore deliberately unavailable without the explicit command-line opt-in.
The report keeps failed attempts separate from retries: a failure is charged
immediately, while a retry is counted only when another model action follows it.

## Validate and replay

Structural validation is read-only:

```sh
cargo run -p a3s-ash-bench --locked -- \
  --validate-agent-trace ./trace.json
```

Paired replay executes model-selected native scripts and must be explicitly
authorized by the operator:

```sh
cargo run -p a3s-ash-bench --release --locked -- \
  --agent-trace ./trace.json --allow-native-agent-exec > report.json
```

Replay starts every task from an isolated copy of the locked fixture. It checks
every tool-result hash, semantic answer, expected file, and complete visible
tree. Failed tasks remain in the report. Provider input plus visible output is
the primary model-specific total; cached input remains charged and hidden
reasoning is reported but excluded. The independent normalized payload counts
the primers, objectives, tool results, model requests, and final output under
both pinned tokenizers.

Replay proves internal consistency, not provider identity. The report therefore
marks provenance as `external-self-attested-trace` and
`provider_attestation_verified: false`. A published experiment must ship the
adapter source or binary digest and whatever provider-side audit evidence is
available; the runner never invents an attestation.

Before the first supported release this schema may change together with the
runner and documentation. A report may claim `agent_results: true` only after a
real adapter produces a strict `model-selected-trace`; deterministic plans keep
their existing `agent_results: false` label.
