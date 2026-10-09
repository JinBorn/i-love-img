# 验证与性能记录

[项目首页](../README.md) · [构建说明](BUILD.md) · [架构](ARCHITECTURE.md)

本文汇总发布前在 Windows 上完成的检查与测量。2026-10-10 整理仓库时删除了生成的测试图片、截图、日志和候选副本；原始测试产物不入库。以下历史结果保留版本、条件和适用范围，可使用仓库中的脚本重新生成新记录。

## 0.2.1 验证范围

Windows GNU LLVM release 版本完成了自动化测试、格式检查、严格 Clippy、原生 GUI 场景和分发包校验。2026-10-10 整理仓库时再次通过完整 103 项自动测试、格式检查和严格 Clippy（`-D warnings`）。自动化测试覆盖静态图像、GIF、批量输出保护、工具参数隔离、便携设置、准备取消、迟到回复及真实面板的指针/状态回归。

最终 GUI 在中文 1000×720 和英文 1220×820 窗口下运行检查；另完成 15 步独立工具流程、20 份输出的独立解码与设置保存/重载。检查包含质量与百分比滑杆、细进度条、预览适配、返回首页、切换工具和取消准备。指针回归通过 egui 注入输入，原生诊断调用应用动作；这些检查不等于操作系统鼠标输入延迟或所有拖放/原生对话框场景的验收。

便携包校验覆盖逐文件哈希、所需运行库和第三方许可。此前 0.2.0/0.1.1 的便携验证覆盖设置保存、文件夹搬移、临时盘符变化、文件锁保存失败重试与损坏配置保护。盘符迁移在同一台机器上模拟，不是物理 U 盘或断电测试。

## 0.2.1 空闲内存

环境：Windows 11 x64、Intel Core i5-14400F、32 GiB 内存、NVIDIA RTX 3050，Rust 1.99.0 / `x86_64-pc-windows-gnullvm` release。窗口正常可见，逻辑尺寸 1220×820，实际缩放 1.5。各页分别取启动后 3–8 秒的 50 个样本，早于第 9 秒截图；统计包含应用及其已观察子进程。

| 页面 | 工作集均值 / MiB | 工作集采样峰值 / MiB | Private Bytes 均值 / MiB | Private Bytes 采样峰值 / MiB |
| --- | ---: | ---: | ---: | ---: |
| 首页 | 91.359 | 91.461 | 96.288 | 96.902 |
| 空压缩页 | 80.595 | 80.598 | 96.643 | 96.918 |
| 空尺寸页 | 80.603 | 80.629 | 95.843 | 96.730 |
| 空转换页 | 80.779 | 80.781 | 97.363 | 97.473 |

上述场景均低于 100 MiB 的启动空闲目标。Private Bytes 是私有提交量，不是驻留内存。采样可能遗漏短暂峰值，不包含专用显存与观察器开销；这些结果不是硬上限或其他机器上的保证，也不是保留大量结果后的空闲测量。

## 历史批量与长动画测量

0.2.1 主要修改界面与导航，未重跑千张批次。以下批量数据属于 0.2.0；长动画数据属于 0.1.0，不能改记为当前版本结果。

0.2.0 千张样本包含 JPEG 244、PNG 253、WebP 253、GIF 250；10 张静态图片为 6000×4000，其余静态图片为 960×640；GIF 为 160×120、48 帧。合成样本用于资源与吞吐测量，不代表真实照片压缩率。默认压缩保持格式和尺寸，JPEG/WebP 质量分别为 85/80，PNG/GIF 使用无损优化，共享预算为 1536 MiB。

| 版本与场景 | 结果 | 耗时 | 进程树工作集采样峰值 | Private Bytes 采样峰值 |
| --- | --- | ---: | ---: | ---: |
| 0.2.0 GUI 千张压缩 | 1000/1000 成功，全部输出通过独立解码 | 38.935 s | 488.766 MiB | 519.137 MiB |
| 0.1.0 CLI 200 帧 GIF 缩放 | 640×480 → 320×240，时长与循环正确 | 1.461 s | 21.44 MiB | 11.10 MiB |
| 0.1.0 CLI 2000 帧 GIF 缩放 | 相同画布，时长与循环正确 | 14.909 s | 23.00 MiB | 11.70 MiB |

千张测量使用同一 Windows 主机，正常可见 1220×820 窗口、缩放 1.0；目标采样间隔 10 ms，实际平均 12.279 ms。样本生成、核验与截图等待不计入批次耗时，测量未与 Cargo 构建或其他图像负载并行。输出检查包含 253 个 PNG 的精确像素、250 个 GIF 的播放内容和原图哈希。

更早的 0.1.0 GUI 取消负载记录到 611.70 MiB 工作集峰值；保留 1000 行结果后为 100.16 MiB，略高于空白启动的 100 MiB 目标。因此启动空闲、带结果空闲与处理峰值应分别报告。

## 复现

先按 [构建说明](BUILD.md) 配置 Rust 与原生依赖，运行：

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Rust 测试自行生成临时输入并清理。可选 Python 核验依赖 Pillow；内存测量还需要 psutil，依赖固定在 `requirements-dev.txt` 中。建议在虚拟环境中运行 `python -m pip install -r requirements-dev.txt`，记录实际版本。下列 Windows 示例需先构建/解压一个没有用户设置的便携包，并把占位路径替换为实际目录；每次使用新的输出目录。

```powershell
python scripts/smoke-cli.py --exe "path/to/Local Image Tools/lit.exe" --output artifacts/cli-check
python scripts/verify-independent-tools.py --portable-dir "path/to/Local Image Tools" --artifacts artifacts/tools-check
python scripts/verify-portable.py --portable-dir "path/to/Local Image Tools" --artifacts artifacts/portable-check --with-subst
python scripts/measure-tools-ui.py --portable-dir "path/to/Local Image Tools" --artifacts artifacts/idle-measurement
```

`--with-subst` 需要两个未被占用的临时盘符，脚本核对归属后清理。测量脚本只管理自己启动的进程。独立核验与测量分开执行，避免核验器影响资源数据。`lit fixtures --help` 说明确定性负载生成；`measure-tools-ui.py --source` 可加入恰好 1000 个受支持文件的批次，`verify-benchmarks.py` 用于测量后校验。

## 平台覆盖

| 目标 | 当前已完成范围 |
| --- | --- |
| Windows 11 x64 / GNU LLVM | 本地自动化、release、CLI、原生 GUI、便携与内存测量 |
| Windows MSVC / Windows 10 | 提供构建和 CI 配置，首次仓库提交前未实测 |
| macOS 13+ Intel / Apple Silicon | 提供独立构建、测试、APP ZIP/DMG 脚本，首次提交前未实机运行 |
| Ubuntu 22.04+ x64 | 提供构建、测试和 DEB 脚本，首次提交前未实机运行 |

GitHub Actions 的结果以 [工作流运行记录](https://github.com/JinBorn/i-love-img/actions/workflows/build.yml) 为准。CI 成功不能代替各平台的 GUI、物理 U 盘、全新系统或端到端输入延迟验证。当前包未做开发者签名、公证或安装/卸载实测。
