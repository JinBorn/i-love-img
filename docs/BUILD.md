# 构建与打包

[项目首页](../README.md) · [English](BUILD.en.md) · [验证记录](VALIDATION.md)

仓库通过 `rust-toolchain.toml` 固定 Rust 1.99.0，使用 `Cargo.lock` 和 `--locked` 固定依赖解析。首次安装工具链、系统依赖和下载 crate 需要网络；已构建应用的图像处理不依赖网络。

## 环境准备

需要 Git、Rust/rustup、Python 3.10+、C/C++ 编译器、CMake 和 NASM。Python 用于打包与许可收集；应用运行不需要 Python。Pillow、psutil 仅供可选验证脚本使用，见 [验证说明](VALIDATION.md)。

| 平台 | 原生构建环境 | 打包目标 |
| --- | --- | --- |
| Windows x64 | Visual Studio 2022 Build Tools 的 Desktop development with C++、Windows SDK；Rust MSVC、CMake、NASM、Python | 便携 ZIP；可选 NSIS 安装器 |
| macOS | Xcode Command Line Tools、CMake、NASM、Python | macOS 13+，Intel / Apple Silicon 分别构建 APP ZIP / DMG |
| Ubuntu | 下方 apt 依赖、Rust、Python | Ubuntu 22.04+ x64 DEB |

Windows 建议在 Developer PowerShell 中构建，确认 `cmake`、`nasm`、`python` 可通过 PATH 调用。默认便携打包不需要 NSIS。

macOS：

```sh
xcode-select --install
brew install cmake nasm
export MACOSX_DEPLOYMENT_TARGET=13.0
```

Ubuntu 22.04：

```sh
sudo apt-get update
sudo apt-get install -y build-essential cmake nasm pkg-config python3 libclang-dev \
  libgl1-mesa-dev libx11-dev libxrandr-dev libxi-dev libxcursor-dev libxinerama-dev \
  libxkbcommon-dev libwayland-dev libgtk-3-dev libssl-dev dpkg-dev
```

获取源码并准备工具链：

```sh
git clone https://github.com/JinBorn/i-love-img.git
cd i-love-img
rustup toolchain install 1.99.0 --profile minimal --component rustfmt --component clippy
```

仓库中的工具链文件会自动选择该版本，不需要修改系统默认版本。`RUSTUP_TOOLCHAIN` 等已有环境变量可能覆盖此选择；排查时先查看 `rustup show`。

## 检查与运行

在仓库根目录执行：

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run --locked --bin local-image-tools
cargo run --locked --bin lit -- --help
cargo build --locked --release --workspace
```

默认输出位于 `target/release/`；指定 `--target` 后为 `target/<target>/release/`，Windows 程序扩展名为 `.exe`。内存和速度测量使用 release 构建。测试生成临时图片，测试源码保留在 crate 中；无需下载测试图片集。

## Windows 便携包

```powershell
rustup target add x86_64-pc-windows-msvc
./scripts/package.ps1
```

脚本默认执行锁定的 release 构建，设置静态 C 运行时参数 `-C target-feature=+crt-static`，收集资源、文档和许可，生成 `dist/local-image-tools-<version>-windows-x64-portable.zip`、逐文件 JSON 清单与 SHA-256 校验和。

完整解压后运行 `Local Image Tools/local-image-tools.exe`。包内还有 CLI `lit.exe`、`portable.flag` 和快速使用说明，设置与默认输出位于程序旁。无需在目标机器安装 Rust、编译器或运行库；交付前仍应在没有开发环境的目标 Windows 上验证。使用说明见 [便携版](PORTABLE.md)。

可选安装器：

```powershell
./packaging/windows/fetch-nsis.ps1
./scripts/package.ps1 -Installer -OutputDirectory dist-with-installer
```

下载脚本从官方源获取固定的 NSIS 3.11 并核对 SHA-256，解压到 `.tools/`。也可用 `-NsisDirectory` 指定已有完整安装。安装器为当前用户安装，使用用户配置目录，载荷不带 `portable.flag`；卸载清单仅包含打包文件。默认交付仍为便携 ZIP。

`-SkipBuild` 仅用于相同目标、相同构建参数的已有 release。脚本拒绝覆盖同名包或清单，重复打包时用 `-OutputDirectory` 选择新目录。旧 `-NoInstaller` 参数保留兼容，不能与 `-Installer` 同时使用。

### 可选 GNU LLVM 路线

已验证的本地 Windows 路线使用 Rust 1.99.0 / `x86_64-pc-windows-gnullvm` 与 LLVM MinGW UCRT。采用此路线需自行安装对应 Rust host/target、LLVM MinGW、CMake、Ninja、NASM，并配置 C/C++ 编译器、链接器及 `LIBCLANG_PATH`。

`scripts/dev-env.ps1` 只激活按其固定目录布局准备好的 `.tools/` 环境，不负责下载安装；`.tools/` 不入库。普通源码使用者优先采用上述 MSVC 路线。已有本地环境可执行：

```powershell
. ./scripts/dev-env.ps1
./scripts/package.ps1 -Target x86_64-pc-windows-gnullvm
```

使用其他 LLVM MinGW 安装位置时，打包参数 `-RuntimeDirectory` 指向其 `bin` 目录，父目录必须含原始 `LICENSE.TXT`。GNU LLVM 包名带 `gnullvm`，随包提供 `libunwind.dll` 及许可；它不代表 MSVC 产物已验证。

## macOS 与 Linux

在相应平台与架构的构建机上执行。macOS 应用内包含 GUI 与 CLI，不提供通用双架构二进制。

```sh
# macOS Intel
rustup target add x86_64-apple-darwin
bash scripts/package.sh --target x86_64-apple-darwin

