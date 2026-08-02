# ash website

The official multilingual website and versioned documentation for
[`ash`](https://github.com/A3S-Lab/ash), built with Rspress.

Chinese is the default language. English routes use `/en/`. The default
documentation line is `next`; `v0.1.0` is a frozen source checkpoint and does
not claim that a supported signed binary has been published.

## Local development

```bash
npm ci
npm run dev
```

The production site is served from `/ash/`. Override `DOCS_BASE` and
`DOCS_ORIGIN` only for another deployment target.

The homepage terminal is a deterministic client-side storyboard of implemented
ASH/1 behavior. It starts only when visible, can be paused or replayed, and
renders the complete evidence frame when reduced motion is requested.

## Verification

```bash
npm run format:check
npm run lint
npm run build
npm run check:site
```

Update both `zh` and `en` pages together. Historical version directories are
immutable after publication; add a new current directory instead of rewriting
an archived contract.

## Dependency audit scope

As of 2026-08-02, `npm audit` reports
[`GHSA-qwww-vcr4-c8h2`](https://github.com/advisories/GHSA-qwww-vcr4-c8h2)
through Rspress's React Router dependency. The advisory applies only to unstable
React Server Components APIs. This website is statically generated and deploys
no RSC server or action endpoint, so that execution path is absent. Keep Rspress
current and remove this exception when its dependency range reaches the patched
React Router release.
