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
# The right-hand controls, from `chrome.rs`: PADDING, then the save control,
# then the layout toggle beside it. Same caveat as `chrome` above — these are
# pinned against `controls()` by its own tests, and repeated here because a
# pointer has to be told a number.
padding=8
bookmark=56
toggle=96
# The scrollbar's width, from `scrollbar.rs`. Same caveat as the numbers above:
# pinned there by its own tests, and repeated here because a pointer has to be
# told a number.
scrollbar=8
toggle_x=$((width - padding - bookmark - toggle / 2))
toggle_y=$((chrome / 2))

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

[ -x "$browser" ] || fail "build it first: cargo build --release"
for tool in Xvfb xdotool xwd python3; do
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
    start_on "$page" "Departure"
}

# The same, on a named page whose rendered title is `$2`. Readiness is the
# title changing from the URL it was launched with, so the caller has to say
# what the page's title actually is.
start_on() {
    local on=$1 ready=$2
    DISPLAY=$display "$browser" open "$on" --width "$width" --height "$height" \
        >/dev/null 2>&1 &
    app=$!
    local waited=0
    while [ "$waited" -lt 60 ]; do
        window=$(DISPLAY=$display xdotool search --onlyvisible --name . 2>/dev/null | head -1 || true)
        if [ -n "$window" ]; then
            title=$(DISPLAY=$display xdotool getwindowname "$window" 2>/dev/null || true)
            # The launch title is the URL; anything else means a page rendered.
            case "$title" in
                *"$ready"*) return 0 ;;
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
# Clicks a point over and over until the window navigates, or gives up.
#
# The retry is not impatience. Unlike a click, a resize produces no signal this
# harness can wait for — the title says nothing about how wide the page was laid
# out — so the click itself is the probe: it misses while the old layout is
# still up and lands once the new one is. Clicking a place with no link does
# nothing, so probing costs nothing.
click_until_navigated() {
    local x=$1 y=$2 waited=0 title=""
    while [ "$waited" -lt 40 ]; do
        DISPLAY=$display xdotool mousemove "$x" "$y"
        DISPLAY=$display xdotool click 1
        sleep 0.5
        title=$(DISPLAY=$display xdotool getwindowname "$window" 2>/dev/null || true)
        case "$title" in
            *Arrival*) echo "$title"; return 0 ;;
        esac
        waited=$((waited + 1))
    done
    echo "$title"
}

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

# 4. The layout toggle re-renders the page rather than only relabelling itself.
#    This fixture classifies as authored, so pressing the toggle is the request
#    that has no automatic counterpart: give this page the document fallback
#    (ADR-0009). The title is the only place the answer shows, which is the
#    point — the tab holds the reader's choice and the child holds the page, and
#    a press that updated the first without reaching the second would leave the
#    button saying one thing and the window showing another. That is exactly
#    what it did before this check existed.
start
after=$(click_and_read "$toggle_x" "$toggle_y")
case "$after" in
    *"rendered as document"*) echo "ok: the layout toggle reached the renderer" ;;
    *) fail "pressing the layout toggle left the window on: $after" ;;
esac
stop

# 5. A burst of resizes ends with the page laid out for the size the window
#    actually finished at, and the window still answers.
#
#    Resizes arrive one per frame during a drag and a render costs several
#    frames' worth, so they are recorded and rendered once the event queue
#    drains rather than serviced one for one. Both halves of that need
#    checking and neither is reachable from `cargo test`: that the coalescing
#    does not swallow the last resize, and that the render it does reaches the
#    child rather than only the chrome.
#
#    The link is the instrument. It sits at y=70 at the wide size and y=90 at
#    the narrow one — no overlap — so a click at the narrow position proves the
#    page was re-laid-out rather than merely repainted.
narrow=420
narrow_link=$("$browser" links "$page" --width "$narrow" | sed -n '2p')
[ -n "$narrow_link" ] || fail "no link at ${narrow}px, so there is nothing to aim at"
n_x=$(echo "$narrow_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\1/')
n_y=$(echo "$narrow_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\2/')
n_w=$(echo "$narrow_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\3/')
n_h=$(echo "$narrow_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\4/')
[ "$n_y" -ne "$link_y" ] || fail "the link is at the same row at both widths, so \
this check could pass without the page ever having been laid out again"

start
# The burst. Every intermediate size is one the window really was, and every
# one of them but the last is meant to be dropped.
for w in 760 720 680 640 600 560 520 480 440 "$narrow"; do
    DISPLAY=$display xdotool windowsize "$window" "$w" "$height"
done
after=$(click_until_navigated $((n_x + n_w / 2)) $((n_y + n_h / 2 + chrome)))
case "$after" in
    *Arrival*) echo "ok: a burst of resizes ended laid out for the last one" ;;
    *) fail "after resizing to ${narrow}px the link was not where that width \
puts it — the window is on: $after" ;;
esac
stop

# 6. The document fallback reaches the screen dark, including the rows the
#    child sent no pixels for.
#
#    Three things have to line up for this and none of them is reachable from
#    `cargo test`: the reader sheet has to be applied, its canvas colour has to
#    cross the pipe with the page, and `draw` has to fill the rows below a short
#    page with *that* rather than with white. The last of those is one line
#    inside the event loop, and the failure it guards against — a lit strip
#    under every article — is exactly the kind that unit tests cannot see.
pixel() {
    DISPLAY=$display xwd -silent -id "$window" | python3 "$here/scripts/xwd-pixel.py" "$1" "$2"
}

# Dark enough to be the fallback's near-black rather than a white page, by
# Rec. 601 brightness scaled to 0-255. A threshold rather than the exact colour:
# this is checking that the page went dark, not what shade the sheet picked.
dark() {
    echo "$1" | awk '{ exit !((0.299 * $1 + 0.587 * $2 + 0.114 * $3) < 60) }'
}

start
# Well below the content of this fixture, so it is a row the band does not
# cover — the very rows `draw` has to colour itself.
below=$((height - 60))
before=$(pixel $((width / 2)) "$below")
dark "$before" && fail "the page is already dark before the toggle, so this \
check would pass without the fallback ever being asked for"

after=$(click_and_read "$toggle_x" "$toggle_y")
case "$after" in
    *"rendered as document"*) ;;
    *) fail "the layout toggle did not reach the renderer: $after" ;;
