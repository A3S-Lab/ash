import { useEffect, useRef, useState } from 'react';

type Locale = 'zh' | 'en';

type Localized = {
  zh: string;
  en: string;
};

type WalkthroughStep = {
  id: string;
  opcode: string;
  operation: string;
  filename: string;
  command: string;
  title: Localized;
  body: Localized;
  shellComment: Localized;
  annotations: Localized[];
  tags: string[];
  request: string[];
  requestFocus: number[];
  result: string[];
  resultFocus: number[];
};

const digestA =
  'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const digestB =
  'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
const digestC =
  'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc';

const steps: WalkthroughStep[] = [
  {
    id: 'search',
    opcode: 'g',
    operation: 'SEARCH',
    filename: '.ash/01-search.ason',
    command: 'ash run < .ash/01-search.ason',
    title: { zh: '先搜到需要看的位置', en: 'Find only the relevant locations' },
    body: {
      zh: '按固定 Schema 搜索工作区。结果只返回命中行，并把重复路径压成字典。',
      en: 'Search the workspace through a fixed schema. Results contain matching lines and intern repeated paths once.',
    },
    shellComment: {
      zh: '字面量搜索 TODO；不启动 shell，也不解析自由文本',
      en: 'Search for literal TODO; no shell process or free-form parsing',
    },
    annotations: [
      {
        zh: '`o:g` 直接选择 search 操作，调用方不需要拼接命令字符串。',
        en: '`o:g` selects search directly; the caller never assembles a command string.',
      },
      {
        zh: '`u` 同时限制输出 Token、记录数与 30 秒截止时间。',
        en: '`u` bounds output tokens, records, and the 30-second deadline together.',
      },
      {
        zh: '`p[]` 只保存一次路径，`d[]` 用整数引用它，减少重复 Token。',
        en: '`p[]` stores each path once; `d[]` references it by integer to avoid repeated tokens.',
      },
    ],
    tags: ['o:g', 'literal', 'path-dict'],
    request: [
      't:1',
      'i:17',
      'o:g',
      'a{q,p,f}:',
      'TODO,[src],0',
      'u{tok,rec,ms}:',
      '256,64,30000',
    ],
    requestFocus: [3, 4, 5, 6, 7],
    result: [
      't:3',
      'i:17',
      's:0',
      'p[1]{i,v}:',
      '1,src/lib.rs',
      'd[2]{p,l,c,t}:',
      '1,42,7,"TODO item"',
      '1,87,3,"FIXME item"',
      'z:0',
      'r:~',
    ],
    resultFocus: [4, 5, 6, 7, 8],
  },
  {
    id: 'read',
    opcode: 'r',
    operation: 'READ',
    filename: '.ash/02-read.ason',
    command: 'ash run < .ash/02-read.ason',
    title: { zh: '只读需要的上下文', en: 'Read the exact context needed' },
    body: {
      zh: '读取指定文件的指定行窗。大内容不会被迫塞回模型上下文。',
      en: 'Read a bounded line window from a known file. Large content is never forced back into model context.',
    },
    shellComment: {
      zh: '读取 src/lib.rs 的前 80 行，并保留可校验摘要',
      en: 'Read the first 80 lines of src/lib.rs and retain its digest',
    },
    annotations: [
      {
        zh: '`m:1` 表示按行读取；`o` 与 `n` 明确偏移和长度。',
        en: '`m:1` selects line mode; `o` and `n` make the offset and length explicit.',
      },
      {
        zh: '结果携带内容摘要，后续 patch 可用它检查前置状态。',
        en: 'The result carries a digest that a later patch can use as a precondition.',
      },
      {
        zh: '超出投影预算的正文可以保留为 `r:@id`，需要时再取。',
        en: 'Text beyond the projection budget can remain as `r:@id` and be fetched only when needed.',
      },
    ],
    tags: ['o:r', 'line-window', 'digest'],
    request: [
      't:1',
      'i:19',
      'o:r',
      'a{p,m,o,n}:',
      '[src/lib.rs],1,1,80',
      'u{tok,rec,ms}:',
      '512,64,30000',
    ],
    requestFocus: [3, 4, 5],
    result: [
      't:3',
      'i:19',
      's:0',
      'p[1]{i,v}:',
      '1,src/lib.rs',
      'd[1]{p,o,n,h,t,r}:',
      `1,1,15,${digestA},"pub mod engine;",~`,
      'z:0',
      'r:~',
    ],
    resultFocus: [4, 5, 6, 7],
  },
  {
    id: 'batch',
    opcode: 'b',
    operation: 'BATCH',
    filename: '.ash/03-batch.ason',
    command: 'ash run < .ash/03-batch.ason',
    title: {
      zh: '把独立工作一次并行发出',
      en: 'Issue independent work in one batch',
    },
    body: {
      zh: '一个请求携带有依赖的任务图。无依赖节点并行，结果仍按节点编号稳定归并。',
      en: 'One request carries a dependency graph. Independent nodes run in parallel while results merge in stable node order.',
    },
    shellComment: {
      zh: '节点 1 搜索；节点 2 等节点 1 完成后再读 Cargo.toml',
      en: 'Node 1 searches; node 2 reads Cargo.toml after node 1 completes',
    },
    annotations: [
      {
        zh: '`a[2]` 是两行任务表；`d` 是依赖节点编号向量。',
        en: '`a[2]` is a two-row task table; `d` is the dependency-id vector.',
      },
      {
        zh: 'Tokio 与 Rayon 在同一 Governor 下取预算，不会无限扩张工作线程。',
        en: 'Tokio and Rayon acquire from one governor, so worker growth remains bounded.',
      },
      {
        zh: '子结果保留为 `@4`、`@5`；批量响应不重复内嵌大正文。',
        en: 'Child results remain at `@4` and `@5`; the batch response does not repeat large payloads.',
      },
    ],
    tags: ['o:b', 'DAG', 'stable-merge'],
    request: [
      't:1',
      'i:80',
      'o:b',
      'a[2]{i,d,o,a}:',
      '1,[],g,"a{q,p,f}:\\nTODO,[src],0\\n"',
      '2,[1],r,"a{p,m,o,n}:\\n[Cargo.toml],0,0,32\\n"',
      'u{tok,rec,ms}:',
      '64,16,30000',
    ],
    requestFocus: [3, 4, 5, 6],
    result: [
      't:3',
      'i:80',
      's:0',
      'd[2]{i,o,s,c,r}:',
      '1,g,0,0,@4',
      '2,r,0,0,@5',
      'z:8',
      'r:~',
    ],
    resultFocus: [4, 5, 6],
  },
  {
    id: 'patch',
    opcode: 'p',
    operation: 'PATCH',
    filename: '.ash/04-patch.ason',
    command: 'ash run < .ash/04-patch.ason',
    title: {
      zh: '带前置摘要提交补丁',
      en: 'Commit a patch with a digest guard',
    },
    body: {
      zh: '补丁声明文件摘要、字节偏移、删除长度和替换值。文件已变化时返回冲突，不覆盖新内容。',
      en: 'A patch declares the file digest, byte offset, delete length, and replacement. Changed files return a conflict instead of being overwritten.',
    },
    shellComment: {
      zh: '只有 src/lib.rs 仍等于预期摘要时才写入 pub',
      en: 'Write pub only while src/lib.rs still matches the expected digest',
    },
    annotations: [
      {
        zh: '`h` 是读取阶段得到的 64 个十六进制字符摘要，形成乐观并发锁。',
        en: '`h` is the 64-digit hexadecimal digest from the read step, forming an optimistic lock.',
      },
      {
        zh: '`i/o/n/v` 分别表示文件索引、偏移、删除长度和替换值。',
        en: '`i/o/n/v` encode the file index, offset, delete length, and replacement value.',
      },
      {
        zh: '`s:0` 表示 committed；冲突、回滚和恢复需求都有稳定状态码。',
        en: '`s:0` means committed; conflict, rollback, and recovery each have stable status codes.',
      },
    ],
    tags: ['o:p', 'preimage', 'transaction'],
    request: [
      't:1',
      'i:23',
      'o:p',
      'a{p,h,i,o,n,v,f}:',
      `[src/lib.rs],[${digestA}],[0],[4],[3],[pub],0`,
      'u{tok,rec,ms}:',
      '512,64,30000',
    ],
    requestFocus: [3, 4, 5],
    result: [
      't:3',
      'i:23',
      's:0',
      'p[1]{i,v}:',
      '1,src/lib.rs',
      'd[1]{p,s,h}:',
      `1,0,${digestB}`,
      'z:0',
      'r:~',
    ],
    resultFocus: [3, 6, 7],
  },
  {
    id: 'test',
    opcode: 'x',
    operation: 'EXEC',
    filename: '.ash/05-test.ason',
    command: 'ash run < .ash/05-test.ason',
    title: { zh: '用 argv 运行测试', en: 'Run tests with an argv vector' },
    body: {
      zh: '程序、参数、工作目录和环境分别编码。没有引号转义歧义，也不会意外执行第二条命令。',
      en: 'Program, arguments, working directory, and environment are encoded separately. There is no quoting ambiguity or accidental second command.',
    },
    shellComment: {
      zh: '执行 cargo test --locked；超时与取消会传到整个进程树',
      en: 'Run cargo test --locked; timeout and cancellation reach the process tree',
    },
    annotations: [
      {
        zh: '`x` 是程序，`v` 是 argv；ASH 不把它们重新拼回 shell 字符串。',
        en: '`x` is the program and `v` is argv; ASH never joins them back into a shell string.',
      },
      {
        zh: '前缀 `-SECRET` 从继承环境中删除敏感变量。',
        en: 'The `-SECRET` entry removes a sensitive variable from the inherited environment.',
      },
      {
        zh: '输出投影与完整输出引用分离；通常只需消费退出码与短摘要。',
        en: 'Output projection is separate from full-output references; agents usually consume only the exit code and short summary.',
      },
    ],
    tags: ['o:x', 'argv', 'process-tree'],
    request: [
      't:1',
      'i:18',
      'o:x',
      'a{x,v,c,e,in,f}:',
      'cargo,[test,--locked],.,["RUST_BACKTRACE=1",-SECRET],~,0',
      'u{tok,rec,ms}:',
      '512,64,120000',
    ],
    requestFocus: [3, 4, 5, 7],
    result: [
      't:3',
      'i:18',
      's:0',
      'd{k,c,ms,o,e,ro,re}:',
      '0,0,842,"test result: ok",~,~,~',
      'z:0',
      'r:~',
    ],
    resultFocus: [3, 4, 5],
  },
  {
    id: 'snapshot',
    opcode: 's',
    operation: 'SNAPSHOT',
    filename: '.ash/06-snapshot.ason',
    command: 'ash run < .ash/06-snapshot.ason',
    title: {
      zh: '用快照证明改了什么',
      en: 'Prove what changed with a snapshot',
    },
    body: {
      zh: '捕获或对比工作区清单。文件哈希并行计算，输出按路径稳定排序，并保留完整清单。',
      en: 'Capture or compare a workspace manifest. File hashes run in parallel, output is path-stable, and the complete manifest remains retained.',
    },
    shellComment: {
      zh: '捕获当前工作区；后续用 m:1 + r:@9 只返回增量',
      en: 'Capture the workspace; a later m:1 + r:@9 request returns only the delta',
    },
    annotations: [
      {
        zh: '`m:0` 捕获基线；`m:1` 携带基线引用计算增量。',
        en: '`m:0` captures a baseline; `m:1` carries its reference to compute a delta.',
      },
      {
        zh: 'Rayon 并行哈希文件，稳定归并保证相同输入得到同一行序。',
        en: 'Rayon hashes files in parallel while stable merge preserves identical row order for identical input.',
      },
      {
        zh: '`z:8` 表示结果已保留，`r:@9` 是完整清单的句柄。',
        en: '`z:8` marks retained output; `r:@9` is the handle to the complete manifest.',
      },
    ],
    tags: ['o:s', 'blake3', 'delta'],
    request: [
      't:1',
      'i:24',
      'o:s',
      'a{p,d,m,r,f}:',
      '[.],64,0,~,0',
      'u{tok,rec,ms}:',
      '512,64,30000',
    ],
    requestFocus: [3, 4, 5],
    result: [
      't:3',
      'i:24',
      's:0',
      'p[2]{i,v}:',
      '1,Cargo.toml',
      '2,src/lib.rs',
      'd[2]{p,c,k,z,h}:',
      `1,0,0,912,${digestB}`,
      `2,0,0,244,${digestC}`,
      'z:8',
      'r:@9',
    ],
    resultFocus: [7, 8, 9, 10, 11],
  },
  {
    id: 'project',
    opcode: '|',
    operation: 'PROJECT',
    filename: '.ash/07-project.ason',
    command: 'ash run < .ash/07-project.ason',
    title: {
      zh: '用公式取回最小证据',
      en: 'Retrieve the minimum evidence by formula',
    },
    body: {
      zh: '完整结果留在引用中，Agent 用短公式搜索、切片或投影，只把决策所需数据放进上下文。',
      en: 'Full results stay behind a reference. The agent searches, slices, or projects them with a short formula and admits only decision-relevant data into context.',
    },
    shellComment: {
      zh: '从 @7 取 d 表，截取 0..64，并只保留 p/l/t 列',
      en: 'From @7, select table d, slice 0..64, and keep only p/l/t',
    },
    annotations: [
      {
        zh: '`o:|` 直接表示列投影，`a` 只保留 source → table → range → columns。',
        en: '`o:|` is column projection directly; `a` keeps only source → table → range → columns.',
      },
      {
        zh: '请求本身只有一行，不需要重新执行原任务，也不重传完整结果。',
        en: 'The request is one row: no original-task replay and no complete-result retransmission.',
      },
      {
        zh: '`z:10` 同时表示 reduced + retained；剩余数据仍可继续投影。',
        en: '`z:10` marks reduced + retained; the remaining data can still be projected again.',
      },
    ],
    tags: ['o:|', 'formula', 'projection'],
    request: [
      't:1',
      'i:44',
      'o:|',
      'a:[@7,d,0,64,p,l,t]',
      'u{tok,rec,ms}:',
      '256,64,30000',
    ],
    requestFocus: [3, 4],
    result: [
      't:3',
      'i:44',
      's:0',
      'd[2]{p,t}:',
      'src/a.rs,TODO',
      'src/b.rs,FIXME',
      'z:10',
      'r:@7',
    ],
    resultFocus: [4, 5, 6, 7, 8],
  },
];

