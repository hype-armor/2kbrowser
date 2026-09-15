#!/usr/bin/env python3
"""Says whether any pixel in a row of an `xwd` dump is not white.

    xwd -silent -id "$window" | xwd-ink.py X0 X1 Y

Prints `ink` or `clear`. `window-clicks.sh` asks this about a strip of a text
field, to find out whether anything was typed into it — and asking it here
rather than by calling `xwd-pixel.py` thirty times is the difference between one
screenshot and thirty. The typing check spent minutes on that before it was
written, which on a harness that already waits for real renders is time nobody
has.

`xwd-pixel.py` and `xwd-to-png.py` read the same header. Kept separate rather
than shared, for the reason `xwd-pixel.py` gives: between them the common part
is the few lines below that unpack a format frozen since the 1980s, and a module
existing only to hold those would be more to find than to repeat.
"""

import struct
import sys


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: xwd-ink.py X0 X1 Y", file=sys.stderr)
        return 2
    x0, x1, want_y = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])

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
    if not 0 <= want_y < height:
        print(f"row {want_y} is outside the {width}x{height} dump", file=sys.stderr)
        return 1

    pixels_at = header_size + ncolors * 12
    stride = bits_per_pixel // 8
    row = pixels_at + want_y * bytes_per_line

    def channel(value: int, mask: int) -> int:
        if mask == 0:
            return 0
        shift = (mask & -mask).bit_length() - 1
        span = mask >> shift
        # Widened to eight bits, so a five-bit channel does not read as dark.
        return (value & mask) >> shift if span >= 255 else ((value & mask) >> shift) * 255 // span

    for x in range(max(x0, 0), min(x1 + 1, width)):
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
            print("ink")
            return 0

    print("clear")
    return 0


if __name__ == "__main__":
    sys.exit(main())
