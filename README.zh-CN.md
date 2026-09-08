<p align="center">
  <img src="./assets/readme/hero.svg" width="100%" alt="ash — AI Native
Shell executing typed work across bounded I/O and CPU planes and
returning compact ASON evidence">
</p>


<p align="center">
  <strong>Language / 语言:</strong>  <a href="README.md">English</a> ·
<a href="README.zh-CN.md">中文</a>
</p>

<p align="center"><strong>AI Native Shell</strong> · 类型化执行 · 受保护的突变 · 紧凑、可检索的证据</p>

<p align="center">
  <a href="https://a3s-lab.github.io/ash/">中文网站</a>·  <a
href="https://a3s-lab.github.io/ash/en/">英文文档</a> ·  <a
href="https://a3s-lab.github.io/ash/guide/capabilities.html">能力</a> ·
<a href="https://a3s-lab.github.io/ash/guide/coding-agents.html">编码代
理技能</a> ·  <a
href="https://a3s-lab.github.io/ash/guide/install.html">安装</a>
</p>

> [!IMPORTANT]
> `ash` 已预发布。源代码实现、跨平台安装程序、
> 和失败关闭的六目标发布工作流程可用，但发布
> 未提供凭据且未提供受支持的签名二进制文件
> 已发表。从源代码构建以进行开发验证。

`ash` 是一个围绕编码代理而不是终端设计的全新 shell用户。它接受类型化的
ASH/1 程序，在显式下执行独立工作预算，并返回规范的 ASON，并参考完整保留
证据。没有隐藏的 shell 字符串、静默截断或完成顺序输出成为合同的一部分。

可选的人类前端已到达交互式 H1 检查点功能门控`ash shell`路线。终端调用打
开行编辑的 REPL具有可配置的提示、私有持久历史记录、选择加入启动配置文件
，和`exit [STATUS]`。相同的持久状态执行顺序`pwd`，`echo`、`cd`、`export`
、`unset`、`set` 管道故障控制、便携式 `ls`、`cat`、和 `grep`、可移植
`cp`、`mv`、`rm` 和仅创建的 `touch`，以及本机主机具有直接参数向量的可执
行文件。管道组成左关联 `&&``||`/ 条件列表和嵌套 `$(...)` 命令替换在命令
字和重定向目标中扩展。内联源代码、本机脚本文件和有界标准输入保持可用，无
需更改`ash run`和`ash rpc`的机器合同。 H2 现在提供明确的process-stdio 模
式、经过验证的本机操作系统管道图、两个的同线管道32 个本机、可移植或实现
的有状态内置阶段，可配置`pipefail`，以及按源顺序的本机、可移植或有状态重
定向。每个承认的外部管道在其任何阶段执行之前已完全预检；短路条件分支不会
被扩展、解析或打开。扩展时间替换按源顺序运行，因此来自如果后续阶段预检失
败，则保留较早的替换。本机对保持直接连接；进程内边界使用显式父级拥有的异
步管道和文件处理并保持操作系统背压。在全局源中打开混合阶段文件产卵前订购
。更换或非消耗性内部端点仍然可以提供服务EOF 或本机断管行为。最终标准输出
和本机标准错误共享剩余的有限捕获津贴。声明父资源后，原生图变成一张
`NativeProcessJob`：等待保存规范顺序，任何生成后的设置、捕获或等待失败都
会终止并收获每一个返回之前本机成员拥有的进程树。第一个 H3 检查点通过相同
的路径路由这四个可移植突变BLAKE3-原像、日志式、无覆盖事务服务，如 ASH/1
`fs`。第二个添加了源跨度、左关联 `&&``||`/ 完整列表管道状态，无需引入隐
式主机 shell。第三个添加了递归解析、跨源命令替换克隆 shell 状态、有界捕
获、引号感知字段行为和无主机外壳字符串。

## 灰覆盖了什么