const copy = {
  zh: {
    aria: 'ASH 命令代码漫游',
    stage: '任务执行窗口',
    step: '步骤',
    request: '规范请求',
    result: '紧凑结果',
    annotations: '字段注释',
    copy: '复制请求',
    copied: '已复制',
    scroll: '继续滚动',
  },
  en: {
    aria: 'ASH command walkthrough',
    stage: 'Task execution window',
    step: 'Step',
    request: 'Canonical request',
    result: 'Compact result',
    annotations: 'Field notes',
    copy: 'Copy request',
    copied: 'Copied',
    scroll: 'Keep scrolling',
  },
};

function localized(value: Localized, locale: Locale) {
  return value[locale];
}

function CodePane({
  label,
  lines,
  focusedLines,
  copyLabel,
  copiedLabel,
  copyable,
}: {
  label: string;
  lines: string[];
  focusedLines: number[];
  copyLabel: string;
  copiedLabel: string;
  copyable?: boolean;
}) {
  const [copied, setCopied] = useState(false);

  async function copyCode() {
    try {
      await navigator.clipboard.writeText(`${lines.join('\n')}\n`);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    } catch {
      setCopied(false);
    }
  }

  return (
    <section className="ash-tour-code-pane">
      <header>
        <span>{label}</span>
        {copyable ? (
          <button onClick={copyCode} type="button">
            {copied ? copiedLabel : copyLabel}
          </button>
        ) : (
          <small>ASON/1</small>
        )}
      </header>
      <pre>
        <code>
          {lines.map((line, index) => {
            const lineNumber = index + 1;
            return (
              <span
                className={
                  focusedLines.includes(lineNumber) ? 'is-focused' : undefined
                }
                data-line-number={String(lineNumber).padStart(2, '0')}
                key={`${lineNumber}-${line}`}
              >
                {line || ' '}
              </span>
            );
          })}
        </code>
      </pre>
    </section>
  );
}

