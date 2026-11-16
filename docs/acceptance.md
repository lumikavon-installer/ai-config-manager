# 验收记录与发布门槛

## 2026-11-12 侧栏同步管理、自定义 Git 与 SDK 0.3.0

同步列表、应用注册、同步记录移到侧边栏；设置保留常规、同步目标、自动化和关于。GitHub 与自定义 Git 共用加密包和逐项合并；新增草稿仓库权限测试。相邻 `.sdk/ai-config-manager` 更新到 0.3.0，包含配置读写、应用/文件登记、双后端目标、权限测试、策略、历史和清理，已生成 wheel 并更新当前 Python 环境的可编辑安装。

| 检查 | 结果 |
| --- | --- |
| 前端 `npm test -- --run` | 297 通过，1 项既有跳过；含 36 项同步界面测试 |
| `npm run build` | TypeScript 与 Vite 生产构建通过 |
| Windows `cargo test --locked` | 82 通过 |
| 默认 WSL `cargo test --locked` | 86 通过 |
| Windows desktop crate | 编译和 6 项测试通过，含新命令和权限注册 |
| Core/CLI Clippy、Rustfmt、`git diff --check` | 通过 |
| SDK pytest，启用真实 Rust fixture | 212 通过，无跳过 |

真实临时 bare 仓库覆盖首次同步、加密快照、其他文件保留、分支 lease 并发冲突、探测分支删除、空仓库探测、Windows 规范化路径兼容，以及 WSL 接收钩子拒绝写入/删除。JSON/REST 集成验证新设置与权限测试；旧 v1 设置缺少 provider/branch 时继续使用 GitHub/main。SDK 与生产 Rust dispatch 的互操作进一步验证不保存草稿、不读取锁定 AI 配置的探测、应用注册、策略和记录，以及另一设备下载较新的配置。

本轮仅使用临时目录、合成 vault/密钥、本地仓库和模拟 GitHub 服务，未访问真实公网仓库或凭据。SSH/HTTPS 认证与分支保护的原生验收仍需一次性远端仓库。未生成或安装新安装包，也未重启当前桌面实例。

## 2026-11-12 应用注册、同步记录与定时合并

本轮使用 Rust 1.90.0、Node.js 24，在 Windows 和默认 WSL `OpenEuler-2403sp3` 验证。WSL 使用独立 Linux target 目录；新增测试仅访问临时目录、模拟 vault 和本地 GitHub HTTP 服务。

| 检查 | 结果 |
| --- | --- |
| `npm ci`、`npm test -- --run` | 11 文件，284 通过，1 项既有跳过 |
| `npm run build` | TypeScript 和生产构建通过 |
| 默认 WSL `cargo test --locked` | 70 通过，含 Unix 权限与符号链接测试 |
| Windows `cargo test --locked` | 68 通过 |
| `cargo test --locked -p ai-config-manager` | Windows 桌面 crate 编译与 6 项测试通过 |
| 独立 settings-tests workspace | 6 通过 |
| Core/CLI `cargo clippy ... --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all -- --check`、`git diff --check` | 通过 |

覆盖应用注册幂等更新、JSON/REST 共享注册/策略/历史、真实 AF_UNIX 回调 GET/PUT、摘要校验、过期记录及 100 条上限的磁盘删除、失败后推进定时 deadline、重启后中断记录去重、清理前恢复 pending history、远端较新写回、本地并发修改保护、GitHub 竞争后拉取合并及三次重试上限、错误密码失败关闭。AI 远端导入另验证未知 YAML 值、原有加密和 revision 冲突检查。

独立浏览器会话配合合成 Tauri IPC 检查六 Tab、应用名/接口展示、同步记录统计和自动化表单；760px 窗口的页面与内容区均无横向溢出。夹具和截图位于工作区外临时目录，未加入生产代码。

本轮未使用真实 GitHub Token 上传、未操作真实同步 vault、未安装新安装包。Linux 验证范围为 core/CLI 和 Unix socket，未运行 Linux 原生 GUI；后台调度的时间与恢复逻辑通过确定性 core 测试及 Windows 桌面编译验证，未做带真实凭据的长时间托盘运行验收。

## 2026-11-12 折叠设置与第三方 socket 同步

执行环境：Windows x64、Rust 1.90.0、Node.js 24.12.0。Gemini 子代理完成样式与折叠分组，主代理完成接线、复核与验证。所有新增同步测试使用临时目录、合成凭据和本地 GitHub API 模拟服务。

