# DSH Launcher

DSH 的原生 macOS 启动器：常驻菜单栏的 Swift/AppKit 小体积外壳负责安装运行时、托管本地 `dsh web` 服务、看门狗和登录启动；DSH 界面仍是 dsh 自带的 Web UI，在默认浏览器中打开。

> 基于 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（`deepseek-harness/` 子模块）构建，非 DeepSeek 官方产品。

## 为什么是这种形态

`dsh web` 本身就在回环地址提供完整的 Web UI 与 API，所以外壳只需要管好这个服务：安装运行时、启动与守护、登录启动，界面交给默认浏览器，不再内嵌 Electron 或 WebView。与官方 DSH Desktop 的对比：

| | DSH Launcher（本项目） | 官方 DSH Desktop |
|---|---|---|
| 外壳 | Swift/AppKit 菜单栏，约 0.7 MB | Electron |
| 服务 | `dsh --profile launcher --no-open`，`127.0.0.1:31080` | 无端口，`dsh-app://` 协议 + 字节管道 |
| UI | 默认浏览器 | Electron 窗口 |
| 运行时 | App 内 `runtime.tar.gz` → `~/.dsh-launcher/runtime` | App 内 seed → `$DSH_HOME/profiles/desktop` |
| 登录启动 | `SMAppService.mainApp` | — |
| 深链接 | `dsh-launcher://` | — |
| 体积 | App 约 90 MB，运行时解压后约 390 MB | 约 770 MB |

## 运行流程

1. **安装运行时**：校验 `payload/manifest.json` 中的 SHA-256，把 `runtime.tar.gz`（官方 Node.js + pnpm + pnpm 安装的 `@deepseek-ai/dsh`）解压到 staging，验证 Node 与 dsh 版本后原子切换 `runtime/current` 软链，保留上一版本用于回滚。已安装的运行时比内置的更新时，保留已安装的版本。
2. **解析环境**：以 `$SHELL -l -i -c 'env -0'` 取得登录 shell 环境（GUI 应用默认只有 launchd 的精简 PATH），dsh 执行 git、包管理器等工具时与终端一致。
3. **启动服务**：`posix_spawn` 启动 `node …/dsh/lib/bin.js --profile launcher [--from-default-profile web] --no-open --host 127.0.0.1 --port 31080`，进程独占一个进程组；端口被占用时顺延。
4. **就绪**：stdout 出现 `dsh web: http://127.0.0.1:<port>/?token=…` 即就绪。token 只保存在内存中，写日志前一律脱敏；手动启动时用它打开默认浏览器，dsh 换发 30 天 cookie 后跳回干净的根路径。
5. **看门狗**：每 15 秒对 `/` 做一次原始 socket 探测（未认证返回 401 即视为存活），连续 3 次失败则重启；睡眠/唤醒有宽限期；异常退出按 1/2/5/10/30 秒退避重启，10 分钟内 5 次则进入失败态并弹窗显示 stderr 摘要。
6. **退出**：服务运行时，托盘菜单的“退出”、`⌘Q`、程序坞的“退出”、活动监视器或 AppleScript 发来的退出都会先弹确认（可勾选“不再询问”）；注销、重启、关机和 `SIGTERM` 不弹确认。确认后先 SIGTERM dsh（最多等 8 秒，再对进程组 SIGKILL），然后壳才退出。壳崩溃遗留的 dsh 会在下次启动时按 `run/daemon.json` 识别并清理。

## 构建

前置：Xcode（Swift 6）、网络（首次下载 Node.js、pnpm 与 npm 包）。

```bash
git submodule update --init   # 版本来源
make lock                     # 仅在升级 dsh 时需要：更新 runtime/ 的锁定
make app                      # payload + 编译 + 组装并签名 build/DSH Launcher.app
make run                      # 构建并启动
make test                     # 单元测试 + 真实进程级集成测试
make dmg                      # 生成拖拽安装的 DMG
```

Node.js 与 pnpm 的版本取自子模块：Node.js 用官方桌面端 `prepare-runtime.ts` 固定的版本，pnpm 用根 `package.json` 的 `packageManager`。

dsh 及其全部依赖由仓库里的运行时项目锁定：

| 文件 | 作用 |
|---|---|
| `runtime/package.json` | 唯一的依赖 `@deepseek-ai/dsh`，精确版本 |
| `runtime/pnpm-workspace.yaml` | 安装设置（平铺安装、自动补齐 peer 依赖、允许运行安装脚本的包） |
| `runtime/pnpm-lock.yaml` | 每个包的确切版本与完整性哈希 |

