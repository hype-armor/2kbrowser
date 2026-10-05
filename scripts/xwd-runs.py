#!/usr/bin/env python3
"""Finds the runs of drawn pixels along one row of an `xwd` dump.

    xwd -silent -id "$window" | xwd-runs.py Y

Prints the stretches of non-white pixels on row `Y` as `first-last` pairs
separated by spaces, or nothing when the row is blank.

`xwd-box.py` answers where a band's ink is as a single box, which is the right
question when a fixture holds one thing. It is the wrong question when a row
holds several: the form fixture has a submit button beside its text field, and
a box drawn round both says only where the button's far edge is — which never
moves however much is typed. Neither does a border help, because the controls
sit flush against each other and their border rows merge into one run. What
separates them is the blank gap between them, and a gap is what this prints:
`window-clicks.sh` reads the last run as the button and measures the typed text
in the columns before it.

Reading the screen rather than the page: measuring a rendered PNG would mean
decoding one, and the only way to do that without writing an inflater is an
image library CI does not have. The window is already on screen and `xwd` is
already the one screenshot tool this harness can count on.

`xwd-pixel.py`, `xwd-ink.py`, `xwd-box.py` and `xwd-to-png.py` read the same
header. Kept separate rather than shared, for the reason `xwd-pixel.py` gives:
between them the common part is the few lines below that unpack a format frozen
since the 1980s, and a module existing only to hold those would be more to find
than to repeat.
"""

import struct
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: xwd-runs.py Y", file=sys.stderr)
        return 2
    y = int(sys.argv[1])

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
    if not 0 <= y < height:
        print(f"row {y} is outside a {width}x{height} dump", file=sys.stderr)
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

    runs: list[tuple[int, int]] = []
    started: int | None = None
    row = pixels_at + y * bytes_per_line
    for x in range(width):
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
            if started is None:
                started = x
        elif started is not None:
            runs.append((started, x - 1))
            started = None
    if started is not None:
        runs.append((started, width - 1))

    if runs:
        print(" ".join(f"{first}-{last}" for first, last in runs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
