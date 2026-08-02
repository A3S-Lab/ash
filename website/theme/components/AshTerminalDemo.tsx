import { useEffect, useRef, useState } from 'react';

type Locale = 'zh' | 'en';

type Phase = 'search' | 'read' | 'batch' | 'patch' | 'test' | 'project';

type Localized = {
  zh: string;
  en: string;
};

type DemoTask = {
  id: Phase;
  opcode: string;
  command: string;
  title: Localized;
  comment: Localized;
  request: string[];
  result: string;
};

const tasks: DemoTask[] = [
  {
    id: 'search',
    opcode: 'g',
    command: 'ash run < .ash/01-search.ason',
    title: { zh: '搜索命中', en: 'Search hits' },
    comment: {
      zh: '只返回相关行；路径只编码一次',
      en: 'Return matching lines; encode each path once',
    },
    request: ['o:g', 'a{q,p,f}:', 'TODO,[src],0'],
    result: '2 hits · 1 path · 38 tok',
  },
  {
    id: 'read',
    opcode: 'r',
    command: 'ash run < .ash/02-read.ason',
    title: { zh: '读取行窗', en: 'Read window' },
    comment: {
      zh: '只读取决策所需的 80 行',
      en: 'Read only the 80 lines needed for the decision',
    },
    request: ['o:r', 'a{p,m,o,n}:', '[src/lib.rs],1,1,80'],
    result: '80 lines · digest=aaaaaaaa…',
  },
  {
    id: 'batch',
    opcode: 'b',
    command: 'ash run < .ash/03-batch.ason',
    title: { zh: '并行批处理', en: 'Parallel batch' },
    comment: {
      zh: '独立节点并发，结果按节点编号归并',
      en: 'Run independent nodes concurrently; merge by node id',
    },
    request: ['o:b', 'a[2]{i,d,o,a}:', '1,[],g,…', '2,[1],r,…'],
    result: 'node 1 → @4 · node 2 → @5',
  },
  {
    id: 'patch',
    opcode: 'p',
    command: 'ash run < .ash/04-patch.ason',
    title: { zh: '摘要保护补丁', en: 'Digest-guarded patch' },
    comment: {
      zh: '前置摘要不匹配就返回冲突',
      en: 'Return a conflict when the preimage digest changed',
    },
    request: ['o:p', 'a{p,h,i,o,n,v,f}:', '[src/lib.rs],[aaaa…],…'],
    result: 'committed · digest=bbbbbbbb…',
  },
  {
    id: 'test',
    opcode: 'x',
    command: 'ash run < .ash/05-test.ason',
    title: { zh: '运行回归测试', en: 'Run regression tests' },
    comment: {
      zh: 'argv 独立编码；取消传到整个进程树',
      en: 'Encode argv separately; cancellation reaches the process tree',
    },
    request: ['o:x', 'a{x,v,c,e,in,f}:', 'cargo,[test,--locked],.,[],~,0'],
    result: 'exit=0 · 842 ms · test result: ok',
  },
  {
    id: 'project',
    opcode: '|',
    command: 'ash run < .ash/07-project.ason',
    title: { zh: '投影最终证据', en: 'Project final evidence' },
    comment: {
      zh: '完整数据留在 @7，只取 p/l/t 三列',
      en: 'Keep full data at @7; retrieve only p/l/t',
    },
    request: ['o:|', 'a:[@7,d,0,64,p,l,t]'],
    result: '2 rows · retained=@7 · stable',
  },
];

const durations: Record<Phase, number> = {
  search: 2200,
  read: 2200,
  batch: 2500,
  patch: 2300,
  test: 2600,
  project: 3600,
};

const copy = {
  zh: {
    aria: 'ash 终端演示：搜索、读取、并行批处理、补丁、测试与结果投影',
    session: 'ash / coding task',
    play: '继续',
    pause: '暂停',
    replay: '重播',
    request: '规范请求',
    result: '结果',
    pipeline: '任务队列',
    running: '运行',
    complete: '完成',
    pending: '等待',
    final: '任务完成',
    finalDetail: '6 个操作 · 1 个紧凑证据包',
  },
  en: {
    aria: 'ash terminal demo: search, read, parallel batch, patch, test, and result projection',
    session: 'ash / coding task',
    play: 'Resume',
    pause: 'Pause',
    replay: 'Replay',
    request: 'Canonical request',
    result: 'Result',
    pipeline: 'Task queue',
    running: 'running',
    complete: 'done',
    pending: 'pending',
    final: 'Task complete',
    finalDetail: '6 operations · 1 compact evidence pack',
  },
};

function localized(value: Localized, locale: Locale) {
  return value[locale];
}

