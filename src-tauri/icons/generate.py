"""Reproduce the original local app icon: a stylised capital A.

Requires Pillow; no remote assets or system fonts. The A is drawn geometrically
so the checked-in PNG/ICO/ICNS files are reproducible on every platform.
"""
from pathlib import Path
from PIL import Image, ImageDraw

root = Path(__file__).resolve().parent
public = root.parent.parent / "public"
public.mkdir(exist_ok=True)

image = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
draw = ImageDraw.Draw(image)
draw.rounded_rectangle((40, 40, 984, 984), radius=220, fill="#171d2c")

ink = "#a6efcf"
# Two thick legs meeting at the apex, plus a crossbar. Round feet and crossbar
# ends keep the letter legible at 16 px without relying on a font file.
draw.line([(292, 804), (512, 220), (732, 804)], fill=ink, width=122, joint="curve")
draw.line([(392, 646), (632, 646)], fill=ink, width=96)
for x, y, radius in [
    (292, 804, 61),
    (732, 804, 61),
    (512, 220, 61),
    (392, 646, 48),
    (632, 646, 48),
]:
    draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=ink)

for size, name in [
    (32, "32x32.png"),
    (128, "128x128.png"),
    (256, "128x128@2x.png"),
]:
    image.resize((size, size), Image.Resampling.LANCZOS).save(root / name)
image.save(
    root / "icon.ico",
    sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
image.save(root / "icon.icns")
image.resize((32, 32), Image.Resampling.LANCZOS).save(public / "favicon.png")
image.resize((48, 48), Image.Resampling.LANCZOS).save(public / "favicon.ico")
