"""Rebuild the embedded CJK subset. Requires fonttools (development only).

Download the original NotoSansCJKsc-Regular.otf next to this script as
NotoSansCJKsc-Regular.full.otf; the original is never included in the app.
"""
from pathlib import Path
from fontTools import subset

directory = Path(__file__).resolve().parent
root = directory.parent.parent
options = subset.Options()
options.name_IDs = ["*"]
options.layout_features = ["*"]
font = subset.load_font(str(directory / "NotoSansCJKsc-Regular.full.otf"), options)
characters = set(range(32, 127))
characters.update(range(0x3000, 0x3040))
characters.update(range(0xFF00, 0xFFEF))
for first in range(0xB0, 0xD8):
    for second in range(0xA1, 0xFF):
        characters.update(map(ord, bytes([first, second]).decode("gb2312", errors="ignore")))
for source in (root / "crates" / "desktop" / "src").glob("*.rs"):
    characters.update(map(ord, source.read_text(encoding="utf-8")))
subsetter = subset.Subsetter(options=options)
subsetter.populate(unicodes=characters)
subsetter.subset(font)
subset.save_font(font, str(directory / "NotoSansSC-UI.otf"), options)
print(f"Embedded subset: {(directory / 'NotoSansSC-UI.otf').stat().st_size:,} bytes")
