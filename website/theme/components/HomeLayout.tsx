import { useLang, useSite, useVersion, withBase } from '@rspress/core/runtime';
import { AshTerminalDemo } from './AshTerminalDemo';
import { InstallSwitcher } from './InstallSwitcher';

type Locale = 'zh' | 'en';

type Localized = {
  zh: string;
  en: string;
};

const copy = {
  zh: {
    eyebrow: 'OPEN SOURCE · RUST · AGENT FIRST',
    titleLead: '为 Coding Agent 构建的',
    titleAccent: 'AI Native Shell',
    subtitle:
      '用类型化请求替代脚本文本，用有界并行执行替代串行往返，用紧凑 ASON 证据替代冗余终端输出。',
    start: '开始使用',
    protocol: '阅读 ASH/1',
    github: 'GitHub',
    installLabel: '跨平台安装',
    status: 'PRE-RELEASE · NO SIGNED BINARY YET',
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
    metricTargets: '原生发布目标',
    metricTargetsValue: '6',
    whyEyebrow: 'WHY ASH',
    whyTitle: 'Shell 应该直接服务模型上下文',
    whyBody:
      'ash 不追求人类交互兼容性。协议、调度、输出和错误都围绕 Coding Agent 完成任务时的 Token、延迟、确定性和恢复能力设计。',
    architectureEyebrow: 'ONE RUNTIME · TWO PLANES',
    architectureTitle: 'I/O 与 CPU 各自并行，但共享同一个预算',
    architectureBody:
      'Tokio 管理进程、管道、RPC、超时与取消；Rayon 使用固定工作窃取池处理搜索、哈希、Diff 和归约。分层 Governor 阻止批图与内部并行相乘。',
    asonEyebrow: 'LLM-NATIVE EVIDENCE',
    asonTitle: '先压缩结构，再交给模型',
    asonBody:
      'ASON 是 ash 原生设计并实现的结构化格式。同构记录按列编码，路径进入字典，大值转为可检索引用；稳定归并保证相同输入得到逐字节一致输出。',
    asonLink: '查看 ASON 设计',
    safetyEyebrow: 'CAPABILITY BOUNDARY',
    safetyTitle: '执行权限是请求的一部分',
    safetyBody:
      '会话协商最小能力集。高风险动作需要绑定会话、动作、策略与过期时间的一次性 Permit；文件事务带摘要守卫、日志、回滚和重启恢复。',
    deliveryEyebrow: 'CROSS-PLATFORM DELIVERY',
    deliveryTitle: '一个协议，六个原生目标',
    deliveryBody:
      'Linux、macOS、Windows 的 x86-64 与 ARM64 使用同一语义契约。发布流水线要求签名、SBOM、来源证明、干净主机安装、更新与回滚门禁。',
    ctaEyebrow: 'START WITH THE CONTRACT',
    ctaTitle: '让 Agent 发结构化请求，而不是拼 Shell 字符串',
    ctaBody:
      '从安装与 ASH/1 协议开始；当前版本是源码检查点，签名稳定版发布前不会隐藏信任边界。',
    ctaPrimary: '安装指南',
    ctaSecondary: 'CLI 参考',
    footer: 'MIT 开源 · Rust 构建 · Linux / macOS / Windows',
  },
  en: {
    eyebrow: 'OPEN SOURCE · RUST · AGENT FIRST',
    titleLead: 'The shell built for',
    titleAccent: 'Coding Agents',
    subtitle:
      'Replace script text with typed requests, serial round trips with bounded parallel execution, and terminal noise with compact ASON evidence.',
    start: 'Get started',
    protocol: 'Read ASH/1',
    github: 'GitHub',
    installLabel: 'Cross-platform install',
    status: 'PRE-RELEASE · NO SIGNED BINARY YET',
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
    metricTargets: 'Native release targets',
    metricTargetsValue: '6',
    whyEyebrow: 'WHY ASH',
    whyTitle: 'A shell should serve model context directly',
    whyBody:
      'ash does not optimize for human shell compatibility. Its protocol, scheduler, output, and errors target token cost, latency, determinism, and recovery for Coding Agent tasks.',
    architectureEyebrow: 'ONE RUNTIME · TWO PLANES',
    architectureTitle: 'I/O and CPU run in parallel under one budget',
    architectureBody:
      'Tokio owns processes, pipes, RPC, deadlines, and cancellation. A fixed Rayon work-stealing pool handles search, hashing, diffs, and reduction. A hierarchical governor prevents graph width from multiplying nested parallelism.',
    asonEyebrow: 'LLM-NATIVE EVIDENCE',
    asonTitle: 'Compact the structure before the model sees it',
    asonBody:
      'ASON is designed and implemented natively by ash. Homogeneous records become columns, paths enter dictionaries, and large values become retrievable references. Stable merge makes identical input byte-identical at the boundary.',
    asonLink: 'Explore ASON',
    safetyEyebrow: 'CAPABILITY BOUNDARY',
    safetyTitle: 'Execution authority travels with the request',
    safetyBody:
      'Sessions negotiate the least capability set. High-risk actions require one-time permits bound to session, action, policy, and expiry. File transactions add digest guards, journaling, rollback, and restart recovery.',
    deliveryEyebrow: 'CROSS-PLATFORM DELIVERY',
    deliveryTitle: 'One protocol, six native targets',
    deliveryBody:
      'Linux, macOS, and Windows on x86-64 and ARM64 share one semantic contract. Release promotion requires signatures, SBOM, provenance, clean-host install, update, and rollback gates.',
    ctaEyebrow: 'START WITH THE CONTRACT',
    ctaTitle: 'Send typed requests instead of assembling shell strings',
    ctaBody:
      'Start with installation and ASH/1. The current line is a source checkpoint, and the trust boundary remains explicit until a signed stable release exists.',
    ctaPrimary: 'Installation guide',
    ctaSecondary: 'CLI reference',
    footer: 'MIT licensed · Built in Rust · Linux / macOS / Windows',
  },
};

