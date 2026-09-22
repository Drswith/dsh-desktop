# 启动窗口设计原型

这是启动进度窗口接入前确认的独立交互原型，完整保留其静态 HTML、CSS、JavaScript、
状态预览图和第三方许可说明，便于回看或再次讨论设计。

在浏览器中直接打开 [index.html](index.html)，或在此目录启动任意静态文件服务器后访问即可。
其中全部动作都是模拟：不会启动 DSH、不会调用 Tauri IPC，也不会读取或修改本机配置。

原型中的窗口边框、标题栏和红绿灯仅用于设计预览；实际应用继续采用系统原生窗口标题栏。
生产实现位于 [`src/`](../../../src/)，不会加载此目录，也不把它纳入 Tauri 打包资源。

`preview-window.png`、`preview-light.png`、`preview-dark.png` 和 `preview-narrow.png`
分别记录单窗口、浅色状态总览、深色状态总览和窄窗口效果。DSH 设计 token 的来源和 MIT
许可见 [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt)。
