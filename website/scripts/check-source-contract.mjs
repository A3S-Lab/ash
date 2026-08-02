import { readdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const websiteRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
);
const repositoryRoot = path.resolve(websiteRoot, '..');

async function text(relativePath) {
  return readFile(path.join(repositoryRoot, relativePath), 'utf8');
}

async function collectTextFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    if (['doc_build', 'node_modules', '.rspress'].includes(entry.name))
      continue;
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectTextFiles(absolute)));
    } else if (/\.(?:css|json|md|mdx|mjs|ts|tsx)$/.test(entry.name)) {
      files.push(absolute);
    }
  }

  return files;
}

function requireIncludes(source, marker, label) {
  if (!source.includes(marker)) {
    throw new Error(`${label} is missing required contract marker: ${marker}`);
  }
}

const [
  cargo,
  unixInstaller,
  windowsInstaller,
  releaseWorkflow,
  config,
  home,
  commandWalkthrough,
  symbolAlgebra,
  switcher,
  alignedStyles,
  runtimeHarness,
  runtimeMixed,
  runtimeReducer,
  runtimePrimitives,
  referenceOperation,
  repetitionReducer,
  execOperation,
  benchmarkZh,
  benchmarkEn,
] = await Promise.all([
  text('Cargo.toml'),
  text('install.sh'),
  text('install.ps1'),
  text('.github/workflows/release.yml'),
  text('website/rspress.config.ts'),
  text('website/theme/components/HomeLayout.tsx'),
  text('website/theme/components/AshCommandWalkthrough.tsx'),
  text('website/theme/components/AshSymbolAlgebra.tsx'),
  text('website/theme/components/InstallSwitcher.tsx'),
  text('website/theme/a3s-aligned.css'),
  text('benches/runner/src/runtime.rs'),
  text('benches/runner/src/runtime/mixed.rs'),
  text('benches/runner/src/runtime/reducer.rs'),
  text('benches/runner/src/runtime/primitives.rs'),
  text('crates/ash-ops/src/reference.rs'),
  text('crates/ash-ops/src/reducer.rs'),
  text('crates/ash-ops/src/exec.rs'),
  text('website/docs/next/zh/guide/benchmarks.mdx'),
  text('website/docs/next/en/guide/benchmarks.mdx'),
]);
const snapshots = JSON.parse(await text('website/version-snapshots.json'));
const report = JSON.parse(await text('benches/reports/v0.1.0/format.json'));
const workspaceVersion = cargo.match(
  /\[workspace\.package\][\s\S]*?\nversion = "([^"]+)"/,
)?.[1];
const archive = snapshots.archives.find(
  (candidate) => candidate.version === `v${workspaceVersion}`,
);

if (!workspaceVersion || !archive) {
  throw new Error(
    'The workspace version must have a matching documentation snapshot.',
  );
}
if (snapshots.current !== 'next') {
  throw new Error('The moving documentation line must remain next.');
}
if (archive.supportedBinaryRelease !== false) {
  throw new Error(
    'The v0.1.0 checkpoint must not claim a supported binary release.',
  );
}

for (const marker of [
  "lang: 'zh'",
  "default: 'next'",
  "versions: ['next', 'v0.1.0']",
  "localeRedirect: 'never'",
]) {
  requireIncludes(config, marker, 'Rspress configuration');
}

requireIncludes(unixInstaller, 'repository="A3S-Lab/ash"', 'Unix installer');
requireIncludes(
  windowsInstaller,
  "$Repository = 'A3S-Lab/ash'",
  'Windows installer',
);

const releaseTargets = [
  'x86_64-unknown-linux-musl',
  'aarch64-unknown-linux-musl',
  'x86_64-apple-darwin',
  'aarch64-apple-darwin',
  'x86_64-pc-windows-msvc',
  'aarch64-pc-windows-msvc',
];
const installDocs = await Promise.all([
  text('website/docs/next/zh/guide/install.mdx'),
  text('website/docs/next/en/guide/install.mdx'),
]);
for (const target of releaseTargets) {
  requireIncludes(releaseWorkflow, target, 'Release workflow');
  for (const document of installDocs) {
    requireIncludes(document, target, 'Installation documentation');
  }
}

const unixCommand =
  "curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh";
const windowsCommand =
  'irm https://raw.githubusercontent.com/A3S-Lab/ash/main/install.ps1 | iex';
const cargoCommand =
  'cargo install --git https://github.com/A3S-Lab/ash --locked a3s-ash';
for (const command of [unixCommand, windowsCommand, cargoCommand]) {
  requireIncludes(switcher, command, 'Homepage install switcher');
}

