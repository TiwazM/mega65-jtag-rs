"""Draw the MEGA65 Studio icon (1024x1024 PNG) without any image library.
Usage: python3 gui/icon.py OUT.png"""
import struct, sys, zlib
size = 1024
bg = (0x1c, 0x1e, 0x2a)
stripes = [(0xff, 0x30, 0x30), (0xff, 0x80, 0x20), (0xff, 0xe0, 0x30), (0x30, 0xd0, 0x40), (0x30, 0xa0, 0xff), (0xc0, 0x40, 0xff)]
rows = []
r_corner = 180
for y in range(size):
    row = bytearray()
    for x in range(size):
        # rounded square mask
        dx = max(r_corner - x, x - (size - 1 - r_corner), 0)
        dy = max(r_corner - y, y - (size - 1 - r_corner), 0)
        if dx * dx + dy * dy > r_corner * r_corner:
            row += b"\0\0\0\0"
            continue
        c = bg
        # six diagonal stripes like the boot banner, from the lower left
        band = (y - 300) // 70
        if 0 <= band < 6:
            x0 = 150 + (5 - band) * 55
            if x0 <= x < x0 + 520:
                c = stripes[band]
        row += bytes(c) + b"\xff"
    rows.append(b"\0" + bytes(row))
raw = b"".join(rows)
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
