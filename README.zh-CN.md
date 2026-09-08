<p align="center">
  <img src="./assets/readme/hero.svg" width="100%" alt="ash — AI Native Shell 在有界 I/O 与 CPU 平面上执行类型化工作，并返回紧凑的 ASON 证据">
</p>

<p align="center">
  <strong>Language / 语言:</strong>
  <a href="README.md">English</a> ·
  <a href="README.zh-CN.md">中文</a>
</p>

<p align="center"><strong>AI Native Shell</strong> · 类型化执行 · 受护变异 · 紧凑可检索证据</p>

<p align="center">
  <a href="https://a3s-lab.github.io/ash/">中文网站</a> ·
  <a href="https://a3s-lab.github.io/ash/en/">English docs</a> ·
  <a href="https://a3s-lab.github.io/ash/guide/capabilities.html">能力</a> ·
  <a href="https://a3s-lab.github.io/ash/guide/coding-agents.html">Coding Agent Skill</a> ·
  <a href="https://a3s-lab.github.io/ash/guide/install.html">安装</a>
</p>

> [!IMPORTANT]
> `ash` 处于预发布阶段。源码实现、跨平台安装器与失败关闭的六目标发布工作流
> 均已可用，但发布凭证尚未配置，也尚未发布受支持的已签名二进制。
> 请从源码构建以进行开发验证。

`ash` 是围绕 Coding Agent 而非终端用户设计的全新 shell。它接受类型化 ASH/1
程序，在显式预算下执行独立工作，并返回规范 ASON，以及对完整保留证据的引用。
隐藏的 shell 字符串、静默截断或按完成顺序的输出都不会成为契约的一部分。

可选的人类前端已达到交互式 H1 检查点，并提供特性门控的 `ash shell` 路由。
在终端调用会打开带行编辑的 REPL，含可配置提示符、私有持久历史、可选启动
Profile，以及 `exit [STATUS]`。同一持久状态执行顺序的 `pwd`、`echo`、`cd`、
`export`、`unset`、`set` pipefail 控制、可移植 `ls`、`cat` 与 `grep`、可移植
`cp`、`mv`、`rm`，以及仅创建的 `touch`，外加带直接参数向量的原生主机可执行文件。
管道可组合为左结合的 `&&`/`||` 条件列表，嵌套的 `$(...)` 命令替换可在命令词与
重定向目标中展开。内联源、原生脚本文件与有界 stdin 仍然可用，且不改变
`ash run` 与 `ash rpc` 的机器契约。H2 现提供显式进程 stdio 模式、经验证的原生
OS 管道图、两到 32 个原生/可移植/已实现有状态内建阶段的同行管道、可配置
`pipefail`，以及按源顺序的原生、可移植或有状态重定向。每个被准入的外层管道在
任一阶段执行前都经过完整预检；短路的条件分支不会被展开、解析或打开。
展开期替换按源顺序运行，因此若较晚阶段预检失败，较早替换的外部效应仍会保留。
原生对保持直接连接；进程内边界使用显式的父拥有异步管道与文件句柄，并保留
OS 背压。混合阶段文件在 spawn 前按全局源顺序打开。被替换或不消费的内部端点
仍交付 EOF 或原生 broken-pipe 行为。最终 stdout 与原生 stderr 共享剩余的有界捕获配额。
在父资源被声明后，原生图成为单一 `NativeProcessJob`：等待保留规范顺序，
且任何 spawn 后的设置、捕获或等待失败都会在返回前终止并回收每个原生成员拥有的进程树。
第一个 H3 检查点将这四种可移植变异路由到与 ASH/1 `fs` 相同的 BLAKE3 原像、
日记式、不覆盖事务服务。第二个检查点在完整管道状态上增加带源跨度、左结合的
`&&`/`||` 列表，且不引入隐式主机 shell。第三个检查点增加递归解析、带源跨度的
命令替换，含克隆的 shell 状态、有界捕获、感知引号的字段行为，且无主机 shell 字符串。

## ash 覆盖范围

