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
  symbolAlgebra,
  switcher,
  alignedStyles,
] = await Promise.all([
  text('Cargo.toml'),
  text('install.sh'),
  text('install.ps1'),
  text('.github/workflows/release.yml'),
  text('website/rspress.config.ts'),
  text('website/theme/components/HomeLayout.tsx'),
  text('website/theme/components/AshSymbolAlgebra.tsx'),
  text('website/theme/components/InstallSwitcher.tsx'),
  text('website/theme/a3s-aligned.css'),
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

requireIncludes(home, "metricTestsValue: '135'", 'Homepage evidence');
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
