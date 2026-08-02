# Task corpus v1

`manifest.json` defines cross-platform task objectives, limits, expected output,
expected file content, and native-shell baselines. `lock.json` is generated from
that manifest plus the fixture trees; it binds each initial and expected final
tree by SHA-256.

This corpus establishes reproducible native-shell baselines. It does not contain
or claim Coding Agent results. Model, prompt, retry, and success evidence belongs
in a later versioned report.
