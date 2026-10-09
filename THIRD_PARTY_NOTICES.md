# Third-party notices / 第三方告知

Local Image Tools project code is licensed under MIT; see `LICENSE`. Third-party components retain their own copyright and license terms. This file identifies major components and where to find the complete notices included in a distribution. It does not replace those license texts.

本项目代码使用 MIT 许可证。第三方组件沿用各自的版权与许可；本文件用于说明来源，完整许可原文随分发包附带。

## Rust crates and native image codecs

Each package includes `dependency-licenses/inventory.json`, generated from the exact `Cargo.lock` graph for its target by `packaging/collect-licenses.py`. The inventory gives crate versions, declared license expressions, upstream links and copied notice paths. `dependency-licenses/<crate>-<version>/` contains original license/notice files, including notices found in vendored native source. Where a published crate omits its license files, reviewed copies from its exact upstream commit are included under `upstream/`, with their source URLs and SHA-256 values in `package.json`. These supplements are stored in `packaging/license-overrides/` in the source repository, so packaging does not download notices. Build/development dependencies in the resolved graph may also be listed; their presence does not imply they are all linked into the application.

Major components include:

| Component | Purpose | Upstream |
| --- | --- | --- |
| egui / eframe / winit | Native desktop interface, windowing and rendering | https://github.com/emilk/egui / https://github.com/rust-windowing/winit |
| image / png / gif | Image identification, pixel buffers and format processing | https://github.com/image-rs |
| fast_image_resize | Pixel resampling | https://github.com/Cykooz/fast_image_resize |
| oxipng | Lossless PNG optimization | https://github.com/shssoichiro/oxipng |
| mozjpeg / mozjpeg-sys / MozJPEG | JPEG encoding and coefficient-domain transformations | https://github.com/kornelski/mozjpeg-rust / https://github.com/mozilla/mozjpeg |
| webp / libwebp-sys / libwebp | WebP encoding | https://github.com/jaredforth/webp / https://chromium.googlesource.com/webm/libwebp |
| moxcms | ICC color management | https://github.com/awxkee/moxcms |
| rfd | Native file and folder dialogs | https://github.com/PolyMeilex/rfd |
| serde / clap | Settings serialization and command-line parsing | https://github.com/serde-rs/serde / https://github.com/clap-rs/clap |

Mozilla JPEG builds upon the Independent JPEG Group and libjpeg-turbo work. The corresponding upstream notices and README license material are retained under the codec dependency directories; the crate's license expression alone is not a substitute for native codec notices.

## Chinese UI font

`assets/fonts/NotoSansSC-UI.otf` is a modified subset of Noto Sans CJK SC Regular, prepared for this application's UI. The source font is from https://github.com/notofonts/noto-cjk. Its font name-table copyright notice is:

> © 2014-2021 Adobe (http://www.adobe.com/).

The font is licensed under the SIL Open Font License, Version 1.1. The complete license is included as `assets/fonts/OFL.txt`. The UI font remains under that license. The full upstream font is excluded from the repository and application packages; download instructions for regenerating the subset are in `assets/fonts/README.md`. The subset is not sold separately. eframe's bundled default fonts have their own notices under `dependency-licenses/epaint_default_fonts-<version>/`.

## Windows GNU LLVM runtime

Windows packages whose filename includes `gnullvm` include `libunwind.dll` from the LLVM MinGW UCRT x64 distribution. The local build uses LLVM MinGW release `20261006`, obtained from https://github.com/mstorsjo/llvm-mingw/releases/tag/20261006. LLVM libunwind uses Apache License 2.0 with LLVM exceptions. The distribution's complete, unmodified `LICENSE.TXT`, including legacy notices, is included under `dependency-licenses/llvm-runtime/` alongside a SHA-256/source record for the copied DLL. MSVC packages do not include this DLL.

## Windows installer

Windows installers are created with NSIS. NSIS copyright: Copyright (C) 1999-2025 Contributors. The installer uses the zlib compression module. The complete NSIS `COPYING` file is included under `dependency-licenses/nsis/`; it also describes optional NSIS compression modules that this installer does not select. Upstream: https://nsis.sourceforge.io/ . The local portable build tool is NSIS 3.11; its original source and binary archives are available at https://sourceforge.net/projects/nsis/files/NSIS%203/3.11/ .

## Operating system libraries

System graphics, windowing, C runtimes and other operating system libraries retain their suppliers' licenses. Linux DEB packages depend on the system packages named in their control metadata instead of embedding those libraries. Apple frameworks and Windows system DLLs are provided by their respective operating systems.