| 检查 | 结果 |
| --- | --- |
| `npm ci` | 通过 |
| `npm test -- --run` | 11 文件，266 通过，1 项既有跳过 |
| `npm run build` | TypeScript 与生产构建通过 |
| `cargo test --locked` | 50 通过；含真实 AF_UNIX 的 JSON/REST 共享同步状态、鉴权、第三方注册/移除、首传/携带 SHA 更新、上传包解密核对、失败状态与并发拒绝 |
| `cargo test --locked -p ai-config-manager` | Windows 桌面 crate 编译与 6 项测试通过 |
| `cargo test --locked --manifest-path=src-tauri/settings-tests/Cargo.toml` | 6 通过 |
| `cargo clippy --locked -p ai-config-core -p ai-config-cli --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all -- --check`、`git diff --check` | 通过 |

浏览器检查使用独立 agent-browser 会话与合成 Tauri IPC 夹具，检查设置分组与配置同步页面。夹具位于工作区外的临时目录，生产代码不包含测试桥接。

本轮未重新生成或安装安装包，未向真实 GitHub 仓库上传，未以真实凭据验证系统同步 vault；这些不由模拟 HTTP、组件测试或编译结果替代。当前同步为显式触发的加密上传/备份，不含自动文件监听、远端下载或本地恢复。

## 2026-11-04 同步、REST 与默认启动验证

执行环境：**Windows 11 x64**、Rust 1.90.0、Node.js 24、Tauri CLI 2.12.4。测试使用合成数据；未向任何 GitHub 仓库上传真实配置，也未保存真实 GitHub Token/加密密码。

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 前端回归 | `npm test -- --run` | 11 文件 / 262 项：261 通过、0 失败、1 项既有的 `it.skip` |
| 类型与生产构建 | `npm run build` | 通过 |
| 核心 + CLI | `cargo test --locked` | 12 + 25 + 7 通过（含 REST 鉴权/分派、同步加密往返、GitHub Contents 上传模拟、同步项管理与可移植导出） |
| 桌面偏好 | `cargo test --locked --manifest-path=src-tauri/settings-tests/Cargo.toml` | 6 通过 |
| 格式 | `cargo fmt --all -- --check` | 通过 |
| 桌面编译 | `cargo check --locked -p ai-config-manager` | 通过 |
| 原生安装包 | `npx tauri build --bundles msi,nsis --ci --no-sign` | 通过；release MSI/NSIS 已生成 |
| Clippy | `cargo clippy ... -D warnings` | **未执行**：当前工具链未安装 clippy 组件 |

### 本机更新与 REST 实测

2026-11-04 在本机提权卸载上一版 per-machine MSI，并安装本轮 release MSI。更新后：

- 设置文件为 `silent_start: true`、`autostart_default_applied: true`；`HKCU\...\Run\AI配置管理器` 指向 `C:\Program Files\AI配置管理器\ai-config-manager.exe --silent`。首次默认设置和更新后的路径刷新均生效。
- 窗口在静默启动时隐藏；再次启动应用会唤起并显示中文标题 `AI配置管理器`。
- 同一 AF_UNIX socket 的 REST 实测：`GET /health` 200；`GET /ai-config` 返回脱敏视图；`GET /sync` 返回同步列表和 `state: never` 状态；无 Token 请求返回 401；`POST /sync/items` 注册临时文件、`GET /sync/status`、`DELETE /sync/items/{id}` 均按预期返回；非法仓库 `POST /sync/settings` 返回 400 `SYNC_REPO`。
- 旧 JSON 协议仍可用：`ai-config status` 返回 `running: true`、`token_present: true`。
- 未执行真实 GitHub 上传；该路径需要一次性测试仓库、Token 和加密密码，且当前没有保存任何真实秘密。
- 侧边栏品牌块已移除；DOM 组件测试覆盖了配置/历史/设置三页，新增同步卡片测试覆盖保存、注册、执行和状态展示。

## 2026-11-03 本轮实现验证

执行环境：**Windows 11 x64**、Rust 1.90.0、Node.js 24、Tauri CLI 2.12.4。全部使用临时目录或合成数据；没有读写用户的 `~/.config/ai.yml`，没有安装本轮生成的安装包。

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 前端回归 | `npm test -- --run` | 10 文件 / 260 项：259 通过、0 失败、1 项既有的 `it.skip` |
| 类型与生产构建 | `npm run build` | 通过 |
| 核心 + CLI | `cargo test --locked -p ai-config-core -p ai-config-cli` | 6 + 25 + 7 通过（含本地 socket 鉴权、默认加密、CLI set 测试） |
| 桌面偏好 | `cargo test --locked --manifest-path=src-tauri/settings-tests/Cargo.toml` | 6 通过 |
| 格式 | `cargo fmt --all -- --check` | 通过 |
| 桌面编译 | `cargo check --locked -p ai-config-manager` | 通过 |
| 原生安装包 | `npx tauri build --debug --bundles msi,nsis --ci --no-sign` | 通过；生成 `AI配置管理器_0.1.1_x64_zh-CN.msi` 与 `AI配置管理器_0.1.1_x64-setup.exe` |
| Clippy | `cargo clippy ... -D warnings` | **未执行**：当前 Rust 1.90 工具链未安装 clippy 组件 |