function WalkthroughStage({
  activeIndex,
  locale,
}: {
  activeIndex: number;
  locale: Locale;
}) {
  const labels = copy[locale];
  const step = steps[activeIndex] ?? steps[0];

  return (
    <div className="ash-tour-stage">
      <header className="ash-tour-stage-toolbar">
        <span>{labels.stage}</span>
        <span aria-live="polite">
          {labels.step} {String(activeIndex + 1).padStart(2, '0')} /{' '}
          {String(steps.length).padStart(2, '0')}
        </span>
      </header>

      <div className="ash-tour-stage-body" key={step.id}>
        <div className="ash-tour-command-bar">
          <div>
            <span aria-hidden="true">$</span>
            <code>{step.command}</code>
          </div>
          <p>
            <span aria-hidden="true">#</span>
            {localized(step.shellComment, locale)}
          </p>
        </div>

        <div className="ash-tour-code-grid">
          <CodePane
            copiedLabel={labels.copied}
            copyable
            copyLabel={labels.copy}
            focusedLines={step.requestFocus}
            label={`${labels.request} · ${step.filename}`}
            lines={step.request}
          />
          <CodePane
            copiedLabel={labels.copied}
            copyLabel={labels.copy}
            focusedLines={step.resultFocus}
            label={labels.result}
            lines={step.result}
          />
        </div>

        <aside className="ash-tour-annotations">
          <header>
            <span>
              {'// '}
              {labels.annotations}
            </span>
            <code>
              o:{step.opcode} / {step.operation}
            </code>
          </header>
          <ol>
            {step.annotations.map((annotation, index) => (
              <li key={localized(annotation, locale)}>
                <span>{String(index + 1).padStart(2, '0')}</span>
                <p>{localized(annotation, locale)}</p>
              </li>
            ))}
          </ol>
        </aside>
      </div>
    </div>
  );
}

