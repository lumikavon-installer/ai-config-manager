# AI配置管理器 / AI Config Manager

Tauri 2 桌面配置工具，默认中文，支持英文。仅在本机管理 `~/.config/ai.yml` 的对话、向量、Laya 决策和 CosyVoice 语音接口配置，提供加密保存、最近十次变更、托盘、自启动、静默启动（二者默认开启）、显式的 `/models` 连接测试、GitHub 加密同步、保存后自动同步、本地 Unix socket 的 JSON/REST 接口以及共享核心的 CLI / Skill。应用图标为在地深色圆角底上的绿色 `A` 字符。

## 环境与安装

- Rust **1.90.0**，由 `rust-toolchain.toml` 固定；Node.js **22 或 24 LTS** 与 npm。
- Windows 10/11：Visual Studio 2022 Build Tools 的“使用 C++ 的桌面开发”、Windows SDK、Microsoft Edge WebView2 Runtime。使用原生 MSVC Rust 工具链构建，不需要管理员权限运行应用。
- macOS：Xcode Command Line Tools，系统 Keychain。打包、签名与公证需要开发者自己的 Apple 身份。
- Linux：GTK 3、WebKitGTK 4.1、AppIndicator、Secret Service 会话服务。Debian/Ubuntu 可安装 `build-essential pkg-config libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf`。加密需要已解锁的 GNOME Keyring/KWallet Secret Service，纯 headless 系统不能视为原生验收环境。

在源码根目录执行：

```sh
npm ci
npm run tauri -- dev
```

首次打开配置页，录入 Base URL、模型和 API Key，保持“加密保存”开启并保存。密钥框在读取已有配置后永远为空，空白表示保留原密钥，而非清空。需要替换时输入新密钥。系统安全存储不可用时会失败，不会退回明文。

### 测试、构建与打包

```sh
npm test -- --run
npm run build
cargo test --locked
cargo fmt --all -- --check
npm run tauri -- build   # 自动启用 custom-protocol（内嵌前端）
# 直接调用 cargo 时：cargo build --locked --release -p ai-config-manager --features custom-protocol
# 缺少 custom-protocol 的 release 构建会以 dev 模式加载 build.devUrl，产物不可分发
cargo build --locked --release -p ai-config-cli
```

`cargo test` 默认只运行核心与 CLI，不要求 GUI 库；桌面原生测试可用 `cargo test --locked -p ai-config-manager`，需要上面的原生依赖。`scripts/verify.sh` / `scripts/verify.ps1` 顺序执行基础验证。安装包位于 `target/release/bundle`，CLI 为 `target/release/ai-config-manager-cli`（Windows 为 `.exe`）；源码构建也保留 `ai-config` 兼容命令。Windows MSI 和 NSIS 需要管理员权限，将 CLI 安装到系统目录；Linux deb/rpm 将 CLI 安装到 `/usr/local/bin`。AppImage 是免安装程序，其 CLI 位于包内，独立 CLI 归档可从 Release 下载并手动安装；macOS app/DMG 同样不修改系统 PATH。Windows NSIS 安装包会先显示安装语言选择（简体中文/English），中文系统默认中文；MSI 固定使用 `zh-CN`。安装后的显示名称为 **AI配置管理器**，桌面可执行文件为 `ai-config-manager.exe`。Windows 安装包在 Windows 原生环境生成。不要把跨平台 `cargo check` 当成安装或桌面运行通过。

推送与项目版本一致的 `v*` 标签会触发 `.github/workflows/release.yml`：先执行前端与 Rust 验证，再在 Windows、macOS（Apple silicon）和 Linux 上构建安装包及独立 CLI，并自动发布为 GitHub Release。构建产物未签名。

仓库提交 `Cargo.lock` 与 `package-lock.json`，构建使用锁文件。CI 配置提供三个 OS 的原生编译检查，但配置存在不代表 CI 已执行。实际验证记录见 [验收记录](docs/acceptance.md)。签名证书、API Key 和开发者身份均不随源码提供；未签名的安装包可能被系统提示来源未验证。

