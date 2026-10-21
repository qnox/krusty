#!/usr/bin/env python3
"""Renders public/card.png, the 1200x630 link-preview card that the SEO tags in
src/pages/index.astro point at. Needs Pillow and the Inter fonts
(`pip install pillow`, fonts-inter on Debian). Run from site/: `python3 scripts/render-card.py`."""
from PIL import Image, ImageDraw, ImageFont

W, H = 1200, 630
BG, LAVENDER, FG, MUTED, ACCENT = (251, 251, 253), (235, 232, 250), (27, 26, 31), (93, 90, 102), (127, 82, 255)
FONTS = "/usr/share/fonts/opentype/inter/"
TAGLINE = "A Kotlin compiler written in Rust"
DESCRIPTION = (
    "A drop-in replacement for kotlinc on the JVM: for the supported Kotlin subset, "
    "kotlinc-compatible flags and .class files that aim to be byte-for-byte identical to kotlinc's."
)
FOOTER = "krustythecompiler.dev  ·  github.com/qnox/krusty  ·  Apache-2.0"

img = Image.new("RGB", (W, H), BG)
draw = ImageDraw.Draw(img)
draw.ellipse((-180, -220, 260, 220), fill=LAVENDER)
draw.ellipse((1040, 420, 1400, 780), fill=LAVENDER)
draw.ellipse((1080, 60, 1150, 130), fill=LAVENDER)

crab = Image.open("public/krusty.png").convert("RGBA")
crab = crab.resize((round(crab.width * 300 / crab.height), 300), Image.LANCZOS)
TEXT_RIGHT = 640  # the text column ends here; the mascot sits entirely to the right of it
img.paste(crab, (W - crab.width - 60, (H - crab.height) // 2), crab)
assert W - crab.width - 60 > TEXT_RIGHT + 20

h1 = ImageFont.truetype(FONTS + "Inter-ExtraBold.otf", 104)
h2 = ImageFont.truetype(FONTS + "Inter-SemiBold.otf", 34)
body = ImageFont.truetype(FONTS + "Inter-Regular.otf", 27)
small = ImageFont.truetype(FONTS + "Inter-Medium.otf", 24)

x = 80
draw.text((x, 120), "krusty", font=h1, fill=FG)
assert draw.textlength(TAGLINE, font=h2) <= TEXT_RIGHT - x
draw.text((x, 250), TAGLINE, font=h2, fill=ACCENT)
lines, line = [], ""
for word in DESCRIPTION.split():
    candidate = f"{line} {word}".strip()
    if draw.textlength(candidate, font=body) > TEXT_RIGHT - x:
        lines.append(line)
        line = word
    else:
        line = candidate
lines.append(line)
y = 320
for text in lines:
    draw.text((x, y), text, font=body, fill=FG)
    y += 40
draw.text((x, H - 80), FOOTER, font=small, fill=MUTED)
img.save("public/card.png", optimize=True)