| 表面 | 操作 | 已实现内容 |
| -------------------- | ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 仓库发现 | `read r`、`list l`、`search g` | 工作区受限的字节/行读取、稳定遍历、字面量与正则搜索 |
| 进程 | `exec x`、`cancel k` | 直接可执行文件 + argv 启动、环境/stdin 控制、截止期限、并发 stdout/stderr 捕获，以及拥有的进程树清理 |
| 受护变异 | `patch p`、`fs f` | BLAKE3 比较并交换编辑，以及带回滚与重启恢复的日记式、不覆盖文件创建/复制/移动/删除 |
| 并行程序 | `batch b` | 经验证的无环图、就绪节点并发、失败后代跳过、独立排空，以及稳定的最低索引错误 |
| 工作区状态 | `snapshot s` | 确定性作用域清单与匹配的 before/after 增量 |
| 保留证据 | `/ # ? - \| >` | 字节与行切片、搜索、释放、有序表投影，以及能力门控的物化 |
| 模型上下文 | ASON、`×N`、`×N#K`、`⋯N` | 列式记录、路径字典、显式归约、稳定合并，以及回指完整源的引用 |
| 信任与交付 | capabilities、permits、已签名更新 | 最小权限协商、会话/动作/策略/过期绑定的一次性许可、重放拒绝、事务性激活、恢复、回滚、SBOM 与溯源 |
| 人类 shell | `ash shell` | H1 REPL 生命周期、H2 受监督流式/重定向，以及 H3 日记式变异、`&&`/`||` 管道列表，外加嵌套 `$(...)` 命令替换 |