# macOS Apple Silicon
rustup target add aarch64-apple-darwin
bash scripts/package.sh --target aarch64-apple-darwin

# Ubuntu x64
rustup target add x86_64-unknown-linux-gnu
bash scripts/package.sh --target x86_64-unknown-linux-gnu
```

macOS 生成 `.app` ZIP 和 DMG，部署目标为 13.0；Linux 生成 DEB 和校验和。Linux 资源位于 `/usr/share/local-image-tools/assets`，说明与许可位于 `/usr/share/doc/local-image-tools/`。文件对话框需要可用的桌面 portal；DEB 推荐 `xdg-desktop-portal` 与 `xdg-desktop-portal-gtk`。当前不生成 AppImage。

Shell 脚本支持 `--skip-build` 和 `--output <directory>`。打包不执行 Developer ID 签名、公证或发布；Apple Silicon 链接器生成的临时签名不等同于正式签名或公证。

## 源码包、许可与离线构建

```sh
python scripts/package-source.py --output dist
```

源码包按白名单包含源码、文档、配置、必要资源和许可证，附逐文件 SHA-256 清单；排除工具链、编译缓存、测试生成物、用户设置和完整字体源文件。已存在同名包时改用新的输出目录。

应用打包时，`packaging/collect-licenses.py` 按锁定依赖收集原始许可。缺失的上游文本从 `packaging/license-overrides/` 的已记录副本补齐，收集过程不联网下载许可。升级依赖时需同步核对这些映射与来源哈希。

提前缓存工具链、原生依赖及目标 crate 后，可离线构建：

```sh
cargo fetch --locked --target x86_64-pc-windows-msvc
# 将目标替换为实际平台；缓存完成后：
cargo build --offline --locked --release --workspace
```

打包时用 `CARGO_NET_OFFLINE=true` 限制 Cargo 联网。源码 ZIP 不包含依赖缓存或编译器。锁定版本用于重现构建流程；系统包、runner 和包内时间戳仍可能变化，当前不承诺逐字节相同的产物。

## GitHub Actions 与验证

[构建工作流](https://github.com/JinBorn/i-love-img/blob/main/.github/workflows/build.yml) 为 Windows MSVC x64、macOS Intel、macOS Apple Silicon 和 Ubuntu x64 配置格式检查、Clippy、测试、release 构建、CLI 冒烟检查和打包。Windows 默认只生成便携包，手动触发时可勾选 `windows_installer`。

成功作业上传 CI 附件，保留 14 天；工作流不创建 GitHub Release。状态以 [运行记录](https://github.com/JinBorn/i-love-img/actions/workflows/build.yml) 为准。首次提交前已完成 Windows GNU LLVM 本机验证，其他目标的配置不等于实机验证，详见 [验证与性能记录](VALIDATION.md)。
