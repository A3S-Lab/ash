import * as path from 'node:path';
import { defineConfig } from '@rspress/core';
import type { LanguageRegistration } from '@shikijs/types';

const base = process.env.DOCS_BASE ?? '/ash/';
const siteOrigin = process.env.DOCS_ORIGIN ?? 'https://a3s-lab.github.io';

// Rspack's persistent cache can stall on Windows file locking. Linux CI keeps
// the cache; local Windows builds favor deterministic completion.
if (process.platform === 'win32') {
  process.env.RSPRESS_PERSISTENT_CACHE ??= 'false';
}

const asonLanguage: LanguageRegistration = {
  name: 'ason',
  scopeName: 'source.ason',
  repository: {},
  patterns: [
    {
      match: '^([A-Za-z][A-Za-z0-9_-]*)(\\[[^\\]]+\\])?(\\{[^}]+\\})?(:)',
      captures: {
        1: { name: 'entity.name.tag.ason' },
        2: { name: 'storage.modifier.ason' },
        3: { name: 'storage.type.ason' },
        4: { name: 'punctuation.separator.key-value.ason' },
      },
    },
    {
      match: '"(?:\\\\.|[^"\\\\])*"',
      name: 'string.quoted.double.ason',
    },
    {
      match: '\\b(?:true|false|null)\\b|~',
      name: 'constant.language.ason',
    },
    {
      match: '-?\\b\\d+(?:\\.\\d+)?\\b',
      name: 'constant.numeric.ason',
    },
  ],
};

export default defineConfig({
  root: path.join(__dirname, 'docs'),
  base,
  siteOrigin,
  title: 'ash',
  description:
    'AI Native Shell with typed requests, bounded parallel execution, and ASON output.',
  lang: 'zh',
  icon: '/favicon.svg',
  logo: `${base}ash-mark.svg`,
  logoText: 'ash',
  outDir: 'doc_build',
  llms: true,
  markdown: {
    shiki: {
      langs: ['bash', 'powershell', asonLanguage],
    },
  },
  multiVersion: {
    default: 'next',
    versions: ['next', 'v0.1.0'],
  },
  locales: [
    {
      lang: 'zh',
      label: '简体中文',
      title: 'ash · AI Native Shell',
      description: 'AI Native Shell：类型化请求、有界并行执行与 ASON 输出。',
    },
    {
      lang: 'en',
      label: 'English',
      title: 'ash · AI Native Shell',
      description:
        'AI Native Shell with typed requests, bounded parallel execution, and ASON output.',
    },
  ],
  head: [
    ['meta', { name: 'theme-color', content: '#000000' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:site_name', content: 'ash' }],
    [
      'meta',
      {
        property: 'og:image',
        content: `${siteOrigin}${base}social-card.svg`,
      },
    ],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
    ['link', { rel: 'preconnect', href: 'https://fonts.googleapis.com' }],
    [
      'link',
      {
        rel: 'preconnect',
        href: 'https://fonts.gstatic.com',
        crossorigin: 'anonymous',
      },
    ],
    [
      'link',
      {
        rel: 'stylesheet',
        href: 'https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;700&display=swap',
      },
    ],
    (route) => [
      'link',
      {
        rel: 'canonical',
        href: `${siteOrigin}${base.replace(/\/$/, '')}${route.routePath}`,
      },
    ],
  ],
  themeConfig: {
    darkMode: 'force-dark',
    search: true,
    localeRedirect: 'never',
    enableContentAnimation: true,
    editLink: {
      docRepoBaseUrl: 'https://github.com/A3S-Lab/ash/tree/main/website/docs',
    },
    lastUpdated: true,
    llmsUI: {
      placement: 'outline',
      viewOptions: ['markdownLink', 'chatgpt', 'claude'],
    },
    socialLinks: [
      {
        icon: 'github',
        mode: 'link',
        content: 'https://github.com/A3S-Lab/ash',
      },
    ],
  },
});
