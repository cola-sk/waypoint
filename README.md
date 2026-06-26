# Waypoint

<p align="center">
  <img src="src-tauri/icons/icon.png" alt="Waypoint logo" width="96" height="96">
</p>

<p align="center">
  <strong>桌面端本地 Agent CLI 会话路由器。</strong>
</p>

<p align="center">
  <a href="README.zh-CN.md">中文文档（详细版）</a>
</p>

<img width="2041" height="1148" alt="image" src="https://github.com/user-attachments/assets/d0e8a22b-6131-4ea8-a091-2c5e0f42558b" />


Waypoint 是一个 Tauri 桌面应用，用于在一个窗口中管理多个本地 AI agent CLI 会话。它通过 PTY 保持会话长期存活，支持在工作区和 agent 之间切换，并提供 handover 流程，将上下文从一个 agent 会话传递给另一个 agent 会话。

它面向 Claude Code、Codex、Antigravity CLI、GitHub Copilot CLI 以及常规 shell 等工具。

## 核心能力

- **本地 PTY 会话托管**：在桌面应用管理的真实终端会话中运行 agent CLI。
- **多 agent 工作区路由**：固定工作区目录，并从每个目录启动可用的 agent。
- **持久会话切换**：在会话之间切换时不杀掉底层进程。
- **Agent handover**：将终端上下文、最近对话时间线和备注转发到另一个 agent 会话以延续工作；git 状态和 diff 不再内联，目标 agent 需要时直接查询 workspace。
- **无工作区会话**：新建会话时可选择「None（不绑定工作区）」，会话归入独立分组，不与具体目录绑定；该标记持久化在 `meta.json` 中，重启后仍然保留。
- **跳过权限确认**：对 Claude Code（`--dangerously-skip-permissions`）和 Codex（`--dangerously-bypass-approvals-and-sandbox`）可在新建会话时勾选 dangerous 选项；该标志同样持久化在 session meta 中，handover 和 native resume 时会自动重新应用。
- **图片粘贴**：在终端中粘贴或拖入图片时，会自动保存为会话附件并在输入行插入 `[paste image N]` 占位符；按 Enter 提交前 Waypoint 会把占位符反解为附件的实际文件路径，再连同回车一起发给 agent。
- **原生桌面外壳**：Tauri v2 + Rust 后端，React + xterm.js 前端。
- **自动 CLI 检测**：通过用户 login shell 解析 agent 命令，使应用看到的 PATH 更接近终端环境。

## 支持的 Agent

Waypoint 当前会识别以下 preset：

| Agent | 命令 |
|---|---|
| Claude Code | `claude` |
| Codex | `codex` |
| Antigravity CLI | `agy` |
| GitHub Copilot | `copilot`、`gh copilot` |
| Shell | `$SHELL` |

如果某个 agent 在你的 terminal 中可用，但在 Waypoint 中显示 missing，请用以下命令验证：

```bash
command -v claude
command -v codex
command -v agy
command -v copilot
command -v gh
```

## 技术栈

- **桌面外壳**：Tauri v2 + Rust
- **前端**：React + TypeScript + Vite
- **终端 UI**：`@xterm/xterm` + `@xterm/addon-fit`
- **PTY 托管**：`portable-pty`

## 环境准备

Waypoint 需要 Node.js、npm 和 Rust 工具链。

在 macOS 上，先确认 Xcode Command Line Tools 已安装：

```bash
xcode-select -p
```

如未安装：

```bash
xcode-select --install
```

使用 `rustup` 安装 Rust：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

验证环境：

```bash
node --version
npm --version
rustc --version
cargo --version
```

## 开发

安装依赖：

```bash
npm install
```

以开发模式启动桌面应用：

```bash
npm run tauri:dev
```

该命令会启动 Vite 开发服务器并打开 Tauri 桌面外壳。

> [!IMPORTANT]
> PTY 管理运行在 Tauri 桌面进程中。在普通浏览器中访问 `http://127.0.0.1:1420/` 只能用于 UI 预览；创建和使用真实 PTY 会话必须在 Tauri 桌面窗口中进行。

构建前端：

```bash
npm run build
```

构建 Tauri 应用（不打包安装器）：