|表面|运营|实施了什么 |
| -------------------- | -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
|存储库发现 | `read r`、`list l`、`search g` |工作空间限制的字节/行读取、稳定遍历、文字和正则表达式搜索 |
|流程| `exec x`、`cancel k` |直接可执行文件 + argv 启动、环境/标准输入控制、截止日期、并发标准输出/标准错误捕获以及拥有的进程树清理 |
|保护突变| `patch p`、`fs f` | BLAKE3 比较和交换编辑以及日志记录、无覆盖文件创建/复制/移动/删除，并具有回滚和重新启动恢复功能 |
|并行程序| `batch b` |已验证的非循环图、就绪节点并发、失败后代跳过、独立消耗和稳定的最低索引错误 |
|工作区状态 | `snapshot s` |确定性范围清单和匹配之前/之后的增量 |
|保留证据| `/ # ? - \| >` |字节和行切片、搜索、发布、有序表投影和功能门控具体化 |
|模型上下文 | ASON、`×N`、`×N#K`、`⋯N` |列式记录、路径字典、显式缩减、稳定合并以及对完整源代码的引用 |
|信任与交付|功能、许可、签名更新|最低权限协商、会话/操作/策略/过期一次性许可、重放拒绝、事务激活、恢复、回滚、SBOM 和来源 |
|人体外壳| `ash shell` | H1 REPL 生命周期、H2 监督流/重定向和 H3 日志突变、`&&`/`||` 管道列表，以及嵌套 `$(...)` 命令替换 |

