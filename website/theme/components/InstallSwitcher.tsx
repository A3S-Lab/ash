import { useState } from 'react';

type Locale = 'zh' | 'en';

type InstallTarget = {
  id: 'linux' | 'macos' | 'windows' | 'source';
  label: string;
  badge: string;
  prompt: string;
  commands: string[];
  architectures: string;
  delivery: 'release' | 'source';
};

const targets: InstallTarget[] = [
  {
    id: 'linux',
    label: 'Linux',
    badge: 'LNX',
    prompt: '$',
    commands: [
      "curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh",
    ],
    architectures: 'x86-64 · ARM64',
    delivery: 'release',
  },
  {
    id: 'macos',
    label: 'macOS',
    badge: 'MAC',
    prompt: '$',
    commands: [
      "curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh",
    ],
    architectures: 'Intel · Apple Silicon',
    delivery: 'release',
  },
  {
    id: 'windows',
    label: 'Windows',
    badge: 'WIN',
    prompt: 'PS›',
    commands: [
      '[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12',
      'irm https://raw.githubusercontent.com/A3S-Lab/ash/main/install.ps1 | iex',
    ],
    architectures: 'x86-64 · ARM64',
    delivery: 'release',
  },
  {
    id: 'source',
    label: 'Cargo',
    badge: 'SRC',
    prompt: '$',
    commands: [
      'cargo install --git https://github.com/A3S-Lab/ash --locked a3s-ash',
    ],
    architectures: 'Rust stable · current source',
    delivery: 'source',
  },
];

const labels = {
  zh: {
    aria: '选择 ash 安装目标',
    copy: '复制',
    copied: '已复制',
    release: '签名发行安装器',
    source: '源码构建',
    releaseNotice:
      '安装器已经过三平台测试；首个签名二进制尚未发布，在此之前会安全失败。',
    sourceNotice: '从当前 main 构建，适合开发验证；它不是受支持的签名发行版。',
  },
  en: {
    aria: 'Choose an ash installation target',
    copy: 'Copy',
    copied: 'Copied',
    release: 'SIGNED RELEASE INSTALLER',
    source: 'SOURCE BUILD',
    releaseNotice:
      'The installer passes all three platform suites. It fails closed until the first signed binary is published.',
    sourceNotice:
      'Builds the current main branch for development validation; this is not a supported signed release.',
  },
} satisfies Record<Locale, Record<string, string>>;

export function InstallSwitcher({
  locale,
  revision = 'main',
}: {
  locale: Locale;
  revision?: string;
}) {
  const [activeId, setActiveId] =
    useState<(typeof targets)[number]['id']>('linux');
  const [copied, setCopied] = useState(false);
  const resolvedTargets = targets.map((target) => ({
    ...target,
    commands: target.commands.map((command) => {
      if (revision === 'main') return command;
      if (target.id === 'source') {
        return `cargo install --git https://github.com/A3S-Lab/ash --rev ${revision} --locked a3s-ash`;
      }
      return command.replace('/ash/main/', `/ash/${revision}/`);
    }),
  }));
  const active =
    resolvedTargets.find((target) => target.id === activeId) ??
    resolvedTargets[0];
  const copy = labels[locale];

  async function copyCommands() {
    try {
      await navigator.clipboard.writeText(active.commands.join('\n'));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  }

  function selectTarget(target: InstallTarget) {
    setActiveId(target.id);
    setCopied(false);
  }

  return (
    <div className="ash-install">
      <div className="ash-install-tabs" role="tablist" aria-label={copy.aria}>
        {resolvedTargets.map((target, index) => {
          const selected = target.id === active.id;
          return (
            <button
              aria-controls="ash-install-panel"
              aria-selected={selected}
              className={selected ? 'is-active' : undefined}
              id={`ash-install-tab-${target.id}`}
              key={target.id}
              onClick={() => selectTarget(target)}
              onKeyDown={(event) => {
                let nextIndex = index;
                if (event.key === 'ArrowRight') nextIndex = index + 1;
                if (event.key === 'ArrowLeft') nextIndex = index - 1;
                if (event.key === 'Home') nextIndex = 0;
                if (event.key === 'End') nextIndex = resolvedTargets.length - 1;
                if (nextIndex === index) return;

                event.preventDefault();
                const normalized =
                  (nextIndex + resolvedTargets.length) % resolvedTargets.length;
                const target = resolvedTargets[normalized];
                selectTarget(target);
                window.requestAnimationFrame(() => {
                  document
                    .getElementById(`ash-install-tab-${target.id}`)
                    ?.focus();
                });
              }}
              role="tab"
              tabIndex={selected ? 0 : -1}
              type="button"
            >
              <span aria-hidden="true">{target.badge}</span>
              <strong>{target.label}</strong>
            </button>
          );
        })}
      </div>

      <div
        aria-labelledby={`ash-install-tab-${active.id}`}
        className="ash-install-panel"
        id="ash-install-panel"
        role="tabpanel"
      >
        <header>
          <span>
            <strong>
              {active.delivery === 'release' ? copy.release : copy.source}
            </strong>
            <small>{active.architectures}</small>
          </span>
          <button
            aria-live="polite"
            className={copied ? 'is-copied' : undefined}
            onClick={copyCommands}
            type="button"
          >
            <span aria-hidden="true">{copied ? '✓' : '⧉'}</span>
            {copied ? copy.copied : copy.copy}
          </button>
        </header>
        <div className="ash-install-code" tabIndex={0}>
          {active.commands.map((command, index) => (
            <div key={`${active.id}-${index}`}>
              <span aria-hidden="true">{active.prompt}</span>
              <code>{command}</code>
            </div>
          ))}
        </div>
        <p
          className={active.delivery === 'release' ? 'is-release' : 'is-source'}
        >
          <span aria-hidden="true" />
          {active.delivery === 'release'
            ? copy.releaseNotice
            : copy.sourceNotice}
        </p>
      </div>
    </div>
  );
}
