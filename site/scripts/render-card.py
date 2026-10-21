#!/usr/bin/env python3
"""Renders public/card.png, the 1200x630 link-preview card that the SEO tags in
src/pages/index.astro point at. Needs Pillow and the Inter fonts
(`pip install pillow`, fonts-inter on Debian). Run from site/: `python3 scripts/render-card.py`."""
from PIL import Image, ImageDraw, ImageFont

W, H = 1200, 630
BG, LAVENDER, FG, ACCENT = (251, 251, 253), (214, 204, 255), (27, 26, 31), (127, 82, 255)
PILL_BG, PILL_EDGE, PILL_FG, TICK = (240, 236, 255), (200, 188, 255), (70, 40, 160), (22, 163, 74)
FONTS = "/usr/share/fonts/opentype/inter/"
TAGLINE = "A Kotlin compiler in Rust."
SUBLINE = "Drop-in for kotlinc. Diffed byte-for-byte."
# Facts a sceptical reader can check on the landing page; no counts, so the card does not go stale.
PILLS = ["Kotlin 2.4", "All kotlinc box tests", "Gradle plugin", "Apache-2.0"]

img = Image.new("RGB", (W, H), BG)
draw = ImageDraw.Draw(img)
draw.ellipse((-200, -260, 220, 180), fill=LAVENDER)
draw.ellipse((1060, 440, 1400, 780), fill=LAVENDER)

crab = Image.open("public/krusty.png").convert("RGBA")
crab = crab.resize((round(crab.width * 270 / crab.height), 270), Image.LANCZOS)
crab_x = W - crab.width - 50
img.paste(crab, (crab_x, 120), crab)

h1 = ImageFont.truetype(FONTS + "Inter-ExtraBold.otf", 104)
h2 = ImageFont.truetype(FONTS + "Inter-SemiBold.otf", 40)
body = ImageFont.truetype(FONTS + "Inter-Regular.otf", 32)
pill = ImageFont.truetype(FONTS + "Inter-Medium.otf", 26)

x = 80
draw.text((x, 130), "krusty", font=h1, fill=FG)
for text, font in ((TAGLINE, h2), (SUBLINE, body)):
    assert draw.textlength(text, font=font) < crab_x - x - 20, f"{text!r} runs into the mascot"
draw.text((x, 270), TAGLINE, font=h2, fill=ACCENT)
draw.text((x, 328), SUBLINE, font=body, fill=FG)


def tick(x0, y0):
    """A green check mark drawn with two strokes, so it never depends on a font glyph."""
    draw.line([(x0, y0 + 10), (x0 + 7, y0 + 17), (x0 + 20, y0 + 2)], fill=TICK, width=4, joint="curve")


px, py = x, 430
for text in PILLS:
    w = draw.textlength(text, font=pill) + 66
    assert px + w <= W - 60, "pill row overflows the card"
    draw.rounded_rectangle((px, py, px + w, py + 44), radius=22, fill=PILL_BG, outline=PILL_EDGE)
    tick(px + 18, py + 12)
    draw.text((px + 46, py + 8), text, font=pill, fill=PILL_FG)
    px += w + 14
img.save("public/card.png", optimize=True)