[complete capability
map](https://a3s-lab.github.io/ash/guide/capabilities.html)记录整个表面
的保证、证据和故意的非目标。

## 编码代理技能

该存储库包含项目本机代理技能：

```text
.agents/skills/use-ash/
├── SKILL.md
├── agents/openai.yaml
└── references/
    ├── operations.md
    └── workflows.md
```

它教代理选择一个操作，发出准确的`t,i,o,a,u`信封，使用消化保护突变，构建
有界 DAG，检索保留的证据，并验证结果。在兼容的编码代理中调用它：

```text
Use $use-ash to inspect this repository, make the requested change, and verify it.
```

阅读[Coding Agent integration
guide](https://a3s-lab.github.io/ash/guide/coding-agents.html)或检查
[Skill source](./.agents/skills/use-ash/SKILL.md)。

## 第一个人类命令

在终端上运行 `ash shell` 以获取默认的 `ash> ` 提示符，或选择一个非交互式
源明确：

```sh
ash shell
ash shell -c "grep -in 'semantic' crates/ash-ops/src/semantic.rs"
ash shell -c "linux:uname -a" # Windows with WSL
ash shell ./script.ash
printf 'echo from-stdin\n' | ash shell --no-profile
ash shell --profile ./profile.ash
```

每个脚本、标准输入源和配置文件都限制为 1 MiB 的有效 UTF-8。使用当文件操
作数以 `-` 开头时，`ash shell -- ./-script.ash`。个人资料可以通过
`--profile FILE`或非空`ASH_PROFILE`选择加入；`--no-profile` 提供确定性恢
复。配置文件已完全解析在它执行任何一个之前。解析失败会停止非交互式启动，
而交互式 shell 报告源跨度并且仍然打开，没有部分轮廓效果。
`exit [STATUS]` 接受 0 到 255，使用之前的状态当省略时，并停止剩余的提交
源。

`ASH_PROMPT` 替换提示并且必须是有效的 UTF-8。 `ASH_HISTORY`选择相对于初
始 cwd 的历史文件；空值会禁用持久性历史。否则默认为
`$XDG_STATE_HOME/ash/history`，`$HOME/.local/state/ash/history`，或
`%LOCALAPPDATA%\ash\history`，取决于主机。不记录以空格或制表符开头的行。
历史档案当它们是符号链接或非常规目标时被拒绝，使用模式Unix 上的 `0600`，
如果持续存在，则降级为内存中会话并发出警告不安全或不可用。

可移植的`ls`列出一个路径（默认`.`），每个路径发出一个稳定的本机名称线，
支持`-a``--all`/、`-d``--directory`/、`-1`、组合短线选项和`--`。不受支持
的 GNU 选项显然会失败。便携式`cat`需要一个文件路径，无需转换即可写入其字
节或添加换行符，接受 `--`，并共享 128 MiB 语义读取/捕获天花板。在多级管
道中，`cat -`消耗传入的字节流；在一个简单的命令中，它使用一个显式的 `<`
文件。一个未重定向的独立的标准输入操作数仍然是一个显式错误。选项和多个文
件没有实施。可移植 `grep` 需要一个有效的 UTF-8 常规文件并使用 Rust 常规
默认表达式。支持`-E``--extended-regexp`/，`-F``--fixed-strings`/、`-i`
`--ignore-case`/、`-n``--line-number`/，组合空头期权和`--`。搜索限制为
64 MiB；没有匹配返回状态1 无诊断。在多级管道中，`grep PATTERN -`消耗具有
相同搜索语义和 128 MiB 输出的传入 UTF-8 流天花板；一个简单的命令可以提供
`-`到`<`。目录，多个文件、未重定向的独立标准输入`-`以及不受支持的选项失
败明确地。

便携式 `cp SOURCE DESTINATION`、`mv SOURCE DESTINATION`、`rm PATH` 和
`touch PATH` 仅接受那些精确的常规文件参数加上 `--`。他们绑定当前cwd作为
持久事务根，拒绝父级遍历，符号链接/重新解析遍历、目录、超过 128 MiB 的文
件以及路径不能由 UTF-8 事务日志表示。复制、移动和触摸切勿覆盖；该检查点
的 `touch` 创建一个新的空文件并执行不更新现有文件上的时间戳。复制、移动
和删除派生BLAKE3 在执行前立即进行原像，然后是共享事务重新验证它而不进行
静默重试。冲突返回状态 1 并保留外部更改或现有的文件。下保留的`.ash`目录
事务根拥有跨进程锁定、回滚和重启恢复。

`export NAME=VALUE` 和 `unset NAME` 更新 shell 变量和导出的变量后续命令
的环境状态。每人接受一项扩展作业或姓名加`--`；名称是 ASCII shell 标识符
，保留空值，并且取消设置丢失的名称成功。引用可能包含字段的值分隔符，例如
`export COPY="$SOURCE"`。列表和多个名称在此检查点中保持显式非特征。

`set -o pipefail` 为持久化启用最右边的故障管道状态shell状态，而
`set +o pipefail`恢复默认的最终阶段策略。其他`set`形式仍然存在明显错误。
Profile 可以先选择策略主要来源运行。在管道中，`set` 在该阶段的状态克隆上
运行并且不能改变父策略。

`$NAME`、`${NAME}`、`$?` 和嵌套的 `$(...)` 在每个之前立即展开命令解决。
单引号和转义美元仍然是字面意思；双引号保留一个字段，而未加引号的值则在固
定的 ASCII 空间上分割，制表符和 LF 分隔符。变量先于主机感知的导出环境查
找，未定义的值为空，并且保留本机参数单元通过直接 argv 启动。

每个命令替换都会递归地解析为相同类型的`Script`计划，具有 32 级嵌套限制和
用于诊断的顶级源跨度。它针对完整的`ShellState`克隆执行：cwd、变量、环境
、选项、最后状态和`exit`保留在本地，而普通外部进程并且文件系统的影响仍然
可见。阿什完成了替换stdout，删除每个尾随 LF，保留 double 内的内部换行符
引号，并且仅在未加引号时应用固定字段拆分。非零嵌套status 不会覆盖父级
`$?` 或阻止外部命令；嵌套的stderr 和诊断传播一次。 NUL 输出被拒绝。 Unix
保留非 UTF-8 标准输出作为本机参数字节，而 Windows 需要有效的 UTF-8。替换
值、stdout 和 stderr 共享剩余的 128 MiB 同步空间捕获允许，并且捕获失败会
阻止外部命令运行。执行命令字和文件重定向目标之间的替换按源顺序；短路的管
道不会执行其中任何一个。

不带引号的参数和命令替换字段拆分后，路径名扩展适用 `*`、`?`、`[abc]`、升
序 `[a-z]` 以及否定 `[!abc]` 或`[^abc]` 命令字段和文件重定向目标的模式。
单引号，双引号和反斜杠保护模式字符；不带引号的参数或替换输出保持模式活跃
。相对模式枚举从持久的 cwd 中，绝对模式保持绝对，匹配按以下顺序排序无损
本机路径单元和 Unix 非 UTF-8 名称仍然可表示。一个通配符永远不会选择前导
点名称，除非该组件以字面点。匹配区分大小写，`**`没有递归意义，并且未终止
、空或降序类或没有匹配项的模式失败在外部命令运行之前。一个命令及其重定向
共享限制32,768 个活动模式单元、65,536 个已检查目录条目和 4,096 个匹配。
短路管道不执行目录扫描。

本机命令通过 shell 状态的 `PATH` 或显式解析`native:`前缀，然后直接使用解
析后的可执行文件启动参数向量、当前目录和导出环境。没有`sh -c`，`cmd /c`
，或插入 PowerShell 命令字符串。未重定向的独立孩子的标准输入保持为空并且
输出保持同步捕获，因此本机本身需要前台终端的程序仍然推迟到H4工作控制工作
。 Stdout 和 stderr 共享剩余的 128 MiB 捕获津贴，并返回本机退出状态。

在 Windows 上，`linux:COMMAND` 显式选择 WSL。分辨率首先定位`wsl.exe`;丢
失的启动器是类型化的后端不可用故障，并且未解决的普通命令永远不会落入 WSL
。包装器接收一个可选的选定发行版，当前的 Windows cwd 至 `--cd`，以及
Linux 命令在 `--exec` 之后分别加上每个参数。无主机或 Linux shell字符串被
合成。当前 CLI 使用用户的默认 WSL分布，而嵌入者可以通过`ShellOptions`选
择一个；所选择的值保留在命令状态中。安装分布探测，一般参数路径映射、显式
环境转发、中断正常化和后端政策仍然是 H5 工作。

本机、WSL 和可移植命令以及已实现的有状态内置命令接受`<`、`>`、`>>`、`2>`
、`2>>`、`2>&1` 和 `1>&2`。重定向是从左到右应用的，因此
`command >out 2>&1` 合并两者流到 `out`，而 `command 2>&1 >out` 将 stderr
保留在原始版本上标准输出捕获。文件目标，包括 `$(...)` 和路径名模式，必须
完全作为一个本地字段完成，解析自持久 cwd，并直接连接到子任务或父任务操作
系统句柄不在 shell 内存中缓冲文件输出。一张图顺序交错按阶段和重定向源顺
序划分的本机、可移植和有状态资源；被取代的目标仍然开放。目标缺失、不明确
或无法打开返回带有重定向诊断的状态 1。有状态和可移植突变参数经过验证在文
件打开之前；成功的打开先于父状态突变，所以稍后内置文件系统故障保留了正常
的 shell 文件副作用。便携式突变交易同样仅在所有重定向文件之后开始打开成
功。有状态的内置函数不发出原始命令输出，并且它们的跨源诊断仍然存在shell
stderr 而不是由命令 stderr 重定向捕获。世界SL文件由 Windows 主机以相同的
源顺序打开并附加直接连接到包装纸的标准手柄。

同一条线`|`构成2到32级的前台流水线。每个阶段都可以是本机主机命令，
Windows 上的显式 WSL 命令，可移植 `pwd`，`echo`、`ls`、`cat`、`grep`、
`cp`、`mv`、`rm`或`touch`，或实现的有状态`cd`、`export`、`unset`、`set`
、或`exit`；别名、函数和未实现的有状态命令在完整的预检期间仍然失败。本机
和 WSL 包装器边缘保留直接操作系统管道。正在进行中边界仅保留显式声明的父
读取器或写入器并运行该阶段与本机执行和捕获同时进行。原生流和可移植的
`cat``grep`/流在下一个流之前不会作为一个整体具体化阶段，并且应用正常的操
作系统背压。仅`cat -` 和 `grep PATTERN -` 消耗传入的标准输入。其他便携式
表格关闭该读取器，允许上游写入器和`pipefail`观察损坏的管道失败。突变阶段
不发出标准输出，执行其文件系统事务，并将冲突或文件系统状态贡献给相同的
`pipefail`向量。有状态阶段同样关闭传入的标准输入，执行独立的`ShellState`
克隆，并在完成后关闭它们的空标准输出，所以他们不能改变父级和下游读者收到
的 EOF。管道`exit` 仅贡献其阶段状态，并且永远不会停止父源。一个最终进程
内阶段是在相同的 128 MiB 聚合下同时捕获的津贴；本机 stderr 按阶段顺序捕
获。所有本机成员（包括 WSL 包装器）仍由一名图级作业主管拥有，同时可移植/
有状态的 future 和捕获消耗被一起轮询。一旦那些结算成功，supervisor 完成
每一次本地等待，无需取消正在进行的收获。设置、捕获或等待失败终止并收获每
一个本机进程树；成功退出与来源保持一致阶段顺序。

管道状态默认为最后阶段。有了`set -o pipefail`，就变成了最右边不成功的阶
段，包括常规的`128 + signal`测绘；完全成功的管道仍然使用其最后阶段。丢失
的便携式文件、不匹配`grep`、有状态内置失败或进程内写入器损坏参与相同的退
出向量。任何本机、WSL、可移植或已实现的有状态阶段可能会重定向 stdin、
stdout 或 stderr 或重复描述符源订单。如果生产者没有更长地写入内部管道，
下游阅读器收到EOF。如果消费者替换了它的管道标准输入，上游编写器接收平台
的断管行为。描述符即使原始描述符已连接，从管道复制的副本仍然保持连接后来
重定向。有状态文件加入相同的全局开放秩序；替换它们的空标准输出通常会关闭
传出管道。 WSL 阶段重用相同的管道和文件图，没有中间中继或全流缓冲区。

`&&` 和 `||` 以相同的优先级和左侧链接完整的管道关联性。 `&&` 仅在状态 0
后才允许下一个管道； `||`承认仅在非零状态之后。跳过的管道不执行扩展，解
析、参数验证、重定向打开、进程启动、状态更改、或文件系统事务，并将前面的
状态保留给下一个链接和`$?`。换行符或注释可以在运算符之后继续列表；`;` 或
未链接的换行符开始一个新的无条件列表。完整源码仍然在任何命令运行之前进行
解析，因此格式错误的尾随分支会阻止所有前缀效果。承认的`exit`会停止源头；
跳过的则不会。

用户可见的终端流、前台交互程序和作业控制、别名、函数、子 shell 状态、剩
余命令语言，剩余的H5 WSL策略、路径、环境和中断合约尚未实施。最小的可以使
用 `--no-default-features` 构建仅限机器的二进制文件。

## 第一个输入的请求

此规范请求使用显式标记在 `src` 中搜索文字 `TODO`，记录和挂钟预算：

```ason
t:1
i:17
o:g
a{q,p,f}:
TODO,[src],0
u{tok,rec,ms}:
256,64,30000
```

从工作区包含 `src` 的源包中运行已检查的装置：

```sh
cd crates/ash-cli
cargo run -p a3s-ash -- run < ../../spec/fixtures/ason/search-request.ason
```

安装的二进制文件使用`ash run < request.ason`。 Windows PowerShell 必须保
留规范的 UTF-8/LF 字节完好无损；使用
`Start-Process ash -ArgumentList run -NoNewWindow -Wait -RedirectStandardInput request.ason`
而不是通过管道传输解码后的字符串。

为了实现长期集成，`ash rpc` 添加了框架握手、并发请求、取消、功能、许可和
保留引用生命周期。在创作请求时使用 `ash ason` 来验证和规范化请求。引用是
会话本地的：以后的`ash run`进程不能使用别名，快照基线、批次子响应、取消
目标或许可较早进程返回的挑战。需要这些值的工作流程必须保持一个框架
`ash rpc`会话处于活动状态。

## 一个运行时，两个执行平面

<p align="center">
  <img src="./assets/readme/architecture.svg" width="100%" alt="ash
architecture from Coding Agent through typed program and hierarchical
governor into Tokio I/O and Rayon CPU planes, stable merge, and
canonical ASON">
</p>

Tokio 拥有 RPC、子进程、管道、截止日期和取消。一个固定的Rayon 工作窃取池
拥有搜索、散列、差异、缩减、存储提交、和其他可分割的CPU工作。一个分层的
调控器限制主机、会话、请求和操作并发。稳定合并消除了工人完成顺序在规范
ASON发布之前。

大型流程流保持无损，不会淹没模型上下文。一个固定头/尾投影立即返回；超过
4 MiB 的证据会话内存上限溢出到私有不可变文件。有界范围获取，别名、重复数
据删除、租用、发布以及经过验证的崩溃孤儿清理已完成商店生命周期。

## 证据，而非愿望

当前的`main`基线包括：

- **327 Rust 工作区测试** 跨协议模式、RPC、每个操作，
  交易、恢复、保留存储、取消、签名更新，以及  人类的外壳。
- **跨工作矩阵的 22 个 schema-14 运行时场景**，包括 8 MiB
  保留捕获跨越 4 MiB 内存上限并仅获取其最终结果  64 KiB。
- **7 个锁定的编码代理任务** 比较本机 shell 和 ash 跟踪
  一项任务、结果和成绩单模式。
- **1,024 个详尽的四节点 DAG/成功掩码案例**，强制完成
  订单变化和稳定的错误选择。
- **30 个远期交易切点加上 12 个恢复切点**，包括
  硬链接身份崩溃窗口。
- 源绑定格式/令牌报告、每周两次模糊测试、AddressSanitizer
  工件、六目标安装程序烟雾测试和第三方许可证门。

检查格式语料库报告规范 ASON 为紧凑行对象的 **62%两个固定标记生成器的
JSON 标记**。明确的减少和参考公式分别测量；完整的来源仍然可以检索。

重现门：

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

阅读[benchmark
contract](https://a3s-lab.github.io/ash/guide/benchmarks.html)在解释数字
之前。

## 安装

> [!WARNING]
> 版本安装程序故意失败关闭，直到受支持的签名
> 二进制文件存在。下面的 Cargo 命令构建当前源；它不是一个
> 签署发布。

Linux 和 macOS、x86-64 和 ARM64：

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/A3S-Lab/ash/main/install.sh | sh
```

Windows PowerShell、x86-64 和 ARM64：

```powershell
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
irm https://raw.githubusercontent.com/A3S-Lab/ash/main/install.ps1 | iex
```

使用 Cargo 构建当前源：

```sh
cargo install --git https://github.com/A3S-Lab/ash --locked a3s-ash
```

请参阅 [installation](https://a3s-lab.github.io/ash/guide/install.html)
进行固定版本、自定义前缀、离线存档、验证、事务激活、升级、回滚、恢复和卸
载。

## 明确边界

ASH/1不是人类的REPL、POSIX兼容层、嵌入式模型、远程执行器，或通用子进程沙
箱。它不提供互动终端语义、shell 语言评估、覆盖、递归目录突变，或批处理节
点之间的运行时值管道。子程序继承ash 进程的操作系统权限。

这些是合同边界，而不是未记录的差距。参见
[security](https://a3s-lab.github.io/ash/guide/security.html) 精确能力、
许可、路径、交易和签名更新模型。

## 文档导航

- [Get started](https://a3s-lab.github.io/ash/guide/) — 读取器路径和第一个请求
- [Complete capabilities](https://a3s-lab.github.io/ash/guide/capabilities.html) — 每项操作、保证和非目标
- [Coding Agent integration](https://a3s-lab.github.io/ash/guide/coding-agents.html) — 技能和驾驭工作流程
- [Architecture](./docs/architecture.md) — 语义 IR、调度程序、调控器和平台边界
- [Portable human-shell architecture](./docs/portable-human-shell.md) 和 [separation decision](./docs/decisions/0002-separate-portable-human-shell-layer.md)
- [ASH/1 and ASON](./docs/protocol.md) — 权威的电汇和数据合约
- [Distribution](./docs/distribution.md) 和 [release operations](./docs/releasing.md)
- [Benchmark methodology](./docs/benchmarks.md)

贡献遵循项目[CONTRIBUTING.md](./CONTRIBUTING.md)[Code of
Conduct](./CODE_OF_CONDUCT.md)和[SECURITY.md](./SECURITY.md)。Rust 工作
区已获得 MIT 许可。
