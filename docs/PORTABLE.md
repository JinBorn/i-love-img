# Windows U 盘便携使用

适用于 0.2.1。[项目首页](../README.md) · [工具使用指南](USER_GUIDE.md) · [构建说明](BUILD.md)

## 启动

完整解压 Windows 便携 ZIP，把整个 `Local Image Tools` 文件夹放到 U 盘或其他可写目录，双击 `local-image-tools.exe`。不要从 ZIP 内直接运行，也不要只复制 EXE。

保留 `portable.flag`、资源、许可文件及附带的运行库。GNU LLVM 包名带 `gnullvm`，还需要同目录的 `libunwind.dll`。目标电脑无需安装 Rust、编译器或运行库，核心图像处理无需联网、账户或管理员权限。

在首页选择压缩图片、调整尺寸或转换格式，再导入图片。工具有各自的参数和队列；继续调整会明确选择下一工具，使用成功输出作为输入，等待用户开始。

## 设置和输出

`portable.flag` 启用便携模式，设置与默认输出随整个文件夹移动：

```text
Local Image Tools/
  local-image-tools.exe
  lit.exe
  portable.flag
  assets/
  dependency-licenses/
  Data/settings.json       首次成功保存设置后创建
  Output/                  默认图片输出位置
```

| 内容 | 行为 |
| --- | --- |
| 语言与工具参数 | 保存到程序旁的 `Data/settings.json`，各工具分别保存处理偏好 |
| 队列、选择与结果列表 | 仅保留在当前会话中，退出后不恢复；输出图片仍在磁盘 |
| 默认输出 | 程序旁的 `Output`，与从哪个工作目录启动无关 |
| 程序目录内的输出位置 | 按相对路径保存，跟随整个文件夹及盘符变化 |
| 其他绝对输出位置 | 保留绝对含义；重启时发现位置不可用则提示并恢复为 `Output` |
| GUI 中填写的相对输出位置 | 以程序目录为基准 |
| CLI 相对路径 | 以命令的当前工作目录为基准 |

输入与输出目录应分开，原图保留，同名输出自动编号。程序文件夹必须可写才能保存便携设置和默认输出；只读位置可另选可写的图片输出位置，但不会自动把设置改存到电脑的用户配置目录。

设置读取失败时会提示并临时使用默认参数，不会自动覆盖损坏配置。保存失败会保留旧配置并说明原因，可解决问题后重试，或按界面提示选择不保存退出。

## 移动和升级

先关闭程序，再复制或移动整个文件夹，完成后安全弹出 U 盘。只复制程序本身不会带走 `Data` 中的偏好；程序目录之外的输出也不会跟随移动。

升级时使用新文件夹并保留旧版备份。关闭旧版后，将旧 `Data` 文件夹复制到新版目录即可迁移偏好。0.2.1 沿用 0.2.0 的设置结构；0.1.x 的统一参数会按工具拆分，在成功保存后才写入新结构。不要把新版 `Data` 反向覆盖给 0.1.x 程序。

没有 `portable.flag` 的普通安装模式使用用户配置目录。可选安装版与便携版的设置位置不同，安装方式见 [构建说明](BUILD.md)。Windows 和显卡驱动仍可能产生自己的缓存或使用记录。

## 验证范围

Windows GNU LLVM 便携应用已完成本机运行、包内容与运行库检查。历史验证覆盖设置保存/重载、文件夹移动、临时盘符切换、文件锁保存失败重试及损坏配置保护。临时盘符在同一台机器上模拟，未做物理 U 盘、全新系统、拔盘或断电测试。当前内存测量和具体边界见 [验证记录](VALIDATION.md)。

## English

Extract the complete Windows portable ZIP to a writable folder or USB drive and run `local-image-tools.exe`. Keep `portable.flag`, assets, notices and any bundled runtime DLLs together. Preferences use `Data/settings.json`; output defaults to `Output` beside the executable. Internal output paths follow folder moves and drive-letter changes; CLI paths use the command's working directory.

Close the application before moving the folder or ejecting the drive. Upgrade into a separate folder and copy the old `Data` after closing the old app. Version 0.2.1 retains the 0.2.0 settings structure; do not copy upgraded settings back to 0.1.x. Read/write errors are reported without silently moving preferences to the host profile. Queues are not restored after exit. See the [English build guide](BUILD.en.md) for packaging and the [validation record](VALIDATION.md) for measured scope.
