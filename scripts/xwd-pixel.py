#!/usr/bin/env python3
"""Reads one pixel out of an `xwd` dump on stdin and prints it as `r g b`.

`window-clicks.sh` needs to look at what actually reached the screen, and the
only screenshot tool this harness can count on is `xwd`. Its format is a fixed
header of big-endian `u32`s followed by the pixel rows, which is little enough
to read here rather than pulling in an image library for three numbers.

    xwd -silent -id "$window" | xwd-pixel.py X Y

`xwd-to-png.py` reads the same header, for `screenshots.sh`. Kept separate
rather than shared: between them the common part is the eight lines below that
unpack a format frozen since the 1980s, and a module existing only to hold
those would be more to find than to repeat.
"""

import struct
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: xwd-pixel.py X Y", file=sys.stderr)
        return 2
    want_x, want_y = int(sys.argv[1]), int(sys.argv[2])

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
    if not (0 <= want_x < width and 0 <= want_y < height):
        print(f"{want_x},{want_y} is outside the {width}x{height} dump", file=sys.stderr)
        return 1

    # The header is followed by the colour map, then the rows.
    pixels_at = header_size + ncolors * 12
    stride = bits_per_pixel // 8
    at = pixels_at + want_y * bytes_per_line + want_x * stride
    raw = data[at : at + stride]
    if len(raw) != stride:
        print("the dump ends before the pixel asked for", file=sys.stderr)
        return 1

    value = int.from_bytes(raw, "big" if byte_order else "little")

    def channel(mask: int) -> int:
        if mask == 0:
            return 0
        shift = (mask & -mask).bit_length() - 1
        span = mask >> shift
        # Widened to eight bits, so a five-bit channel does not read as dark.
        return (value & mask) >> shift if span >= 255 else ((value & mask) >> shift) * 255 // span

    print(f"{channel(red_mask)} {channel(green_mask)} {channel(blue_mask)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
