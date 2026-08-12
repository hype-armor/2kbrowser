#!/usr/bin/env bash
#
# Drives the window with a real pointer and checks that clicking a link follows
# it.
#
# This is the one part of the browser that `cargo test` cannot reach. The hit
# test's arithmetic is pinned by unit tests in `window.rs`, but nothing there
# proves the pointer position winit reports, the rectangles the child sent, and
# the rows actually painted all line up in a running window — and a browser
# whose links do not answer a click is broken however well its arithmetic tests.
#
# Two rules, both learned the hard way while chasing a bug that turned out to be
# this harness's own fault:
#
#   * Wait for a signal, never for a clock. An earlier version slept seven
#     seconds and clicked whether or not the page had rendered; against a slow
#     debug build it clicked early, found no links, and reported a browser bug
#     that did not exist. Readiness here is the window title, which stays the
#     raw URL until a page has been rendered into it.
#
#   * Drive with XTEST, never XSendEvent. `xdotool click --window <id>` sends a
#     synthetic event that winit ignores entirely, so every click silently does
#     nothing and every assertion fails identically to a real regression.
#
# Local fixtures and no network: the coordinates come from `2kbrowser links`
# rather than being hard-coded, so this survives a layout change, but it cannot
# survive a page that redesigns itself between runs.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
browser="$here/target/release/2kbrowser"
display=":88"
width=800
height=900
# The bar owns the top of the window, so a document coordinate is this far down
# the screen. `window.rs` pins this against what `draw` does; what it cannot
# pin is that the number here matches the browser being run.
chrome=46

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

[ -x "$browser" ] || fail "build it first: cargo build --release"
for tool in Xvfb xdotool; do
    command -v "$tool" >/dev/null || fail "$tool is not installed"
done

Xvfb "$display" -screen 0 1200x1000x24 >/dev/null 2>&1 &
xvfb=$!
app=""
cleanup() {
    [ -n "$app" ] && kill "$app" 2>/dev/null
    kill "$xvfb" 2>/dev/null
    return 0
}
trap cleanup EXIT
sleep 2

page="$here/tests/window/from.html"

# Where the link is, asked of the browser rather than assumed. One rectangle,
# because the fixture has one link.
link=$("$browser" links "$page" --width "$width" | sed -n '2p')
[ -n "$link" ] || fail "the fixture has no links, so there is nothing to click"
link_x=$(echo "$link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\1/')
link_y=$(echo "$link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\2/')
link_w=$(echo "$link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\3/')
link_h=$(echo "$link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\4/')
click_x=$((link_x + link_w / 2))
click_y=$((link_y + link_h / 2 + chrome))

# Starts the browser on the fixture and blocks until it has rendered something,
# which is the title changing from the URL it was launched with.
start() {
    DISPLAY=$display "$browser" open "$page" --width "$width" --height "$height" \
        >/dev/null 2>&1 &
    app=$!
    local waited=0
    while [ "$waited" -lt 60 ]; do
        window=$(DISPLAY=$display xdotool search --onlyvisible --name . 2>/dev/null | head -1 || true)
        if [ -n "$window" ]; then
            title=$(DISPLAY=$display xdotool getwindowname "$window" 2>/dev/null || true)
            # The launch title is the URL; anything else means a page rendered.
            case "$title" in
                *Departure*) return 0 ;;
            esac
        fi
        sleep 0.5
        waited=$((waited + 1))
    done
    fail "no page rendered within 30s — the browser never became ready, so no \
click below would have meant anything"
}

# Stops the browser and waits for its window to actually go, which is not the
# same thing. `kill` returns as soon as the signal is sent; the window survives
# it by however long the process takes to tear down. A `start` that ran in that
# gap found the *previous* window — still mapped, still showing the page the
# last click navigated to — and then waited for a title it was never going to
# show. Fast enough to pass here and slow enough to fail on CI, which is the
# signature of every clock this harness has already been bitten by.
stop() {
    kill "$app" 2>/dev/null || true
    wait "$app" 2>/dev/null || true
    app=""
    local waited=0 remaining
    while [ "$waited" -lt 40 ]; do
        remaining=$(DISPLAY=$display xdotool search --onlyvisible --name . 2>/dev/null | head -1 || true)
        [ -z "$remaining" ] && return 0
        sleep 0.5
        waited=$((waited + 1))
    done
    fail "a window outlived the browser it belonged to, so the next check would \
have been driving a dead one"
}

# Clicks a point and returns the title once it has settled, waiting for a change
# rather than assuming one: navigation is a fetch and a render, not an instant.
click_and_read() {
    local x=$1 y=$2 before waited title
    before=$(DISPLAY=$display xdotool getwindowname "$window")
    DISPLAY=$display xdotool mousemove "$x" "$y"
    sleep 0.3
    DISPLAY=$display xdotool click 1
    waited=0
    while [ "$waited" -lt 20 ]; do
        title=$(DISPLAY=$display xdotool getwindowname "$window" 2>/dev/null || true)
        [ "$title" != "$before" ] && { echo "$title"; return 0; }
        sleep 0.5
        waited=$((waited + 1))
    done
    # Unchanged is a real answer: the negative cases below expect it.
    echo "$before"
}

echo "link at ${link_x},${link_y} ${link_w}x${link_h} — clicking ${click_x},${click_y}"

# 1. A click on a link follows it.
start
after=$(click_and_read "$click_x" "$click_y")
case "$after" in
    *Arrival*) echo "ok: clicking the link followed it" ;;
    *) fail "clicking the link left the window on: $after" ;;
esac
stop

# 2. A click on the page but not on a link goes nowhere. Directly below the
#    link, where a hit test that had lost its scroll or chrome offset would
#    most plausibly still find one.
start
after=$(click_and_read "$click_x" $((click_y + link_h * 3)))
case "$after" in
    *Departure*) echo "ok: clicking beside the link did nothing" ;;
    *) fail "a click that was on no link navigated to: $after" ;;
esac
stop

# 3. A click on the chrome is not a click on the page. The bar is above every
#    document coordinate, so a browser that forgot to subtract it would follow
#    a link from up here.
start
after=$(click_and_read "$click_x" $((chrome / 2)))
case "$after" in
    *Departure*) echo "ok: clicking the chrome did not follow a link" ;;
    *) fail "a click on the bar navigated to: $after" ;;
esac
stop

echo "all window click checks passed"