### 本机更新实测

2026-11-03 23:09 在本机 Windows 11 上执行了真实更新：先停止旧进程，提权卸载旧版 per-machine MSI `AI Config Manager 0.1.1`，再安装 release MSI `AI配置管理器 0.1.1`。更新后：

- 卸载列表名称变为 `AI配置管理器`，安装目录为 `C:\Program Files\AI配置管理器`，旧目录和旧注册项已移除。
- `ai-config-manager.exe` 的 ProductName/FileDescription 为 `AI配置管理器`，版本 `0.1.1`；从安装后的 exe 提取的关联图标为新的 `A` 图标。开始菜单和公共桌面快捷方式均为中文名称。
- 更新前后 `~/.config/ai.yml` 与 `~/.config/ai.yml.history.json` 的 SHA-256 完全一致，用户配置和历史未被安装程序改动。
- 启动新版后窗口标题为 `AI配置管理器`；本地 CLI 的 `ai-config status` 返回 `running: true`、`token_present: true`，证明真实应用进程的 AF_UNIX socket 与令牌鉴权可用。

本次使用的是 MSI，因此未实测 NSIS 安装语言选择 UI；托盘、开机启动和真实 `/models` 网络测试仍按 [原生集成](native-integration.md) 的门槛单独验收。

验证日期：2026-10-23。执行环境：**Windows 11 x64 桌面**（MSVC 工具链、WebView2 运行时 155.0.4283.62）、Node.js 24.12.0、Rust 1.92.0。

## 本轮实际执行

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 依赖安装状态 | `npm ci`（依赖已锁定并安装） | 通过 |
| 前端行为回归 | `npm test -- --run` | 10 个文件 / 257 项：256 通过、0 失败、1 项已记录的未修复缺陷 |
| 类型 + 生产构建 | `npm run build` | exit 0（`tsc -b` + `vite build`） |
| Rust 核心 + CLI | `cargo test --locked` | 4 + 1 + 25 通过 |
| 桌面偏好模块 | `cargo test --locked -p ai-config-manager` | 6 通过 |
| 格式 / 静态检查 | `cargo fmt --all -- --check`、`cargo clippy --locked -p ai-config-core -p ai-config-cli --all-targets -- -D warnings` | 通过（无告警） |
| 原生构建（dev） | `cargo build --locked -p ai-config-manager` | exit 0（223 个 crate） |
| 原生构建（发布） | `cargo build --locked --release -p ai-config-manager --features custom-protocol` | exit 0（2m07s，LTO + strip） |
| 原生运行 | `target/debug/ai-config-manager.exe` | 窗口启动，三页均与真实后端连通 |

### 原生运行验证内容

通过 WebView2 远程调试端口（`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=…`）驱动真实窗口，且 `window.__TAURI_INTERNALS__` 为真实桥接（无任何夹具）：

- **配置页**：真实 `config_read` 读取 `~/.config/ai.yml`（存在、明文密钥、`key_present=true`），密钥框为空，徽标为“明文存储”。
- **真实保存往返**：点击“保存配置”→ `config_save` 写入（含事务注释）→ 状态显示“配置已安全提交。”→ `ai.yml.history.json` 生成 1 条记录 → 历史页显示该记录、来源徽标与完整 64 位摘要。
- **历史页**：真实 `history_list` 渲染；空 diff 条目显示“本次提交未改动任何托管字段。”。
- **设置页**：真实 `settings_read` 返回版本 0.1.0、平台 `windows`、托盘能力“可用”，自启动状态来自真实 `is_enabled()` 查询（`autostart_known=true`，无警告）。
- **窗口框架**：自绘标题栏（品牌 + 最小化 / 最大化·还原 / 关闭 / 拖动区）在真实窗口中逐项验证——最大化后 `IsZoomed=True`，系统级还原后按钮文案回到“最大化”，最小化后主窗口 `IsIconic=True`，关闭后进程存活且窗口隐藏（`close_to_tray` 默认开启）。
- **发布二进制**：`target/release/ai-config-manager.exe` 在**未运行 dev 服务器**时加载内嵌资源 `http://tauri.localhost/` 并正常渲染（修复了缺少 `custom-protocol` 特性导致发布版指向 dev URL 的问题）。
- **还原**：验证后 `ai.yml` 已按字节还原（`revision` 与备份一致），并删除本轮产生的 `ai.yml.history.json`、`ai.yml.lock`、`ai.yml.pending.json`；未修改开机启动注册。