```bash
npm run tauri -- build --debug --no-bundle --ci
```

## 手动验收流程

1. 用 `npm run tauri:dev` 启动桌面应用。
2. 确认左侧 agent 环境列表显示可用与缺失的本地 agent。
3. 固定或选择一个本地工作区目录。
4. 从该工作区启动一个可用 agent。
5. 确认终端在选定工作区中启动。
6. 启动第二个会话并切回第一个。
7. 确认第一个 PTY 进程仍然存活。

## 会话选项与图片粘贴

- **跳过权限确认（dangerous）**：在「新对话」弹窗中，选择 Claude Code 或 Codex 时会显示该复选框。Claude Code 注入 `--dangerously-skip-permissions`，Codex 注入 `--dangerously-bypass-approvals-and-sandbox`。该标志写入 session meta，后续 handover、continue、native resume 都会自动重新应用。
- **无工作区会话（None）**：工作区下拉框选择「None（不绑定工作区）」时，会话归入左侧「无工作区会话」分组，启动目录由系统当前目录兜底。该标记持久化在 `meta.json` 中，重启后仍然保留。
- **图片粘贴/拖入**：在已挂起的 agent 终端里粘贴（Cmd+V）或拖入图片时，Waypoint 会把图片保存为会话附件，并在输入行插入 `[paste image N]` 占位符。按 Enter 提交前，占位符会被反解为附件的实际文件路径，再连同回车一起发送给 agent。附件保存在 `<workspace>/.waypoint-attachments/<session-id>/` 下（自动写入 `.gitignore`），落在 agent 的工作区目录内，因此 Claude Code 等具有项目作用域读取权限的 agent 可以直接通过 Read 工具读取该路径。
- **冒号与符号输入**：xterm 在某些 agent 输入框中会吞掉半角/全角冒号、`+` 等单字符符号；Waypoint 通过 keydown + beforeinput 拦截所有无修饰键的单字符可打印输入（含 `:`、`：`、`+` 等），绕过 textarea 直接走 PTY 输入通道，并跳过 IME 组合态以避免影响中文输入。
- **侧栏快速启动 + dangerous**：左侧工作区目录上的 `+` 按钮弹出快速启动菜单时，菜单底部提供「跳过权限确认」复选框；勾选后点击 Claude Code / Codex 等支持的 Agent 会以 dangerous 模式启动，并在 Agent 名称旁显示 `dangerous` 标记。该复选框状态在快速启动菜单间保持，便于连续启动多个 dangerous 会话。

## Continue / Handover

Continue 流程将当前会话的上下文传递给另一个 agent。

新目标会话流程：

1. 打开源会话。
2. 点击 **Continue**。
3. 选择 **New Session**。
4. 选择目标 agent 和工作区。
5. 添加可选的 note，描述下一个 agent 应该关注的内容。
6. 点击 **Create & Continue**。

Waypoint 会按 chat 顺序收集最近的对话时间线，将 handover 文件写到 `~/.waypoint/<workspace-name>/handover-*.md`，启动目标会话，并注入一段简短指令，指引目标 agent 读取该 handover 文件。目标 agent 需要 git 状态或 diff 时会直接从 workspace 查询。
Continue 弹窗右侧的 handover Markdown 支持直接编辑，点击 `Create & Continue` / `Forward` 时会使用编辑后的内容写入 handover 文件。

也可以通过 **Existing Session** 模式将 handover 注入已有会话。

### Native Session ID 与恢复

Waypoint 自己的 session 元数据存储在 `~/.waypoint/sessions/<session-id>/meta.json` 中。对于支持原生恢复的 agent，还会记录 `nativeSessionRef`，包含 provider、native id、可选 project、resume 命令和发现时间。重新激活历史会话时，后端会先刷新该 native 引用，再构造 agent 专属的 resume 命令。

> **Dev / Prod 存储隔离**：`npm run tauri:dev`（debug 构建）使用 `~/.waypoint-dev/`，安装版 DMG（release 构建）使用 `~/.waypoint/`。两套 session、handover、附件互不干扰，重装 DMG 不会影响 dev 会话，反之亦然。前端 localStorage 同样按 `waypoint-dev:` / `waypoint:` 前缀隔离。