export function AshCommandWalkthrough({ locale }: { locale: Locale }) {
  const labels = copy[locale];
  const stepRefs = useRef<Array<HTMLElement | null>>([]);
  const [activeIndex, setActiveIndex] = useState(0);

  useEffect(() => {
    const elements = stepRefs.current.filter(
      (element): element is HTMLElement => element !== null,
    );
    if (!elements.length || typeof IntersectionObserver === 'undefined') {
      return undefined;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        const centered = entries
          .filter((entry) => entry.isIntersecting)
          .sort((left, right) => {
            const viewportCenter = window.innerHeight / 2;
            const leftCenter =
              left.boundingClientRect.top + left.boundingClientRect.height / 2;
            const rightCenter =
              right.boundingClientRect.top +
              right.boundingClientRect.height / 2;
            return (
              Math.abs(leftCenter - viewportCenter) -
              Math.abs(rightCenter - viewportCenter)
            );
          });
        const index = Number(centered[0]?.target.getAttribute('data-index'));
        if (Number.isInteger(index)) setActiveIndex(index);
      },
      { rootMargin: '-42% 0px -42% 0px' },
    );

    for (const element of elements) observer.observe(element);
    return () => observer.disconnect();
  }, []);

  return (
    <div aria-label={labels.aria} className="ash-command-tour">
      <div className="ash-tour-steps">
        {steps.map((step, index) => {
          const isActive = activeIndex === index;
          return (
            <article
              data-index={index}
              data-selected={isActive ? 'true' : 'false'}
              key={step.id}
              ref={(element) => {
                stepRefs.current[index] = element;
              }}
            >
              <button
                aria-current={isActive ? 'step' : undefined}
                onClick={() => setActiveIndex(index)}
                onFocus={() => setActiveIndex(index)}
                onMouseEnter={() => setActiveIndex(index)}
                type="button"
              >
                <span className="ash-tour-step-number">
                  {String(index + 1).padStart(2, '0')}
                </span>
                <span className="ash-tour-step-operation">
                  o:{step.opcode} / {step.operation}
                </span>
                <h3>{localized(step.title, locale)}</h3>
                <p>{localized(step.body, locale)}</p>
                <span className="ash-tour-step-tags">
                  {step.tags.map((tag) => (
                    <i key={tag}>{tag}</i>
                  ))}
                </span>
                <span className="ash-tour-step-progress" aria-hidden="true" />
              </button>

              <div className="ash-tour-mobile-preview">
                <WalkthroughStage activeIndex={index} locale={locale} />
              </div>
            </article>
          );
        })}
      </div>

      <aside className="ash-tour-sticky">
        <WalkthroughStage activeIndex={activeIndex} locale={locale} />
        <span className="ash-tour-scroll-hint" aria-hidden="true">
          {labels.scroll} ↓
        </span>
      </aside>
    </div>
  );
}
