"""把用户提供的日/月图转成多尺寸 .ico（包含 PNG 帧，Windows Vista+ 支持）。"""
from PIL import Image

SRC = r"C:\Users\12\.workbuddy\clipboard-images\clipboard-2026-08-25T16-02-44-980Z-6c680650.jpg"
OUT = r"C:\Users\12\WorkBuddy\2026-08-25-22-44-53\wintheme-auto\assets\icon.ico"
SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

img = Image.open(SRC).convert("RGBA")
# 确保是正方形（源图就是）并居中
w, h = img.size
side = max(w, h)
canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
canvas.paste(img, ((side - w) // 2, (side - h) // 2), img)

canvas.save(OUT, format="ICO", sizes=SIZES)
print(f"wrote {OUT}")

# 验证
ico = Image.open(OUT)
print(f"ico size={ico.size}, n_frames={getattr(ico, 'n_frames', 1)}")