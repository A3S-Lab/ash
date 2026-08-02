import { useLang, useSite, useVersion, withBase } from '@rspress/core/runtime';
import { AshCommandWalkthrough } from './AshCommandWalkthrough';
import { AshTerminalDemo } from './AshTerminalDemo';
import { InstallSwitcher } from './InstallSwitcher';

type Locale = 'zh' | 'en';

const copy = {
  zh: {
    eyebrow: '开源 · RUST · CODING AGENT 优先',
    titleLead: 'AI Native',
    titleAccent: 'Shell.',
    subtitle:
      '用一条类型化命令完成搜索、读取、并行执行、补丁、测试与证据提取。',
    start: '阅读文档',
    architectureAction: '观看命令漫游',
    github: 'GitHub',
    commandLabel: '运行 ASH 任务',
    command: 'ash run < .ash/task.ason',
    installLabel: '安装 ash',
    status: '预发布 · 暂无签名二进制',
    runtimeLabel: '执行拓扑',
    request: 'ASH/1 REQUEST',
    governor: 'HIERARCHICAL GOVERNOR',
    io: 'TOKIO I/O PLANE',
    cpu: 'RAYON CPU PLANE',
    merge: 'STABLE MERGE',
    result: 'CANONICAL ASON',
    metricToken: 'ASON / 行式 JSON',
    metricTokenValue: '62%',
    metricTests: 'Rust 测试',
    metricTestsValue: '135',
    metricTargets: '原生目标',
    metricTargetsValue: '6',
    signalProtocol: 'g · r · b · p · x · s · h',
    signalParallel: '并行执行 · 稳定归并',
    signalAson: 'ASON · 按需投影',
    signalTargets: 'Linux · macOS · Windows',
    whyEyebrow: '命令漫游',
    whyTitle: '看 ASH 完成一次代码任务',
    whyBody:
      '向下滚动，终端会跟随当前步骤切换。每一步都展示真实命令、规范请求、紧凑结果和字段注释。',
    tourHint: '滚动选择步骤，也可以点击任一步直接查看。',
    architectureEyebrow: '运行时',
    architectureTitle: 'Tokio 与 Rayon，共用一套预算',
    architectureBody:
      'Tokio 处理进程、管道、RPC、超时与取消；Rayon 处理搜索、哈希、Diff 和归约。Governor 限制全局、会话和请求并发。',
    asonEyebrow: '输出格式',
    asonTitle: 'ASON：紧凑、稳定、可取回',
    asonBody:
      '同构记录按列编码，路径进入字典，大值保留为引用。稳定归并让相同输入得到相同输出。',
    asonLink: '查看 ASON 格式',
    safetyEyebrow: '权限',
    safetyTitle: '高风险操作需要一次性 Permit',
    safetyBody:
      'Permit 绑定会话、动作、策略和过期时间。文件事务带摘要校验、日志与回滚。',
    deliveryEyebrow: '平台',
    deliveryTitle: 'Linux、macOS 与 Windows',
    deliveryBody:
      'x86-64 与 ARM64 使用同一协议。发布流程校验签名、SBOM、来源证明、安装、更新和回滚。',
    installEyebrow: '安装 / 本机',
    installTitle: '选择平台并复制命令',
    installBody:
      '当前为预发布版本。Cargo 可从源码安装；Release 安装器会在签名二进制发布前退出。',
    installDocs: '查看安装说明',
    checkpoint: 'V0.1.0 · 源码检查点',
    ctaEyebrow: '文档与源码',
    ctaTitle: '从协议、CLI 或源码开始。',
    ctaBody: '接口说明、架构决策和实现都在仓库中。',
    ctaPrimary: '打开文档',
    ctaSecondary: '浏览 GitHub',
    footerDescription: '面向 Coding Agent 的开源 Shell。',
    footerDocs: '文档',
    footerProtocol: 'ASH/1 协议',
    footerSource: '源码',
    footerLicense: 'MIT 许可 · Rust 构建',
  },
  en: {
    eyebrow: 'OPEN SOURCE · RUST · CODING AGENT FIRST',
    titleLead: 'AI Native',
    titleAccent: 'Shell.',
    subtitle:
      'One typed command for search, read, parallel work, patching, tests, and evidence retrieval.',
    start: 'Read the docs',
    architectureAction: 'Watch the command tour',
    github: 'GitHub',
    commandLabel: 'Run an ASH task',
    command: 'ash run < .ash/task.ason',
    installLabel: 'Install ash',
    status: 'PRE-RELEASE · NO SIGNED BINARY',
    runtimeLabel: 'Execution topology',
    request: 'ASH/1 REQUEST',
    governor: 'HIERARCHICAL GOVERNOR',
    io: 'TOKIO I/O PLANE',
    cpu: 'RAYON CPU PLANE',
    merge: 'STABLE MERGE',
    result: 'CANONICAL ASON',
    metricToken: 'ASON / row JSON',
    metricTokenValue: '62%',
    metricTests: 'Rust tests',
    metricTestsValue: '135',
    metricTargets: 'Native targets',
    metricTargetsValue: '6',
    signalProtocol: 'g · r · b · p · x · s · h',
    signalParallel: 'PARALLEL RUN · STABLE MERGE',
    signalAson: 'ASON · PROJECT ON DEMAND',
    signalTargets: 'LINUX · MACOS · WINDOWS',
    whyEyebrow: 'COMMAND WALKTHROUGH',
    whyTitle: 'Watch ASH complete a coding task',
    whyBody:
      'Scroll to move the terminal through each step. Every stage shows the real command, canonical request, compact result, and field-level notes.',
    tourHint:
      'Scroll to select a step, or click any step to inspect it directly.',
    architectureEyebrow: 'RUNTIME',
    architectureTitle: 'Tokio and Rayon share one budget',
    architectureBody:
      'Tokio handles processes, pipes, RPC, deadlines, and cancellation. Rayon handles search, hashing, diffs, and reduction. The governor bounds global, session, and request concurrency.',
    asonEyebrow: 'OUTPUT FORMAT',
    asonTitle: 'ASON: compact, stable, retrievable',
    asonBody:
      'Homogeneous records use columns, paths use dictionaries, and large values remain available by reference. Stable merge keeps output deterministic.',
    asonLink: 'Read the ASON format',
    safetyEyebrow: 'PERMISSIONS',
    safetyTitle: 'Risky actions require a one-time permit',
    safetyBody:
      'A permit binds the session, action, policy, and expiry. File transactions add digest checks, a journal, and rollback.',
    deliveryEyebrow: 'PLATFORMS',
    deliveryTitle: 'Linux, macOS, and Windows',
    deliveryBody:
      'x86-64 and ARM64 use the same protocol. Releases verify signatures, SBOM, provenance, installation, updates, and rollback.',
    installEyebrow: 'INSTALL / LOCAL',
    installTitle: 'Choose a platform and copy the command',
    installBody:
      'This is a pre-release. Cargo can build from source; release installers exit until signed binaries are available.',
    installDocs: 'Read installation notes',
    checkpoint: 'V0.1.0 · SOURCE CHECKPOINT',
    ctaEyebrow: 'DOCS AND SOURCE',
    ctaTitle: 'Start with the protocol, CLI, or source.',
    ctaBody:
      'Interfaces, architecture decisions, and implementation are public.',
    ctaPrimary: 'Open documentation',
    ctaSecondary: 'Browse GitHub',
    footerDescription: 'An open-source shell for coding agents.',
    footerDocs: 'Documentation',
    footerProtocol: 'ASH/1 protocol',
    footerSource: 'Source',
    footerLicense: 'MIT licensed · Built in Rust',
  },
};

function ArrowIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 16 16">
      <path d="M3 8h9M8.5 3.5 13 8l-4.5 4.5" />
    </svg>
  );
}

function RuntimeDiagram({ labels }: { labels: (typeof copy)[Locale] }) {
  return (
    <div className="ash-runtime" aria-label={labels.runtimeLabel}>
      <header>
        <span aria-hidden="true" />
        {labels.runtimeLabel}
        <small>LIVE / BOUNDED</small>
      </header>
      <div className="ash-runtime-request">{labels.request}</div>
      <div className="ash-runtime-line" />
      <div className="ash-runtime-governor">{labels.governor}</div>
      <div className="ash-runtime-fork" aria-hidden="true">
        <span />
        <i />
        <span />
      </div>
      <div className="ash-runtime-planes">
        <div>
          <span>IO</span>
          <strong>{labels.io}</strong>
          <small>rpc · process · pipe · cancel</small>
        </div>
        <div>
          <span>CPU</span>
          <strong>{labels.cpu}</strong>
          <small>search · hash · diff · reduce</small>
        </div>
      </div>
      <div className="ash-runtime-merge">
        <span>{labels.merge}</span>
        <strong>{labels.result}</strong>
      </div>
    </div>
  );
}

function MarkdownHome({ locale }: { locale: Locale }) {
  const labels = copy[locale];
  return (
    <main>
      <h1>
        {labels.titleLead} {labels.titleAccent}
      </h1>
      <p>{labels.subtitle}</p>
      <h2>{labels.installLabel}</h2>
      <pre>
        <code>{`curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh`}</code>
      </pre>
      <h2>{labels.whyTitle}</h2>
      <p>{labels.whyBody}</p>
      <h2>{labels.architectureTitle}</h2>
      <p>{labels.architectureBody}</p>
      <h2>{labels.asonTitle}</h2>
      <p>{labels.asonBody}</p>
      <h2>{labels.safetyTitle}</h2>
      <p>{labels.safetyBody}</p>
    </main>
  );
}

