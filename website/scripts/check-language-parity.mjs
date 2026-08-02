import { readdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const websiteRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
);
const docsRoot = path.join(websiteRoot, 'docs');
const snapshots = JSON.parse(
  await readFile(path.join(websiteRoot, 'version-snapshots.json'), 'utf8'),
);
const versions = [
  snapshots.current,
  ...snapshots.archives.map((archive) => archive.version),
];

async function collectFiles(directory, prefix = '') {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];

  for (const entry of entries) {
    const relative = path.posix.join(prefix, entry.name);
    if (entry.isDirectory()) {
      files.push(
        ...(await collectFiles(path.join(directory, entry.name), relative)),
      );
    } else if (/\.(?:json|md|mdx)$/.test(entry.name)) {
      files.push(relative);
    }
  }

  return files.sort();
}

for (const version of versions) {
  const chinese = await collectFiles(path.join(docsRoot, version, 'zh'));
  const english = await collectFiles(path.join(docsRoot, version, 'en'));
  const missingEnglish = chinese.filter((file) => !english.includes(file));
  const missingChinese = english.filter((file) => !chinese.includes(file));

  if (missingEnglish.length || missingChinese.length) {
    throw new Error(
      [
        `${version} language routes are not symmetric.`,
        ...missingEnglish.map((file) => `  missing en/${file}`),
        ...missingChinese.map((file) => `  missing zh/${file}`),
      ].join('\n'),
    );
  }

  for (const required of [
    'index.mdx',
    '_nav.json',
    '_meta.json',
    'guide/index.mdx',
  ]) {
    if (!chinese.includes(required)) {
      throw new Error(
        `${version} is missing required bilingual route: ${required}`,
      );
    }
  }
}

for (const archive of snapshots.archives) {
  if (!/^[0-9a-f]{40}$/.test(archive.sourceCommit)) {
    throw new Error(`${archive.version} must pin a full source commit.`);
  }
  if (archive.supportedBinaryRelease !== false) {
    throw new Error(
      `${archive.version} cannot claim a supported binary release without release evidence.`,
    );
  }
}

console.log(
  `Language parity verified for ${versions.length} documentation versions.`,
);