## 路径与配置格式

所有系统均使用当前用户 home 下 `.config/ai.yml`，Windows 例如 `C:\Users\Alice\.config\ai.yml`。不是 `%APPDATA%` 中的 AI 配置副本。桌面应用偏好设置则使用 Tauri 的应用配置目录。

```yaml
ai:
  openai:
    base_url: https://api.openai.com/v1
    api_key: ENC(v1:BASE64_ENVELOPE)
    model: your-model-id
  embedding:
    base_url: http://127.0.0.1:8848/v1
    model: embedding-model
    api_key: ""
  laya:
    base_url: http://127.0.0.1:8848/v1
    model: laya-multilingual
    api_key: ""
  cosyvoice:
    base_url: http://127.0.0.1:8848/v1
    model: cosyvoice3-0.5b-2512
    api_key: ""
  another_provider:
    enabled: false
other_settings:
  labels: [internal, workstation]
```

四个接口块彼此独立：保存任一块不会改动其他块的地址、模型或密钥。`openai` 必须提供密钥；`embedding`、`laya`、`cosyvoice` 面向本地无鉴权服务，`api_key` 可为空，空值不会发送 `Authorization`。`/v1/models` 测试对四类接口共用；qwen3 本地网关通常为 `http://127.0.0.1:8848/v1`。

示例的 `BASE64_ENVELOPE` 仅表示格式，不能当作真实加密值。已有明文 API Key 可读取且不会回传前端；再次保存时可迁移为加密。保存保留未修改字段的 YAML **值语义**，不保留注释、缩进、锚点表现形式或键的原始排列。拒绝重复键、多文档、复杂非字符串映射键、自定义标签及非有限数值等歧义输入；格式错误时不覆盖原文件。

Base URL 限定 HTTP(S)，必须有主机，不允许用户名、密码、查询参数或 fragment。四个接口均要求模型非空且不含控制字符；`openai` 密钥不得为空。普通保存与 `check` 不做网络可达性检查；离线校验也不证明上游账号、模型权限或计费额度有效。**测试 AI 连接**（GUI 按钮或 `ai-config-manager-cli test`）只有在用户明确触发时，才会携带已保存密钥（空密钥不带授权头）对 `{base_url}/models` 发起一次 GET 请求；不读取响应正文、不跟随重定向、不使用模型对话端点。该检查假设服务兼容 OpenAI 的 `/models` 端点；不支持时返回 `endpoint_unsupported`。HTTP 会明文传输网络数据，只有信任本机代理时才应使用。

## 加密、解密与安全边界

使用 RustCrypto AES-256-GCM，随机 256 位数据密钥来自操作系统随机源，并保存在操作系统凭据存储中。每次加密产生新的 96 位 nonce，固定附加认证数据为 UTF-8 `ai-config-manager:api-key:v1`。

`ENC(v1:BASE64)` 的 BASE64 使用标准 alphabet 与 padding，解码字节为 `12 字节 nonce || AES-GCM ciphertext || 16 字节认证标签`。密文可位于 `ai.openai.api_key` 及各命名接口的 `api_key`；空密钥保持为空，不生成密文。没有自定义密码学，也没有源码内固定密钥。

读取时先识别 ENC 版本，再向安全存储读取原密钥，验证 GCM 标签后在 Rust 内存中使用明文；可信 SDK 显式请求解密读取接口时，可通过已鉴权的本地 socket 获得该明文。缺失密钥、错误格式、未知版本、锁定 vault 或认证失败都会明确失败，不新建替代解密密钥、不降级、不把密钥返回前端。首次加密保存才可以创建新密钥。配置中的保存值和凭据存储中的数据密钥必须同时可用；单独复制 YAML 到另一台机器通常无法解密。