构建时用 `pnpm install --frozen-lockfile` 安装，同一份锁文件每次装出相同的依赖；锁文件与 `package.json` 不一致时安装失败。构建还会检查锁定的 dsh 版本等于子模块 `apps/cli/package.json` 的版本（或显式的 `DSH_VERSION`），不一致就报错并提示运行 `make lock`。锁文件包含各平台的可选依赖条目，arm64 与 Intel 构建共用一份。

升级 dsh：更新子模块后运行 `make lock`，提交 `runtime/` 的改动。`make lock` 只改 dsh 的版本号，仍然满足依赖范围的其他包保持原版本。要打包子模块以外的版本，两条命令都带上版本：

```bash
make lock DSH_VERSION=0.1.6-alpha.2 && make app DSH_VERSION=0.1.6-alpha.2
```

其他可覆盖的设置：

```bash
make app ARCH=x86_64                      # Intel 包（Apple Silicon 上需 Rosetta）
make app BUILD_NUMBER=202609191  # 显式构建号（分享出去的包建议这样做）
make app COPYRIGHT="© 2026 Drswith"        # 访达“显示简介”中的版权信息
```

payload 构建方式对齐官方桌面端 seed：由内置 Node 运行固定版本 pnpm，隔离 store/config，`nodeLinker: hoisted`，只允许经过评审的依赖构建脚本（node-pty、koffi、fs-ext、dsh-subprocess-local）。唯一差异是开启 `autoInstallPeers`：官方把全部第一方包显式列为依赖，而从 registry 安装时需要 pnpm 补齐插件包的 service peerDependencies。pnpm 11 安装前会对锁文件里的全部包做一遍供应链策略检查，约需 1～2 分钟。构建末尾会用临时 `DSH_HOME` 实际启动一次并等待就绪行（`SKIP_SMOKE=1` 可跳过）。payload 只在锁文件、版本或签名身份变化时重建（`FORCE=1` 强制重建）。

### 签名

签名身份放在不提交的 `signing.local.env`（格式见 `signing.local.env.example`），环境变量优先于该文件：

```bash
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:-Apple Development: you@example.com (ABCDE12345)}"
CODESIGN_TEAM_ID="${CODESIGN_TEAM_ID:-XXXXXXXXXX}"   # 证书的 OU，不是名称括号里的 ID
```

做法参考官方桌面端（`apps/desktop/scripts/verify-macos-signature.mjs`、`macos-runtime.ts`）：

- 运行时里的每个 Mach-O（node、`.node` 原生模块、spawn-helper、ripgrep、libvips 等）先单独签名，全部启用 hardened runtime 并带安全时间戳；Node 额外带 [`node.entitlements`](Resources/Entitlements/node.entitlements)（`allow-jit`、`allow-unsigned-executable-memory`、`disable-library-validation`，与官方桌面端的 Node 相同）。签名后再做冒烟启动，确认 hardened runtime 下能正常运行。
- App 与 DMG 用同一证书签名。
- 每次签名后都会校验：`codesign --verify --strict`，签名者（`Authority`）、`TeamIdentifier`、时间戳，以及 hardened runtime 标志，任何一项不符都会让构建失败。
- 没有 `signing.local.env` 时退回 ad-hoc 签名，只有本机认可。

Apple Development 证书适合本机和开发调试；要让其他 Mac 双击打开，需要 Developer ID Application 证书并完成公证。

### 版本号

| 字段 | 来源 | 示例 |
|---|---|---|
| `CFBundleShortVersionString` | `APP_VERSION` | `0.1.0` |
| `CFBundleVersion` | `BUILD_NUMBER`，未指定时取提交数 | `2` |
| `DSHLauncherGitCommit` | 完整 commit，有未提交改动时加 `-dirty` | `fde1dc81…-dirty` |
| `DSHLauncherBuildDate` | 构建时间（ISO 8601） | `2026-09-19T13:05:24+08:00` |
| `DSHLauncherRepoURL` | `REPO_URL`，未指定时取 `origin`（转成 https，去掉账号信息）；“关于”中显示为 `GitHub: Drswith/dsh-desktop` | `https://github.com/Drswith/dsh-desktop` |

