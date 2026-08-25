"""把用户提供的日/月图转成多尺寸 .ico（包含 PNG 帧，Windows Vista+ 支持）。

关键：源图是 JPG（无 alpha），四周是纯白背景。这里用「圆角矩形 alpha 蒙版」
把四个角置为透明，让图标周边保持透明（不要白角），符合应用图标习惯。

如果源图本身是带透明通道的 PNG（周围已透明），把 _USE_ROUND_MASK 设为 False 即可。
"""
from PIL import Image, ImageDraw

SRC = r"C:\Users\12\.workbuddy\clipboard-images\clipboard-2026-08-25T16-02-44-980Z-6c680650.jpg"
OUT = r"C:\Users\12\WorkBuddy\2026-08-25-22-44-53\wintheme-auto\assets\icon.ico"
SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

# 源图为不透明 JPG 时，用圆角蒙版把四个角透明化
_USE_ROUND_MASK = True
_MASK_PAD = 0.05   # 边缘留白比例（占边长）
_MASK_RADIUS = 0.19  # 圆角半径比例

img = Image.open(SRC).convert("RGBA")
# 确保是正方形（源图就是）并居中
w, h = img.size
side = max(w, h)
canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
canvas.paste(img, ((side - w) // 2, (side - h) // 2), img)

if _USE_ROUND_MASK:
    mask = Image.new("L", (side, side), 0)
    d = ImageDraw.Draw(mask)
    pad = int(side * _MASK_PAD)
    r = int(side * _MASK_RADIUS)
    d.rounded_rectangle(
        [pad, pad, side - pad, side - pad],
        radius=r,
        fill=255,
    )
    canvas.putalpha(mask)

canvas.save(OUT, format="ICO", sizes=SIZES)
print(f"wrote {OUT}")

# 验证
ico = Image.open(OUT)
print(f"ico size={ico.size}, n_frames={getattr(ico, 'n_frames', 1)}")
