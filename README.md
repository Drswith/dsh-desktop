# DSH Launcher

[![CI](https://github.com/Drswith/dsh-launcher/actions/workflows/ci.yml/badge.svg)](https://github.com/Drswith/dsh-launcher/actions/workflows/ci.yml)

DSH 的托盘启动器：[Tauri 2](https://tauri.app) 写的小外壳，启动本地 dsh Web Profile，就绪后在默认浏览器里打开它。

> 运行 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（npm 上的 `@deepseek-ai/dsh`），非 DeepSeek 官方产品。

## 这个版本做什么、不做什么

范围刻意控制在启动器职责内：运行时安装/升级仍不内置，但启动、监督、配置、日志和深链接已经形成闭环。

1. 启动时先看一眼上次有没有遗留的孤儿 `dsh` 进程，有就先清掉，再按 `~/.dsh-launcher/config.json` 构造 `dsh --profile … --no-open --host 127.0.0.1 --port …`；首选端口被占用时顺延最多 20 个端口；
2. 常驻一个托盘图标和菜单，可以启动、停止、重启 dsh，打开浏览器、复制访问链接、编辑配置、打开日志、打开 DSH 数据目录、管理登录启动和查看 About；
3. 从子进程 stdout 里认出 `dsh web: http://127.0.0.1:<port>/?token=…` 这行就绪信号后，菜单项从禁用的「启动中…」变成可点的「打开 DSH」，正常启动会自动打开默认浏览器，点击菜单也可以再次打开；
4. 看门狗会在 180 秒内未就绪、进程异常退出或连续 3 次 HTTP 健康探测失败时重启，使用 1/2/5/10/30 秒退避，并在 10 分钟内 5 次失败后熔断；
5. 支持 `dsh-launcher://open|start|stop|restart|logs` 深链接，macOS 应用包会注册 `dsh-launcher` scheme，Windows/Linux 由 deep-link + single-instance 转发到已有实例；
6. 把 dsh stdout/stderr 写入 `~/.dsh-launcher/logs/dsh.log`，启动器行为写入 `launcher.log`，日志中的 token 会脱敏；
7. 点「退出」会先弹一个确认框，确认后才真的停掉 `dsh` 子进程、退出外壳；也可以选择「退出且不再询问」，偏好写入 `~/.dsh-launcher/preferences.json`。
8. About 会显示版本、构建号、commit、构建时间、外部 `dsh --version`、`node --version`、`pnpm --version` 和系统架构；登录启动使用 Tauri 官方 autostart 插件。

**用的都是 Tauri 官方 API/插件**（`tauri::tray`/`tauri::menu` 建托盘和菜单，`tauri-plugin-opener` 开浏览器并定位日志文件，`tauri-plugin-dialog` 弹退出确认框与 About，`tauri-plugin-autostart` 管理登录启动，`tauri-plugin-store` 保存用户偏好，`tauri-plugin-single-instance` 防止开两份；进程树管理在 Unix 上用独立进程组，在 Windows 上用 Job Object，孤儿记录检测用了 [`sysinfo`](https://crates.io/crates/sysinfo)），没有新增 webview，也没有直接调用任何 macOS-only 的原生 API，理论上能跨平台编译——CI 会在 macOS / Windows / Linux 上各跑一次 `cargo check` 确认这件事，但**只在 macOS 上真正跑过、点过**。

本项目之前有一版完整搬过 Swift 版能力的实现（运行时安装/校验/原子升级、崩溃看门狗、登录启动、Dock 右键菜单、多语言、About 窗口……大量直接调用 AppKit 的 Rust 代码），复杂度和"托盘 + 启动一个进程"这个目标不成比例，已经推倒重来。历史实现留在 git 历史里（`wip: objc2 直写 AppKit 的 Tauri 重构` 那次提交之前），以后要按需加什么功能可以回去参考，但不建议整体恢复。

### 已知限制（都是有意暂时不做，不是漏掉了）

- **不装运行时**：`dsh` 要能在 `PATH` 里找到。找不到或启动失败时托盘菜单会显示「启动失败」，不会崩溃。
- **配置仍是文件驱动**：菜单可以创建并用系统默认编辑器打开 `~/.dsh-launcher/config.json`，支持 `port`、`profile`、`dshHome`、`extraArgs`、`environment` 和可选的 `runtime.node`/`runtime.entry`；修改后通过「重启」或重新启动启动器生效。
- **看门狗暂未接入睡眠/唤醒通知**：进程异常退出、启动超时和 HTTP 健康失败的监督已经实现；macOS 睡眠期间的 60 秒宽限尚未单独接入系统电源事件。
- **没有 Swift 版的运行时安装/升级和运行时保留切换**：`dsh` 默认仍要求在 `PATH` 中；配置可以显式指定 Node/entry，但不会下载或校验运行时。
- **进程树清理有平台边界**：macOS/Linux 启动 `dsh` 时会先用 `setpgid(0, 0)` 建立独立进程组，退出时先向整个组发 `SIGTERM`，等待 3 秒后仍存在才发 `SIGKILL`；Windows 使用 Job Object 管理普通子进程。普通 shell、脚本和 worker 会继承这个边界，但主动调用 `setsid`、daemonize 或创建新进程组的程序可能逃逸。
- **孤儿清理只在下次启动时发生**：外壳被外部信号杀掉（`kill`、系统注销/关机、Activity Monitor 强制退出）时，Unix 上的 `dsh` 进程组会暂时继续存在；下次启动会读取 `<应用数据目录>/dsh.pid` 中的 pid、端口、进程组 ID 和启动时间，确认仍然是原来的 `dsh` 后清理整个进程组。应用如果之后一直不重开，孤儿会一直运行；Windows 成功加入 Job Object 时，Job 句柄关闭会自动清理整组。
- **退出确认框同一时间只能有一个**：连点几下托盘「退出」不会堆出好几个确认框——这是手工测试时真堆出来过之后加的保护（一个 `AtomicBool` 标志位），不是预防性写的。弹窗用的是 `tauri-plugin-dialog` 的非阻塞 `.show(回调)`，不是 `blocking_show()`：那个方法文档明确写了不能在主线程调用，之前手写 AppKit 弹窗时在主线程同步等过一次，直接死锁过，这次注意避开了。
- **没有 Dock 菜单、Dock 图标切换和多语言**：这些仍是 Swift 版拥有而 Tauri 版没有的平台产品能力；登录启动已经通过跨平台 autostart 插件实现，但 macOS 底层是 LaunchAgent，不承诺与 Swift 的 `SMAppService` 设置界面完全一致。
- **About 不包含 Swift 版的 bundled runtime receipt**：本项目有意不在应用内管理 runtime；Node.js 和 pnpm 版本会从配置的 Node 路径或登录 shell 的 `PATH` 中读取。

### 启动配置

默认配置等价于 Swift 版的 `launcher` profile；首次使用自定义 profile 时会追加
`--from-default-profile web`。例如：

```json
{
  "port": 31080,
  "profile": "launcher",
  "dshHome": "~/.dsh",
  "extraArgs": ["--trusted-host", "my-host.local"],
  "environment": { "HTTPS_PROXY": "http://127.0.0.1:7890" },
  "runtime": { "node": "/opt/homebrew/bin/node", "entry": "/opt/dsh/dsh.js" }
}
```

启动时会读取登录 shell 的环境作为基础，再叠加配置里的 `environment`；设置
`DSH_LAUNCHER_NO_OPEN=1` 可保留 Swift 版开发模式的静默启动行为。深链接动作示例：
`open 'dsh-launcher://restart'`。

## 开发

前置：[mise](https://mise.jdx.dev)；网络（首次要下 Rust crates 和 npm 包；配了 `HTTP(S)_PROXY`/`NO_PROXY` 会自动走代理）。

```bash
mise install       # 按 mise.toml 装好 Rust / Node.js / pnpm
mise run dev        # 开发模式（pnpm tauri dev，热重载）
mise run test        # cargo test
mise run lint        # cargo fmt --check + clippy
mise run build        # 出正式包：.app / .dmg（Windows 是 .msi/.exe，Linux 是 .deb/.rpm/.AppImage，但都没有实际编过）
mise run clean        # 清掉 node_modules / target / gen
```

`mise run build` 用的是官方 `tauri build`，产物在 `src-tauri/target/release/bundle/`。macOS 下是 ad-hoc 签名（没有 Apple 开发者证书就是这样），首次打开需要在「系统设置 → 隐私与安全性」里允许。

## 代码结构

```
src/                    静态占位页面——没有窗口，这个页面平时不会被打开
src-tauri/
  src/
    main.rs             入口
    lib.rs              Builder 装配：插件、深链接、托盘、退出清理
    tray.rs             托盘图标 + 菜单动作 + 退出确认框
    config.rs           启动配置、profile、运行时覆盖、shell 环境和启动参数
    dsh.rs              spawn dsh、看门狗、端口探测、浏览器打开、日志和进程树清理
    logging.rs            launcher/dsh 持久化日志与 token 脱敏
    process_tree.rs      Unix 进程组 / Windows Job Object 的平台封装
    ready_line.rs         解析 `dsh web: http://…/?token=…` 这一行
  icons/                 沿用自 Swift 版的图标；layers/ 是设计源文件，AppIcon.png 是拿来跑
                          `tauri icon` 重新生成整套图标用的 1024px 源图
  capabilities/          Tauri 权限声明（当前没有窗口，基本用不上）
```

## 许可证

MIT，见 [LICENSE](LICENSE)。