除构建号外，这些字段都显示在“关于”窗口里；构建号只供系统比较新旧，和 commit 一起写进启动日志。构建号必须是 1～3 段数字（Apple 的要求）；`make dmg` 在构建号不是显式指定、或工作区有未提交改动时会给出提示。

## 目录布局

```
~/.dsh-launcher/
├── runtime/current -> dsh-<ver>-node-<ver>-<arch>-<sha8>/
│   ├── node/  pnpm/  app/node_modules/@deepseek-ai/dsh
│   └── bin/dsh, bin/pnpm        # 终端可用的 shim，例如 dsh plugin --profile launcher add <pkg>
├── logs/launcher.log            # 外壳日志
├── logs/dsh.log                 # dsh stdout/stderr（token 已脱敏）
├── run/daemon.json, launcher.lock
└── config.json                  # 可选，菜单“编辑配置…”会创建

~/.dsh/                          # dsh 自己的数据：会话、设置、凭据，与 CLI 共享
└── profiles/launcher/           # 本应用专属 profile，插件与 CLI 的 web profile 隔离
```

## 配置

`~/.dsh-launcher/config.json` 的所有字段都可省略，修改后在菜单中选择“重启服务”生效：

```json
{
  "port": 31080,
  "profile": "launcher",
  "dshHome": "~/.dsh",
  "extraArgs": ["--trusted-host", "my-host.local"],
  "environment": { "HTTPS_PROXY": "http://127.0.0.1:7890" },
  "runtime": {
    "node": "/path/to/node",
    "entry": "/path/to/deepseek-harness/apps/cli/lib/bin.js"
  }
}
```

`runtime` 用于开发：直接运行一份已构建的 deepseek-harness 源码而不使用内置运行时。`profile` 设为 `web` 可与 `dsh web` 共用插件配置。

## 菜单与深链接

菜单：状态与运行时版本、打开 DSH（⌘O）、复制访问链接、重启/停止/启动服务、登录时启动（含“需要批准”状态）、隐藏 / 显示 Dock 图标、在访达中显示日志、打开 DSH 数据目录、编辑配置、重新安装内置运行时、关于、退出。

“关于”窗口参照 VS Code 的格式：应用图标和名称下逐行列出 Version、GitHub（仓库不在 GitHub 时为 Repo）、Commit、Date（附相对时间）、DSH / Node.js / pnpm 版本和 OS；“复制”按钮把这些信息放进剪贴板，方便反馈问题。状态栏菜单和 Dock 模式下的应用菜单共用这个窗口。

程序坞：启动后显示程序坞图标，点击它会打开 DSH；右键菜单列出状态、打开 DSH、复制访问链接、重启 / 停止 / 启动服务。菜单栏里的“隐藏 Dock 图标”只对本次运行有效，再次打开应用（访达、启动台或程序坞）会恢复图标并打开 DSH。

深链接：`dsh-launcher://open`、`start`、`stop`、`restart`、`logs`。

开发时设置 `DSH_LAUNCHER_NO_OPEN=1` 直接运行可执行文件，启动时不会自动打开浏览器。

## 代码结构

| 路径 | 职责 |
|---|---|
| `Sources/DSHLauncherCore/RuntimeInstaller.swift` | payload 校验、解压、版本验证、原子切换与清理 |
| `Sources/DSHLauncherCore/DaemonSupervisor.swift` | 启动、就绪识别、看门狗、崩溃退避、优雅停止、孤儿清理 |
| `Sources/DSHLauncherCore/ProcessSpawner.swift` | `posix_spawn`：独立进程组、默认信号处置、不泄漏描述符 |
| `Sources/DSHLauncherCore/ShellEnvironment.swift` | 登录 shell 环境解析与合成 |
| `Sources/DSHLauncher/` | AppKit：状态栏菜单、状态窗口、登录项、深链接、中英文本地化 |
| `scripts/` | payload 构建、冒烟启动、App 组装与签名、DMG |

## 后续工作

- **发版**：打 tag 时由 CI 构建并上传到 GitHub Releases。dsh 版本随 App 一起发布，不单独热更新运行时。
- **对外分发**：需要 Developer ID Application 证书（付费开发者计划）并完成公证（`notarytool` 提交、`stapler` 装订）；运行时 Mach-O 的逐个签名已经就绪。
- **自更新**：App 可接入 Sparkle；新版本自带的运行时会在启动时自动替换。
- **可选窗口模式**：用 WKWebView 承载同一 URL，作为浏览器之外的选项。