平台使用 keyring 3.6.3：Windows Credential Manager、macOS Keychain、Linux Secret Service。确切凭据条目命名及一致性机制见 [核心设计](docs/core-design.md)。所有密钥管理动作都发生在当前用户身份下；应用不自动提升权限。开发测试使用合成密钥和内存 vault，不注册真实凭据。

密钥输入默认密码显示；不会打印到日志、命令参数、历史或异常中。Rust 秘密类型使用脱敏 Debug 并清零自身缓冲区；语言运行时、序列化及系统组件可能产生短期副本，因此不宣称抵御内存取证。同一系统用户、管理员或已入侵进程仍可能读取本机凭据或截取输入。不要将自己的密钥填入截图、Issue 或聊天。

默认加密。显式选择明文保存需要界面确认，文件仍按私密权限写入。Unix 新文件为 0600；Windows 私密文件使用当前用户受保护 DACL。不会批量改动父目录及其他配置的权限。文件权限不是备份加密的替代品。不要通过共享目录同步明文配置。

## 原子保存、并发与变更历史

前端读取的 `revision` 是当前文件原始字节的 SHA-256；首次不存在为 `missing`。保存要求版本匹配，并以文件锁串行化本工具的多个进程。临时文件写入、刷新，再在同一目录替换目标。外部程序若不遵守相同锁协议，仍可能在最终检查后写入；本工具不能对不合作程序提供文件系统级 compare-and-swap。

历史记录包含 `id`、RFC3339 UTC `timestamp`、`diff`、`sha256` 和 `source`。最新在前，最多十条。仅记录托管字段差异；密钥变更只显示 `[EMPTY]` / `[REDACTED]`。重新输入同一密钥仍作为一次替换操作记录，不能从历史还原旧密钥。

配置旁的私密 `.history.json` 保存历史，`.pending.json` 是不含密钥的提交意图，`.lock` 用于跨进程锁。提交先写意图，再替换 YAML，最后根据原始文件版本完成历史。崩溃后根据配置的实际版本对账，不伪造回滚。每次保存添加一个不含秘密的随机事务注释，保证即使明文内容未改变，原始字节版本也能区别提交前后；语义 SHA-256 不受影响。恢复同时校验历史前态摘要，过期旁车文件不能覆盖更新的历史。`HISTORY_PENDING` 表示配置已经提交但历史待恢复，应重新读取并查询历史，不能盲目重复保存。格式损坏的历史不会阻止读取当前配置，但会阻止下一次保存，避免丢弃旧证据。请先备份相关文件，再按错误恢复说明处理。

### SHA-256 的对象

摘要针对**完整配置的语义内容，包括在后端解密后的全部接口 API Key**，不是随机 ENC 密文，也不是脱敏视图。先把每个已配置接口的 `api_key` 替换为内存中的明文，递归转换为 JSON：映射键按字典序，数组保持顺序，字符串/布尔/null/有限数字按 JSON 编码，无额外空白；对 UTF-8 字节计算小写十六进制 SHA-256。注释、YAML 格式及加密 nonce 不影响摘要；其他字段的语义变化会影响摘要。不进行 URL 大小写等业务归一化。详情与具体数字规范见核心设计。

摘要不是签名，也不是第三方可信审计证明。由于包含密钥，持有历史并已知其他内容的人可猜测低熵密钥，因此必须使用高熵 API Key 并保护历史文件。加密与明文存储模式不改变语义摘要。

## 托盘、自启动与静默

静默启动和开机启动默认开启（首次运行会尝试注册当前用户的开机启动，之后用户可在设置中关闭）。关闭窗口默认隐藏到托盘。托盘菜单可显示、隐藏、打开设置、真正退出。设置中可以关闭“关闭到托盘”。`--silent` 或持久化的静默设置会在启动后不显示主窗口；开机启动注册携带 `--silent`。如果托盘不可用，应用必须显示窗口而不静默失联。

