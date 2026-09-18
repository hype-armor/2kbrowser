#!/usr/bin/env python3
"""Finds the box of everything drawn in a band of an `xwd` dump.

    xwd -silent -id "$window" | xwd-box.py Y0 Y1 [X0 X1]

Prints `left top right bottom` of the non-white pixels between rows `Y0` and
`Y1`, or nothing when that band is blank. `X0` and `X1` narrow it to a range of
columns, which matters whenever something else is drawn on the same rows: the
form fixture has a submit button beside its text field, so a box taken across
the whole width answers about the *button* however much is typed into the
field. `window-clicks.sh` asks this to find
out where a form control actually landed, on a fixture that has nothing else on
it — the same rule the link coordinates follow, which is that a coordinate comes
from the browser rather than from a number typed into the harness.

Reading the screen rather than the page: measuring a rendered PNG would mean
decoding one, and the only way to do that without writing an inflater is an
image library CI does not have. The window is already on screen and `xwd` is
already the one screenshot tool this harness can count on.

`xwd-pixel.py`, `xwd-ink.py` and `xwd-to-png.py` read the same header. Kept
separate rather than shared, for the reason `xwd-pixel.py` gives: between them
the common part is the few lines below that unpack a format frozen since the
1980s, and a module existing only to hold those would be more to find than to
repeat.
"""

import struct
import sys


def main() -> int:
    if len(sys.argv) not in (3, 5):
        print("usage: xwd-box.py Y0 Y1 [X0 X1]", file=sys.stderr)
        return 2
    y0, y1 = int(sys.argv[1]), int(sys.argv[2])
    x0, x1 = (int(sys.argv[3]), int(sys.argv[4])) if len(sys.argv) == 5 else (0, None)

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

    left, top, right, bottom = width, height, -1, -1
    last = width if x1 is None else min(x1 + 1, width)
    for y in range(max(y0, 0), min(y1 + 1, height)):
        row = pixels_at + y * bytes_per_line
        for x in range(max(x0, 0), last):
            at = row + x * stride
            raw = data[at : at + stride]
            if len(raw) != stride:
                break
            value = int.from_bytes(raw, "big" if byte_order else "little")
            pixel = (
                channel(value, red_mask),
                channel(value, green_mask),
                channel(value, blue_mask),
            )
            if pixel != (255, 255, 255):
                left, right = min(left, x), max(right, x)
                top, bottom = min(top, y), max(bottom, y)

    if right < 0:
        return 0
    print(f"{left} {top} {right} {bottom}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