不同 agent 的 native id 策略对比：

| Agent | 创建时是否注入 native id | native id 来源 | 恢复命令 |
|---|---|---|---|
| Claude Code | 是，将 Waypoint session id 作为 `--session-id` 注入；若启动参数已含 `--resume`/`-r`/`--session-id` 则不重复注入 | Waypoint session id | `claude --resume <id>` |
| Codex | 否，不在创建时强制指定 | 若 meta 已有则用之，否则无 | 有 native id：`codex resume <id>`；无：`codex resume --last` |
| Antigravity CLI (agy) | 否，agy 不支持外部指定 conversation id | 首次真实提交用户输入时写入 `<!-- waypoint_session_id: <id> -->` 标记，扫描 brain 目录下的 transcript 反查得到 conversation id；若终端 transcript 出现 "Resume in the same project" 行，则解析 `--project=<project>` 作为补充 | `agy --conversation=<conversation-id> [--project=<project>]` |
| GitHub Copilot | 是，将 Waypoint session id 作为 `--session-id=<id>` 注入；若启动参数已含 `--continue`/`--resume`/`-r`/`--session-id` 则不重复注入；`gh copilot` 形式在必要时通过 `--` 分隔参数 | Waypoint session id | `copilot --resume=<id>` 或 `gh copilot -- --resume=<id>` |
| Shell | 不适用 | 无 agent 原生 session id | 仅保留 Waypoint 自身的 PTY transcript 和 replay |

Claude Code 恢复前还会确认 `~/.claude/projects/<workspace-as-claude-project>/<id>.jsonl` 存在；若标准路径不存在，会在 `~/.claude/projects` 下按文件名兜底搜索 `<id>.jsonl`。

Codex 在读取原生 transcript 时，优先用 native id 在 `~/.codex/sessions` 和 `~/.codex/archived_sessions` 中查找；没有 native id 时按 workspace cwd 与 session 创建时间选最近 transcript。

agy 通过扫描 `~/.gemini/antigravity-cli/brain/*/.system_generated/logs/transcript.jsonl` 来匹配 `waypoint_session_id` 标记，匹配到的 brain 目录名即为 agy conversation id。

### Handover 文件生成

Handover 不会把完整上下文塞进目标 agent 的命令行，而是先生成文件，再让目标 agent 读取该精确文件。

文件布局与模式选择：

| 项 | 说明 |
|---|---|
| 主文件 | `~/.waypoint/<workspace-name>/handover-<uuid>.md` |
| Compact 模式完整证据文件 | `~/.waypoint/<workspace-name>/handover-<uuid>-full-evidence.md` |
| `workspace-name` 取值 | workspace 路径最后一级目录名；无法解析时使用 `workspace` |
| Recommended 模式 | 估算上下文超过 24,000 字符时使用 Compact，否则使用 Full |
| 显式模式 | 用户可手动选择 Compact 或 Full |

handover 文件收集的内容：

1. 源会话与目标会话的 agent、命令、workspace。
2. Continue 面板中用户填写的 note。
3. 最近对话时间线，尽量按原始 chat 顺序保留 User / Assistant 往返。
4. 上一跳 inherited handover context。
5. agy 会话生成的 markdown artifacts（来自 `~/.gemini/antigravity-cli/brain/<conversation-id>/*.md`）。

不同 agent 的上下文来源优先级对比：

| Agent | 首选来源 | 回退来源 |
|---|---|---|
| Claude Code | `~/.claude/projects/.../<native-id>.jsonl` 原生 transcript | Waypoint 自身的 terminal/chat buffer |
| Codex | 有 native id：`~/.codex/sessions` 或 `archived_sessions` 中匹配 native id 的 transcript；无 native id：按 workspace 与创建时间选最近 transcript | Waypoint 自身的 terminal/chat buffer |
| Antigravity CLI | 先通过 `waypoint_session_id` 标记反查 conversation id，再读 `~/.gemini/antigravity-cli/brain/<conversation-id>/.system_generated/logs/transcript.jsonl` | Waypoint 自身的 terminal/chat buffer |
| GitHub Copilot / Shell | Waypoint 自身的 terminal/chat buffer 与输入 ring | — |

