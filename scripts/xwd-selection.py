#!/usr/bin/env python3
"""Counts selection-tinted pixels in a band of an `xwd` dump.

    xwd -silent -id "$window" | xwd-selection.py Y0 Y1

Prints the count. `window-clicks.sh` asks this to find out whether text on the
page is highlighted, which no other helper here can answer: `xwd-pixel.py` reads
one point and the highlight is wherever the text happens to be, and
`xwd-ink.py` says only that a row is not white — which every row of a page with
words on it already is.

The wash is a multiply by (120, 170, 255), so a highlighted white row lands on
that colour and anything highlighted comes out with blue clearly ahead of red.
That is a family of colours rather than one, because the tint multiplies
whatever was underneath: black text stays black, grey text goes blue-grey. So
this counts rather than matching, and the caller compares against a count taken
before whatever it is testing — a page is allowed its own blue, and the fixture
has some.
"""

import struct
import sys

# How far ahead of red the blue channel has to be. Below this are the greys and
# the near-greys, which a page is full of and a tint is not.
LEAN = 40


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: xwd-selection.py Y0 Y1", file=sys.stderr)
        return 2
    y0, y1 = int(sys.argv[1]), int(sys.argv[2])

    data = sys.stdin.buffer.read()
    if len(data) < 100:
        print("the dump is too short to hold a header", file=sys.stderr)
        return 1

    # XWDFileHeader, always big-endian whatever the machine is.
    fields = struct.unpack(">25I", data[:100])
    header_size = fields[0]
    width, height = fields[4], fields[5]
    byte_order = fields[7]
    bits_per_pixel = fields[11]
    bytes_per_line = fields[12]
    red_mask, green_mask, blue_mask = fields[14], fields[15], fields[16]
    ncolors = fields[19]

    if bits_per_pixel not in (24, 32):
        print(f"unsupported depth: {bits_per_pixel} bits per pixel", file=sys.stderr)
        return 1

    pixels_at = header_size + ncolors * 12
    stride = bits_per_pixel // 8

    def channel(value: int, mask: int) -> int:
        if mask == 0:
            return 0
        shift = (mask & -mask).bit_length() - 1
        span = mask >> shift
        # Widened to eight bits, so a five-bit channel does not read as dark.
        return (value & mask) >> shift if span >= 255 else ((value & mask) >> shift) * 255 // span

    tinted = 0
    for y in range(max(y0, 0), min(y1, height)):
        row = pixels_at + y * bytes_per_line
        for x in range(width):
            at = row + x * stride
            raw = data[at : at + stride]
            if len(raw) != stride:
                break
            value = int.from_bytes(raw, "big" if byte_order else "little")
            red = channel(value, red_mask)
            blue = channel(value, blue_mask)
            if blue - red >= LEAN:
                tinted += 1
    print(tinted)
    return 0


if __name__ == "__main__":
    sys.exit(main())