esac
# Near the right-hand edge but clear of the scrollbar column, which is drawn
# over the page and has a colour of its own.
edge=$((width - scrollbar - 4))
for point in "$((width / 2)) 200" "$((width / 2)) $below" "$edge 400"; do
    # shellcheck disable=SC2086
    got=$(pixel $point)
    dark "$got" || fail "the document fallback left ($point) at $got, which is \
not a dark page"
done
echo "ok: the document fallback reached the screen dark, edge to edge"
stop

# 7. The scrollbar is drawn on a page taller than the window, and dragging its
#    thumb scrolls the page.
#
#    The geometry is pinned by unit tests in `scrollbar.rs`. What those cannot
#    see is any of the parts that live in the event loop: that the bar is drawn
#    at all, that a press on it is recognised as a grab rather than as a click
#    on the page, and that the pointer moving while held reaches the page. A
#    scrollbar that draws and does not drag looks exactly like a working one
#    until you try to use it.
long="$here/tests/window/long.html"
bar_x=$((width - scrollbar / 2))

# The first and last rows of the page area whose right-hand column is not the
# page's own background, which is where the thumb is.
thumb_span() {
    local first="" last="" row rgb
    DISPLAY=$display xwd -silent -id "$window" > "$dump"
    for row in $(seq $((chrome + 2)) 8 $((height - 4))); do
        rgb=$(python3 "$here/scripts/xwd-pixel.py" "$bar_x" "$row" < "$dump")
        if [ "$rgb" != "255 255 255" ]; then
            [ -z "$first" ] && first=$row
            last=$row
        fi
    done
    echo "$first $last"
}

dump=$(mktemp)
trap 'rm -f "$dump"; cleanup' EXIT

start_on "$long" "Long"
before=$(thumb_span)
[ "$before" != " " ] || fail "no scrollbar on a page far taller than the window"
before_top=${before% *}

# Grab the thumb and pull it down the track.
DISPLAY=$display xdotool mousemove "$bar_x" "$((before_top + 4))"
sleep 0.3
DISPLAY=$display xdotool mousedown 1
for step in 100 200 300 400; do
    DISPLAY=$display xdotool mousemove "$bar_x" $((chrome + step))
    sleep 0.2
done
DISPLAY=$display xdotool mouseup 1
sleep 1

after=$(thumb_span)
after_top=${after% *}
[ -n "$after_top" ] || fail "the thumb vanished during the drag"
[ "$after_top" -gt "$before_top" ] || fail "dragging the thumb from $before_top \
left it at $after_top — the drag never reached the page"
echo "ok: the scrollbar drew and its thumb followed a drag"

