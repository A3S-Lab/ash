import { useState } from 'react';

type Locale = 'zh' | 'en';

type Operator = {
  id: string;
  symbol: string;
  semantic: string;
  operands: string;
  tokens: string;
  title: Record<Locale, string>;
  description: Record<Locale, string>;
};

const operators: Operator[] = [
  {
    id: 'bytes',
    symbol: '/',
    semantic: 'β(@r,o,n)',
    operands: 'a:[@r,o,n]',
    tokens: '16/16 → 13/13',
    title: { zh: '字节切片', en: 'Byte slice' },
    description: {
      zh: '从保留结果 @r 的零基字节偏移 o 开始，读取 n 个字节。',
      en: 'Read n bytes from zero-based offset o in retained result @r.',
    },
  },
  {
    id: 'lines',
    symbol: '#',
    semantic: 'λ(@r,o,n)',
    operands: 'a:[@r,o,n]',
    tokens: '14/15 → 12/12',
    title: { zh: '行切片', en: 'Line slice' },
    description: {
      zh: '从一基行号 o 开始，读取 n 行；行号语义由 # 固定。',
      en: 'Read n lines from one-based line o; # fixes line-number semantics.',
    },
  },
  {
    id: 'search',
    symbol: '?',
    semantic: 'σ(q,@r[o:o+n],f)',
    operands: 'a:[@r,o,n,q,f]',
    tokens: '21/21 → 18/18',
    title: { zh: '结果内搜索', en: 'Search result' },
    description: {
      zh: '只在保留结果的指定窗口内执行字面量或正则筛选。',
      en: 'Apply literal or regex selection only inside the retained window.',
    },
  },
  {
    id: 'release',
    symbol: '-',
    semantic: 'drop(@r)',
    operands: 'a:[@r]',
    tokens: '11/11 → 8/8',
    title: { zh: '释放引用', en: 'Release reference' },
    description: {
      zh: '从会话存储中减去 @r；仍被读取的引用会拒绝释放。',
      en: 'Subtract @r from session storage; an active lease blocks release.',
    },
  },
  {
    id: 'project',
    symbol: '|',
    semantic: 'πC(T[o:o+n])',
    operands: 'a:[@r,T,o,n,C…]',
    tokens: '19/19 → 16/16',
    title: { zh: '列投影', en: 'Column projection' },
    description: {
      zh: '从表 T 的行窗口中，只保留有序列集合 C。',
      en: 'Keep only ordered column set C from the selected row window of T.',
    },
  },
  {
    id: 'materialize',
    symbol: '>',
    semantic: 'μ(path,@r)',
    operands: 'a:[@r,path]',
    tokens: '16/16 → 13/13',
    title: { zh: '安全落盘', en: 'Safe materialization' },
    description: {
      zh: '把不可变结果写入工作区；要求写权限并拒绝覆盖。',
      en: 'Write immutable bytes into the workspace with write permission and no overwrite.',
    },
  },
];

const copy = {
  zh: {
    eyebrow: '公式语法',
    title: '符号就是操作',
    body: '悬停或聚焦符号查看批注；触屏设备点击打开。每个符号都有固定参数和唯一规范编码。',
    aria: 'ASH 数学操作符说明',
    wire: '规范 ASH',
    token: 'CL100K / O200K',
    evidence: '六条公式合计 · 126 B · 80 / 80 TOKEN',
    note: '旧包装 → 当前符号',
  },
  en: {
    eyebrow: 'FORMULA SYNTAX',
    title: 'The symbol is the operation',
    body: 'Hover or focus a symbol for its note; tap to open on touch. Every operator has fixed operands and one canonical encoding.',
    aria: 'ASH mathematical operator notes',
    wire: 'CANONICAL ASH',
    token: 'CL100K / O200K',
    evidence: 'SIX FORMULAS · 126 B · 80 / 80 TOKENS',
    note: 'WRAPPER → SYMBOL',
  },
};

export function AshSymbolAlgebra({ locale }: { locale: Locale }) {
  const labels = copy[locale];
  const [selected, setSelected] = useState<string | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);
  const [focused, setFocused] = useState<string | null>(null);
  const active = hovered ?? focused ?? selected;

  return (
    <div className="ash-section ash-symbol-algebra">
      <header className="ash-section-header">
        <div>
          <span>{labels.eyebrow}</span>
          <h2>{labels.title}</h2>
        </div>
        <p>{labels.body}</p>
      </header>
      <div
        className="ash-symbol-grid"
        data-active={active === null ? 'false' : 'true'}
        aria-label={labels.aria}
      >
        {operators.map((operator) => {
          const open = active === operator.id;
          const noteId = `ash-symbol-note-${operator.id}`;
          return (
            <div
              className="ash-symbol-item"
              data-open={open ? 'true' : 'false'}
              key={operator.id}
              onPointerEnter={(event) => {
                if (event.pointerType !== 'touch') setHovered(operator.id);
              }}
              onPointerLeave={(event) => {
                if (event.pointerType !== 'touch') setHovered(null);
              }}
            >
              <button
                type="button"
                aria-describedby={noteId}
                aria-expanded={open}
                onBlur={() => setFocused(null)}
                onClick={(event) => {
                  const closing = selected === operator.id;
                  setSelected(closing ? null : operator.id);
                  if (closing) event.currentTarget.blur();
                }}
                onFocus={() => setFocused(operator.id)}
                onKeyDown={(event) => {
                  if (event.key === 'Escape') {
                    setFocused(null);
                    setSelected(null);
                  }
                }}
              >
                <span aria-hidden="true">{operator.symbol}</span>
                <strong>{operator.title[locale]}</strong>
                <small>o:{operator.symbol}</small>
              </button>
              <aside id={noteId} role="tooltip" aria-hidden={!open}>
                <header>
                  <span>{operator.symbol}</span>
                  <div>
                    <strong>{operator.title[locale]}</strong>
                    <code>{operator.semantic}</code>
                  </div>
                </header>
                <p>{operator.description[locale]}</p>
                <dl>
                  <div>
                    <dt>{labels.wire}</dt>
                    <dd>
                      <code>o:{operator.symbol}</code>
                      <code>{operator.operands}</code>
                    </dd>
                  </div>
                  <div>
                    <dt>{labels.token}</dt>
                    <dd>
                      <code>{operator.tokens}</code>
                      <small>{labels.note}</small>
                    </dd>
                  </div>
                </dl>
              </aside>
            </div>
          );
        })}
      </div>
      <footer>
        <span>{labels.evidence}</span>
        <small>tiktoken-rs 0.12.0 · cl100k_base · o200k_base</small>
      </footer>
    </div>
  );
}
