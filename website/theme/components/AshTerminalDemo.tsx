import { useEffect, useRef, useState } from 'react';

type Locale = 'zh' | 'en';
type WorkStatus = 'pending' | 'running' | 'complete';

const phases = [
  'request',
  'govern',
  'parallel',
  'permit',
  'evidence',
  'recover',
  'done',
] as const;
type Phase = (typeof phases)[number];

const durations: Record<Phase, number> = {
  request: 1900,
  govern: 1800,
  parallel: 2600,
  permit: 2200,
  evidence: 2800,
  recover: 2300,
  done: 3800,
};

const copy = {
  zh: {
    aria: 'ash 终端演示：请求、预算、并行执行、Permit、ASON 与恢复',
    session: 'ash / workspace',
    play: '播放',
    pause: '暂停',
    replay: '重播',
    request: '请求',
    govern: '预算',
    parallel: '执行',
    permit: 'Permit',
    evidence: 'ASON',
    recover: '恢复',
    done: '完成',
    handshake: 'ASH/1 会话就绪',
    capabilities: 'caps=read,search,batch,patch',
    budget: '预算已接受',
    budgetDetail: 'host / session / request',
    graph: '执行计划',
    graphDetail: '3 nodes · bounded',
    read: '读取 + 列表',
    search: '工作区搜索',
    reduce: '哈希 + Diff 归约',
    io: 'Tokio I/O',
    cpu: 'Rayon CPU',
    pending: '等待',
    running: '运行',
    complete: '完成',
    permitTitle: 'Permit 已校验',
    permitDetail: 'cap.fs.patch · digest guard · 30s',
    evidenceTitle: 'ASON 结果',
    evidenceDetail: '完整结果可按引用取回',
    recoveryTitle: '进程树已关闭，事务日志干净',
    recoveryDetail: 'process-tree=reaped · journal=clean',
    stable: 'stable merge',
    efficiency: '0.62× JSON',
  },
  en: {
    aria: 'ash terminal demo: request, budget, parallel execution, permit, ASON, and recovery',
    session: 'ash / workspace',
    play: 'Play',
    pause: 'Pause',
    replay: 'Replay',
    request: 'Request',
    govern: 'Budget',
    parallel: 'Execute',
    permit: 'Permit',
    evidence: 'ASON',
    recover: 'Recover',
    done: 'Done',
    handshake: 'ASH/1 session ready',
    capabilities: 'caps=read,search,batch,patch',
    budget: 'Budget accepted',
    budgetDetail: 'host / session / request',
    graph: 'Execution plan',
    graphDetail: '3 nodes · bounded',
    read: 'Read + list',
    search: 'Search workspace',
    reduce: 'Hash + diff reduce',
    io: 'Tokio I/O',
    cpu: 'Rayon CPU',
    pending: 'pending',
    running: 'running',
    complete: 'done',
    permitTitle: 'Permit checked',
    permitDetail: 'cap.fs.patch · digest guard · 30s',
    evidenceTitle: 'ASON result',
    evidenceDetail: 'Full result is available by reference',
    recoveryTitle: 'Process tree closed; journal clean',
    recoveryDetail: 'process-tree=reaped · journal=clean',
    stable: 'stable merge',
    efficiency: '0.62× JSON',
  },
};

function statusFor(activeIndex: number, rowIndex: number): WorkStatus {
  if (activeIndex < 2) return 'pending';
  if (activeIndex === 2 && rowIndex > 0) return 'running';
  return 'complete';
}

