# DSH Launcher

[![CI](https://github.com/Drswith/dsh-launcher/actions/workflows/ci.yml/badge.svg)](https://github.com/Drswith/dsh-launcher/actions/workflows/ci.yml)

DSH 的托盘启动器：[Tauri 2](https://tauri.app) 写的小外壳，启动本地 `dsh web` 服务，就绪后从托盘菜单在默认浏览器里打开它。

> 运行 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（npm 上的 `@deepseek-ai/dsh`），非 DeepSeek 官方产品。

## 这个版本做什么、不做什么

范围刻意很小，只有这几件事：

1. 启动时先看一眼上次有没有遗留的孤儿 `dsh` 进程，有就先清掉，再执行 `dsh web --no-open --host 127.0.0.1 --port 31080`；
2. 常驻一个托盘图标和菜单；
3. 从子进程 stdout 里认出 `dsh web: http://127.0.0.1:31080/?token=…` 这行就绪信号后，菜单项从禁用的「启动中…」变成可点的「打开 DSH」，点击后用系统默认浏览器打开这个带 token 的地址；
4. 点「退出」会先弹一个确认框，确认后才真的停掉 `dsh` 子进程、退出外壳。

**用的都是 Tauri 官方 API**（`tauri::tray`/`tauri::menu` 建托盘和菜单，`tauri-plugin-opener` 开浏览器，`tauri-plugin-dialog` 弹退出确认框，`tauri-plugin-single-instance` 防止开两份；进程树管理在 Unix 上用独立进程组，在 Windows 上用 Job Object，孤儿记录检测用了 [`sysinfo`](https://crates.io/crates/sysinfo)），没有直接调用任何 macOS-only 的原生 API，理论上能跨平台编译——CI 会在 macOS / Windows / Linux 上各跑一次 `cargo check` 确认这件事，但**只在 macOS 上真正跑过、点过**。

本项目之前有一版完整搬过 Swift 版能力的实现（运行时安装/校验/原子升级、崩溃看门狗、登录启动、Dock 右键菜单、多语言、About 窗口……大量直接调用 AppKit 的 Rust 代码），复杂度和"托盘 + 启动一个进程"这个目标不成比例，已经推倒重来。历史实现留在 git 历史里（`wip: objc2 直写 AppKit 的 Tauri 重构` 那次提交之前），以后要按需加什么功能可以回去参考，但不建议整体恢复。

### 已知限制（都是有意暂时不做，不是漏掉了）

- **不装运行时**：`dsh` 要能在 `PATH` 里找到。找不到时托盘菜单会显示「未找到 dsh 命令」，不会崩溃。
- **端口写死 31080，不重试**：被占用时 `dsh web` 启动会失败，目前没有退避或换端口逻辑。
- **不看门狗**：`dsh` 进程崩溃或退出后不会自动重启。
- **进程树清理有平台边界**：macOS/Linux 启动 `dsh` 时会先用 `setpgid(0, 0)` 建立独立进程组，退出时先向整个组发 `SIGTERM`，等待 3 秒后仍存在才发 `SIGKILL`；Windows 使用 Job Object 管理普通子进程。普通 shell、脚本和 worker 会继承这个边界，但主动调用 `setsid`、daemonize 或创建新进程组的程序可能逃逸。
- **孤儿清理只在下次启动时发生**：外壳被外部信号杀掉（`kill`、系统注销/关机、Activity Monitor 强制退出）时，Unix 上的 `dsh` 进程组会暂时继续存在；下次启动会读取 `<应用数据目录>/dsh.pid` 中的 pid、端口、进程组 ID 和启动时间，确认仍然是原来的 `dsh` 后清理整个进程组。应用如果之后一直不重开，孤儿会一直运行；Windows 成功加入 Job Object 时，Job 句柄关闭会自动清理整组。
- **退出确认框同一时间只能有一个**：连点几下托盘「退出」不会堆出好几个确认框——这是手工测试时真堆出来过之后加的保护（一个 `AtomicBool` 标志位），不是预防性写的。弹窗用的是 `tauri-plugin-dialog` 的非阻塞 `.show(回调)`，不是 `blocking_show()`：那个方法文档明确写了不能在主线程调用，之前手写 AppKit 弹窗时在主线程同步等过一次，直接死锁过，这次注意避开了。
- **没有登录启动、没有 Dock 菜单、没有多语言、没有 About 窗口**：都是上面提到的旧实现里有、这版暂时没有的东西。

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
    lib.rs              Builder 装配：插件、托盘、退出清理
    tray.rs             托盘图标 + 菜单 + 菜单事件 + 退出确认框
    dsh.rs               spawn `dsh web`、记录和清理进程树、后台线程读 stdout
    process_tree.rs      Unix 进程组 / Windows Job Object 的平台封装
    ready_line.rs         解析 `dsh web: http://…/?token=…` 这一行
  icons/                 沿用自 Swift 版的图标；layers/ 是设计源文件，AppIcon.png 是拿来跑
                          `tauri icon` 重新生成整套图标用的 1024px 源图
  capabilities/          Tauri 权限声明（当前没有窗口，基本用不上）
```

## 许可证

MIT，见 [LICENSE](LICENSE)。