const features = [
  {
    index: '01',
    title: { zh: '类型化操作', en: 'Typed operations' },
    body: {
      zh: 'exec、read、list、search、patch、fs、snapshot 与 batch 都有严格 Schema，不需要脆弱的命令拼接。',
      en: 'exec, read, list, search, patch, fs, snapshot, and batch use strict schemas instead of fragile command assembly.',
    },
    tags: ['ASH/1', 'schema', 'argv'],
  },
  {
    index: '02',
    title: { zh: '可控多核并行', en: 'Governed multicore work' },
    body: {
      zh: '批图、I/O 与 CPU 工作可以重叠；全局、会话和请求预算限制并发、输出、时间与保留证据。',
      en: 'Batch graphs, I/O, and CPU work can overlap while global, session, and request budgets bound concurrency, output, time, and retained evidence.',
    },
    tags: ['Tokio', 'Rayon', 'DAG'],
  },
  {
    index: '03',
    title: { zh: '紧凑可取回证据', en: 'Compact retrievable evidence' },
    body: {
      zh: '输出可以投影、截断并保留完整引用。Agent 先读取必要部分，再按 slice 或 search 精确取回。',
      en: 'Output can be projected, truncated, and retained by reference. Agents read the useful slice first and retrieve more with slice or search.',
    },
    tags: ['ASON', 'ref', 'projection'],
  },
  {
    index: '04',
    title: { zh: '失败可恢复', en: 'Recoverable failure' },
    body: {
      zh: '取消会传播到进程树与批图；文件事务和更新流程在崩溃后恢复，并保留可验证状态。',
      en: 'Cancellation reaches process trees and batch graphs. File transactions and updates recover after crashes with verifiable state.',
    },
    tags: ['cancel', 'journal', 'rollback'],
  },
] satisfies Array<{
  index: string;
  title: Localized;
  body: Localized;
  tags: string[];
}>;

function value(localized: Localized, locale: Locale) {
  return localized[locale];
}

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

  return (
    <main className="ash-home">
      <section className="ash-hero">
        <div className="ash-hero-copy">
          <span className="ash-eyebrow">
            <i aria-hidden="true" />
            {labels.eyebrow}
          </span>
          <h1>
            {locale === 'zh' ? (
              <>
                为 Coding Agent
                <br />
                构建的
              </>
            ) : (
              labels.titleLead
            )}
            <span>{labels.titleAccent}</span>
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
            <a className="ash-button" href={route('/guide/protocol.html')}>
              {labels.protocol}
            </a>
            <a
              className="ash-button ash-button-quiet"
              href="https://github.com/A3S-Lab/ash"
            >
              {labels.github}
            </a>
          </div>
          <div className="ash-install-wrap">
            <div className="ash-install-heading">
              <span>{labels.installLabel}</span>
              <small>
                {isCheckpoint ? 'V0.1.0 · SOURCE CHECKPOINT' : labels.status}
              </small>
            </div>
            <InstallSwitcher locale={locale} revision={sourceRevision} />
          </div>
        </div>
        <div className="ash-hero-visual">
          <div className="ash-orbit ash-orbit-one" aria-hidden="true" />
          <div className="ash-orbit ash-orbit-two" aria-hidden="true" />
          <AshTerminalDemo locale={locale} />
        </div>
      </section>

      <section className="ash-metrics" aria-label="Project evidence">
        <div>
          <strong>{labels.metricTokenValue}</strong>
          <span>{labels.metricToken}</span>
        </div>
        <div>
          <strong>{labels.metricTestsValue}</strong>
          <span>{labels.metricTests}</span>
        </div>
        <div>
          <strong>{labels.metricTargetsValue}</strong>
          <span>{labels.metricTargets}</span>
        </div>
        <a href={route('/guide/benchmarks.html')}>
          <span>REPRODUCIBLE EVIDENCE</span>
          <ArrowIcon />
        </a>
      </section>

      <section className="ash-section ash-why" id="why">
        <header className="ash-section-header">
          <div>
            <span>{labels.whyEyebrow}</span>
            <h2>{labels.whyTitle}</h2>
          </div>
          <p>{labels.whyBody}</p>
        </header>
        <div className="ash-feature-grid">
          {features.map((feature) => (
            <article key={feature.index}>
              <span>{feature.index}</span>
              <h3>{value(feature.title, locale)}</h3>
              <p>{value(feature.body, locale)}</p>
              <div>
                {feature.tags.map((tag) => (
                  <small key={tag}>{tag}</small>
                ))}
              </div>
            </article>
          ))}
        </div>
      </section>

      <section className="ash-section ash-architecture" id="architecture">
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
            <small>vs compact row-object JSON</small>
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

      <section className="ash-cta">
        <span>{labels.ctaEyebrow}</span>
        <h2>{labels.ctaTitle}</h2>
        <p>{labels.ctaBody}</p>
        <div>
          <a
            className="ash-button ash-button-primary"
            href={route('/guide/install.html')}
          >
            {labels.ctaPrimary}
            <ArrowIcon />
          </a>
          <a className="ash-button" href={route('/guide/cli.html')}>
            {labels.ctaSecondary}
          </a>
        </div>
      </section>

      <footer className="ash-footer">
        <span>ash / AI NATIVE SHELL</span>
        <span>{labels.footer}</span>
        <a href="https://github.com/A3S-Lab/ash">A3S-Lab/ash ↗</a>
      </footer>
    </main>
  );
}
