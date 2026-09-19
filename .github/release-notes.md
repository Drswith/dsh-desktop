## 下载

| 机型 | 安装包 |
|---|---|
| Apple 芯片（M 系列） | `DSH-Launcher-{{VERSION}}-arm64.dmg` |
| Intel 芯片 | `DSH-Launcher-{{VERSION}}-x86_64.dmg` |

打开 DMG，把 DSH Launcher 拖进“应用程序”。

这个版本没有经过 Apple 公证，第一次打开会被 macOS 拦下。关掉提示后，前往“系统设置 › 隐私与安全性”，在安全性部分点“仍要打开”；也可以在终端执行：

```bash
xattr -dr com.apple.quarantine "/Applications/DSH Launcher.app"
```

内置 DSH {{DSH_VERSION}}。`SHA256SUMS.txt` 可用于校验下载的文件。
