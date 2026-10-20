"""生成应用图标源图与菜单栏模板图标。

用法：python3 scripts/gen_icons.py && npx tauri icon src-tauri/icons/icon-source.png
图形取自设计稿的「卡片 + 向上箭头」线性图标（20×20 视框）。
"""
import os
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "src-tauri", "icons")
os.makedirs(OUT, exist_ok=True)


def glyph(draw, s, ox, oy, color, width, arrow=True, check=False):
    """在 (ox, oy) 处按缩放 s 画 20×20 视框内的卡片图形"""
    P = lambda x, y: (ox + x * s, oy + y * s)
    w = max(1, round(width * s))
    r = 1.5 * s
    # 卡片外框：左上 (3.5,2.5) 右下 (16.5,17.5)，右上切角
    pts = [P(5, 2.5), P(12, 2.5), P(16.5, 7), P(16.5, 16), P(15, 17.5), P(5, 17.5), P(3.5, 16), P(3.5, 4), P(5, 2.5)]
    draw.line(pts, fill=color, width=w, joint="curve")
    for (x, y) in [(5, 2.5), (12, 2.5), (16.5, 7), (16.5, 16), (15, 17.5), (5, 17.5), (3.5, 16), (3.5, 4)]:
        cx, cy = P(x, y)
        draw.ellipse([cx - w / 2, cy - w / 2, cx + w / 2, cy + w / 2], fill=color)
    if arrow:
        draw.line([P(10, 14.5), P(10, 9)], fill=color, width=w)
        draw.line([P(7.5, 11.2), P(10, 8.7), P(12.5, 11.2)], fill=color, width=w, joint="curve")
        for (x, y) in [(10, 14.5), (7.5, 11.2), (12.5, 11.2), (10, 8.7)]:
            cx, cy = P(x, y)
            draw.ellipse([cx - w / 2, cy - w / 2, cx + w / 2, cy + w / 2], fill=color)


def app_icon(size=1024, ss=4):
    S = size * ss
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # macOS 图标网格：824×824 内容区，圆角约 22.5%
    m = 100 * ss
    d.rounded_rectangle([m, m, S - m, S - m], radius=185 * ss, fill=(44, 107, 122, 255))
    inner = (S - 2 * m)
    s = inner / 20 * 0.62
    off = m + (inner - 20 * s) / 2
    glyph(d, s, off, off, (255, 255, 255, 255), 1.5)
    return img.resize((size, size), Image.LANCZOS)


def tray_icon(pt=18, scale=2, ss=8):
    size = pt * scale
    S = size * ss
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    s = S / 20
    glyph(d, s, 0, 0, (0, 0, 0, 255), 1.6)
    return img.resize((size, size), Image.LANCZOS)


app_icon().save(os.path.join(OUT, "icon-source.png"))
tray_icon(scale=1).save(os.path.join(OUT, "tray.png"))
tray_icon(scale=2).save(os.path.join(OUT, "tray@2x.png"))
print("ok", OUT)
