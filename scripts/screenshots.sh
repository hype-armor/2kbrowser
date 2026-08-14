#!/usr/bin/env bash
# Regenerates the screenshots in docs/images/.
#
# Real pages on a virtual display, so what the README shows is what the browser
# does rather than a mock-up — and so a screenshot that has quietly gone stale
# can be replaced by running this rather than by remembering how it was made.
#
# Needs Xvfb and xwd (the same two packages CI installs for the window smoke
# test) and a release build. The PNG encoding is `scripts/xwd-to-png.py`,
# because no image tool is assumed to be installed.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
browser="$here/target/release/2kbrowser"
images="$here/docs/images"
display=":77"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ ! -x "$browser" ]; then
    echo "build it first: cargo build --release" >&2
    exit 1
fi
for tool in Xvfb xwd; do
    command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }
done

Xvfb "$display" -screen 0 1400x1100x24 >/dev/null 2>&1 &
xvfb=$!
trap 'kill $xvfb 2>/dev/null || true; rm -rf "$work"' EXIT
sleep 2

shot() {
    local url=$1 out=$2 width=$3 height=$4 settle=$5
    DISPLAY=$display "$browser" open "$url" --width "$width" --height "$height" >/dev/null 2>&1 &
    local app=$!
    sleep "$settle"
    DISPLAY=$display xwd -root -silent > "$work/shot.xwd"
    kill $app 2>/dev/null || true
    wait $app 2>/dev/null || true
    python3 "$here/scripts/xwd-to-png.py" "$work/shot.xwd" "$out" "$width" "$height"
}

shot "https://news.ycombinator.com/" "$images/hacker-news.png" 900 700 12
shot "http://info.cern.ch/hypertext/WWW/TheProject.html" "$images/first-website.png" 820 520 10
shot "https://www.rust-lang.org/" "$images/document-fallback.png" 900 560 14

# The chrome bar needs no window: the example draws every state of it.
(cd "$here" && cargo run --quiet -p shell --example chrome-strip >/dev/null)
mv "$here/chrome-strip.png" "$images/chrome.png"
echo "wrote $images"
