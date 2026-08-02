import { access, readdir, readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const websiteRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
);
const outputRoot = path.join(websiteRoot, 'doc_build');
const base = '/ash/';

const requiredFiles = [
  'index.html',
  'en/index.html',
  'guide/index.html',
  'en/guide/index.html',
  'guide/install.html',
  'en/guide/install.html',
  'v0.1.0/index.html',
  'v0.1.0/en/index.html',
  'v0.1.0/guide/index.html',
  'v0.1.0/en/guide/index.html',
  'llms.txt',
  'llms-full.txt',
  'en/llms.txt',
  'en/llms-full.txt',
  'v0.1.0/llms.txt',
  'v0.1.0/en/llms.txt',
  'ash-mark.svg',
  'favicon.svg',
  'social-card.svg',
];

async function collectHtmlFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectHtmlFiles(absolute)));
    } else if (entry.name.endsWith('.html')) {
      files.push(absolute);
    }
  }

  return files;
}

async function collectFilesByExtension(directory, extension) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectFilesByExtension(absolute, extension)));
    } else if (entry.name.endsWith(extension)) {
      files.push(absolute);
    }
  }

  return files;
}

for (const file of requiredFiles) {
  await access(path.join(outputRoot, file));
}

const homepages = {
  'index.html': [
    '<html lang="zh"',
    'AI Native',
    '看 ASH 完成一次代码任务',
    'ash-install',
    'ash-terminal-demo',
    'data-phase="project"',
    'ash-command-tour',
    'ash-symbol-grid',
    'ash-symbol-note-project',
    'role="tooltip"',
    '悬停或聚焦符号查看批注',
    '字段注释',
    '07-project.ason',
    '×64#2',
    '⋯18',
    'running 180 tests',
    'CANONICAL ASON',
    'A3S-Lab/ash/main/install.sh',
  ],
  'en/index.html': [
    '<html lang="en"',
    'AI Native',
    'Watch ASH complete a coding task',
    'ash-install',
    'ash-terminal-demo',
    'data-phase="project"',
    'ash-command-tour',
    'ash-symbol-grid',
    'ash-symbol-note-project',
    'role="tooltip"',
    'Hover or focus a symbol for its note',
    'Field notes',
    '07-project.ason',
    '×64#2',
    '⋯18',
    'running 180 tests',
    'CANONICAL ASON',
    'A3S-Lab/ash/main/install.sh',
  ],
  'v0.1.0/index.html': [
    'V0.1.0 · 源码检查点',
    'd8756614ad6a54128336f50a6a52fcb6f92d1305',
  ],
  'v0.1.0/en/index.html': [
    'V0.1.0 · SOURCE CHECKPOINT',
    'd8756614ad6a54128336f50a6a52fcb6f92d1305',
  ],
  'guide/benchmarks.html': [
    'schema 14',
    '二十二个场景',
    'io-spill-saturated-compute',
    'fragmented',
    'bursty',
    '×N#K',
    '⋯N',
  ],
  'en/guide/benchmarks.html': [
    'Schema 14',
    'twenty-two scenarios',
    'io-spill-saturated-compute',
    'fragmented',
    'bursty',
    '×N#K',
    '⋯N',
  ],
};

for (const [homepage, markers] of Object.entries(homepages)) {
  const html = await readFile(path.join(outputRoot, homepage), 'utf8');
  for (const marker of markers) {
    if (!html.includes(marker)) {
      throw new Error(`${homepage} is missing homepage marker: ${marker}`);
    }
  }
}

const javaScriptFiles = await collectFilesByExtension(
  path.join(outputRoot, 'static/js'),
  '.js',
);
const javaScript = (
  await Promise.all(javaScriptFiles.map((file) => readFile(file, 'utf8')))
).join('\n');
for (const marker of [
  'A3S-Lab/ash/main/install.ps1',
  'cargo install --git https://github.com/A3S-Lab/ash --locked a3s-ash',
]) {
  if (!javaScript.includes(marker)) {
    throw new Error(`Built JavaScript is missing install marker: ${marker}`);
  }
}

async function resolvesToBuiltFile(relativeReference) {
  const decoded = decodeURIComponent(relativeReference);
  const candidates =
    decoded === '' || decoded.endsWith('/')
      ? [path.join(decoded, 'index.html')]
      : [decoded, `${decoded}.html`, path.join(decoded, 'index.html')];

  for (const candidate of candidates) {
    const outputPath = path.resolve(outputRoot, candidate);
    if (
      outputPath !== outputRoot &&
      !outputPath.startsWith(`${outputRoot}${path.sep}`)
    ) {
      continue;
    }
    try {
      if ((await stat(outputPath)).isFile()) return true;
    } catch {
      // Try the next supported output form.
    }
  }
  return false;
}

const brokenReferences = [];
const htmlFiles = await collectHtmlFiles(outputRoot);
const referencePattern = /(?:href|src)="([^"]+)"/g;

for (const htmlFile of htmlFiles) {
  const html = await readFile(htmlFile, 'utf8');
  for (const [, rawReference] of html.matchAll(referencePattern)) {
    if (
      rawReference.startsWith('#') ||
      rawReference.startsWith('data:') ||
      rawReference.startsWith('mailto:') ||
      /^[a-z]+:\/\//i.test(rawReference)
    ) {
      continue;
    }
    if (rawReference.startsWith('/') && !rawReference.startsWith(base)) {
      brokenReferences.push(
        `${path.relative(outputRoot, htmlFile)} -> ${rawReference} (outside ${base})`,
      );
      continue;
    }
    if (!rawReference.startsWith(base)) continue;

    const withoutBase = rawReference
      .slice(base.length)
      .split(/[?#]/, 1)[0]
      .replace(/\/+/g, '/');
    if (!(await resolvesToBuiltFile(withoutBase))) {
      brokenReferences.push(
        `${path.relative(outputRoot, htmlFile)} -> ${rawReference}`,
      );
    }
  }
}

if (brokenReferences.length) {
  throw new Error(
    `Built-site reference check failed:\n${brokenReferences
      .map((reference) => `  - ${reference}`)
      .join('\n')}`,
  );
}

console.log(
  `Built-site routes and references verified across ${htmlFiles.length} HTML pages.`,
);