export function HomeLayout() {
  const locale: Locale = useLang() === 'zh' ? 'zh' : 'en';
  const labels = copy[locale];
  const version = useVersion();
  const { site } = useSite();
  const isCheckpoint = version === 'v0.1.0';
  const sourceRevision = isCheckpoint
    ? 'd8756614ad6a54128336f50a6a52fcb6f92d1305'
    : 'main';
  const routePrefix = [
    version && version !== site.multiVersion.default ? version : '',
    locale !== site.lang ? locale : '',
  ]
    .filter(Boolean)
    .join('/');
  const route = (pathname: string) => {
    const normalized = pathname.replace(/^\/+/, '');
    return withBase(`/${[routePrefix, normalized].filter(Boolean).join('/')}`);
  };

  if (import.meta.env.SSG_MD) {
    return <MarkdownHome locale={locale} />;
  }

  const signals = [
    labels.signalProtocol,
    labels.signalParallel,
    labels.signalAson,
    labels.signalTargets,
  ];

  return (
    <main className="ash-home" data-lang={locale}>
      <section className="ash-hero" aria-labelledby="ash-hero-title">
        <div className="ash-hero-grid">
          <div className="ash-hero-copy">
            <span className="ash-eyebrow">
              <i aria-hidden="true" />
              {labels.eyebrow}
            </span>
            <h1 id="ash-hero-title">
              <span>{labels.titleLead}</span> <em>{labels.titleAccent}</em>
            </h1>
            <p>{labels.subtitle}</p>
            <div className="ash-hero-actions">
              <a
                className="ash-button ash-button-primary"
                href={route('/guide/')}
              >
                {labels.start}
                <ArrowIcon />
              </a>
              <a className="ash-button" href="#core">
                {labels.architectureAction}
              </a>
            </div>
            <div className="ash-hero-command" aria-label={labels.commandLabel}>
              <span>$</span>
              <code>{labels.command}</code>
              <i aria-hidden="true" />
            </div>
          </div>
          <AshTerminalDemo locale={locale} />
        </div>
      </section>

      <section className="ash-signal-strip" aria-label="ash project facts">
        <div>
          {signals.map((signal, index) => (
            <span key={signal}>
              <i aria-hidden="true">{String(index + 1).padStart(2, '0')}</i>
              {signal}
            </span>
          ))}
        </div>
      </section>

      <section className="ash-command-section" id="core">
        <div className="ash-section ash-command-section-inner">
          <header className="ash-section-header">
            <div>
              <span>{labels.whyEyebrow}</span>
              <h2>{labels.whyTitle}</h2>
            </div>
            <p>{labels.whyBody}</p>
          </header>
          <div className="ash-command-tour-hint">
            <span aria-hidden="true">↓</span>
            {labels.tourHint}
          </div>
          <AshCommandWalkthrough locale={locale} />
        </div>
      </section>

      <section className="ash-architecture" id="architecture">
        <div className="ash-section ash-architecture-inner">
          <header className="ash-section-header">
            <div>
              <span>{labels.architectureEyebrow}</span>
              <h2>{labels.architectureTitle}</h2>
            </div>
            <p>{labels.architectureBody}</p>
          </header>
          <div className="ash-architecture-stage">
            <RuntimeDiagram labels={labels} />
            <div className="ash-budget-stack">
              <span>HOST BUDGET</span>
              <span>SESSION BUDGET</span>
              <span>REQUEST BUDGET</span>
              <span>ACTION PERMIT</span>
            </div>
          </div>
        </div>
      </section>

      <section className="ash-section ash-ason" id="ason">
        <header className="ash-section-header">
          <div>
            <span>{labels.asonEyebrow}</span>
            <h2>{labels.asonTitle}</h2>
          </div>
          <p>{labels.asonBody}</p>
        </header>
        <div className="ash-ason-stage">
          <div className="ash-code-card">
            <header>
              <span>ASON / CANONICAL</span>
              <small>columns + refs</small>
            </header>
            <pre>{`s:0\na:search\nd{p}:\n0:"src/runtime.rs"\nr{p,l,c}:\n0,42,governor\n0,88,stable_merge\nz{shown,total}:\n2,17`}</pre>
          </div>
          <div className="ash-ason-score">
            <span>CL100K / O200K</span>
            <strong>0.62×</strong>
            <small>compact row-object JSON</small>
            <a href={route('/guide/ason.html')}>
              {labels.asonLink}
              <ArrowIcon />
            </a>
          </div>
        </div>
      </section>

      <section className="ash-section ash-boundaries">
        <article>
          <span>{labels.safetyEyebrow}</span>
          <h2>{labels.safetyTitle}</h2>
          <p>{labels.safetyBody}</p>
          <a href={route('/guide/security.html')}>
            SECURITY
            <ArrowIcon />
          </a>
        </article>
        <article>
          <span>{labels.deliveryEyebrow}</span>
          <h2>{labels.deliveryTitle}</h2>
          <p>{labels.deliveryBody}</p>
          <div className="ash-targets">
            <small>linux</small>
            <small>macos</small>
            <small>windows</small>
            <i>x86-64 + arm64</i>
          </div>
        </article>
      </section>

      <section className="ash-section ash-quickstart" id="install">
        <div className="ash-quickstart-copy">
          <span className="ash-section-eyebrow">{labels.installEyebrow}</span>
          <h2>{labels.installTitle}</h2>
          <p>{labels.installBody}</p>
          <a href={route('/guide/install.html')}>
            {labels.installDocs}
            <ArrowIcon />
          </a>
          <small>{isCheckpoint ? labels.checkpoint : labels.status}</small>
        </div>
        <InstallSwitcher locale={locale} revision={sourceRevision} />
      </section>

      <section className="ash-cta" aria-labelledby="ash-cta-title">
        <div className="ash-cta-mark" aria-hidden="true">
          <span>&gt;</span>
          <i />
        </div>
        <div>
          <span>{labels.ctaEyebrow}</span>
          <h2 id="ash-cta-title">{labels.ctaTitle}</h2>
          <p>{labels.ctaBody}</p>
          <div className="ash-cta-actions">
            <a className="ash-button ash-button-light" href={route('/guide/')}>
              {labels.ctaPrimary}
              <ArrowIcon />
            </a>
            <a
              className="ash-button ash-button-outline"
              href="https://github.com/A3S-Lab/ash"
            >
              {labels.ctaSecondary}
            </a>
          </div>
        </div>
      </section>

      <footer className="ash-footer">
        <div className="ash-footer-inner">
          <div className="ash-footer-brand">
            <a href={route('/')}>
              <span aria-hidden="true">&gt;_</span>
              <strong>ash</strong>
            </a>
            <p>{labels.footerDescription}</p>
          </div>
          <div className="ash-footer-column">
            <b>{labels.footerDocs}</b>
            <a href={route('/guide/')}>{labels.footerDocs}</a>
            <a href={route('/guide/protocol.html')}>{labels.footerProtocol}</a>
          </div>
          <div className="ash-footer-column">
            <b>{labels.footerSource}</b>
            <a href="https://github.com/A3S-Lab/ash">GitHub</a>
            <a href="https://github.com/A3S-Lab">A3S Lab</a>
          </div>
        </div>
        <div className="ash-footer-base">
          <span>© {new Date().getFullYear()} A3S Lab</span>
          <span>{labels.footerLicense}</span>
          <span>RUST / ASYNC / OPEN</span>
        </div>
      </footer>
    </main>
  );
}
