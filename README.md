# waypoint

waypoint 是一个桌面端本地 Agent CLI 会话路由器。它的目标是通过 Tauri + Rust 管理多个本机 PTY 会话，让 Claude Code、Codex、Gemini CLI、GitHub Copilot、Shell 等 agent 会话可以长期存活、随时切换，并支持跨 agent 上下文交接（Handover）。

## 技术栈

- **桌面外壳**：Tauri v2 + Rust
- **前端界面**：React + TypeScript + `@xterm/xterm` + `@xterm/addon-fit`
- **PTY 托管**：`portable-pty`
- **构建工具**：Vite

---

## 当前 MVP 能力

当前版本支持：

- 自动识别本机 Agent CLI。
- 选择 agent preset 创建 session。
- 指定 workspace 目录创建 PTY session。
- 多个 session 并存，切换 UI 时不杀掉已有 PTY 进程。
- 将一个 session 的上下文转发到另一个 session。
- Xterm 终端输入、输出和 resize。

内置识别的 agent：

```text
Claude Code:
  claude

Codex:
  codex

Gemini CLI:
  gemini

GitHub Copilot:
  copilot
  gh copilot

Shell:
  $SHELL
```

检测逻辑会通过用户的 login shell 执行 `command -v`，因此比直接读取桌面进程 PATH 更接近你在 terminal 里的环境。

如果某个 agent 在 terminal 里可用，但 waypoint 里显示 missing，可以先检查：

```bash
command -v claude
command -v codex
command -v gemini
command -v copilot
command -v gh
```

如果 agent 安装在自定义路径，后续版本会提供可编辑 Agent Preset；当前版本先依赖 login shell 的 PATH 自动发现。

---

## 环境准备与安装

运行 waypoint 需要本地安装有 Node.js 与 Rust 工具链。

### 1. 安装 Xcode Command Line Tools (macOS)
在终端中执行以下命令检查是否已安装：
```bash
xcode-select -p
```
如未安装，执行：
```bash
xcode-select --install
```

### 2. 安装 Rust 工具链
推荐使用 `rustup` 安装 Rust：
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```
在安装过程中选择默认选项（输入 `1`）。
安装完成后，使当前终端加载 Cargo 环境变量：
```bash
source "$HOME/.cargo/env"
```
建议在你的 Shell 配置文件（如 `~/.zshrc`）中添加：
```bash
source "$HOME/.cargo/env"
```

### 3. 验证环境安装
确认以下命令均能正常输出版本号：
```bash
node --version
npm --version
rustc --version
cargo --version
```

---

## 项目运行与启动

### 1. 安装项目依赖
在项目根目录下，安装前端依赖：
```bash
npm install
```

### 2. 启动开发模式 (桌面窗口)
```bash
npm run tauri:dev
```
此命令会同时启动 Vite 前端开发服务器与 Tauri 桌面外壳，并自动打开桌面应用窗口。
> [!IMPORTANT]
> 由于 PTY 后端运行在 Tauri 桌面进程中，在普通浏览器中（如访问 `http://127.0.0.1:1420/`）只能预览 UI 界面，无法实际创建和使用 PTY 会话。请务必在 Tauri 桌面窗口中进行操作。

### 3. 项目打包与构建 (生产包)
如果需要打包生成本地可执行的 App 安装包，执行：
```bash
npm run build
```

---

## 常见问题与排查

## 手动验收

启动桌面应用：

```bash
npm run tauri:dev
```

然后执行：

```text
1. 打开 waypoint 桌面窗口。
2. 左侧 Agent 下拉框会显示 Claude Code / Codex / Gemini CLI / GitHub Copilot。
3. 可用 agent 会显示 resolved command，不可用 agent 会显示 missing。
4. 在 Workspace 输入本地项目目录，例如：
   /Users/liuzhe.x/coding/waypoint
5. 选择一个 available agent。
6. 点击 Start。
7. 确认右侧 terminal 启动到该目录下的对应 agent CLI。
8. 创建第二个 session，切换回来确认第一个 session 没有退出。
```

### Continue / Handover 验收

创建一个 source session 后执行：