自启动状态向操作系统实际查询，不以普通 JSON 的布尔值冒充注册成功。更改失败或查询未知会显示错误。Windows 使用用户启动注册，macOS 使用 LaunchAgent，Linux 使用桌面会话 autostart；具体后台限制见 [桌面集成](docs/native-integration.md)。同一应用重复启动会唤起已有实例。

## 本地代理接口与 Skill

桌面应用在 `~/.config/ai-config-manager/agent.sock` 提供 Unix domain socket（Windows 上同样使用 AF_UNIX）。若该路径超出 AF_UNIX 长度限制，应用与 CLI 会一致地改用临时目录下带用户哈希的子目录。同目录的 `agent.token` 为 0600/受保护 DACL 的本地随机令牌，每个请求都必须携带；令牌不会输出到 CLI、日志或界面。Unix 上目录为 0700、socket 为 0600。

同一 socket 同时支持两种本地协议：

- 单行 JSON：`ping`、`read`、`check`、`history`、`test`、`save`，以及命名接口的 `read_endpoint`、`read_endpoint_decrypted`、`check_endpoint`、`test_endpoint`、`save_endpoint`。响应只包含脱敏元数据、配置视图、历史或 `ProbeResult`，从不含密钥（显式解密操作除外）。`save` 复用核心的 revision 冲突检查、锁、原子替换和历史记录，成功后向 GUI 发送 `config-changed` 事件，并将新配置标记为待自动同步；GUI 无未保存草稿时自动刷新，有草稿时标记为外部冲突并要求重新加载。
- SDK 显式解密读取：单行 JSON 操作 `read_decrypted`，或 REST `GET /ai-config/decrypted`，在同一 `agent.token` 鉴权后返回 `base_url`、`model`、明文 `api_key` 和存储状态 `encrypted`。命名接口使用 `read_endpoint {kind}`、`read_endpoint_decrypted {kind}`、`test_endpoint {kind}`、`check_endpoint {kind}` 和 `save_endpoint {kind, request}`，`kind` 为 `openai`、`embedding`、`laya` 或 `cosyvoice`；原 `read`/`save`/`test` 仍等价于 `openai`。所有字段来自同一次已验证的配置快照。此接口只供同一系统用户的可信本地程序使用；不要记录响应，令牌持有者可以获取 API Key。
- HTTP/1.1 REST：请求行、`Authorization: Bearer <agent.token>`（或 `X-Agent-Token`）和 JSON 响应。每个连接处理一个请求，响应格式为 `{ "ok": true, "data": ... }` 或 `{ "ok": false, "error": ... }`。

单行 JSON 也支持全部同步操作：`sync_read`、`sync_save_settings`、`sync_add_item`、`sync_set_item_enabled`、`sync_remove_item`、`sync_run`、`sync_register_application`、`sync_run_application`、`sync_set_application_auto_sync`。请求外层保持 `{"version":1,"token":"<agent.token>","op":{...}}` 并以换行结束；例如 `op` 为 `{"op":"sync_add_item","label":"MyApp","path":"/absolute/path/app.json","enabled":true}`。保存目标使用 `{"op":"sync_save_settings","request":{"repo":"owner/repo","github_token":null,"encryption_secret":null}}`，两个秘密字段为空时保留已存值。调用者从私密令牌文件读取鉴权信息，不将其打印或放进命令参数。

第三方应用通过 socket 注册文件或应用回调后，可调用全局 `sync_run`（REST `POST /sync/run`），或只同步自己的 `sync_run_application { app_id }`（REST `POST /sync/applications/{app_id}/run`）。`sync_set_application_auto_sync { app_id, enabled }`（REST `PATCH /sync/applications/{app_id}/automation`，body `{ "enabled": false }`）控制该应用是否参与定时同步；手动同步仍可用，且不会推迟全局调度。管理器会合并已保存的 GitHub 或自定义 Git 仓库中的加密快照。同一进程同步期间的目标/项目修改及重复同步返回 `SYNC_BUSY`。外部同步操作完成后，桌面刷新列表与结果，同时保留用户正在编辑的同步草稿。调用已发出但响应丢失时，先查询 `/sync` 或 `sync_read` 确认结果。

