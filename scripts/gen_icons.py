#!/usr/bin/env python3
"""Generate simple KIT-VPN extension icons without any external deps.

Blue rounded square with a white "tunnel mountain" glyph.
Usage: python3 scripts/gen_icons.py [outdir]
"""
import os
import struct
import sys
import zlib

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "extension", "icons"
)

BLUE = (0x1F, 0x5C, 0x9E)
WHITE = (0xFF, 0xFF, 0xFF)
TRANSPARENT = (0, 0, 0, 0)


def png_bytes(size, pixel):
    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    raw = b"".join(
        b"\x00" + b"".join(struct.pack("4B", *p) for p in row) for row in pixel
    )
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def in_triangle(x, y, a, b, c):
    def sign(p1, p2, p3):
        return (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])

    d1 = sign((x, y), a, b)
    d2 = sign((x, y), b, c)
    d3 = sign((x, y), c, a)
    neg = d1 < 0 or d2 < 0 or d3 < 0
    pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (neg and pos)


def make_icon(size):
    r = size * 0.20  # corner radius
    # triangle (tunnel): peak near top, wide base
    a = (size * 0.16, size * 0.86)
    b = (size * 0.50, size * 0.16)
    c = (size * 0.84, size * 0.86)
    # tunnel opening: dark square near the bottom centre
    o = (size * 0.40, size * 0.62, size * 0.60, size * 0.86)

    rows = []
    for y in range(size):
        row = []
        for x in range(size):
            cx, cy = min(x, size - 1 - x), min(y, size - 1 - y)
            if cx < r and cy < r:  # rounded corner
                dx, dy = r - cx - 0.5, r - cy - 0.5
                if dx * dx + dy * dy > r * r:
                    row.append(TRANSPARENT)
                    continue
            color = BLUE + (255,)
            px, py = x + 0.5, y + 0.5
            if o[0] <= px <= o[2] and o[1] <= py <= o[3]:
                color = (0x12, 0x37, 0x5F, 255)  # dark tunnel mouth
            elif in_triangle(px, py, a, b, c):
                color = WHITE + (255,)
            row.append(color)
        rows.append(row)
    return rows


def main():
    os.makedirs(OUT, exist_ok=True)
    for size in (16, 32, 48, 96):
        path = os.path.join(OUT, f"icon-{size}.png")
        with open(path, "wb") as f:
            f.write(png_bytes(size, make_icon(size)))
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