> 对于 Copilot / Shell，如果 Waypoint 无法构造有序的 User / Assistant 时间线，会在同一个 timeline 区块中注明只捕获到用户输入，而不会把 assistant/context 与 user inputs 拆成两个独立 evidence 区块。

Full 与 Compact 模式差异：

| 维度 | Full 模式 | Compact 模式 |
|---|---|---|
| 主文件对话内容 | 完整结构 + 最近有序对话 | 更短的有序对话 |
| git 状态 / diff | 不内联，目标 agent 按需查询 workspace | 不内联，目标 agent 按需查询 workspace |
| 附件清单 | 不内联，沿用会话记录里的附件上下文 | 不内联，沿用会话记录里的附件上下文 |
| 完整证据文件 | ❌ | ✅（`*-full-evidence.md`，含更长的最近对话证据） |
| 目标 agent 读取方式 | 直接读主文件 | 主文件引用 evidence 文件路径，目标 agent 按需读取 |

### Agent Handover 启动/注入策略

不同 agent 在 New Session 与 Existing Session 下的注入方式对比：

| Agent | New Session 启动形态 | 目录授权 | startup prompt 内容 | 其他说明 |
|---|---|---|---|---|
| Claude Code | `claude "<startup prompt>"` | — | 只读取 handover 文件，并包含新的 `waypoint_session_id` 标记 | 创建目标 session 后记录 `parentSessionId` 和 `handoverRootId` |
| Codex | 默认命令带 `--no-alt-screen` | 通过 `--add-dir` 加入 handover 文件目录 | 指向 handover 文件，并包含新的 `waypoint_session_id` 标记 | 新建后等待更长启动延迟再注入，降低 Codex 未就绪时写入失败概率 |
| Antigravity CLI (agy) | `agy --prompt-interactive "<startup prompt>"` | 通过 `--add-dir` 授权 handover 目录 | 只含 handover 文件路径和新的 `waypoint_session_id` 标记，避免长 diff/context 直接进入 agy TUI | — |
| GitHub Copilot | `copilot -i "<startup prompt>"` | 通过 `--add-dir` 传入 handover 目录；`gh copilot` 形态通过 `--` 分隔参数 | startup prompt | — |

Existing Session / Forward 模式：

- 先生成 handover 文件。
- 通过 PTY bracketed paste 注入一段短提示：只读取该 handover 文件，确认 context loaded，然后等待下一步指令。
- 注入前会检查目标进程是否已退出；失败时错误信息会包含 target session 的最近输出。

Create Handover File：

- 顶栏的 handover-file 按钮只生成文件，不启动或注入任何 agent。
- target 被标记为 Manual handover，便于将文件路径手动复制给外部工具。

每次 handover 的目标 session 会记住这次 handover 摘要；如果之后继续从该目标 session 再 handover 到第三个 agent，Waypoint 会把上一跳 handover 作为 inherited context 一并写入新的 handover 文件。

## 故障排查

### `cargo` 或 `rustc` command not found

在当前 shell 中加载 Cargo：

```bash
source "$HOME/.cargo/env"
```

然后检查：

```bash
rustc --version
cargo --version
```

### `npm run tauri:dev` 提示 Rust 未安装

查看 Tauri 能检测到的环境：

```bash
npm run tauri -- info
```

如果未检测到 `rustc`，重新加载 shell 环境或将 Cargo 加入 shell profile。

### 浏览器中提示 `Tauri runtime unavailable`

这是预期行为。Tauri 的 PTY 会话、本地文件访问、原生命令等 API 只存在于 Tauri 桌面外壳内部。

### Continue 时报 `failed to write handover`

目标 agent 可能已在 Waypoint 注入 handover prompt 前退出，或正在等待登录/配置。

请先直接启动目标 agent，确认它能保持在可交互状态。如果它立即退出，先解决其自身的认证或 CLI 配置问题，再将其作为 handover 目标。

## 相关文档

- [技术设计](AGENTRELAY_TECHNICAL_DESIGN.md)
- [架构摘要](AGENTRELAY_ARCHITECTURE_SUMMARY.md)