requireIncludes(home, "metricTestsValue: '180'", 'Homepage evidence');
requireIncludes(
  home,
  `metricTokenValue: '${report.gates.ason_vs_record_json_cl100k_percent}%'`,
  'Homepage evidence',
);
requireIncludes(home, "metricTargetsValue: '6'", 'Homepage evidence');
requireIncludes(
  home,
  '<AshSymbolAlgebra locale={locale} />',
  'Homepage algebra',
);
for (const marker of [
  "event.pointerType !== 'touch'",
  'setHovered(operator.id)',
  'setHovered(null)',
  'onFocus={() => setFocused(operator.id)}',
  'const closing = selected === operator.id',
  'if (closing) event.currentTarget.blur()',
  'aria-describedby={noteId}',
  'role="tooltip"',
  '悬停或聚焦符号查看批注',
]) {
  requireIncludes(symbolAlgebra, marker, 'Formula hover notes');
}
for (const operator of report.formula_algebra.operators) {
  requireIncludes(
    symbolAlgebra,
    `symbol: '${operator.symbol}'`,
    `Formula operator ${operator.id}`,
  );
}
requireIncludes(
  symbolAlgebra,
  `${report.formula_algebra.candidates.canonical_symbols.cl100k_tokens} / ${report.formula_algebra.candidates.canonical_symbols.o200k_tokens} TOKEN`,
  'Formula benchmark evidence',
);
if (!report.formula_algebra.gates.passed) {
  throw new Error('The checked formula algebra gate must pass.');
}
if (
  report.schema !== 5 ||
  !report.repeated_line_reduction?.gates?.passed ||
  !report.repeated_block_reduction?.gates?.passed ||
  !report.error_focus_reduction?.gates?.passed
) {
  throw new Error('The checked output-reduction token gates must pass.');
}

for (const marker of [
  'schema: 14',
  '"list-recursive"',
  '"search-literal"',
  '"search-regex"',
  '"snapshot-blake3"',
  '"exec-capture-fragmented"',
  '"exec-capture-bursty"',
  'const FRAGMENTED_CHUNKS: &[usize] = &[1, 7, 31, 257, 4_093, 16_384, 65_521]',
  'const BURST_PAUSE_MICROS: u64 = 2_000',
  'capture_profile_descriptor()',
  'require_equivalent_output(&scenarios, "search-literal", "search-regex")',
  'reducer::measure_structured_projection_scenario(',
  'reducer::measure_repeated_line_scenario(&config)',
  'reducer::measure_repeated_block_scenario(&config)',
  'reducer::measure_error_focus_scenario(&config)',
  'primitives::measure_path_dictionary_scenario(&config)',
  'primitives::measure_dag_scenario(nodes, id, &config)',
]) {
  requireIncludes(runtimeHarness, marker, 'Runtime benchmark schema');
}
for (const marker of [
  '"io-spill-idle-compute"',
  '"io-spill-saturated-compute"',
  '"xorshift64-busy-loop"',
  'all-compute-workers-active-at-capture-finish',
  'alternating-paired-order',
  'active_workers != parallelism.compute_workers().get()',
  'speedup_basis_points: None',
]) {
  requireIncludes(runtimeMixed, marker, 'Mixed I/O runtime benchmark');
}
for (const marker of [
  '"ref-project-structured"',
  '"reduce-repeated-lines"',
  '"reduce-repeated-blocks"',
  '"reduce-error-focused"',
  'const ROWS_PER_FIXTURE_FILE: usize = 64',
  'const REPEATED_LINES_PER_FIXTURE_FILE: usize = 512',
  'const REPEATED_BLOCK_LINES: usize = 8',
  'const REPEATED_BLOCK_REPETITIONS: usize = 64',
  'const ERROR_FOCUS_LINES_PER_GROUP: usize = 512',
  'pool.install(|| collapse_repeated_lines(&workload.input))',
  'pool.install(|| collapse_repeated_blocks(&workload.input))',
  'pool.install(|| focus_error_output(&workload.input))',
  'validate_response(&response, workload, reference)',
  'runtime_run(',
]) {
  requireIncludes(runtimeReducer, marker, 'Runtime reducer benchmark');
}
for (const marker of [
  '.par_iter()',
  '.collect::<Result<Vec<_>, OperationError>>()?',
]) {
  requireIncludes(referenceOperation, marker, 'Ordered parallel projection');
}
for (const marker of [
  "const REPEAT_SYMBOL: char = '×';",
  '.par_chunks(PARTITION_LINES)',
  "line.ends_with('\\n') && marker.len() < omitted_bytes",
  'previous.count += run.count',
  'const BLOCK_CANDIDATE_BATCH_LINES: usize = 4_096',
  'const MAX_BLOCK_LINES: usize = 32',
  'pub fn collapse_repeated_blocks(text: &str)',
  'verified_candidate(&layout, index, hashed)',
  'candidate_is_exact(layout, start, candidate)',
  '(1..repetitions).into_par_iter().all(matches)',
  'format!("{REPEAT_SYMBOL}{repetitions}#{block_lines}\\n")',
  "const OMISSION_SYMBOL: char = '⋯';",
  'const ERROR_CONTEXT_BEFORE: usize = 2',
  'const ERROR_CONTEXT_AFTER: usize = 6',
  'pub fn focus_error_output(text: &str)',
  '.map(|line| is_diagnostic_anchor(layout.line(line)))',
  'if omission_marker_len(lines) < source_bytes',
  'format!("{OMISSION_SYMBOL}{lines}\\n")',
]) {
  requireIncludes(repetitionReducer, marker, 'Output reducer');
}
for (const marker of [
  'let line_reduction = collapse_repeated_lines(&normalized_text);',
  'let block_reduction = collapse_repeated_blocks(line_reduction.text());',
  'let error_reduction = focus_error_output(block_reduction.text());',
  'success: false',
  'let finalizing = matches!(stop, Stop::TimedOut | Stop::Cancelled);',
  '.run(move || {',
  'project_captures(',
]) {
  requireIncludes(execOperation, marker, 'Exec compute-plane projection');
}
for (const marker of [
  '"path-dictionary-hot"',
  '"dag-schedule-64"',
  '"dag-schedule-256"',
  '"dag-schedule-1024"',
  'compute_workers: 1',
  'speedup_basis_points: None',
  'parallel_efficiency_basis_points: None',
]) {
  requireIncludes(runtimePrimitives, marker, 'Runtime primitive benchmark');
}
for (const [document, markers, label] of [
  [
    benchmarkZh,
    [
      'schema 14',
      '二十二个场景',
      'io-spill-saturated-compute',
      'fragmented',
      'bursty',
      '⋯N',
    ],
    'Chinese benchmark documentation',
  ],
  [
    benchmarkEn,
    [
      'Schema 14',
      'twenty-two scenarios',
      'io-spill-saturated-compute',
      'fragmented',
      'bursty',
      '⋯N',
    ],
    'English benchmark documentation',
  ],
]) {
  for (const marker of markers) requireIncludes(document, marker, label);
}
for (const marker of [
  '×64#2',
  '⋯18',
  'e{c,q,p,x,a}:',
  'z:11',
  "tags: ['o:x', 'argv', '×N#K', '⋯N', 'retained']",
]) {
  requireIncludes(commandWalkthrough, marker, 'Output-reduction command tour');
}