可直接运行的接入示例见 [examples/sync_config.py](examples/sync_config.py)（Linux/macOS Python AF_UNIX）；Windows 可通过下方 Rust CLI 完成相同流程。

REST 接口：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/health` | 服务与版本 |
| GET | `/ai-config` | 查询脱敏 AI 配置 |
| GET | `/ai-config/decrypted` | 可信 SDK 显式获取解密后的 AI 配置，响应禁止缓存 |
| GET | `/ai-config/endpoints/{kind}` | 查询指定接口的脱敏配置 |
| GET | `/ai-config/endpoints/{kind}/decrypted` | 显式获取指定接口的解密配置，响应禁止缓存 |
| POST | `/ai-config/endpoints/{kind}` | 保存指定接口，body 与 `save` 请求相同 |
| POST | `/ai-config/endpoints/{kind}/check` | 离线校验指定接口 |
| POST | `/ai-config/endpoints/{kind}/test` | 显式测试指定接口的 `/models` |
| GET | `/ai-config/history` | 最近十次脱敏变更 |
| GET | `/sync` | 同步列表、配置状态与实际同步结果 |
| GET | `/sync/status` | 仅同步状态 |
| POST | `/sync/settings` | 保存 `provider`、`repo`、`git_branch`、`github_token`、`encryption_secret` |
| POST | `/sync/test` | 测试草稿目标的仓库读写权限并清理临时分支，不保存设置 |
| POST | `/sync/items` | 注册 `label`、`path`、`enabled` 同步项 |
| PATCH | `/sync/items/{id}` | 启用/停用同步项 |
| DELETE | `/sync/items/{id}` | 移除同步项 |
| POST | `/sync/run` | 执行一次加密合并同步，应用较新配置 |
| POST | `/sync/applications/{app_id}/run` | 仅同步该应用的已启用同步项 |
| PATCH | `/sync/applications/{app_id}/automation` | 设置该应用是否参与定时同步 |
| POST / GET | `/sync/applications` | 注册/查询第三方应用配置，含名称和 Unix socket 回调接口 |
| POST | `/sync/policy` | 保存定时开关、分钟间隔、历史保留天数 |
| GET | `/sync/history` | 查询最近最多 100 条同步记录 |
| POST | `/sync/history/cleanup` | 立即清理超过保留策略的同步和配置历史 |

示例（Unix；Windows 的 curl 构建需支持 `--unix-socket`）：

```sh
TOKEN=$(cat ~/.config/ai-config-manager/agent.token)
curl --unix-socket ~/.config/ai-config-manager/agent.sock \
  -H "Authorization: Bearer $TOKEN" http://localhost/ai-config
curl --unix-socket ~/.config/ai-config-manager/agent.sock \
  -H "Authorization: Bearer $TOKEN" http://localhost/sync
