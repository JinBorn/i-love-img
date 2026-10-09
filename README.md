# 本地图像工具 · Local Image Tools

用 Rust 编写的原生桌面图片工具，支持批量压缩、调整像素尺寸和转换格式。图片始终在本机处理，无需账户或联网；Windows 便携版可放在 U 盘中使用。

界面参考 [iLoveIMG](https://www.iloveimg.com/) 的独立工具流程：先选择功能，再导入图片和设置参数。每个工具拥有自己的文件列表、参数与结果，单独压缩时无需修改其他功能的配置。本项目是独立实现，与 iLoveIMG 无关联。

当前版本：**0.2.1** · [使用指南](docs/USER_GUIDE.md) · [Windows 便携使用](docs/PORTABLE.md) · [构建说明](docs/BUILD.md) · [English build guide](docs/BUILD.en.md)

## 功能

| 工具 | 操作 | 保持不变 |
| --- | --- | --- |
| 压缩图片 | JPEG / WebP 质量控制、无损优化、元数据选项 | 输入格式、像素尺寸、GIF 动画 |
| 调整尺寸 | 按宽高边界、最长边或百分比缩放；默认保持比例且不放大 | 输入格式、GIF 动画 |
| 转换格式 | 输出 JPEG、PNG、静态 WebP 或 GIF，提供对应编码和透明背景选项 | 输入像素尺寸 |

- 批量导入文件、文件夹或拖放图片，支持递归子目录与批量选择。
- 原图保留，输出沿用相对目录结构，同名文件自动编号，不覆盖已有文件。
- 中文 / English 界面、输入与输出预览、逐项状态和实际体积变化。
- “重新调整”从最初原图重新生成；“继续调整…”把成功输出传给所选工具，确认后再开始。
- 各工具分别保存参数；本次会话中的队列和结果可随工具切换保留。
- Rust 图像核心与 egui/eframe 界面，受内存预算约束的并发、有限预览缓存和逐帧 GIF 处理。

0.2.1 优化了滑杆、细进度条与预览布局，增加明确的“所有工具”返回入口，并修复继续调整准备期间的切换和取消行为。详见 [更新记录](CHANGELOG.md)。

## 开始使用

Windows 便携包完整解压后，运行文件夹中的 `local-image-tools.exe`。保留整个文件夹及 `portable.flag`、资源、许可文件；GNU LLVM 构建还需同目录的 `libunwind.dll`。无需安装 Rust、编译器或运行库。

1. 在首页选择“压缩图片”“调整尺寸”或“转换格式”。
2. 导入图片或文件夹，选中需要处理的项目。
3. 设置当前工具参数，选择与输入分开的输出目录，开始处理。
4. 查看结果，按需从原图重新调整，或把成功输出继续交给其他工具。

便携模式把设置写入程序旁的 `Data/settings.json`，默认输出到旁边的 `Output`。程序目录需可写；关闭应用后再移动整个文件夹或弹出 U 盘。队列和编辑历史不会在退出后恢复。升级与路径迁移见 [便携说明](docs/PORTABLE.md)。

本仓库保存源码，不包含编译后的应用包。可按下方说明本地构建；[GitHub Actions](https://github.com/JinBorn/i-love-img/actions/workflows/build.yml) 配置用于构建和上传 CI 附件，具体可用产物以成功运行记录为准。

## 格式与范围

输入支持 JPG/JPEG、PNG、静态 WebP、静态及动画 GIF。压缩或调整 GIF 尺寸保留动画内容、时长与循环；动画导出为静态图需要明确选择首帧。

压缩不保证每张图片都更小。PNG 默认无损，JPEG 压缩提供系数域无损优化，WebP 可选择无损编码。无损编码不代表缩放、色彩或格式转换本身没有信息损失。新建 GIF 最多 256 色且只支持二值透明；PNG 16 位转为低精度格式会损失精度。

当前不支持 HDR、APNG、动画 WebP、多页图、精确文件体积上限或 AI 去背景。默认删除拍摄等非显示必需元数据，可选择保留目标格式支持的字段。完整行为见 [使用指南](docs/USER_GUIDE.md)。

## 构建与命令行

先按 [构建说明](docs/BUILD.md) 安装平台原生依赖与 Rust；工具链固定在 `rust-toolchain.toml`，依赖固定在 `Cargo.lock`。

```sh
cargo run --locked --bin local-image-tools
cargo build --locked --release --workspace
cargo run --locked --bin lit -- --help
```

辅助命令行程序 `lit` 与桌面应用共用处理核心。在程序目录中运行，例如：

```powershell
./lit.exe inspect "D:/Photos/example.jpg"
./lit.exe process "D:/Photos" --recursive --output "D:/Photos-export" --long-edge 1920
./lit.exe process "D:/Photos/example.png" --output "D:/WebP-export" --format webp --webp-quality 80
```

macOS / Linux 使用 `./lit`，参数相同。CLI 可组合处理参数，相对路径按当前工作目录解析。批次部分失败时仍保留成功输出，并以非零退出码结束。

## 验证与平台状态

0.2.1 已在 Windows GNU LLVM 构建下通过 103 项自动化测试、格式检查和严格 Clippy，并完成原生 GUI、独立工具流程、输出解码与便携包检查。Windows 11 本机空白首页工作集采样峰值约 91.5 MiB，空工具页约 80.6–80.8 MiB。

资源目标为启动空闲 ≤100 MiB、处理峰值 ≤2048 MiB，优先控制内存。0.2.0 历史千张合成图片批次完成 1000/1000，耗时 38.935 秒，工作集采样峰值约 488.8 MiB；0.2.1 未重跑该负载。测量依赖图片、机器、窗口和驱动，不能视为所有输入的硬上限。

提供 Windows x64、macOS 13+ Intel / Apple Silicon 和 Ubuntu 22.04+ x64 的构建及打包脚本。首次仓库提交前仅完成 Windows GNU LLVM 本机运行验证；其他目标与远程 CI 的实际结果需要分别确认。详见 [验证范围与性能记录](docs/VALIDATION.md)。

## 项目文档

- [使用指南](docs/USER_GUIDE.md)：参数、输入来源、取消与格式限制。
- [Windows 便携使用](docs/PORTABLE.md)：U 盘、设置、输出路径与升级。
- [中文构建说明](docs/BUILD.md) / [English build guide](docs/BUILD.en.md)：依赖、平台打包和 CI。
- [架构与资源控制](docs/ARCHITECTURE.md)、[术语与约定](CONTEXT.md)、[验证记录](docs/VALIDATION.md)。
- [贡献指南](CONTRIBUTING.md)、[更新记录](CHANGELOG.md)、[第三方告知](THIRD_PARTY_NOTICES.md)。

自动化回归测试和可复用核验脚本随源码保留；生成的测试图片、日志、截图、工具链、用户设置和分发包不纳入 Git。

## English

Local Image Tools is an offline native Rust desktop app for batch image compression, resizing and conversion. Each tool has separate settings, queues and results. It supports JPEG, PNG, static WebP and static/animated GIF, preserves originals, and offers a Windows portable layout suitable for USB drives. The interface supports Chinese and English. See the [English build guide](docs/BUILD.en.md) for platform dependencies and packaging. Windows GNU LLVM has been tested locally; macOS/Linux runtime validation is still pending.

## 许可

项目代码采用 [MIT License](LICENSE)。第三方库、字体和其他资源保留各自许可，详见 [第三方告知](THIRD_PARTY_NOTICES.md)；打包脚本收集分发所需的依赖许可。