# And letting go of the thumb is not a click on whatever the pointer has
# wandered over by then. A hand dragging a scrollbar leaves the bar constantly,
# and this fixture has a link near the top for it to land on.
long_link=$("$browser" links "$long" --width "$width" | sed -n '2p')
[ -n "$long_link" ] || fail "the long fixture has no link, so releasing the \
thumb has nothing to land on and this check proves nothing"
l_x=$(echo "$long_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\1/')
l_y=$(echo "$long_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\2/')
l_w=$(echo "$long_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\3/')
l_h=$(echo "$long_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\4/')

# Grab the thumb near its *bottom* and drag back to the top. Near the bottom
# because the drag keeps the pointer's offset within the thumb: with a big
# offset the pointer can wander a long way down the window and the page stays
# at the top, which is what puts the link back where the layout says it is.
# Grabbing near the top instead pulls the page down again the moment the
# pointer leaves the bar, and the release lands on nothing.
after_bottom=${after#* }
DISPLAY=$display xdotool mousemove "$bar_x" "$((after_bottom - 8))"
sleep 0.3
DISPLAY=$display xdotool mousedown 1
DISPLAY=$display xdotool mousemove "$bar_x" "$chrome"
sleep 0.5
DISPLAY=$display xdotool mousemove $((l_x + l_w / 2)) $((l_y + l_h / 2 + chrome))
sleep 0.5
DISPLAY=$display xdotool mouseup 1
sleep 1
after=$(DISPLAY=$display xdotool getwindowname "$window")
case "$after" in
    *Long*) ;;
    *) fail "releasing the scrollbar navigated to: $after" ;;
esac

# And the point it was released on really is a link, so the assertion above is
# about the release and not about the pointer having landed on blank page. An
# earlier version of this check omitted this and passed against a browser that
# followed the link on release, because the drag had left the pointer nowhere
# in particular.
after=$(click_and_read $((l_x + l_w / 2)) $((l_y + l_h / 2 + chrome)))
case "$after" in
    *Arrival*) echo "ok: letting go of the thumb was not a click on the page" ;;
    *) fail "a plain click where the thumb was released did not follow a link \
either, so the check above proved nothing: $after" ;;
esac
stop

# 8. The loading bar appears while a navigation is in flight and goes away
#    when it lands.
#
#    A navigation is synchronous — the fetch blocks, and so does the round trip
#    to the child that lays the page out — so the bar is painted from inside
#    `show` rather than by a redraw that will not happen until the page is
#    already up. Whether that painting reaches the screen is not something
#    `cargo test` can see, and a progress bar that never appears is the one
#    failure that matters.
#
#    The fixture is generated rather than committed: it has to take long enough
#    to lay out that the bar is on screen for more than a frame, which means it
#    has to be megabytes, and a megabyte of filler is not worth keeping.
heavy=$(mktemp -d)
trap 'rm -f "$dump"; rm -rf "$heavy"; cleanup' EXIT
python3 - "$heavy" <<'FIXTURE'
import pathlib, sys
into = pathlib.Path(sys.argv[1])
(into / "heavy.html").write_text(
    "<!doctype html>\n<title>Heavy</title>\n<body>\n"
    + "".join(
        f"<p>Paragraph number {i} with a good few words in it so that shaping "
        "and layout have real work to do on this line.</p>\n"
        for i in range(20000)
    )
    + "</body>\n"
)
(into / "heavy-from.html").write_text(
    "<!doctype html>\n<title>Heavy departure</title>\n<body>\n"
    '<p><a href="heavy.html">go to the heavy page</a></p>\n</body>\n'
)
FIXTURE

heavy_link=$("$browser" links "$heavy/heavy-from.html" --width "$width" | sed -n '2p')
[ -n "$heavy_link" ] || fail "the generated fixture has no link to follow"
h_x=$(echo "$heavy_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\1/')
h_y=$(echo "$heavy_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\2/')
h_w=$(echo "$heavy_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\3/')
h_h=$(echo "$heavy_link" | sed -E 's/^ *([0-9]+),([0-9]+) +([0-9]+)x([0-9]+).*/\4/')

# The bar's accent, from `window.rs`, as the pixel reader prints it.
accent="58 110 165"

start_on "$heavy/heavy-from.html" "Heavy departure"
DISPLAY=$display xdotool mousemove $((h_x + h_w / 2)) $((h_y + h_h / 2 + chrome))
sleep 0.3
DISPLAY=$display xdotool click 1

# Sampled a fifth of the way across, which the bar covers at its first stage.
seen=""
for _ in $(seq 1 60); do
    if [ "$(pixel 100 $((chrome + 1)))" = "$accent" ]; then
        seen=yes
        break
    fi
done
[ -n "$seen" ] || fail "no loading bar during a navigation that takes a second"

waited=0
while [ "$waited" -lt 60 ]; do
    case "$(DISPLAY=$display xdotool getwindowname "$window")" in
        Heavy\ —*) break ;;
    esac
    sleep 0.5
    waited=$((waited + 1))
done
[ "$waited" -lt 60 ] || fail "the heavy page never finished loading"
after=$(pixel 100 $((chrome + 1)))
[ "$after" != "$accent" ] || fail "the loading bar is still on screen after the \
page arrived, so it says nothing about whether anything is loading"
echo "ok: the loading bar showed during a navigation and went away after it"
stop

echo "all window click checks passed"