```text
1. 切到源 session。
2. 点击右上角 Continue。
3. 默认选择 New Session。
4. 选择目标 agent。
5. Workspace 默认使用源 session 的目录，也可以手动修改。
6. 在 Note 中填写目标 agent 接下来要关注的任务。
7. 点击 Create & Continue。
8. waypoint 会创建一个新的目标 session。
9. waypoint 会收集源 session 最近 terminal context、workspace git status、git diff、staged diff。
10. waypoint 会生成 handover prompt，并注入新 session。
11. UI 自动切换到新 session。
12. 源 session 仍然保持运行，可以随时切回。
```

如果已经有一个目标 session，也可以使用高级模式：

```text
1. 点击 Continue。
2. 切换到 Existing Session。
3. 选择目标 session。
4. 在 Note 中填写目标 agent 接下来要关注的任务。
5. 点击 Forward。
6. waypoint 会把 handover prompt 注入已有目标 session。
```

当前 handover 是 MVP 实现：它使用后端 ring buffer 作为最近上下文，并通过 bracketed paste 将 prompt 写入目标 PTY。主流程是创建新 session 并继续上下文；Existing Session 模式作为高级能力保留。后续版本会加入可预览 prompt、可编辑 prompt、结构化 transcript、session lineage 和 agent-specific adapter。

不同 agent 的 Continue 注入策略：

```text
Gemini CLI:
  新 session 使用 gemini --prompt-interactive <handover>。
  这样 Gemini 会先执行 handover prompt，然后继续留在交互模式。

Codex:
  使用 codex --no-alt-screen 启动，减少嵌入式 xterm 中的 alternate screen 闪屏。
  handover 暂时通过 PTY bracketed paste 注入。

Claude Code / Shell:
  handover 暂时通过 PTY bracketed paste 注入。
```

---

### `cargo` 或 `rustc` command not found
通常是因为当前 Shell 环境未加载 Cargo 的 PATH。请尝试执行：
```bash
source "$HOME/.cargo/env"
```
并重新检查 `rustc --version`。

### `npm run tauri:dev` 提示 Rust 未安装
同上，通常是 Tauri 未读取到 Rust 路径。你可以在项目根目录下执行以下命令来确认 Tauri 能够检测到的运行环境：
```bash
npm run tauri -- info
```
如果在输出中看到 `rustc: installed` 即代表环境检测成功。

### 浏览器中提示 `Tauri runtime unavailable`
此报错为预期行为。Tauri 的底层 API（如 PTY 会话管理、本地文件读写等）必须在编译后的桌面外壳容器中才能调用，在普通浏览器中访问会导致该错误，请使用 `npm run tauri:dev` 启动桌面端。

### Continue 时报 `failed to write handover`

这通常表示目标 agent CLI 没有进入可交互状态，或者命令启动后很快退出。waypoint 会在新建 target session 后短暂等待并重试注入 handover；如果目标进程已经退出，错误信息会包含 target session 的最近输出。

常见原因：

```text
1. 目标 CLI 需要先登录或配置 API key。
2. 目标 CLI 不是持久交互式 chat 命令，例如某些 Copilot CLI 命令只执行一次就退出。
3. 目标 CLI 启动后进入权限确认、初始化失败或帮助页后退出。
```

排查方式：

```text
1. 先直接 Start 目标 agent，确认它能保持在可输入状态。
2. 如果目标 agent 会立即退出，先解决它自己的登录/配置问题。
3. 如果该 CLI 本身不是持久交互式 agent，暂时用 Shell、Claude Code、Codex 或 Gemini CLI 做 Continue target。
```

---

## 相关文档

* [AGENTRELAY_TECHNICAL_DESIGN.md](file:///Users/liuzhe.x/coding/waypoint/AGENTRELAY_TECHNICAL_DESIGN.md) - MVP 详细技术设计方案
* [AGENTRELAY_ARCHITECTURE_SUMMARY.md](file:///Users/liuzhe.x/coding/waypoint/AGENTRELAY_ARCHITECTURE_SUMMARY.md) - 精简版技术架构与流程说明