[完整能力图](https://a3s-lab.github.io/ash/guide/capabilities.html)
记录完整表面的保证、证据与刻意非目标。

## Coding Agent Skill

仓库包含项目原生的 Agent Skill：

```text
.agents/skills/use-ash/
├── SKILL.md
├── agents/openai.yaml
└── references/
    ├── operations.md
    └── workflows.md
```

它教导 Agent 选择操作、发出精确的 `t,i,o,a,u` 信封、使用摘要保护的变异、
构建有界 DAG、检索保留证据并验证结果。在兼容的 Coding Agent 中这样调用：

```text
Use $use-ash to inspect this repository, make the requested change, and verify it.
```

阅读 [Coding Agent 集成指南](https://a3s-lab.github.io/ash/guide/coding-agents.html)
或查看 [Skill 源码](./.agents/skills/use-ash/SKILL.md)。

## 首批人类命令

在终端运行 `ash shell` 以获得默认 `ash> ` 提示符，或显式选择
非交互源：

```sh
ash shell
ash shell -c "grep -in 'semantic' crates/ash-ops/src/semantic.rs"
ash shell -c "linux:uname -a" # Windows with WSL
ash shell ./script.ash
printf 'echo from-stdin\n' | ash shell --no-profile
ash shell --profile ./profile.ash
```

每个脚本、stdin 源与 Profile 限制为 1 MiB 有效 UTF-8。当文件操作数以 `-`
开头时，使用 `ash shell -- ./-script.ash`。
Profile 通过 `--profile FILE` 或非空 `ASH_PROFILE` 选择启用；
`--no-profile` 提供确定性恢复。Profile 在执行任何部分前被完整解析。
解析失败会停止非交互启动，而交互 shell 报告源跨度并以无部分
Profile 效应的状态打开。`exit [STATUS]` 接受 0 到 255，省略时使用上一状态，
并停止剩余已提交源。

`ASH_PROMPT` 替换提示符且必须为有效 UTF-8。`ASH_HISTORY` 选择相对初始 cwd
的历史文件；空值禁用持久历史。否则默认是 `$XDG_STATE_HOME/ash/history`、
`$HOME/.local/state/ash/history`，或 `%LOCALAPPDATA%\ash\history`，取决于
主机。以空格或制表符开头的行不被记录。历史文件在为符号链接或非普通目标时
被拒绝，在 Unix 上使用模式 `0600`，并在持久化不安全或不可用时降级为带警告
的内存会话。

可移植 `ls` 列出一个路径（默认 `.`），每行发出一个稳定原生名，并支持
`-a`/`--all`、`-d`/`--directory`、`-1`、组合短选项与 `--`。不支持的 GNU
选项明确失败。
可移植 `cat` 需要一个文件路径，原样写出其字节且不转换、不追加换行，接受
`--`，并共享 128 MiB 语义读/捕获上限。在多阶段管道中，`cat -` 消费传入
字节流；在简单命令中它消费显式 `<` 文件。未重定向的独立 stdin 操作数仍是
显式错误。选项与多文件尚未实现。
可移植 `grep` 需要一个有效 UTF-8 普通文件，默认使用 Rust 正则表达式。它支持
`-E`/`--extended-regexp`、`-F`/`--fixed-strings`、`-i`/`--ignore-case`、
`-n`/`--line-number`、组合短选项与 `--`。搜索限制为 64 MiB；无匹配返回状态
1 且无诊断。在多阶段管道中，`grep PATTERN -` 以相同搜索语义消费传入 UTF-8
流，输出上限为 128 MiB；简单命令可通过 `<` 提供 `-`。目录、多文件、未重定向
的独立 stdin `-` 与不支持选项均明确失败。

可移植 `cp SOURCE DESTINATION`、`mv SOURCE DESTINATION`、`rm PATH` 与
`touch PATH` 仅接受这些精确的普通文件元数外加 `--`。它们将当前 cwd 绑定为
持久事务根，拒绝父级遍历、符号链接/reparse 遍历、目录、超过 128 MiB 的文件，
以及无法由 UTF-8 事务日记表示的路径。复制、移动与 touch 从不覆盖；本检查点的
`touch` 创建新的空文件，且不更新已有文件的时间戳。复制、移动与删除在执行前
立即派生 BLAKE3 原像，然后共享事务在无静默重试的情况下重新验证它。冲突返回
状态 1 并保留外部已更改或已存在的文件。事务根下保留的 `.ash` 目录拥有跨进程
锁定、回滚与重启恢复。

`export NAME=VALUE` 与 `unset NAME` 同时更新 shell 变量与导出环境状态，供后续
命令使用。每个接受一个已展开赋值或名称外加 `--`；名称是 ASCII shell 标识符，
空值被保留，取消不存在的名称成功。可能包含字段分隔符的值请加引号，例如
`export COPY="$SOURCE"`。列出与多个名称在本检查点仍是显式非功能。

`set -o pipefail` 为持久 shell 状态启用最右失败管道状态，而 `set +o pipefail`
恢复默认的最终阶段策略。其他 `set` 形式仍是显式错误。Profile 可在主源运行前
选择该策略。在管道中，`set` 作用于该阶段的状态克隆，不能变更父策略。

`$NAME`、`${NAME}`、`$?` 与嵌套 `$(...)` 在每个命令解析前立即展开。单引号与
转义美元符保持字面；双引号保留一个字段，而未加引号的值按固定 ASCII 空格、
制表符与 LF 分隔符拆分。变量优先于主机感知的导出环境查找，未定义值为空，
原生参数单元经直接 argv 启动保留。

每个命令替换被递归解析为同一类型化 `Script` 计划，嵌套限制 32 层，诊断使用
顶层源跨度。它针对完整 `ShellState` 克隆执行：cwd、变量、环境、选项、上一
状态与 `exit` 保持本地，而普通外部进程与文件系统效应仍然可见。Ash 捕获完整
替换 stdout，移除每个尾随 LF，在双引号内保留内部换行，且仅在未加引号时应用
固定字段拆分。非零嵌套状态不会覆盖父 `$?` 或阻止外层命令；嵌套 stderr 与
诊断传播一次。NUL 输出被拒绝。Unix 将非 UTF-8 stdout 保留为原生参数字节，
而 Windows 要求有效 UTF-8。替换值、stdout 与 stderr 共享剩余 128 MiB 同步
捕获配额，捕获失败会阻止外层命令运行。跨命令词与文件重定向目标的替换按源
顺序执行；短路管道不执行其中任何一个。

在未加引号的参数与命令替换字段拆分之后，路径名展开将 `*`、`?`、`[abc]`、
升序 `[a-z]` 以及取反的 `[!abc]` 或 `[^abc]` 模式应用于命令字段与文件重定向
目标。单引号、双引号与反斜杠保护模式字符；未加引号的参数或替换输出仍保持
模式活跃。相对模式从持久 cwd 枚举，绝对模式保持绝对，匹配按无损原生路径
单元排序，Unix 非 UTF-8 名称仍可表示。通配符从不选择以点开头的名称，除非
该组件以字面点开头。匹配区分大小写，`**` 无递归含义，未终止、空或降序的
字符类或无匹配的模式在外层命令运行前失败。一个命令及其重定向共享 32,768
个活跃模式单元、65,536 个已检查目录项与 4,096 个匹配的限制。短路管道不
执行目录扫描。

原生命令通过 shell 状态的 `PATH` 或显式 `native:` 前缀解析，然后以解析后的
参数向量、当前目录与导出环境直接启动已解析可执行文件。不插入 `sh -c`、
`cmd /c` 或 PowerShell 命令字符串。未重定向的独立子进程 stdin 保持为空，
输出保持同步捕获，因此本身需要前台终端的原生程序仍推迟到 H4 作业控制工作。
Stdout 与 stderr 共享剩余 128 MiB 捕获配额，并返回原生退出状态。

在 Windows 上，`linux:COMMAND` 显式选择 WSL。解析首先定位 `wsl.exe`；缺少
启动器是类型化的后端不可用失败，未解析的普通命令从不回退到 WSL。包装器接收
可选的选定发行版、通过 `--cd` 的当前 Windows cwd，以及在 `--exec` 之后分别
传入的 Linux 命令与每个参数。不合成主机或 Linux shell 字符串。当前 CLI 使用
用户的默认 WSL 发行版，而嵌入方可经 `ShellOptions` 选择一个；所选值保留在
命令状态中。已安装发行版探测、通用参数路径映射、显式环境转发、中断规范化与
后端策略仍属 H5 工作。

原生、WSL 与可移植命令以及已实现的有状态内建接受 `<`、`>`、`>>`、`2>`、
`2>>`、`2>&1` 与 `1>&2`。
重定向从左到右应用，因此 `command >out 2>&1` 将两个流合并到 `out`，而
`command 2>&1 >out` 将 stderr 留在原始 stdout 捕获上。文件目标（含 `$(...)`
与路径名模式）必须结束为恰好一个原生字段，从持久 cwd 解析，并直接连接到
子或父任务 OS 句柄，而不在 shell 内存中缓冲文件输出。一个图顺序按阶段与
重定向源顺序交错原生、可移植与有状态资源；被取代的目标仍会被打开。缺失、
歧义或无法打开的目标返回状态 1 与重定向诊断。有状态与可移植变异参数在文件
打开前校验；成功打开先于父状态变异，因此较晚的内建文件系统失败仍保留正常的
shell 文件副作用。可移植变异事务同样仅在其所有重定向文件成功打开后开始。
有状态内建不发出原始命令输出，其带源跨度的诊断仍是 shell stderr，而非被命令
stderr 重定向捕获。WSL 文件由 Windows 主机按相同源顺序打开，并直接附加到
包装器的标准句柄。

同行的 `|` 形成两到 32 个阶段的前台管道。每个阶段可以是原生主机命令、
Windows 上的显式 WSL 命令、可移植 `pwd`、`echo`、`ls`、`cat`、`grep`、`cp`、
`mv`、`rm` 或 `touch`，或已实现的有状态 `cd`、`export`、`unset`、`set` 或
`exit`；别名、函数与未实现的有状态命令仍在完整预检期间失败。原生与 WSL
包装器边保持直接 OS 管道。进程内边界仅保留显式声明的父读取器或写入器，并与
原生执行与捕获并发运行该阶段。原生流与可移植 `cat`/`grep` 流在下一阶段之前
不会整体物化，并应用正常 OS 背压。仅 `cat -` 与 `grep PATTERN -` 消费传入
stdin。其他可移植形式关闭该读取器，使上游写入器与 `pipefail` 能观察到
broken-pipe 失败。变异阶段不发出 stdout，执行其文件系统事务，并将冲突或
文件系统状态贡献到同一 `pipefail` 向量。有状态阶段同样关闭传入 stdin，针对
独立 `ShellState` 克隆执行，并在完成时关闭其空 stdout，因此它们不能变更父
状态，下游读取器收到 EOF。管道 `exit` 仅贡献其阶段状态，从不停止父源。最终
进程内阶段在同一 128 MiB 聚合配额下并发捕获；原生 stderr 按阶段顺序捕获。
所有原生成员（含 WSL 包装器）在可移植/有状态 future 与捕获排空一起轮询时，
仍由一个图级作业监督器拥有。一旦这些成功落定，监督器在不取消进行中回收的
情况下完成每次原生等待。设置、捕获或等待失败会终止并回收每个原生进程树；
成功退出仍与源阶段顺序对齐。

管道状态默认为最终阶段。启用 `set -o pipefail` 时，它变为最右不成功阶段，
含常规 `128 + signal` 映射；全成功管道仍使用其最终阶段。缺失可移植文件、
无匹配 `grep`、失败的有状态内建或损坏的进程内写入器参与同一退出向量。任何
原生、WSL、可移植或已实现有状态阶段可按源顺序重定向 stdin、stdout 或 stderr，
或复制描述符。若生产者不再写入内部管道，下游读取器收到 EOF。若消费者替换其
管道 stdin，上游写入器收到平台的 broken-pipe 行为。从管道复制的描述符即使
原始描述符稍后被重定向仍保持连接。有状态文件加入同一全局打开顺序；替换其空
stdout 会正常关闭外向管道。WSL 阶段复用同一管道与文件图，无中间中继或全流
缓冲。

`&&` 与 `||` 以相等优先级与左结合链接完整管道。`&&` 仅在状态 0 后准入下一
管道；`||` 仅在非零状态后准入。被跳过的管道不执行展开、解析、参数校验、
重定向打开、进程启动、状态变更或文件系统事务，并将前一状态留给下一链接与
`$?`。换行或注释可在运算符后继续列表；`;` 或未链接的换行开始新的无条件列表。
完整源仍在任何命令运行前解析，因此格式错误的尾随分支会阻止所有前缀效应。
被准入的 `exit` 停止源；被跳过的不会。

面向用户的终端流式、前台交互程序与作业控制、别名、函数、子 shell 状态、剩余
命令语言，以及剩余的 H5 WSL 策略、路径、环境与中断契约尚未实现。可用
`--no-default-features` 构建最小的仅机器二进制。

## 首个类型化请求

此规范请求在显式 token、记录与挂钟预算下，于 `src` 中搜索字面量 `TODO`：

```ason
t:1
i:17
o:g
a{q,p,f}:
TODO,[src],0
u{tok,rec,ms}:
256,64,30000
```

从工作区包含 `src` 的源码 crate 运行已校验 fixture：

```sh
cd crates/ash-cli
cargo run -p a3s-ash -- run < ../../spec/fixtures/ason/search-request.ason
```

已安装二进制使用 `ash run < request.ason`。Windows PowerShell 必须保持规范
UTF-8/LF 字节完整；请使用
`Start-Process ash -ArgumentList run -NoNewWindow -Wait -RedirectStandardInput request.ason`
，而不是通过管道传入已解码字符串。

对于长生命周期集成，`ash rpc` 增加成帧握手、并发请求、取消、能力、许可与
保留引用生命周期。使用 `ash ason` 在编写时校验并规范化请求。引用是会话本地
的：稍后的 `ash run` 进程不能消费较早进程返回的别名、快照基线、批量子响应、
取消目标或许可挑战。需要这些值的工作流必须保持一个成帧的 `ash rpc` 会话存活。

## 一个运行时，两个执行平面

<p align="center">
  <img src="./assets/readme/architecture.svg" width="100%" alt="ash 架构：从 Coding Agent 经类型化程序与分层治理器进入 Tokio I/O 与 Rayon CPU 平面，再经稳定合并输出规范 ASON">
</p>

Tokio 拥有 RPC、子进程、管道、截止期限与取消。固定的 Rayon work-stealing 池
拥有搜索、哈希、diff、归约、存储提交及其他可拆分 CPU 工作。一个分层治理器
约束主机、会话、请求与操作并发。稳定合并在发出规范 ASON 之前抹除 worker
完成顺序。

大型进程流在不淹没模型上下文的情况下保持无损。固定的头/尾投影立即返回；
超出 4 MiB 会话内存上限的证据溢出到私有不可变文件。有界范围获取、别名、
去重、租约、释放与经证明的崩溃孤儿清理完成存储生命周期。

## 证据，而非愿望

当前 `main` 基线包括：

- 跨协议模式、RPC、每个操作、事务、恢复、保留存储、取消、已签名更新与人类
  shell 的 **327 个 Rust 工作区测试**。
- 跨 worker 矩阵的 **22 个 schema-14 运行时场景**，包括越过 4 MiB 内存上限的
  8 MiB 保留捕获，且仅获取其最后 64 KiB。
- **7 个锁定的 Coding Agent 任务**，在同一任务、结果与转录模式下比较原生
  shell 与 ash 轨迹。
- **1,024 个穷尽的四节点 DAG/成功掩码用例**，含强制完成顺序变更与稳定错误
  选择。
- **30 个前向事务切点外加 12 个恢复切点**，含硬链接身份崩溃窗口。
- 源绑定的格式/token 报告、每周两次模糊测试、AddressSanitizer 产物、六目标
  安装器冒烟测试，以及第三方许可证门。

已校验格式语料报告：对两个固定分词器，规范 ASON 均为紧凑行对象 JSON token
的 **62%**。显式归约与引用公式单独度量；完整源仍可检索。

复现这些门：

```sh
cargo test --workspace --all-targets
cargo run -p a3s-ash-bench --release --locked -- \
  --check benches/reports/v0.1.0/format.json
cargo run -p a3s-ash-bench --release --locked -- \
  --check-task-lock benches/tasks/v1/lock.json
cargo run -p a3s-ash-bench --release --locked -- --tasks
cargo build -p a3s-ash --release --locked
cargo run -p a3s-ash-bench --release --locked -- --runtime
npm --prefix website ci
npm --prefix website run check
```

在解读数字之前，请阅读
[基准契约](https://a3s-lab.github.io/ash/guide/benchmarks.html)。

## 安装

> [!WARNING]
> 发布安装器在受支持的已签名二进制存在之前刻意失败关闭。下方 Cargo 命令构建
> 当前源码；它不是已签名发布。

Linux 与 macOS，x86-64 与 ARM64：

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh
```

Windows PowerShell，x86-64 与 ARM64：

```powershell
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
irm https://raw.githubusercontent.com/A3S-Lab/ash/main/install.ps1 | iex
```

用 Cargo 构建当前源码：

```sh
cargo install --git https://github.com/A3S-Lab/ash --locked a3s-ash
```

固定版本、自定义前缀、离线归档、验证、事务性激活、升级、回滚、恢复与卸载见
[安装](https://a3s-lab.github.io/ash/guide/install.html)。

## 刻意边界

ASH/1 不是人类 REPL、POSIX 兼容层、嵌入式模型、远程执行器或通用子进程沙箱。
它不提供交互终端语义、shell 语言求值、覆盖、递归目录变异，或批处理节点之间
的运行时值管道。子程序继承 ash 进程的操作系统权限。

这些是契约边界，而非未文档化的缺口。精确的能力、许可、路径、事务与已签名
更新模型见 [安全](https://a3s-lab.github.io/ash/guide/security.html)。

## 文档地图

- [入门](https://a3s-lab.github.io/ash/guide/) — 读者路径与首次请求
- [完整能力](https://a3s-lab.github.io/ash/guide/capabilities.html) — 每个操作、保证与非目标
- [Coding Agent 集成](https://a3s-lab.github.io/ash/guide/coding-agents.html) — Skill 与 harness 工作流
- [架构](./docs/architecture.md) — 语义 IR、调度器、治理器与平台边界
- [可移植人类 shell 架构](./docs/portable-human-shell.md) 与 [分离决策](./docs/decisions/0002-separate-portable-human-shell-layer.md)
- [ASH/1 与 ASON](./docs/protocol.md) — 权威线缆与数据契约
- [分发](./docs/distribution.md) 与 [发布运维](./docs/releasing.md)
- [基准方法](./docs/benchmarks.md)

贡献遵循 [CONTRIBUTING.md](./CONTRIBUTING.md)、项目
[行为准则](./CODE_OF_CONDUCT.md) 与 [SECURITY.md](./SECURITY.md)。
Rust 工作区采用 MIT 许可。
