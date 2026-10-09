# Embedded UI font

`NotoSansSC-UI.otf` is a subset of Noto Sans CJK SC Regular, licensed under
the SIL Open Font License 1.1 (`OFL.txt`). Copyright and naming records from
the upstream font are preserved inside the subset.

Source: https://github.com/notofonts/noto-cjk/blob/main/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf

The subset contains the application's text, common Simplified Chinese
characters (GB2312 level one), ASCII and common CJK punctuation. It is
embedded for offline operation on Windows, macOS and Linux. Uncommon
characters in filenames may display a fallback glyph; file paths and file
contents are preserved without alteration.

To reproduce it, install Python `fonttools`, download the source as
`NotoSansCJKsc-Regular.full.otf`, then run `python assets/fonts/subset_font.py`.
The large `.full.otf` source is excluded from distribution.