```

应用退出时删除 socket 与令牌。REST 请求体中的 Token/加密密码由本地核心零化处理，响应不回传同步凭据；只有显式解密读取接口返回 AI API Key。Rust JSON/HTTP 响应缓冲在释放时清零，HTTP 响应包含 `Cache-Control: no-store`。

配套 Python SDK 位于同级工作区的 `.sdk/ai-config-manager`：`load_config()` 直接读取并解密本地 YAML；`load_config_from_agent()` 直接通过 Unix socket 获取解密配置。后者不要求 Python 自己读取系统凭据库，支持 Windows 的 Winsock AF_UNIX 适配。旧版管理器没有解密读取操作，需要构建并运行包含该接口的新版本。

`ai-config-manager-cli` 默认先尝试代理：应用运行时由应用读写；应用未运行时直接使用同一核心读写文件。显式 `--config PATH` 始终走文件路径。代理连接失败不会在“请求已经发出”之后盲目重试，避免重复提交。

## GitHub 与自定义 Git 配置同步

侧边栏提供“配置 / 历史记录 / 同步列表 / 应用注册 / 同步记录 / 设置”六个入口。同步列表只展示已注册第三方应用的配置同步项，支持启停、移除和手动同步；没有应用项时引导先注册。应用注册页展示应用列表，点击“注册应用”打开注册弹窗。设置页保留“常规 / 同步目标 / 自动化 / 关于”四个 Tab。同步管理页面与设置之间切换保留未保存输入，离开到配置/历史或关闭窗口时提供草稿保护。

- AI 配置默认注册，包含解密后的可移植 YAML；其他配置通过绝对文件路径注册，单文件上限 1 MiB。
- GitHub Token 与加密密码只保存在系统凭据存储（Windows Credential Manager / macOS Keychain / Linux Secret Service），不写入 `sync.json`，界面只显示是否已保存。
- 自定义 Git 选择 `provider: "git"`，支持 HTTPS、SSH、SCP SSH 地址和绝对本地仓库路径，可指定分支（默认 `main`）。依赖系统安装的 Git，通过当前用户的 credential helper 或 SSH Agent 认证；请提前配置凭据和 SSH 主机信任。Git 不弹出交互提示，URL 中不允许嵌入密码或 HTTP 用户名。该模式不需要 GitHub Token，同步加密密码仍保存在系统凭据库。
- “测试仓库读写权限”测试当前表单草稿，无需保存或提供加密密码。两种模式都需要系统 Git：实际读取远端，再推送并删除唯一临时分支 `ai-config-manager-probe/<随机标识>`，不改同步分支。界面分别展示读取、写入、清理结果；清理失败会给出待手工删除的分支。通用写权限通过后，指定分支仍可能受保护规则限制。
- 同步包使用 Argon2id 派生密钥、AES-256-GCM 加密，上传到仓库的 `.ai-config-manager/sync.enc`。密码丢失后无法恢复同步内容。
- 保存任一 AI 接口成功后，核心会写入一个待同步标记。桌面调度器在约 2 秒去抖后提前下一次计划同步，并复用同一加密快照；若保存发生在同步进行中，本轮结束后会再安排一次，避免新配置等待完整间隔。目标未配置、自动同步关闭或 AI 同步项停用时标记保留，待条件满足后生效。内容未变化的快照不会重复上传。
- Token 需要目标仓库的 Contents 读写权限。每次同步先解密远端快照，按每项 `updated_at` 比较，新配置覆盖旧配置；远端较新的内容会应用到本机已注册且启用的配置。AI 配置通过原有版本检查、加密和历史事务写回；其他文件检查内容摘要后原子替换，应用回调通过 `expected_sha256` 拒绝并发编辑。远端路径永远不用于本地写入。
- `sync.json` 只保存仓库类型与分支、同步项/应用注册、策略/下次时间、实际状态与同步记录，不保存秘密。
- REST 的 `/sync`、`/sync/settings`、`/sync/items`、`/sync/run` 提供同样的列表、注册、配置与执行能力。

第三方应用使用稳定的 `app_id` + `config_id` 注册，同一配置在各设备上使用相同标识。同步接口为本机 Unix socket 的 HTTP 回调：GET 返回 base64 配置和更新时间，PUT 接收较新配置并进行摘要比较。注册是幂等更新，接口路径可因设备而不同。完整请求、响应及示例见 [同步协议](docs/sync-protocol.md)。

“同步记录”展示触发来源、时间、结果、上传/下载/冲突数量与固定错误码，按完成时间倒序最多保留 100 条，超出的记录会从磁盘自动删除。“自动同步与清理”默认开启定时同步，间隔默认 10 分钟，可配置 1–1440 分钟；尚未保存同步目标时只推进下次同步时间，不记录失败；关闭窗口到托盘后仍运行，退出进程后停止。历史默认保留 30 天，可配置 0–3650 天，0 表示仅保留数量上限；该时间策略也清理 AI 配置历史，其 10 条上限保持不变。应用启动、后台每分钟及同步完成时执行清理，并提供立即清理按钮。

第三方应用未运行、socket 已移除或回调超时时记为 `SYNC_APPLICATION_OFFLINE`（REST 503），不会让整次同步失败：该应用本次跳过，远端配置保留，并出现在同步页的“等待自动重试的同步项”中。网络或仓库临时不可用（`GITHUB_NETWORK`、`GITHUB_TIMEOUT`、`GITHUB_SERVER`、`GIT_*` 超时/读写失败、`SYNC_VAULT`）以及存在待重试项时，会按 1、2、4、8 分钟退避重试，上限为配置的同步间隔；应用后上线或网络恢复后自动补齐。连续尝试不会形成紧凑重试循环，永久性错误（Token/仓库/密码错误）仍按正常间隔重试。

GitHub 409/422 写入竞争最多自动拉取、合并并重试 3 次；相同时间戳按内容摘要确定一致顺序。请保持各设备时钟准确。错误密码或无效远端数据会失败，避免覆盖无法验证的内容。

自定义 Git 在临时 bare 仓库中读写加密快照，通过分支 lease 检查并发更新，最多重试合并 3 次；不检出工作树、不运行本地 hooks、不修改分支中的其他文件。SDK 已在相邻目录 [`.sdk/ai-config-manager`](../.sdk/ai-config-manager/README.md) 更新到 0.4.0，提供四类接口配置、应用注册、同步项管理、双仓库目标、权限测试、策略、记录和清理的完整 `AgentClient` 接口。

## CLI 与 Skill

安装包提供 `ai-config-manager-cli`；源码构建的兼容命令 `ai-config` 功能相同。将 `skill/ai-config` 复制到使用者的 Skill 目录（例如 `~/.codex/skills/ai-config`）。它不自动安装、不保存新凭据；只有显式 `test` 才访问配置中指定的服务。

```sh
ai-config-manager-cli status
ai-config-manager-cli read
ai-config-manager-cli check
ai-config-manager-cli test
ai-config-manager-cli history
ai-config-manager-cli read --endpoint embedding
ai-config-manager-cli check --endpoint laya --config /path/to/ai.yml
ai-config-manager-cli test --endpoint cosyvoice
ai-config-manager-cli set --base-url https://api.example.com/v1 --model example-model
printf '%s' "$API_KEY" | ai-config-manager-cli set --api-key-stdin --encrypt
ai-config-manager-cli set --endpoint embedding --base-url http://127.0.0.1:8848/v1 --model embedding-model
ai-config-manager-cli read --config /path/to/ai.yml
ai-config-manager-cli sync-status
ai-config-manager-cli sync-add --path /absolute/path/app.json --label MyApp
ai-config-manager-cli sync
```

CLI 只输出单个 JSON 结果，成功退出码 0，失败 1；`--help` 输出帮助文本。read 返回脱敏元数据，check 验证并在内存解密，history 返回脱敏记录，test 返回连接测试分类（**顶层 `ok` 表示命令完成，`data.ok` 表示连接结果**）。`set` 默认加密；未提供 `--api-key-stdin` 时保留原密钥，只有显式 `--plaintext` 才写入明文。密钥只能从 stdin 读取，从不接受 argv。没有导出明文密钥选项。读操作可能创建私密锁文件；history 会完成已经提交的待恢复历史，因此不是字节意义上的零写入诊断。

可信 Rust 工具直接依赖 `crates/core` 并使用 `Store::with_config`。回调获得同一快照的 Base URL、模型和 `with_secret` 访问器；不得将访问器中的秘密打印、序列化或交给不可信子进程。实际发送授权头需要调用工具自己获得对指定服务的授权，该库不自动发送。

`sync-status`、`sync-add` 和 `sync` 要求桌面应用运行并走同一个 AF_UNIX socket，Linux/macOS/Windows 均可使用。首次在设置页保存 GitHub 或自定义 Git 目标后，其他应用只需注册路径并触发同步。同步命令不支持 `--config`，应用未运行时返回 `AGENT_UNAVAILABLE`，不会另建同步状态或直接访问仓库。

其他同步管理操作可以通过 `agent` 从标准输入传入一个 socket 协议 JSON 对象，覆盖应用注册、目标设置、同步项开关、策略和历史等操作；完整操作及字段见 [同步协议](docs/sync-protocol.md)。例如：

```sh
printf '%s' '{"op":"sync_read"}' | ai-config-manager-cli agent
printf '%s' '{"op":"sync_run_application","app_id":"my-editor"}' | ai-config-manager-cli agent
```

`agent` 需要运行中的桌面程序，认证令牌由 CLI 自动读取。涉及密钥的 JSON 应由可信本地进程经 stdin 传递，不要放入命令参数或日志。CLI 拒绝返回明文凭据的操作。

## 结构

```
src/                     React UI 与集中式文案
src-tauri/               桌面命令、窗口、托盘与应用设置
crates/core/             配置、加密、历史、原子文件、平台权限、本地代理/REST、连接测试与 GitHub 同步
crates/cli/              无凭据输出的 CLI（代理优先，文件回退）
skill/ai-config/         可复用 Skill
scripts/                 验证及可复现源码打包
.github/workflows/       构建验证配置
docs/                    架构、交互、核心语义及验收限制
```

## 故障排查与维护

- `CONFLICT`：其他实例或编辑器已改变文件，重新加载并重新应用修改。
- `INVALID_CONFIG`：备份原 YAML，再用本地编辑器修复；不要复制包含凭据的解析上下文到日志。
- `KEY_MISSING` / `DECRYPT_FAILED`：恢复同一用户的安全存储备份，或在 GUI 输入新的 API Key；旧密文无法靠重置应用解密。
- `VAULT_UNAVAILABLE`：解锁系统凭据存储/Secret Service；不要选择自动明文降级。
- `COMMIT_UNCERTAIN`：提交后状态无法确认，重新读取有效配置，再决定后续操作，禁止盲目重试。
- `LOCK_TIMEOUT`：另一个进程持锁超过五秒，关闭繁忙实例或稍后重试，不要手动删除仍在使用的锁文件。
- `HISTORY_CORRUPT` / `HISTORY_PENDING`：先重新读取配置确认是否提交，备份 YAML 和所有旁车文件，再修复历史。不要只删除 pending 文件，否则可能丢失已提交记录。
- `UNSAFE_PATH`：不支持符号链接等不安全路径；将配置放在受当前用户控制的真实目录中。
- `AGENT_UNAVAILABLE` / `AGENT_TOKEN` / `UNAUTHORIZED`：应用未运行、socket 或令牌不可用，或令牌被拒绝。CLI 在确认应用未运行时会直接读写文件；若保存请求已经发出，不要盲目重试。
- `endpoint_unsupported` / `network` / `timeout`：连接测试无法通过 `/models` 验证；检查 Base URL、网络/代理或服务兼容性，这不影响本地 YAML 的有效性。
- `IO`：确认用户有目标目录写权限、文件没有被其他程序独占、磁盘空间足够；不需要盲目管理员运行。
- 托盘、启动注册和窗口问题见设置警告与原生验收清单。

升级前在应用退出后备份 YAML、历史及系统凭据存储；应用卸载不应主动删除用户配置或 vault 条目。测试、清理操作只针对专用临时目录。配置格式版本由 ENC v1、应用设置 schema_version 和源码迁移说明管理；未知版本拒绝而非覆盖。

上游参考：[Tauri 2](https://v2.tauri.app/)、[Autostart](https://v2.tauri.app/plugin/autostart/)、[keyring 3.6.3](https://docs.rs/keyring/3.6.3/)、[AES-GCM](https://docs.rs/aes-gcm/0.10.3/)。本项目 MIT；依赖保留各自许可证，未捆绑用户凭据、系统凭据数据库或第三方模型客户端。