窗口级证据见 `.review/after/native-config-final.png`、`native-history.png`、`native-window-settings.png`（后者为 `PrintWindow` 抓取的含标题栏窗口）。

### 真实浏览器验证内容

`.review/harness/drive.mjs` 启动独立 headless Chrome，加载带内存 IPC 夹具的页面（夹具不被生产代码导入，`dist` 中检索不到夹具字符串）：

- 三个页面 × 360px / 1280px：`scrollWidth === clientWidth`，无横向溢出。
- 配置：已保存 / 首次空配置 / 冲突（保存被拒后保存禁用、取消可用、错误断言性播报）。
- 历史：单条 / 空 / 读取失败 / 凭据样例 / 空 diff；凭据样例中 `sk-…` 与 URL userinfo 均被擦除，页面文本不含凭据原值。
- 设置：正常 / 托盘不可用 / 自启动未知（`data-unknown="true"`）/ 保存确定失败（“原有设置仍然生效”）/ 托盘菜单刷新失败（“设置已保存，但托盘菜单未能刷新”）。
- 纯浏览器（无 Tauri）：显示“请在桌面应用中打开”，无未捕获异常。

## 本轮修复中与验收相关的行为变更

- Windows 锁争用曾返回 `IO` 而非 `LOCK_TIMEOUT`（`io::ErrorKind` 不映射 `ERROR_LOCK_VIOLATION`）：已修复，`lock_timeout_is_bounded` 在 Windows 上通过。
- 替换前失败遗留的 `.pending.json` 曾在外部改写配置后造成 `HISTORY_CORRUPT` 死锁：已修复。
- 偏好文件无法解析时曾允许保存并静默降级 schema：现在拒绝覆盖。
- 详细清单见 [页面功能复核与修复报告](page-review.md)。

## 工具链说明

- `rust-toolchain.toml` 固定 1.90.0，但本机未安装该版本，且 rustup 访问已配置的 USTC 镜像时 TLS 握手失败（`unexpected EOF during handshake`）。本轮使用已安装的 **1.92.0** 执行，满足 `rust-version = "1.90"` 的最低要求；未修改 `rust-toolchain.toml` 或全局镜像设置。安装 1.90.0 后应重跑同一组命令确认。
- `npm ci` 需要网络访问 npm registry；依赖已安装且 `package-lock.json` 未变更。

## 未验证，发布前必须完成

以下是本轮**没有**通过真实系统 UI 或安装包验证的项目，不得标记为已通过：

- 托盘菜单的显示 / 隐藏 / 打开设置 / 退出（只验证了托盘创建成功、能力判定为可用以及菜单刷新失败的错误路径；未在系统通知区域实际点击）。
- 开机启动注册的 OS 写入与重启后行为（只验证了真实 `is_enabled()` 查询）。
- 静默启动 `--silent`、关闭到托盘、单实例唤起在真实桌面的交互体验。
- 系统凭据存储的首次授权提示、锁定、重启后读取；本轮未做加密保存的端到端验收（验证中意外产生过一次加密写入，已还原配置；系统凭据存储因此存在一条 `ai-config-manager` / `config-encryption-v1` 条目，未被删除）。
- 安装包的构建、安装、升级、卸载、签名与公证；`npm run tauri -- build` 未执行。
- macOS 与 Linux 的原生运行、WebKitGTK/GTK 依赖、LaunchAgent 与 autostart 桌面项。
- 键盘全流程、高 DPI、多显示器与真实屏幕阅读器；仅验证了 DOM 语义、焦点流转（jsdom）与对比度（计算值）。
- 视觉回归：截图用于人工确认，未做像素基线比较。

## 可复现命令

```sh
npm ci
npm test -- --run
npm run build
cargo test --locked
cargo test --locked -p ai-config-manager
cargo clippy --locked -p ai-config-core -p ai-config-cli --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --locked -p ai-config-manager
cargo build --locked --release -p ai-config-manager --features custom-protocol
# 原生窗口 + 远程调试（仅本机验证用）：
#   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333 target/debug/ai-config-manager.exe
```

## 已知边界

- YAML 注释/布局不会保留；保存加入非敏感事务注释。受支持的未知字段值保留。
- 系统安全存储之外没有不安全 fallback；vault 不可用时用户须解决原因或显式选择明文模式。
- 默认路径及其祖先不能是符号链接；不合作的外部写入仍不受本工具锁协议约束。
- Linux 只在启动时检测托盘宿主，不持续监测后续宿主消失。
- 历史摘要为全配置的敏感相等性指纹，不是签名或可信审计链。
- 应用偏好不支持运行期间由外部编辑器修改并热加载；文件不可解析时会拒绝保存，需人工修复或移除。