for (const marker of [
  '--ash-type-caption: 10px',
  '--ash-type-label: 11px',
  '--ash-type-body: 13px',
  '--ash-type-code: 13px',
]) {
  requireIncludes(alignedStyles, marker, 'Homepage typography');
}
const undersizedTypography = [
  ...alignedStyles.matchAll(/font-size:\s*(\d+(?:\.\d+)?)px/g),
]
  .map((match) => Number(match[1]))
  .filter((size) => size < 10);
if (undersizedTypography.length > 0) {
  throw new Error(
    `Homepage typography must remain at least 10px; found: ${undersizedTypography.join(', ')}`,
  );
}

const archiveRoot = path.join(websiteRoot, 'docs', archive.version);
for (const file of await collectTextFiles(archiveRoot)) {
  const source = await readFile(file, 'utf8');
  if (source.includes('github.com/A3S-Lab/ash') && source.includes('/main/')) {
    throw new Error(
      `${path.relative(websiteRoot, file)} links archive content to moving main.`,
    );
  }
}
requireIncludes(
  await readFile(path.join(archiveRoot, 'en/guide/install.mdx'), 'utf8'),
  archive.sourceCommit,
  `${archive.version} installation documentation`,
);

const legacyNames = [`a3${'sh'}`, `t${'son'}`];
for (const file of await collectTextFiles(websiteRoot)) {
  const source = await readFile(file, 'utf8');
  const normalized = source.toLowerCase();
  const forbidden = legacyNames.find((name) => normalized.includes(name));
  if (forbidden) {
    throw new Error(
      `${path.relative(websiteRoot, file)} contains forbidden legacy naming: ${forbidden[0]}`,
    );
  }
}

console.log(
  `Source contract verified for ash ${workspaceVersion}, ${releaseTargets.length} release targets, and ${snapshots.archives.length} frozen snapshot.`,
);
