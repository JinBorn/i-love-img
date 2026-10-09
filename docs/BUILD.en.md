# Build and package

[Project home](../README.md) · [中文](BUILD.md) · [Validation](VALIDATION.md)

The repository pins Rust 1.99.0 in `rust-toolchain.toml` and uses `Cargo.lock` with `--locked`. Installing tools and fetching dependencies initially requires a network connection. Image processing in a built application works offline.

## Prerequisites

Install Git, Rust/rustup, Python 3.10+, a C/C++ compiler, CMake and NASM. Python supports packaging and license collection; the application itself does not need it. Optional verification scripts use the packages in `requirements-dev.txt`.

| Platform | Native dependencies | Package |
| --- | --- | --- |
| Windows x64 | Visual Studio 2022 Build Tools with Desktop development with C++, Windows SDK, Rust MSVC, CMake, NASM, Python | Portable ZIP; optional per-user NSIS installer |
| macOS | Xcode Command Line Tools, CMake, NASM, Python | macOS 13+ APP ZIP / DMG, built separately for Intel and Apple Silicon |
| Ubuntu | Packages below, Rust, Python | Ubuntu 22.04+ x64 DEB |

On Windows, use Developer PowerShell and ensure `cmake`, `nasm` and `python` are on PATH. The default portable build does not need NSIS.

macOS:

```sh
xcode-select --install
brew install cmake nasm
export MACOSX_DEPLOYMENT_TARGET=13.0
```

Ubuntu 22.04:

```sh
sudo apt-get update
sudo apt-get install -y build-essential cmake nasm pkg-config python3 libclang-dev \
  libgl1-mesa-dev libx11-dev libxrandr-dev libxi-dev libxcursor-dev libxinerama-dev \
  libxkbcommon-dev libwayland-dev libgtk-3-dev libssl-dev dpkg-dev
```

```sh
git clone https://github.com/JinBorn/i-love-img.git
cd i-love-img
rustup toolchain install 1.99.0 --profile minimal --component rustfmt --component clippy
```

The checked-in toolchain file selects the version without changing your global default. Existing environment variables such as `RUSTUP_TOOLCHAIN` can override it; inspect `rustup show` when diagnosing a mismatch.

## Check and run

```sh
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run --locked --bin local-image-tools
cargo run --locked --bin lit -- --help
cargo build --locked --release --workspace
```

Binaries are placed in `target/release/`, or `target/<target>/release/` when using `--target`; Windows executables use `.exe`. Measure performance with release builds. Rust tests generate their inputs in temporary directories, so no image dataset download is required.

## Windows

```powershell
rustup target add x86_64-pc-windows-msvc
./scripts/package.ps1
```

The script builds the locked release with `-C target-feature=+crt-static`, collects assets, documentation and notices, and creates `dist/local-image-tools-<version>-windows-x64-portable.zip`, a per-file JSON manifest and SHA-256 checksums.

Extract the complete `Local Image Tools` folder and launch `local-image-tools.exe`. Keep `lit.exe`, `portable.flag`, assets and notices together. Preferences use `Data/settings.json`; output defaults to `Output` beside the application. End users need no Rust, compiler or runtime installation. Validate the package on a Windows machine without development tools before distribution. See [portable usage](PORTABLE.md).

Optional installer:

```powershell
./packaging/windows/fetch-nsis.ps1
./scripts/package.ps1 -Installer -OutputDirectory dist-with-installer
```

The fetch script downloads pinned NSIS 3.11 from its official source, verifies SHA-256 and extracts it into `.tools/`. Alternatively pass `-NsisDirectory` for an existing full installation. The installer uses the current user's application/configuration directories and a separate payload without `portable.flag`. Its uninstall manifest lists packaged files only.

`-SkipBuild` requires an existing release built for the same target with matching flags. Packages and manifests are never overwritten; use a new `-OutputDirectory`. The legacy `-NoInstaller` switch remains compatible but cannot be combined with `-Installer`.

### Optional GNU LLVM route

The locally validated build uses Rust 1.99.0 / `x86_64-pc-windows-gnullvm` and LLVM MinGW UCRT. This route requires installing the matching Rust host/target, LLVM MinGW, CMake, Ninja and NASM, then configuring the compiler, linker and `LIBCLANG_PATH`.

`scripts/dev-env.ps1` only activates an existing `.tools/` installation with its documented directory layout; it does not download tools. `.tools/` is not in Git. Prefer the MSVC instructions for a normal fresh checkout. With that local layout already prepared:

```powershell
. ./scripts/dev-env.ps1
./scripts/package.ps1 -Target x86_64-pc-windows-gnullvm
```

For another LLVM MinGW installation, pass its `bin` directory using `-RuntimeDirectory`; its parent must contain the original `LICENSE.TXT`. GNU LLVM packages have `gnullvm` in their names and include `libunwind.dll` and its notices. Their validation does not establish MSVC compatibility.

## macOS and Linux

Run on the corresponding native host and architecture:

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

macOS packages contain the GUI and CLI in `.app` ZIP and DMG files, with a 13.0 deployment target. Intel and Apple Silicon packages are separate. Linux produces a DEB; assets install under `/usr/share/local-image-tools/assets`, documentation under `/usr/share/doc/local-image-tools/`. Native file dialogs need a working desktop portal; the DEB recommends `xdg-desktop-portal` and `xdg-desktop-portal-gtk`. AppImage is not supplied.

The shell script supports `--skip-build` and `--output <directory>`. Scripts do not perform Developer ID signing, notarization or publication. An Apple Silicon linker's ad-hoc signature is not developer signing or notarization.

## Source archive, notices and offline builds

```sh
python scripts/package-source.py --output dist
```

The source archive includes maintained source, tests, documentation, configuration, assets and licenses, with per-file SHA-256 records. It excludes local tools, build caches, generated verification data, user settings and full source fonts. Existing archives require a new output directory.

Application packaging collects notices from the locked dependency graph. Missing upstream license texts are supplied from reviewed copies in `packaging/license-overrides/`; collection does not download notices. Review mappings and source hashes when changing dependencies.

Cache the toolchain, native packages and crates before offline builds:

```sh
cargo fetch --locked --target x86_64-pc-windows-msvc
# Replace the target with your platform; once cached:
cargo build --offline --locked --release --workspace
```

Use `CARGO_NET_OFFLINE=true` when packaging offline. Source archives do not bundle compiler or crate caches. Version pinning reproduces the process; changing system packages, runners and timestamps mean byte-identical output is not guaranteed.

## CI and platform status

The [workflow](https://github.com/JinBorn/i-love-img/blob/main/.github/workflows/build.yml) configures formatting, Clippy, tests, release builds, CLI smoke checks and packaging for Windows MSVC x64, macOS Intel, macOS Apple Silicon and Ubuntu x64. Windows builds portable ZIPs by default; manual runs may select `windows_installer`.

Successful jobs upload artifacts with 14-day retention and do not create a GitHub Release. Refer to [workflow runs](https://github.com/JinBorn/i-love-img/actions/workflows/build.yml) for actual status. Before the first repository commit, local validation covered Windows GNU LLVM only; scripts and CI configuration do not establish runtime validation on other platforms. Conditions and measurements are in [VALIDATION.md](VALIDATION.md).
