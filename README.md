# Waypoint

Waypoint 是一个桌面端本地 Agent CLI 会话路由器。它的目标是通过 Tauri + Rust 管理多个本机 PTY 会话，让 Claude Code、Goose、Gemini CLI、Shell 等 agent 会话可以长期存活、随时切换，并支持跨 agent 上下文交接（Handover）。

## 技术栈

- **桌面外壳**：Tauri v2 + Rust
- **前端界面**：React + TypeScript + `@xterm/xterm` + `@xterm/addon-fit`
- **PTY 托管**：`portable-pty`
- **构建工具**：Vite

---

## 环境准备与安装

运行 AgentRelay 需要本地安装有 Node.js 与 Rust 工具链。

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

---

## 相关文档

* [AGENTRELAY_TECHNICAL_DESIGN.md](file:///Users/liuzhe.x/coding/waypoint/AGENTRELAY_TECHNICAL_DESIGN.md) - MVP 详细技术设计方案
* [AGENTRELAY_ARCHITECTURE_SUMMARY.md](file:///Users/liuzhe.x/coding/waypoint/AGENTRELAY_ARCHITECTURE_SUMMARY.md) - 精简版技术架构与流程说明
