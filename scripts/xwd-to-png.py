"""Turns an X11 window dump into a PNG. No image libraries are installed here."""
import struct, sys, zlib

data = open(sys.argv[1], 'rb').read()
# XWD header: 100+ bytes of big-endian u32s, then the window name, then pixels.
(hsize, ver, fmt, depth, width, height, xoff, byte_order, bitmap_unit,
 bitmap_bit_order, bitmap_pad, bits_per_pixel, bytes_per_line) = struct.unpack('>13I', data[:52])
ncolors = struct.unpack('>I', data[76:80])[0]
pixels = hsize + ncolors * 12
stride = bytes_per_line
crop_w = int(sys.argv[3]) if len(sys.argv) > 3 else width
crop_h = int(sys.argv[4]) if len(sys.argv) > 4 else height
crop_w, crop_h = min(crop_w, width), min(crop_h, height)
scan = []
for y in range(crop_h):
    row = bytearray()
    base = pixels + y * stride
    for x in range(crop_w):
        p = base + x * (bits_per_pixel // 8)
        b, g, r = data[p], data[p + 1], data[p + 2]
        row += bytes((r, g, b))
    scan.append(bytes(row))

# Per-row filtering, chosen by the usual minimum-sum-of-absolute-values
# heuristic. Screenshots are mostly flat colour, so this roughly halves them.
def filtered(row, prior):
    n = len(row)
    none = bytes(row)
    sub = bytes((row[i] - (row[i - 3] if i >= 3 else 0)) & 0xff for i in range(n))
    up = bytes((row[i] - prior[i]) & 0xff for i in range(n))
    best = min(((0, none), (1, sub), (2, up)),
               key=lambda c: sum(v if v < 128 else 256 - v for v in c[1]))
    return bytes((best[0],)) + best[1]

prior = bytes(len(scan[0])) if scan else b''
rows = []
for row in scan:
    rows.append(filtered(row, prior))
    prior = row
raw = zlib.compress(b''.join(rows), 9)

def chunk(tag, body):
    return struct.pack('>I', len(body)) + tag + body + struct.pack('>I', zlib.crc32(tag + body))

png = (b'\x89PNG\r\n\x1a\n'
       + chunk(b'IHDR', struct.pack('>IIBBBBB', crop_w, crop_h, 8, 2, 0, 0, 0))
       + chunk(b'IDAT', raw) + chunk(b'IEND', b''))
open(sys.argv[2], 'wb').write(png)
print(f'{crop_w}x{crop_h} -> {sys.argv[2]}')
