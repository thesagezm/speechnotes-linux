#!/usr/bin/env python3
"""Renders the speechnotes-linux app icon (no SVG rasterizer on the box —
PIL draws it directly). Output: config/icon.png (256²), the file
install.sh copies into the user's hicolor icon theme."""
from PIL import Image, ImageDraw

SIZE = 256
img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# Rounded-square background — deep indigo, subtle vertical two-tone.
BG_TOP = (63, 62, 148)
BG_BOTTOM = (38, 38, 96)
icon = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
di = ImageDraw.Draw(icon)
di.rounded_rectangle([8, 8, SIZE - 8, SIZE - 8], radius=48, fill=BG_TOP)
# Lower half tint: overlay a rounded rect clipped to the icon shape.
overlay = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
do = ImageDraw.Draw(overlay)
do.rounded_rectangle([8, SIZE // 2, SIZE - 8, SIZE - 8], radius=48, fill=BG_BOTTOM)
mask = Image.new("L", (SIZE, SIZE), 0)
dm = ImageDraw.Draw(mask)
dm.rounded_rectangle([8, 8, SIZE - 8, SIZE - 8], radius=48, fill=255)
img.paste(Image.composite(Image.alpha_composite(icon, overlay), icon, mask), (0, 0), mask)

d = ImageDraw.Draw(img)

# Note sheet (white rounded rect) on the left.
SHEET = [56, 52, 152, 204]
d.rounded_rectangle(SHEET, radius=16, fill=(250, 250, 252, 255))
# Text lines on the sheet.
line_color = (120, 122, 158, 255)
for i, width in enumerate([72, 72, 56, 72, 40]):
    y = 84 + i * 22
    d.rounded_rectangle([76, y, 76 + width, y + 8], radius=4, fill=line_color)

# Sound bars on the right — the TTS waveform, accent teal.
BAR = (94, 226, 196, 255)
center_y = 128
heights = [44, 88, 128, 88, 44]
x = 172
for h in heights:
    top = center_y - h // 2
    d.rounded_rectangle([x, top, x + 14, top + h], radius=7, fill=BAR)
    x += 24

img.save("config/icon.png")
print("wrote config/icon.png")