export function AshTerminalDemo({ locale }: { locale: Locale }) {
  const labels = copy[locale];
  const terminalRef = useRef<HTMLDivElement>(null);
  const hasStartedRef = useRef(false);
  const playOnceRef = useRef(false);
  const [activeIndex, setActiveIndex] = useState(phases.length - 1);
  const [isPlaying, setIsPlaying] = useState(false);
  const [isVisible, setIsVisible] = useState(false);
  const [prefersReducedMotion, setPrefersReducedMotion] = useState(false);
  const [typedCount, setTypedCount] = useState(0);
  const phase = phases[activeIndex] ?? phases[0];
  const command = 'ash rpc < task.ason';
  const typedCommand =
    activeIndex === 0 ? command.slice(0, typedCount) : command;
  const isRunning = isPlaying && isVisible;
  const rows = [
    { label: labels.read, plane: labels.io, detail: '2 ops' },
    { label: labels.search, plane: labels.cpu, detail: '8 workers' },
    { label: labels.reduce, plane: labels.cpu, detail: 'stable key' },
  ];

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return undefined;

    const motionPreference = window.matchMedia(
      '(prefers-reduced-motion: reduce)',
    );
    setPrefersReducedMotion(motionPreference.matches);

    const observer = new IntersectionObserver(
      ([entry]) => {
        const visible = entry?.isIntersecting ?? false;
        setIsVisible(visible);

        if (visible && !hasStartedRef.current) {
          hasStartedRef.current = true;
          if (motionPreference.matches) {
            setActiveIndex(phases.length - 1);
            setTypedCount(command.length);
          } else {
            setActiveIndex(0);
            setTypedCount(0);
            setIsPlaying(true);
          }
        }
      },
      { threshold: 0.3 },
    );

    observer.observe(terminal);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!isRunning) return undefined;

    const timer = window.setTimeout(() => {
      if (activeIndex === phases.length - 1) {
        if (playOnceRef.current) {
          playOnceRef.current = false;
          setIsPlaying(false);
        } else {
          setActiveIndex(0);
          setTypedCount(0);
        }
      } else {
        setActiveIndex((index) => index + 1);
      }
    }, durations[phase]);

    return () => window.clearTimeout(timer);
  }, [activeIndex, isRunning, phase]);

  useEffect(() => {
    if (!isRunning || activeIndex !== 0 || typedCount >= command.length) {
      return undefined;
    }

    const timer = window.setTimeout(
      () => setTypedCount((count) => count + 1),
      54,
    );
    return () => window.clearTimeout(timer);
  }, [activeIndex, command.length, isRunning, typedCount]);

  function togglePlayback() {
    if (isPlaying) {
      setIsPlaying(false);
      return;
    }

    playOnceRef.current = prefersReducedMotion;
    if (activeIndex === phases.length - 1) {
      setActiveIndex(0);
      setTypedCount(0);
    }
    setIsPlaying(true);
  }

  const stageLabels = phases.map((item) => labels[item]);

  return (
    <div
      aria-label={labels.aria}
      className={`ash-terminal-demo ${isRunning ? 'is-running' : ''}`}
      data-phase={phase}
      ref={terminalRef}
    >
      <header className="ash-terminal-titlebar">
        <span aria-hidden="true" className="ash-terminal-dots">
          <i />
          <i />
          <i />
        </span>
        <strong>{labels.session}</strong>
        <small>{labels[phase]}</small>
        <button aria-pressed={isPlaying} onClick={togglePlayback} type="button">
          <i aria-hidden="true">{isPlaying ? 'Ⅱ' : '▶'}</i>
          {isPlaying
            ? labels.pause
            : activeIndex === phases.length - 1
              ? labels.replay
              : labels.play}
        </button>
      </header>

      <div className="ash-terminal-screen">
        <div className="ash-terminal-command">
          <span aria-hidden="true">›</span>
          <code>{typedCommand}</code>
          <i aria-hidden="true" />
        </div>

        <div
          aria-hidden={activeIndex < 1}
          className={`ash-terminal-line ${activeIndex >= 1 ? 'is-visible' : ''}`}
        >
          <i aria-hidden="true">✓</i>
          <span>{labels.handshake}</span>
          <code>{labels.capabilities}</code>
        </div>

        <div
          aria-hidden={activeIndex < 1}
          className={`ash-terminal-budget ${activeIndex >= 1 ? 'is-visible' : ''}`}
        >
          <span>GOV</span>
          <div>
            <strong>{labels.budget}</strong>
            <small>{labels.budgetDetail}</small>
          </div>
          <i aria-hidden="true">
            <b />
            <b />
            <b />
          </i>
        </div>

        <section
          aria-hidden={activeIndex < 2}
          className={`ash-terminal-graph ${activeIndex >= 2 ? 'is-visible' : ''}`}
        >
          <header>
            <span>{labels.graph}</span>
            <small>{labels.graphDetail}</small>
          </header>
          <div>
            {rows.map((row, index) => {
              const status = statusFor(activeIndex, index);
              return (
                <p className={`is-${status}`} key={row.label}>
                  <i aria-hidden="true">
                    {status === 'complete'
                      ? '✓'
                      : status === 'running'
                        ? '●'
                        : '○'}
                  </i>
                  <strong>{row.label}</strong>
                  <code>{row.plane}</code>
                  <small>
                    {row.detail} · {labels[status]}
                  </small>
                </p>
              );
            })}
          </div>
        </section>

        <div
          aria-hidden={activeIndex < 3}
          className={`ash-terminal-permit ${activeIndex >= 3 ? 'is-visible' : ''}`}
        >
          <span aria-hidden="true">◆</span>
          <div>
            <strong>{labels.permitTitle}</strong>
            <code>{labels.permitDetail}</code>
          </div>
          <b>ALLOW ONCE</b>
        </div>

        <div
          aria-hidden={activeIndex < 4}
          className={`ash-terminal-evidence ${activeIndex >= 4 ? 'is-visible' : ''}`}
        >
          <div>
            <header>
              <strong>{labels.evidenceTitle}</strong>
              <small>{labels.evidenceDetail}</small>
            </header>
            <pre>{`r{p,l,c}:\n0,42,governor\n0,88,stable_merge\nz{shown,total}:2,17\nref:@r-7f2a`}</pre>
          </div>
          <aside>
            <strong>{labels.efficiency}</strong>
            <small>CL100K / O200K</small>
          </aside>
        </div>

        <div
          aria-hidden={activeIndex < 5}
          className={`ash-terminal-recovery ${activeIndex >= 5 ? 'is-visible' : ''}`}
        >
          <span aria-hidden="true">↺</span>
          <div>
            <strong>{labels.recoveryTitle}</strong>
            <code>{labels.recoveryDetail}</code>
          </div>
        </div>

        <div
          aria-hidden={activeIndex < 6}
          className={`ash-terminal-complete ${activeIndex >= 6 ? 'is-visible' : ''}`}
        >
          <span aria-hidden="true">●</span>
          <strong>{labels.stable}</strong>
          <code>42 ms · ok</code>
        </div>
      </div>

      <footer className="ash-terminal-footer">
        <div aria-hidden="true">
          {stageLabels.map((label, index) => (
            <i
              className={
                index === activeIndex
                  ? 'is-active'
                  : index < activeIndex
                    ? 'is-complete'
                    : undefined
              }
              key={label}
              title={label}
            />
          ))}
        </div>
        <span>ASH/1 · ASON/1</span>
      </footer>
    </div>
  );
}
