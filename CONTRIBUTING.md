# 贡献指南

欢迎通过 [Issues](https://github.com/JinBorn/i-love-img/issues) 报告问题、讨论功能，或提交 Pull Request。先阅读 [使用指南](docs/USER_GUIDE.md) 和 [架构说明](docs/ARCHITECTURE.md)，了解独立工具、输入来源与资源约束。

## 开发环境

按 [中文构建说明](docs/BUILD.md) / [English build guide](docs/BUILD.en.md) 安装平台依赖。仓库通过 `rust-toolchain.toml` 固定 Rust 1.99.0，使用 `Cargo.lock` 锁定依赖。首次下载工具链与依赖需要网络，图片处理本身不需要网络。

```sh
git clone https://github.com/JinBorn/i-love-img.git
cd i-love-img
cargo run --locked --bin local-image-tools
```

核心处理位于 `crates/image-core`，桌面界面位于 `crates/desktop`，CLI 位于 `crates/cli`。尽量复用核心，不在 GUI 线程中解码图片。新增工具应有独立参数和会话，并复用预算调度和有界预览服务。

## 提交前检查

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
```

保留已有自动化回归测试。涉及输出内容、输入来源、取消、元数据或路径保护时，补充能验证行为的测试；测试图片尽量在临时目录生成。界面改动还需检查中英文、较小窗口和相关导航流程。编译成功不能替代实际 GUI 检查。

可选 Python 核验与性能脚本使用以下依赖：

```sh
python -m venv .venv
# 激活虚拟环境后：
python -m pip install -r requirements-dev.txt
```

具体调用和测量条件见 [验证记录](docs/VALIDATION.md)。性能测量使用 release 构建，并与编译、独立输出核验分开执行。报告版本、系统、硬件、输入组成、耗时和内存采样口径；不要把合成样本、历史版本或单机测量扩展为所有输入的保证。

## 仓库内容

提交源码、自动化测试、必要资源、许可和文档。不提交 `target/`、`.tools/`、`dist/`、`artifacts/`、虚拟环境、生成的测试图片或截图，也不提交用户 `Data/`、`Output/`、私人照片和凭据。需要复现问题时优先提供可公开的最小样本或生成脚本。

第三方依赖或资源变更需同步检查许可与分发告知。涉及新选项、格式限制、便携行为或操作流程时，更新相应使用文档和 [更新记录](CHANGELOG.md)。构建与打包规则变更需同时考虑 Windows、macOS 和 Linux，不以本机通过代替其他平台验证。

## 问题与 Pull Request

问题报告请包含应用版本、操作系统、使用步骤、预期与实际结果，以及可公开的图片格式、尺寸或错误信息。图片可能包含个人信息或拍摄元数据，请自行确认样本可以公开分享。

Pull Request 说明解决的问题、最终行为与实际执行的验证。未执行的平台或 GUI 场景直接注明。保持改动聚焦，避免将生成包和本机诊断日志混入源码提交。贡献的项目代码按本仓库 [MIT License](LICENSE) 分发，第三方组件仍遵循其自身许可。