export function AshTerminalDemo({ locale }: { locale: Locale }) {
  const labels = copy[locale];
  const terminalRef = useRef<HTMLDivElement>(null);
  const hasStartedRef = useRef(false);
  const [activeIndex, setActiveIndex] = useState(tasks.length - 1);
  const [isPlaying, setIsPlaying] = useState(false);
  const [isVisible, setIsVisible] = useState(false);
  const [prefersReducedMotion, setPrefersReducedMotion] = useState(false);
  const [typedCount, setTypedCount] = useState(
    tasks.at(-1)?.command.length ?? 0,
  );
  const active = tasks[activeIndex] ?? tasks[0];
  const phase = active.id;
  const isRunning = isPlaying && isVisible;
  const typedCommand = active.command.slice(0, typedCount);

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
            setActiveIndex(tasks.length - 1);
            setTypedCount(tasks.at(-1)?.command.length ?? 0);
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
      if (activeIndex === tasks.length - 1) {
        setIsPlaying(false);
        return;
      }
      setActiveIndex((index) => index + 1);
      setTypedCount(0);
    }, durations[phase]);

    return () => window.clearTimeout(timer);
  }, [activeIndex, isRunning, phase]);

  useEffect(() => {
    if (!isRunning || typedCount >= active.command.length) return undefined;

    const timer = window.setTimeout(
      () => setTypedCount((count) => count + 1),
      34,
    );
    return () => window.clearTimeout(timer);
  }, [active.command.length, isRunning, typedCount]);

  function togglePlayback() {
    if (prefersReducedMotion) {
      const nextIndex = activeIndex === tasks.length - 1 ? 0 : tasks.length - 1;
      setActiveIndex(nextIndex);
      setTypedCount(tasks[nextIndex]?.command.length ?? 0);
      return;
    }
    if (isPlaying) {
      setIsPlaying(false);
      return;
    }
    if (activeIndex === tasks.length - 1) {
      setActiveIndex(0);
      setTypedCount(0);
    }
    setIsPlaying(true);
  }

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
        <small>
          o:{active.opcode} / {active.id}
        </small>
        <button aria-pressed={isPlaying} onClick={togglePlayback} type="button">
          <i aria-hidden="true">{isPlaying ? 'Ⅱ' : '▶'}</i>
          {isPlaying
            ? labels.pause
            : activeIndex === tasks.length - 1
              ? labels.replay
              : labels.play}
        </button>
      </header>

      <div className="ash-terminal-screen ash-terminal-workflow">
        <div className="ash-terminal-command">
          <span aria-hidden="true">$</span>
          <code>{typedCommand}</code>
          <i aria-hidden="true" />
        </div>

        <section className="ash-terminal-live" key={active.id}>
          <header>
            <span>
              {String(activeIndex + 1).padStart(2, '0')} /{' '}
              {String(tasks.length).padStart(2, '0')}
            </span>
            <strong>{localized(active.title, locale)}</strong>
          </header>
          <p>
            <span aria-hidden="true">#</span>
            {localized(active.comment, locale)}
          </p>
          <div>
            <small>{labels.request}</small>
            <pre>{active.request.join('\n')}</pre>
          </div>
          <footer>
            <span>✓ {labels.result}</span>
            <code>{active.result}</code>
          </footer>
        </section>

        <section className="ash-terminal-queue">
          <header>{labels.pipeline}</header>
          <div>
            {tasks.map((task, index) => {
              const status =
                index < activeIndex
                  ? labels.complete
                  : index === activeIndex
                    ? labels.running
                    : labels.pending;
              return (
                <p
                  className={
                    index < activeIndex
                      ? 'is-complete'
                      : index === activeIndex
                        ? 'is-active'
                        : undefined
                  }
                  key={task.id}
                >
                  <i aria-hidden="true">
                    {index < activeIndex
                      ? '✓'
                      : index === activeIndex
                        ? '●'
                        : '○'}
                  </i>
                  <code>o:{task.opcode}</code>
                  <span>{localized(task.title, locale)}</span>
                  <small>{status}</small>
                </p>
              );
            })}
          </div>
        </section>

        <div
          className={`ash-terminal-final ${activeIndex === tasks.length - 1 ? 'is-visible' : ''}`}
        >
          <span aria-hidden="true">◆</span>
          <div>
            <strong>{labels.final}</strong>
            <small>{labels.finalDetail}</small>
          </div>
          <code>s:0</code>
        </div>
      </div>

      <footer className="ash-terminal-footer">
        <div aria-hidden="true">
          {tasks.map((task, index) => (
            <i
              className={
                index === activeIndex
                  ? 'is-active'
                  : index < activeIndex
                    ? 'is-complete'
                    : undefined
              }
              key={task.id}
              title={task.id}
            />
          ))}
        </div>
        <span>ASH/1 · ASON/1 · {active.id.toUpperCase()}</span>
      </footer>
    </div>
  );
}
