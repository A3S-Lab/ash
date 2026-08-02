# Task corpus v1

`manifest.json` defines seven cross-platform task objectives, limits, expected
output, expected file content, declarative ASH plans, and native-shell
baselines. `lock.json` is generated only after both executors reach the same
state; it binds the manifest plus each initial and expected final visible tree
by SHA-256.

`--tasks` executes each plan through the production ASH session and separately
executes the current platform baseline. It counts every canonical ASON request
and response, but the plans are hand-authored and deterministic. This corpus
therefore establishes tool-protocol denominators and correctness, not Coding
Agent results. Model, prompt, retry, and model-selected trace evidence belongs
in a later versioned report. The strict paired input and replay rules for that
report are defined in [`../../agents/v1/`](../../agents/v1/README.md).
