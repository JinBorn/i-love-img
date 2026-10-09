"""Reproduce the MIT-licensed application artwork with Pillow (no downloaded art)."""

from pathlib import Path
from PIL import Image, ImageDraw


def main():
    root = Path(__file__).resolve().parents[1]
    destination = root / "assets" / "icons"
    destination.mkdir(parents=True, exist_ok=True)
    image = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((36, 36, 988, 988), radius=216, fill="#113D35")
    draw.rounded_rectangle((173, 190, 739, 725), radius=75, fill="#66AE94")
    draw.rounded_rectangle((249, 264, 851, 815), radius=75, fill="#F3F6EE")
    draw.rounded_rectangle((297, 312, 803, 765), radius=39, fill="#D8EADD")
    draw.ellipse((647, 371, 748, 472), fill="#EDB754")
    draw.polygon(((297, 666), (434, 483), (579, 666), (673, 552), (803, 697), (803, 744), (297, 744)), fill="#246F5C")
    draw.rounded_rectangle((297, 706, 803, 765), radius=32, fill="#246F5C")
    # A compact outward arrow is the tool's only operation glyph.
    draw.line(((642, 217), (787, 217), (787, 362)), fill="#F3F6EE", width=30, joint="curve")
    draw.line(((787, 217), (659, 345)), fill="#F3F6EE", width=30)
    small = image.resize((512, 512), Image.Resampling.LANCZOS)
    small.save(destination / "app.png", optimize=True)
    small.save(root / "assets" / "icon.png", optimize=True)
    image.save(destination / "app.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    image.save(destination / "app.icns")
    print("Created application PNG, ICO and ICNS assets.")


if __name__ == "__main__":
    main()
