<div align="center">

<img src="docs/assets/logo.png" alt="Berimor" width="640">

**模型思考，代码决定。**

[Русский](README.md) · [English](README.en.md) · [Deutsch](README.de.md) · [Français](README.fr.md) · [Español](README.es.md) · **[简体中文](README.zh-CN.md)** · [日本語](README.ja.md) · [한국어](README.ko.md)

</div>

面向 LLM 的通用智能体，拥有确定性内核：任务路由、流程分支、上下文筛选与执行许可均由代码决定——模型只执行狭窄、可验证的步骤。兼容本地与云端模型，无论强弱。

[![GitHub release](https://img.shields.io/github/v/release/devpilgrin/berimor?logo=github&label=release)](https://github.com/devpilgrin/berimor/releases/latest)
[![npm](https://img.shields.io/npm/v/berimor?logo=npm&label=npm)](https://www.npmjs.com/package/berimor)
[![CI](https://img.shields.io/github/actions/workflow/status/devpilgrin/berimor/ci.yml?branch=main&label=CI)](https://github.com/devpilgrin/berimor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-927%20green-brightgreen)](#项目基础设施)

![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)
![WebAssembly](https://img.shields.io/badge/sandbox-Wasmtime-654FF0?logo=webassembly&logoColor=white)
![QuickJS](https://img.shields.io/badge/guest-QuickJS-F7DF1E?logo=javascript&logoColor=black)
![SQLite](https://img.shields.io/badge/storage-SQLite%20%2B%20FTS5%20%2B%20vec-003B57?logo=sqlite&logoColor=white)
![tokio](https://img.shields.io/badge/async-tokio-0B7B8A)
![MCP](https://img.shields.io/badge/protocol-MCP-5B5BD6)
![ratatui](https://img.shields.io/badge/TUI-ratatui-E95420)
![sigstore](https://img.shields.io/badge/supply--chain-sigstore%20keyless-2E8B57)
[![Socket](https://badge.socket.dev/npm/package/berimor)](https://socket.dev/npm/package/berimor)
![oxc](https://img.shields.io/badge/static%20analysis-oxc__parser-black)

---

## 为什么需要它

大多数"AI 智能体"的结构如出一辙：给模型一组工具，让它自己决定该做什么。做演示——很方便；做实际工作——不可靠：模型会忘记步骤、编造事实、跑偏方向，而危险命令在你习惯性地按下"y"后就直奔终端而去。

Berimor 建立在相反的假设之上：**不能把编排（orchestration）交给模型——只能把执行交给它。** 任务要么预先拆解为步骤，要么由确定性循环来驱动；模型输出的一切在可被依赖之前都要经过严格校验；一切可能造成伤害的操作都必须通过一道门控——它不会因为按下 Enter 就被取消。

| | 典型的智能体 CLI | Berimor |
|---|---|---|
| 谁决定下一步做什么 | 模型（寄希望于它的理智） | 代码（流程图、确定性循环） |
| 任务中途失败 | "重启并祈祷" | 事件日志：从中断点精确续跑 |
| 危险操作 | 会因疲劳而变成 YOLO 的确认提示 | Deny 静态规则：被禁止的操作根本不会被询问 |
| 弱模型/本地模型 | "买个贵点的模型吧" | 中介（Mediation）：带错误说明重试 → 升级给人类 |
| 扩展 | 插件获得一切权限 | 子智能体/插件获得父级权限的子集——由代码保证 |
| 可复现性 | 没有 | 完整：日志 → 回放 → 任意时刻的状态 |

## 有何不同

**1. 决策是确定性代码，而不是提示词里的文字。**
分支、循环、超时、带 join 屏障的并行分支、运行中流程的版本迁移——这些都属于 Process Engine，而不是寄希望于模型记住指令。弱模型不能被信任来做上下文筛选和路由——所以这些由代码负责。

**2. 安全是结构，而不是用户的自律。**
破坏性操作的 Deny 表无法通过确认来推翻。文件系统 jail 不会越出工作目录。网络门控不放行封闭网段（包括 NAT64/6to4/Teredo 伪装以及经由重定向和 URL 中 userinfo 的绕过）。密钥在所有泄漏点上都会被掩码——但准入门控看到的是真实值：掩码不会让检查失明。

**3. 自由循环——在监督之下。**
"推理 → 行动 → 观察"模式，用于无法预先拆成步骤的任务。循环内的每个行动都要经过与流程步骤相同的 capability 门控——推理的自由不等于免于规则的自由。可选：自我批评以及"提议 — 执行 — 验证"策略。

**4. 模型生成的代码在真正的沙箱中执行。**
对于"合并 12 张表并找出异常"这类任务，模型会编写 JavaScript 程序。程序先经真实解析器做静态分析（标识符白名单——`eval`/`Function`/`Math.random` 在执行前即被拒绝），然后由 WebAssembly（Wasmtime）内的 QuickJS 执行，带有燃料限制、内存上限和工具调用次数上限。WASI 权限集为空：连潜在的文件和网络访问都没有。唯一的宿主函数同样经过那道门控。

**5. 记忆是一套工程系统，而不是缓冲区。**
工作记忆在预算超支时会被压缩。情节记忆——全文搜索（FTS5）。语义记忆——事实去重，冲突不会被静默覆盖，存储故障与"没有事实"不可区分，也不会产生虚假重复。实体图谱——事实之间的关联，持久化。技能——解决相似任务的可复用配方，是可读的文件。

**6. 有权限上限的扩展生态。**
- **技能**（SKILL.md）——聊天中的专家角色：触发由代码完成（而非模型），工具上限由调度器过滤器保证。
- **子智能体**（agent.yaml）——嵌套的智能体循环，拥有独立的预算和日志；子级权限 = 与父级权限的交集，无法扩大。嵌套派生仅在显式 `allow_spawn: true` 时允许，深度由代码限制。
- **插件**——隔离进程，带 ACL 清单和 sigstore 无钥匙签名：从可信列表安装并做 TOFU 确认，如同 SSH。
- **MCP**——通过开放协议 Model Context Protocol 接入的外部工具服务器（官方 Rust SDK rmcp，ADR-0023）：在配置中以 `[[mcp_servers]]` 小节连接，排在内置工具和插件之后进入统一调度器，并通过与任何流程步骤相同的 capability 门控。反过来也成立：Berimor 也可以通过 MCP 对外提供自己的工具。

这一切都可以用一条命令安装——来自目录或**任意 git 仓库**：`berimor skill install code-review-ru --from https://github.com/...`。

## 功能

### 内置工具

工具内置于二进制文件中（非插件），所有调用都经过 capability 门控：**修改型**（标记 *）按门控模式需要确认，只读型直接执行。

| 分组 | 工具 | 作用 |
|---|---|---|
| 文件 | `files.read`、`files.list`、`files.write`*、`files.edit`* | 读取/列目录；整体写入；按字符串锚点精确编辑（old_string → new_string，唯一性校验） |
| 搜索 | `files.search`、`session.search` | 对文件内容做 regex（带行号和上下文）或对文件名做 glob——跳过 `.git`/`target`/`node_modules`；对历史会话内容做子串搜索并返回摘录 |
| VCS | `vcs.git` | git status/diff/log/show——只读：仓库辅助功能（fsmonitor、外部 diff、textconv）已禁用，不接受任意标志 |
| 终端 | `terminal.exec`*、`terminal.start`*、`terminal.output`、`terminal.kill` | 带超时和输出上限的命令执行；可轮询和停止的后台进程（最多 32 个并发） |
| 网络 | `http.fetch`、`web.search` | 带响应体上限和网络门控的 GET；DuckDuckGo 搜索结果（标题/链接/摘要） |
| 记忆 | `memory.search`、`memory.save` | 语义记忆事实搜索；带去重的事实写入——默认关闭（需显式开启：`[memory] tool_writes = true`），写入前秘密会被掩码 |
| 组织 | `todo.read`、`todo.write`、`human.ask` | 会话任务列表（存于 `.berimor/todo.json`）；在智能体循环中直接向用户提问 |
| 快照 | `snapshot.list`、`snapshot.restore`* | 自动：每次覆盖文件前保存其状态（轮换 50 份）；list——标签与路径，restore——回滚（自身也会先生成快照） |
| 子智能体 | `agents.run` | 以权限交集委托给嵌套智能体 |

除内置工具外，还有插件和 MCP 服务器提供的工具（同一门控策略）。聊天中的完整列表：启动行“工具：…”。

### 聊天菜单 (TUI)

输入 `/`——命令面板会显示带界面语言描述的命令，并随输入过滤。子菜单用空格展开：`/config ` 显示后续选项。

| 命令 | 作用 |
|---|---|
| `/help` | 命令列表 |
| `/models` | 提供商：列表，`/models add`——向导（预设 → 选择 → 密钥/OAuth），删除——通过带确认的选择器 |
| `/skills`、`/agents` | 技能与子智能体（全局/项目），按 Enter 查看行内容 |
| `/config` | **参数菜单**：显示生效配置和“界面语言”项（含当前值）→ 从 8 种语言中选择（ru, en, de, fr, es, zh-CN, ja, ko）。保存到本地配置（`[ui]`），立即生效。快捷方式：`/config locale ja` |
| `/mouse` | 鼠标开关：捕获时——滚轮滚动日志，点击日志获得滚动焦点；释放时——终端原生选择/复制（捕获时按住 Shift 选择） |
| `/copy` | 将智能体最后一条回复复制到剪贴板（wl-copy/xclip/xsel/pbcopy） |
| `/clear`、`/exit` | 清空对话日志；退出 |

界面中的其余部分：危险操作的**确认模态框**（选项“仅此一次 / 本会话内 / 本项目”——用 ←→↑↓ 箭头选择，y/n——立即确认）；**智能体提问**（`human.ask`）——自由输入的模态框，Enter——回答，Esc——拒绝；**多行输入**——Alt+Enter 换行，输入框最高扩展到屏幕的三分之一，剪贴板粘贴作为单个事件插入；**鼠标**——滚轮与点击聚焦（见 `/mouse`）。

## 项目基础设施

**Rust workspace，每个组件一个 crate**——Process Engine、Mediation、Executors、Memory、Capability、Model Pool、Actors、Tool Runtime、Context Engine、Eval、Storage。Guest WASM 模块（`codeact-guest/`）作为独立的 crate 存在，并以预构建产物提交——常规构建不会变慢。

**检查纪律。** 每次发布：`cargo fmt` + `clippy -D warnings` + `cargo test --workspace`（927 个测试：单元、集成、通过真实二进制的 e2e、流程和恶意输入的黄金夹具）。关键组件必须通过强制性的独立评审。完整的独立审计（`docs/audit-2026-07-31.md`）——**所有发现均已关闭或有意识地记录在案**。

**成人级的供应链。** 跨平台发布（Linux x64/arm64、macOS arm64、Windows x64），使用 cosign/sigstore 无钥匙签名——私钥在任何地方都不存在。验证：`berimor verify <归档文件>`。npm 发布带 provenance，流水线中包含 SBOM（CycloneDX），自更新（`berimor self-update`）基于 Process Engine 原语实现——与普通流程共享同一套日志与故障恢复，而非临时脚本。

**架构先于代码成文。** `docs/arch/`——自足完备的规范，可在任何技术栈上实现；`docs/ADR/`——决策日志，包含被否决的备选方案；`docs/ROADMAP.md`——任务队列，每项任务标注执行者模型等级。

## 安装

### 方式 1：npm（最简单）

```sh
npm install -g berimor
berimor --version
```

安装程序会自动识别平台，从 GitHub 最新发布下载已签名的二进制文件，并在解压前校验 SHA-256。包以 provenance 发布（构建与 CI 工作流绑定）。

### 方式 2：从 GitHub 获取预编译二进制

最新版本见[发布页面](https://github.com/devpilgrin/berimor/releases/latest)。以下是下载特定版本的命令（如有更新版本发布，请将 `v0.19.0` 替换为所需版本）。

**Linux**（x64 或 arm64）：

```sh
VERSION=v0.19.0
ARCH=x64   # 或 arm64
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-linux-${ARCH}.tar.gz"
tar -xzf "berimor-${VERSION}-linux-${ARCH}.tar.gz"
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**macOS**（仅 Apple Silicon——M1/M2/M3 及更新；暂未发布 Intel 构建，Intel Mac 请使用下方方式 3）：

```sh
VERSION=v0.19.0
curl -LO "https://github.com/devpilgrin/berimor/releases/download/${VERSION}/berimor-${VERSION}-darwin-arm64.tar.gz"
tar -xzf "berimor-${VERSION}-darwin-arm64.tar.gz"
xattr -d com.apple.quarantine berimor   # 二进制尚未经 Apple 签名——否则 Gatekeeper 将拒绝运行
chmod +x berimor
sudo mv berimor /usr/local/bin/
berimor --version
```

**Windows**（x64），PowerShell：

```powershell
$Version = "v0.19.0"
Invoke-WebRequest -Uri "https://github.com/devpilgrin/berimor/releases/download/$Version/berimor-$Version-win32-x64.zip" -OutFile berimor.zip
Expand-Archive -Path berimor.zip -DestinationPath .\
.\berimor.exe --version
```

二进制尚未签名——Windows SmartScreen 可能显示"Windows 已保护你的电脑"警告："更多信息" → "仍要运行"。若要任意目录下调用 `berimor`，请将 `berimor.exe` 移入已在 `PATH` 中的目录，或自行将当前目录加入 `PATH`。

每个归档文件都附带 `<归档文件>.sigstore.json`——cosign/sigstore 无钥匙签名，绑定到构建该发布的 CI 工作流身份（ADR-0026）。验证：`berimor verify <归档文件>`——该命令已包含在下载的二进制中（首次调用时会通过网络安装最新的 sigstore 可信根）。这是独立于 Apple/Microsoft 的签名——它不会消除上述 Gatekeeper/SmartScreen 警告，那是另一个尚未完成的步骤。

### 方式 3：从源码构建（任意操作系统）

只需要 [Rust](https://rustup.rs/)（稳定版）：

```sh
git clone https://github.com/devpilgrin/berimor.git
cd berimor
cargo build --release -p berimor-cli
./target/release/berimor --version
```

在 Windows 上，最后一条命令是 `.\target\release\berimor.exe --version`。

## 快速开始

```sh
berimor          # = berimor chat：与智能体的交互式对话
```

首次启动时，向导会建议从预设中接入模型（Kimi、DeepSeek、OpenAI、经 OpenRouter 的 Claude、经 Ollama/llama.cpp/LM Studio 的本地模型）——选择编号或名称，粘贴 API 密钥（它会以"仅所有者"权限写入 `~/.config/berimor/secrets.env`，而不是配置文件）。除了 API 密钥，也可以用订阅登录——`berimor login`（带 PKCE 的 OAuth：Claude Pro/Max、ChatGPT Plus/Pro；令牌保存在同一个 `secrets.env` 中，刷新透明无感）。之后同样可以通过 `berimor setup` 或直接在聊天中用 `/models add` 命令完成。

常用的聊天命令：`/help`、`/models`、`/skills`、`/config`、`/exit`。TUI 界面语言 — `/config locale`(8 种语言:ru、en、de、fr、es、zh-CN、ja、ko;选择会保存到本地配置 `[ui]` 节)。

确定性流程（带严格契约的声明式 YAML 计划——主要的"实战"模式）：`berimor run <process.yaml>`。流程与配置示例见 [`fixtures/golden/processes/`](fixtures/golden/processes/) 和 [`CONTRIBUTING.md`](CONTRIBUTING.md)。

流程之上的自动化：`berimor schedule add` + `berimor daemon`——按计划执行流程；`berimor serve`——构建在 run/schedule/sessions 之上的 HTTP 服务（带令牌，不允许匿名访问）；`berimor sessions`——主机存活会话的注册表；`berimor trace <实例>`——将任意一次运行的日志以人类可读的形态追踪呈现。

一条命令安装扩展：

```sh
berimor skill install code-review-ru                                    # 来自目录
berimor skill install my-skill --from https://github.com/user/repo      # 来自任意 git
berimor agent install researcher
berimor plugin install devpilgrin/berimor-plugin-hello                  # 已签名插件
berimor plugin install-local ./my-plugin --allow-unsigned               # 本地安装，明知而为
```

## 项目结构

| 层级 | 目录 | 内容 |
|---|---|---|
| 智能体内核 | `crates/` | Rust workspace——每个组件一个 crate：Process Engine、Mediation、Executors、Memory、Capability、Model Pool、Actors、Tool Runtime、Context Engine、Eval、Storage |
| CodeAct 沙箱 | `codeact-guest/` | 面向 wasm32-wasip1 的 QuickJS guest——独立 crate，以预构建产物提交 |
| Bootstrap | `bootstrap/` | 安装/更新用的 npm 包（TypeScript），见上文"安装" |
| 架构 | `docs/arch/` | 自足完备的规范——原则、组件、图（`docs/arch/views/`）。见 `docs/arch/README.md` |
| 决策 | `docs/ADR/` | 架构决策日志：背景、备选方案、后果。见 `docs/ADR/README.md` |
| 开发计划 | `docs/ROADMAP.md` | 按阶段划分的任务队列、子任务分解、复杂度、执行者模型等级 |
| 审计 | `docs/audit-2026-07-31.md` | 独立安全审计——所有发现均已关闭或有意识地记录在案 |
| 测试数据 | `fixtures/golden/` | 黄金数据集：流程、契约、恶意输入的示例 |
| 研究 | `docs/rnd/` | 辅助层：现有智能体框架的资料来源与分析。见 `docs/rnd/README.md` |

`crates/` 和 `bootstrap/` 是智能体本身，是按照 `docs/ROADMAP.md` 中的队列编写的代码。`docs/arch/` 是其背后的纯决策层：不提及具体项目和产品（`docs/arch/deployment.md` 与 `docs/arch/stack.md` 除外，那是有意识的例外），以可在任何技术栈上实现的方式阐述架构。`docs/ADR/` 记录每项决策的理由，包括被否决的备选方案。`docs/rnd/` 是设计所依据的辅助资料层，不属于智能体本身。

## 许可证

Apache License 2.0——见 [`LICENSE`](LICENSE)。

## 参与贡献

任务选择请见 [`CONTRIBUTING.md`](CONTRIBUTING.md) 与 [`docs/ROADMAP.md`](docs/ROADMAP.md)。
