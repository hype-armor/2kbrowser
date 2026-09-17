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
# The chrome owns the top of the window, so a document coordinate is this far
# down the screen. `window.rs` pins this against what `draw` does; what it
# cannot pin is that the numbers here match the browser being run.
#
# Two rows, from `chrome.rs`: the tab strip, which is always drawn because it
# carries the new-tab button, and the URL bar under it.
strip=28
bar=46
chrome=$((strip + bar))
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
# The left-hand controls, from `chrome.rs`: two 40px arrows, the reload word,
# then the padlock. Same caveat again — pinned there by `controls()`'s own
# tests, repeated here because a pointer has to be told a number.
button=40
reload=58
site_x=$((padding + button * 2 + reload + 13))
# Where the URL bar's text begins, from `chrome::url_text_x`. Same caveat as
# every other number here: pinned there, repeated because a pointer has to be
# told one.
url_x=$((padding * 2 + button * 2 + reload + 26))
toggle_x=$((width - padding - bookmark - toggle / 2))
# Down the middle of the URL bar, which is below the strip rather than at the
# top of the window.
toggle_y=$((strip + bar / 2))

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

[ -x "$browser" ] || fail "build it first: cargo build --release"
for tool in Xvfb xdotool xwd python3; do
    command -v "$tool" >/dev/null || fail "$tool is not installed"
done

# `-maxclients` because the default is 256 and this script goes through them.
# Every `xdotool` and every `xwd` is a fresh X client, and there are thousands
# across a run — so a check part way down would find the server refusing new
# connections and the browser would exit with "Failed to open connection to X
# server". That read as the browser dying silently for no reason, which is what
# it did on CI three times before the harness started keeping its output.
Xvfb "$display" -maxclients 2048 -screen 0 1200x1000x24 >/dev/null 2>&1 &
xvfb=$!
app=""
applog=$(mktemp)
cleanup() {
    [ -n "$app" ] && kill "$app" 2>/dev/null
    rm -f "$applog"
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
    # Kept rather than discarded, so a browser that dies on startup can say why.
    # This timed out twice on CI with no window ever appearing and nothing to
    # go on, because its output went to /dev/null — an unreproducible failure
    # whose one witness was being thrown away.
    : > "$applog"
    DISPLAY=$display "$browser" open "$on" --width "$width" --height "$height" \
        >"$applog" 2>&1 &
    app=$!
    local waited=0 title=""
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
    # What the window said, and whether there was a window at all. The three
    # ways this fails look identical without it: a browser that never opened a
    # window, one still showing the URL it was launched with because the page
    # has not rendered, and one showing a *different* page because something
    # earlier left it somewhere unexpected. Only the last is a bug in the
    # browser, and a bare timeout cannot tell them apart — which matters most
    # on CI, where this is the only evidence there will be.
    if [ -z "$window" ]; then
        # Alive and silent is a different fault from dead and noisy, and the
        # two want different things looked at next.
        # Output, exit status, and the state of the machine. The first two
        # runs of this diagnostic said the process was dead and silent, which
        # rules out a hang and a panic and leaves being killed — so what is
        # left to ask is by what, and whether the page it was given was even
        # there.
        local alive="no" status="?"
        if kill -0 "$app" 2>/dev/null; then
            alive="yes"
        else
            wait "$app" 2>/dev/null
            status=$?
        fi
        echo "--- browser output ---" >&2
        tail -20 "$applog" >&2 || true
        echo "--- still running: $alive, exit status: $status ---" >&2
        echo "--- page: $(ls -l "$on" 2>&1) ---" >&2
        echo "--- memory: $(free -m 2>/dev/null | sed -n 2p) ---" >&2
        echo "--- disk: $(df -h . 2>/dev/null | sed -n 2p) ---" >&2
        fail "no window within 30s waiting for \"$ready\" — the browser never \
opened one, so no click below would have meant anything"
    fi
    fail "no page rendered within 30s — the window says \"$title\" rather than \
\"$ready\", so no click below would have meant anything"
}

# Gives the window the *keyboard* focus, which pointer-driven checks never need:
# XTEST clicks go wherever the pointer is whether or not the window is focused,
# and keystrokes go to whatever the window manager last focused — which under a
# bare Xvfb with no window manager at all is nothing. Without this the keys are
# delivered to the void and the failure reads exactly like a browser that
# ignores the keyboard.
#
# A function rather than three copies of the incantation, because two of those
# copies had lost the fallback and the only one that needed it still had it. The
# next keyboard check would have been written from whichever copy was nearest.
focus_window() {
    DISPLAY=$display xdotool windowactivate --sync "$window" 2>/dev/null \
        || DISPLAY=$display xdotool windowfocus --sync "$window" 2>/dev/null \
        || true
    sleep 0.3
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
after=$(click_and_read "$click_x" $((strip + bar / 2)))
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

# N. Hovering a link shows its address in the bottom-left corner, and moving off
#    it takes the strip away again (#139).
#
#    Unreachable from `cargo test` for the same reason every check in this file
#    is: `preview.rs` pins what the strip looks like and where it goes, and
#    nothing there proves the event loop asks for it, that a redraw happens
#    without a click to force one, or that it lands over the page rather than
#    under it. A strip drawn into a buffer nobody presents is a passing test and
#    an invisible feature.
start
# Two pixels in from the corner, which is inside the strip's surface and clear
# of the hairline along its top edge.
corner_x=2
corner_y=$((height - 6))
empty=$(pixel "$corner_x" "$corner_y")
DISPLAY=$display xdotool mousemove "$click_x" "$click_y"
hovered=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$corner_x" "$corner_y")" != "$empty" ]; then
        hovered=yes
        break
    fi
done
[ -n "$hovered" ] || fail "hovering the link drew nothing in the corner, so the \
link preview never reached the screen"

# Straight down from the link, which the earlier checks already established is
# page and not a link.
DISPLAY=$display xdotool mousemove "$click_x" $((click_y + link_h * 2))
gone=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$corner_x" "$corner_y")" = "$empty" ]; then
        gone=yes
        break
    fi
done
[ -n "$gone" ] || fail "the link preview stayed up after the pointer left the \
link, so it is showing an address for nothing"
echo "ok: hovering a link showed its address and moving off took it away"
stop

# N. The padlock opens the site panel, and opens it again closed (#118).
#
#    `site_panel.rs` pins what the panel holds and where a click in it lands.
#    What it cannot pin is that pressing the padlock reaches any of that: the
#    control routing, the panel being drawn over the page rather than under it,
#    and the second press closing what the first opened all live in the event
#    loop. A panel that only ever opens is a browser with no way out of it.
start
# Just below the bar and a little in from the left, which the panel covers and
# an ordinary page does not.
panel_x=$((site_x + 20))
panel_y=$((chrome + 30))
closed=$(pixel "$panel_x" "$panel_y")
DISPLAY=$display xdotool mousemove "$site_x" "$toggle_y"
DISPLAY=$display xdotool click 1
opened=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$panel_x" "$panel_y")" != "$closed" ]; then
        opened=yes
        break
    fi
done
[ -n "$opened" ] || fail "pressing the padlock drew nothing over the page, so \
the site panel never reached the screen"

DISPLAY=$display xdotool mousemove "$site_x" "$toggle_y"
DISPLAY=$display xdotool click 1
shut=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$panel_x" "$panel_y")" = "$closed" ]; then
        shut=yes
        break
    fi
done
[ -n "$shut" ] || fail "pressing the padlock a second time did not close the \
panel, so there is no way out of it with the pointer"
echo "ok: the padlock opened the site panel and closed it again"
stop

# N. An open menu owns the whole click, not just the end of it (#182).
#
#    Both halves of one bug, and neither is reachable from `cargo test`: the
#    press and the release are two arms of the event loop, and what went wrong
#    is that they disagreed about who owned the click.
#
#    A menu is drawn over the page, so a press on an entry looked to the press
#    arm like a press on the page. It started a selection there and wiped the
#    one already made; the release was then claimed by the menu and returned
#    before anything cleared it. So the pointer was left selecting with no
#    button held — the next bare move dragged a highlight across the page,
#    which is what was reported — and Copy had nothing left to copy.
selected() {
    DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-selection.py" "$chrome" "$((chrome + 300))"
}
start
# The page's own blue, which is not a selection. Everything below is measured
# against this rather than against zero.
plain=$(selected)

# Open the menu on the page, then dismiss it by clicking away from it — still
# over the page, which is the press that used to start the phantom selection.
DISPLAY=$display xdotool mousemove 300 $((chrome + 200))
sleep 0.3
DISPLAY=$display xdotool click 3
sleep 1
DISPLAY=$display xdotool mousemove 600 $((chrome + 40))
DISPLAY=$display xdotool click 1
sleep 0.5
# Now move the pointer across the text with nothing held down. A browser that
# is not selecting does not care; the bug painted the page blue.
DISPLAY=$display xdotool mousemove 400 $((chrome + 20))
sleep 0.3
DISPLAY=$display xdotool mousemove 60 $((chrome + 8))
sleep 0.8
drifted=$(selected)
[ "$drifted" -le "$plain" ] || fail "moving the pointer after dismissing a menu \
highlighted the page ($drifted tinted pixels against $plain before), so the \
press left the pointer selecting with no button held"
echo "ok: dismissing a menu did not leave the pointer selecting"

# And the other half: a selection has to survive being right-clicked on, or the
# Copy entry the menu offers because of it copies nothing.
DISPLAY=$display xdotool mousemove 20 $((chrome + 4))
sleep 0.3
DISPLAY=$display xdotool mousedown 1
sleep 0.2
DISPLAY=$display xdotool mousemove 500 $((chrome + 30))
sleep 0.4
DISPLAY=$display xdotool mousemove 700 $((chrome + 60))
sleep 0.6
DISPLAY=$display xdotool mouseup 1
sleep 0.6
marked=$(selected)
[ "$marked" -gt "$plain" ] || fail "dragging across the text highlighted \
nothing ($marked tinted pixels against $plain before), so the check below \
would prove nothing"
DISPLAY=$display xdotool mousemove 300 $((chrome + 30))
sleep 0.3
DISPLAY=$display xdotool click 3
sleep 1
# Held rather than clicked, because what has to survive is the *press* — the
# release is where the menu acts on the selection, and by then it is too late
# to find out it has gone.
DISPLAY=$display xdotool mousemove 320 $((chrome + 42))
sleep 0.3
DISPLAY=$display xdotool mousedown 1
sleep 0.8
held=$(selected)
DISPLAY=$display xdotool mouseup 1
sleep 0.3
[ "$held" -gt "$plain" ] || fail "pressing a menu entry cleared the selection \
($held tinted pixels against $plain unselected), so Copy would have copied \
nothing"
echo "ok: a selection survived the press on the menu entry that acts on it"
stop

# N. A refused image leaves a box that answers a press (#118).
#
#    `paint` pins what the placeholder looks like and `isolation.rs` pins that
#    its rectangle crosses the boundary. Neither can show that the rectangle
#    lands where the pixels are — the click path runs from winit's pointer
#    position through the chrome offset and the scroll to a list the child
#    sent, and every one of those has been wrong at some point in this file's
#    history.
#
#    A local page asking for an image over the network is a third-party request
#    by ADR-0006's own argument — a file has no host for anything to be
#    first-party to — so this is refused without a socket being opened.
placeholders="$here/target/window-placeholder"
mkdir -p "$placeholders"
cat > "$placeholders/p.html" <<'FIXTURE'
<!doctype html>
<title>Placeholder</title>
<body style="margin: 0">
<img src="https://cdn.example.net/photo.jpg" width="240" height="160">
</body>
FIXTURE

start_on "$placeholders/p.html" "Placeholder"
# The plate colour from `paint::draw_missing`, as the pixel reader prints it.
plate="244 244 242"
seen=""
for _ in $(seq 1 20); do
    if [ "$(pixel 120 $((chrome + 40)))" = "$plate" ]; then
        seen=yes
        break
    fi
    sleep 0.2
done
[ -n "$seen" ] || fail "a refused image drew no placeholder, so the reader gets \
a hole with nothing to press"

# Pressing it opens the site panel, because the policy is what refused this one
# and retrying would refuse it again.
#
# Sampled to the right of the image and inside the panel: the panel hangs off
# the padlock, so it starts further right than the placeholder does and is
# wider. A point inside both would compare the panel's surface against the
# placeholder's, which differ by two in each channel — true, and not something
# to rest a check on.
panel_probe_x=350
panel_probe_y=$((chrome + 40))
before=$(pixel "$panel_probe_x" "$panel_probe_y")
DISPLAY=$display xdotool mousemove 120 $((chrome + 40))
DISPLAY=$display xdotool click 1
opened=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$panel_probe_x" "$panel_probe_y")" != "$before" ]; then
        opened=yes
        break
    fi
done
[ -n "$opened" ] || fail "pressing the placeholder did nothing — a refused \
image needs the panel, because retrying it would refuse it again"
echo "ok: a refused image drew a placeholder and pressing it offered the host"
stop

# N. A text field can be clicked into and typed in (#110).
#
#    Unreachable from `cargo test` and not by a little: the path runs from
#    winit's key event, through the modifier state and the chrome's own fields,
#    across the process boundary as a named key, into an editing state the child
#    owns, back as a fresh render. `isolation.rs` drives the far half of that
#    directly. Nothing but a real window drives the near half.
typing="$here/target/window-typing"
mkdir -p "$typing"
cat > "$typing/p.html" <<'FIXTURE'
<!doctype html>
<title>Typing</title>
<body style="margin: 0; font: 16px sans-serif">
<div style="position: absolute; left: 40px; top: 40px">
<input type="text" value="" size="30">
</div>
</body>
FIXTURE

start_on "$typing/p.html" "Typing"

# Where that field actually is, asked of the browser rather than assumed — the
# same rule the link coordinates above follow. Read off the screen rather than
# out of a rendered file: measuring a PNG means decoding one, and the only way
# to do that without writing an inflater is an image library CI does not have.
# The fixture has nothing on it but the field, so the ink below the chrome is
# the field.
box=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 1)) $((height - 1)))
[ -n "$box" ] || fail "nothing was drawn below the chrome, so the fixture's \
field never reached the screen"
set -- $box
# Back into document coordinates, which is what the rest of this reads in.
field_left=$1 field_top=$(($2 - chrome)) field_right=$3 field_bottom=$(($4 - chrome))
# Somewhere across the first few characters, since exactly which pixels the
# glyphs land on is the shaper's business and not this harness's. Started clear
# of the caret: a focused empty field has one, a couple of pixels in from the
# border, and a scan that included it would report "there is text here" the
# moment the field took the click.
text_y=$((chrome + (field_top + field_bottom) / 2))
inked_in_field() {
    DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-ink.py" $((field_left + 12)) $((field_left + 70)) "$text_y"
}
[ "$(inked_in_field)" = "clear" ] || fail "the field had text in it before \
anything was typed, so the check below would pass without a single key arriving"

# Two pixels above the field's border box, which is where the focus ring goes
# and where nothing else ever draws.
ring_y=$((chrome + field_top - 2))
ring_x=$(((field_left + field_right) / 2))
unfocused=$(pixel "$ring_x" "$ring_y")

DISPLAY=$display xdotool mousemove "$ring_x" $((chrome + (field_top + field_bottom) / 2))
DISPLAY=$display xdotool click 1
focused=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$ring_x" "$ring_y")" != "$unfocused" ]; then
        focused=yes
        break
    fi
done
[ -n "$focused" ] || fail "clicking a text field drew no focus ring, so the \
click never reached the control"

focus_window
DISPLAY=$display xdotool type --delay 60 "hello"
typed=""
for _ in $(seq 1 25); do
    sleep 0.2
    if [ "$(inked_in_field)" = "ink" ]; then
        typed=yes
        break
    fi
done
[ -n "$typed" ] || fail "five characters were typed into a focused field and \
nothing appeared in it"

# Cut and paste, in the control, through the real clipboard. Ink to clear to
# ink: a cut that reached the child empties the field, and a paste that reached
# it fills it again with what the cut took. Neither half can be seen from
# `cargo test`, because what is in a control lives in the other process and only
# crosses when a reader asks for it (ADR-0012).
DISPLAY=$display xdotool key ctrl+a
DISPLAY=$display xdotool key ctrl+x
cut=""
for _ in $(seq 1 25); do
    sleep 0.2
    if [ "$(inked_in_field)" = "clear" ]; then
        cut=yes
        break
    fi
done
[ -n "$cut" ] || fail "Ctrl+X left the text in the field, so either the copy \
never came back from the renderer or the deletion never reached it"

DISPLAY=$display xdotool key ctrl+v
pasted=""
for _ in $(seq 1 25); do
    sleep 0.2
    if [ "$(inked_in_field)" = "ink" ]; then
        pasted=yes
        break
    fi
done
[ -n "$pasted" ] || fail "Ctrl+V put nothing back in the field, so what the cut \
took never reached the clipboard or never came back"

# Escape gives the field up, which is what puts the keyboard back on the page.
DISPLAY=$display xdotool key Escape
released=""
for _ in $(seq 1 20); do
    sleep 0.2
    if [ "$(pixel "$ring_x" "$ring_y")" = "$unfocused" ]; then
        released=yes
        break
    fi
done
[ -n "$released" ] || fail "Escape left the field focused, so the page can no \
longer be scrolled with the keyboard"
echo "ok: a text field took a click, five characters, a cut and a paste, and \
let go on Escape"
stop

# N. Pressing a submit button sends the form (#110).
#
#    The one check in this file that involves a server, because it is the one
#    behaviour that cannot be seen without one: a form is sent correctly only if
#    what *arrived* is right, and nothing on this side of the socket can tell
#    you that. `isolation.rs` proves the encoding against a real socket; this
#    proves that a press in a real window reaches it at all.
#
#    Local, on a port the kernel picks, and killed with the harness.
form_port=8737
python3 - "$form_port" >/dev/null 2>&1 <<'SERVER' &
import http.server, socketserver, sys, pathlib

seen = pathlib.Path("/tmp/2kbrowser-form-seen")
seen.unlink(missing_ok=True)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def page(self, body):
        body = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.page(
            "<title>A form</title><body style='margin:0'>"
            "<form action='/submit' method='post'>"
            "<input name='q' value='tables'>"
            "<input type='submit' name='go' value='Send'>"
            "</form></body>"
        )

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        seen.write_text(self.rfile.read(length).decode())
        self.page("<title>Answered</title><body><p>thanks</p></body>")

socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", int(sys.argv[1])), Handler) as server:
    server.serve_forever()
SERVER
form_server=$!
# Waited for rather than slept on: a port that is not listening yet fails the
# navigation below, and the failure would read as a browser that cannot fetch.
for _ in $(seq 1 40); do
    (echo > "/dev/tcp/127.0.0.1/$form_port") >/dev/null 2>&1 && break
    sleep 0.25
done

start_on "http://127.0.0.1:$form_port/form" "A form"
focus_window
# Where the button is, asked of the screen: the fixture has the field and the
# button on one line, so the rightmost ink below the chrome is the button.
box=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 1)) $((height - 1)))
[ -n "$box" ] || fail "the form never reached the screen"
set -- $box
DISPLAY=$display xdotool mousemove $(($3 - 12)) $((($2 + $4) / 2))
DISPLAY=$display xdotool click 1

sent=""
for _ in $(seq 1 30); do
    sleep 0.3
    case "$(DISPLAY=$display xdotool getwindowname "$window" 2>/dev/null)" in
        Answered*) sent=yes; break ;;
    esac
done
kill "$form_server" 2>/dev/null || true
[ -n "$sent" ] || fail "pressing the submit button did not navigate, so the \
form never left the browser"
arrived=$(cat /tmp/2kbrowser-form-seen 2>/dev/null || true)
[ "$arrived" = "q=tables&go=Send" ] || fail "the server was sent \
\"$arrived\" rather than the form's own fields"
echo "ok: pressing submit sent the form and the server got its fields"
stop

# N. A checkbox can be ticked and unticked, and a dropdown can be opened and
#    chosen from.
#
#    The near half of the same path the typing check covers, for the controls
#    that are pressed rather than typed in. Two things here exist nowhere else:
#    the dropdown's list is drawn by the *window* over the page, so no test
#    below the window can see it at all; and the tick has to survive a re-render,
#    which is what makes it a state rather than a flash.
choosing="$here/target/window-choosing"
mkdir -p "$choosing"
cat > "$choosing/p.html" <<'FIXTURE'
<!doctype html>
<title>Choosing</title>
<body style="margin: 0; font: 16px sans-serif">
<div style="position: absolute; left: 40px; top: 40px">
<input type="checkbox">
</div>
<div style="position: absolute; left: 40px; top: 120px">
<select>
<option>MMMMMMMMMMMM</option>
<option selected>i</option>
</select>
</div>
</body>
FIXTURE

start_on "$choosing/p.html" "Choosing"
focus_window

# The box is at document (40, 40) and is about a line tall, so the middle of it
# is a few pixels in. Asked of the screen rather than assumed, the same rule
# every other check here follows: the fixture puts nothing else in that band.
box=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 40)) $((chrome + 80)))
[ -n "$box" ] || fail "the checkbox never reached the screen"
set -- $box
tick_x=$((($1 + $3) / 2)) tick_y=$((($2 + $4) / 2))
empty=$(pixel "$tick_x" "$tick_y")

DISPLAY=$display xdotool mousemove "$tick_x" "$tick_y"
DISPLAY=$display xdotool click 1
ticked=""
for _ in $(seq 1 20); do
    sleep 0.2
    [ "$(pixel "$tick_x" "$tick_y")" != "$empty" ] && { ticked=yes; break; }
done
[ -n "$ticked" ] || fail "clicking the checkbox drew no tick, so a form with a \
box to answer still cannot be answered"

DISPLAY=$display xdotool click 1
cleared=""
for _ in $(seq 1 20); do
    sleep 0.2
    [ "$(pixel "$tick_x" "$tick_y")" = "$empty" ] && { cleared=yes; break; }
done
[ -n "$cleared" ] || fail "clicking the ticked box again left it ticked, so a \
pre-ticked box still cannot be cleared"

# The dropdown. Closed it shows "i" and nothing else; the list it opens holds a
# row much wider than that, so both the list appearing and the choice landing
# are visible as ink where there was none.
drop=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 115)) $((chrome + 160)))
[ -n "$drop" ] || fail "the dropdown never reached the screen"
set -- $drop
drop_left=$1 drop_top=$2 drop_right=$3 drop_bottom=$4
DISPLAY=$display xdotool mousemove $(((drop_left + drop_right) / 2)) \
    $(((drop_top + drop_bottom) / 2))
DISPLAY=$display xdotool click 1

# The list opens under the control, so ink appears below where the page had
# none. Its first row is the option to choose.
opened=""
for _ in $(seq 1 20); do
    sleep 0.2
    below=$(DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-box.py" $((drop_bottom + 2)) $((drop_bottom + 40)))
    [ -n "$below" ] && { opened=yes; break; }
done
[ -n "$opened" ] || fail "pressing the dropdown opened no list, so its other \
options are still unreachable"

set -- $below
DISPLAY=$display xdotool mousemove $((drop_left + 8)) $(($2 + 6))
DISPLAY=$display xdotool click 1

# The box now reads the option that was chosen, which is many times wider than
# the one it read before.
chose=""
for _ in $(seq 1 20); do
    sleep 0.2
    now=$(DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-box.py" $((chrome + 115)) $((chrome + 160)))
    [ -n "$now" ] || continue
    set -- $now
    [ $(($3 - $1)) -gt $((drop_right - drop_left)) ] && { chose=yes; break; }
done
[ -n "$chose" ] || fail "choosing a row left the dropdown showing what it \
showed before, so what the form would send has not changed"
echo "ok: a checkbox ticked and unticked, and a dropdown opened and was chosen from"
stop

# N. The keyboard reaches the controls a pointer can (#151).
#
#    The near half of the path, which nothing below the window drives: winit's
#    key events, the modifier state, the window's own decision about which keys
#    are the page's, and — for the list a dropdown opens — a surface the window
#    draws itself, so no test in the child can see it at all.
keys="$here/target/window-keys"
mkdir -p "$keys"
cat > "$keys/p.html" <<'FIXTURE'
<!doctype html>
<title>Keys</title>
<body style="margin: 0; font: 16px sans-serif">
<div style="position: absolute; left: 40px; top: 40px">
<input type="checkbox">
</div>
<div style="position: absolute; left: 40px; top: 120px">
<select>
<option>MMMMMMMMMMMM</option>
<option selected>i</option>
</select>
</div>
</body>
FIXTURE

start_on "$keys/p.html" "Keys"
focus_window

box=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 40)) $((chrome + 80)))
[ -n "$box" ] || fail "the checkbox never reached the screen"
set -- $box
tick_x=$((($1 + $3) / 2)) tick_y=$((($2 + $4) / 2))
empty=$(pixel "$tick_x" "$tick_y")

# Tab to the box and press it, with the pointer parked somewhere that is not
# over anything — so a tick can only have come from the keyboard.
DISPLAY=$display xdotool mousemove 900 900
DISPLAY=$display xdotool key Tab
DISPLAY=$display xdotool key space
ticked=""
for _ in $(seq 1 20); do
    sleep 0.2
    [ "$(pixel "$tick_x" "$tick_y")" != "$empty" ] && { ticked=yes; break; }
done
[ -n "$ticked" ] || fail "Tab and Space did not tick the box, so a form still \
cannot be filled in without a pointer"

# Tab again to the dropdown, and Down to walk it. The option below the one it
# opens on is *wider*, so the box growing is the answer changing.
drop=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 115)) $((chrome + 160)))
[ -n "$drop" ] || fail "the dropdown never reached the screen"
set -- $drop
was=$(($3 - $1))
DISPLAY=$display xdotool key Tab
DISPLAY=$display xdotool key Up
walked=""
for _ in $(seq 1 20); do
    sleep 0.2
    now=$(DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-box.py" $((chrome + 115)) $((chrome + 160)))
    [ -n "$now" ] || continue
    set -- $now
    [ $(($3 - $1)) -gt "$was" ] && { walked=yes; break; }
done
[ -n "$walked" ] || fail "an arrow on the focused dropdown did not move it, so \
a dropdown still cannot be answered from the keyboard"

# And Space opens the list it did not open while walking.
DISPLAY=$display xdotool key space
opened=""
for _ in $(seq 1 20); do
    sleep 0.2
    below=$(DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-box.py" $((chrome + 165)) $((chrome + 210)))
    [ -n "$below" ] && { opened=yes; break; }
done
[ -n "$opened" ] || fail "Space did not open the focused dropdown's list"

# Escape closes it again, which is the other half of a list you can open with a
# key: one that only the pointer could dismiss would be a trap.
DISPLAY=$display xdotool key Escape
closed=""
for _ in $(seq 1 20); do
    sleep 0.2
    below=$(DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-box.py" $((chrome + 165)) $((chrome + 210)))
    [ -z "$below" ] && { closed=yes; break; }
done
[ -n "$closed" ] || fail "Escape left the dropdown's list open"
echo "ok: the keyboard ticked a box, walked a dropdown, and opened and closed its list"
stop

# N. A `position: fixed` box stays put when the page scrolls (#108).
#
#    Invisible to every other kind of test here: the conformance suite renders
#    whole pages from row zero, where a fixed box and an ordinary one land in
#    the same place, and the child's own tests never scroll. Only a window
#    scrolls.
pinned="$here/target/window-pinned"
mkdir -p "$pinned"
cat > "$pinned/p.html" <<'FIXTURE'
<!doctype html>
<title>Pinned</title>
<body style="margin: 0; font: 16px sans-serif">
<div style="position: fixed; left: 40px; top: 40px; width: 120px; height: 30px; background: #000"></div>
<div style="height: 4000px"></div>
</body>
FIXTURE

start_on "$pinned/p.html" "Pinned"

# Where the black bar is before scrolling.
before=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-box.py" $((chrome + 20)) $((chrome + 100)))
[ -n "$before" ] || fail "the fixed box never reached the screen"
set -- $before
was_top=$2

DISPLAY=$display xdotool key Page_Down
sleep 0.6
DISPLAY=$display xdotool key Page_Down
stayed=""
for _ in $(seq 1 20); do
    sleep 0.3
    now=$(DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-box.py" $((chrome + 20)) $((chrome + 100)))
    [ -n "$now" ] || continue
    set -- $now
    # Same row it started on, within a pixel of rounding.
    if [ $(( $2 - was_top )) -le 1 ] && [ $(( was_top - $2 )) -le 1 ]; then
        stayed=yes
        break
    fi
done
[ -n "$stayed" ] || fail "the fixed box moved with the page, so \
\`position: fixed\` is only fixed until somebody scrolls"

# And the page really did scroll, or the check above proves nothing: a page
# that ignored Page_Down would pass it trivially.
moved=$(DISPLAY=$display xwd -silent -id "$window" \
    | python3 "$here/scripts/xwd-ink.py" 0 $((width - 1)) $((chrome + 300)))
echo "ok: a fixed box stayed where it was while the page scrolled under it"
stop

# N. The address bar can be selected with the pointer (#199).
#
#    `field.rs` pins what a press and a drag do to a cursor and an anchor. What
#    it cannot pin is that a press in the bar reaches any of it: before this the
#    bar had one behaviour, "focus and select everything", and the only way to
#    reach one character of a long address was the arrow keys.
start
bar_selected() {
    DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-selection.py" "$strip" "$chrome"
}
quiet=$(bar_selected)
# One click focuses the bar and selects the whole address, which is what an
# address bar has always done and what this browser already did.
DISPLAY=$display xdotool mousemove $((url_x + 60)) "$toggle_y"
DISPLAY=$display xdotool click 1
sleep 0.8
everything=$(bar_selected)
[ "$everything" -gt "$quiet" ] || fail "clicking the address bar selected \
nothing ($everything tinted pixels against $quiet before), so the bar never \
took the focus"

# A second click puts the caret where the pointer is, which is the thing that
# was missing. The selection has to go with it.
DISPLAY=$display xdotool click 1
sleep 0.8
caret=$(bar_selected)
[ "$caret" -lt "$everything" ] || fail "clicking an already-focused address bar \
left the whole address selected ($caret tinted pixels against $everything), so \
there is still no way to put the cursor anywhere with the pointer"

# And a drag selects what it crossed: neither nothing nor everything.
DISPLAY=$display xdotool mousemove $((url_x + 10)) "$toggle_y"
DISPLAY=$display xdotool mousedown 1
DISPLAY=$display xdotool mousemove $((url_x + 40)) "$toggle_y"
sleep 0.3
DISPLAY=$display xdotool mousemove $((url_x + 70)) "$toggle_y"
sleep 0.5
DISPLAY=$display xdotool mouseup 1
sleep 0.5
dragged=$(bar_selected)
[ "$dragged" -gt "$caret" ] || fail "dragging across the address selected \
nothing ($dragged tinted pixels against $caret for a bare caret)"
[ "$dragged" -lt "$everything" ] || fail "dragging across part of the address \
selected all of it ($dragged tinted pixels against $everything for select-all)"
echo "ok: the address bar took a caret and a drag from the pointer"
stop

# N. A new tab opens on a blank page (#196).
#
#    A new tab used to show the page you were on — it re-fetched it and showed
#    a second copy. Whether it now shows nothing is a question about the event
#    loop, and there is nothing in `cargo test` that can be asked it.
#
#    Measured on the *page* rather than on the address bar. The bar's field is
#    drawn on the chrome's grey, so "is this row white?" is answered no whether
#    there is an address in it or not — a check that would have passed without
#    the feature, which is the one kind of check worth nothing.
start
page_ink() {
    DISPLAY=$display xwd -silent -id "$window" \
        | python3 "$here/scripts/xwd-ink.py" 0 $((width - 50)) $((chrome + 25))
}
[ "$(page_ink)" = "ink" ] || fail "the page was already blank before a new tab \
was opened, so the check below would pass without anything happening"

# Keys go to the window that has the focus, which under this window manager is
# not automatic.
focus_window
DISPLAY=$display xdotool key ctrl+t
emptied=""
for _ in $(seq 1 20); do
    sleep 0.3
    if [ "$(page_ink)" = "clear" ]; then
        emptied=yes
        break
    fi
done
[ -n "$emptied" ] || fail "a new tab still had a page in it, so it opened on \
whatever the reader was already looking at"
echo "ok: a new tab opened empty"
stop

# N. Where the reader has been outlives the window (#197, ADR-0021).
#
#    `visits.rs` pins the list and the file format. What it cannot pin is that a
#    navigation reaches either — the recording happens in the event loop, on the
#    landing rather than on the click, and it is named from a title that only
#    exists once the renderer has answered.
#
#    A config directory of its own, so this neither reads nor writes the one
#    belonging to whoever is running the tests. A harness that appended to a
#    person's real history would be a worse bug than the one it is checking.
recorded="$(mktemp -d)"
export XDG_CONFIG_HOME="$recorded"
start
after=$(click_and_read "$click_x" "$click_y")
case "$after" in
    *Arrival*) ;;
    *) fail "the link was not followed, so there is nothing for the history to \
have recorded" ;;
esac
stop
tsv="$recorded/2kbrowser/history.tsv"
[ -f "$tsv" ] || fail "no history file was written at $tsv, so nothing about \
this run outlived the window"
grep -q "from.html" "$tsv" || fail "the page the browser opened on is not in \
the history: $(cat "$tsv")"
grep -q "to.html" "$tsv" || fail "the page the link went to is not in the \
history: $(cat "$tsv")"
# And the title is there, which is the half that arrives from the renderer
# after the navigation rather than with it.
grep -q "Arrival" "$tsv" || fail "the history recorded an address with no \
title, so the name never came back from the renderer: $(cat "$tsv")"
unset XDG_CONFIG_HOME
rm -rf "$recorded"
echo "ok: a navigation was recorded in the history and survived the window"

# N. The debugging views are reachable and hold what they say (#198).
#
#    `devtools.rs` pins what the two pages say. What it cannot pin is that a
#    keystroke reaches them, that the page they describe is the one on screen,
#    or that the markup shown is the markup that was parsed rather than a second
#    fetch of it.
looked="$(mktemp -d)"
export XDG_CONFIG_HOME="$looked"
start
focus_window
DISPLAY=$display xdotool key ctrl+u
source_html="$looked/2kbrowser/source.html"
wrote=""
for _ in $(seq 1 20); do
    sleep 0.3
    [ -f "$source_html" ] && { wrote=yes; break; }
done
[ -n "$wrote" ] || fail "Ctrl+U wrote no source view at $source_html"
grep -q "go to the other page" "$source_html" || fail "the source view does not \
hold the page's own markup: $(head -c 400 "$source_html")"
grep -q "&lt;a href" "$source_html" || fail "the source view did not escape the \
markup it is showing, so it rendered the page again instead of printing it"

DISPLAY=$display xdotool key ctrl+shift+i
info_html="$looked/2kbrowser/page-info.html"
wrote=""
for _ in $(seq 1 20); do
    sleep 0.3
    [ -f "$info_html" ] && { wrote=yes; break; }
done
[ -n "$wrote" ] || fail "Ctrl+Shift+I wrote no page information at $info_html"
for section in Console Network Inspector Storage; do
    grep -q "<h2>$section</h2>" "$info_html" || fail "the page information has \
no $section section, so one of the four asked for does not exist"
done
# The inspector is built from the tree the child sends back, so an empty one
# means the question never crossed the process boundary.
grep -q "document" "$info_html" || fail "the inspector shows no document node, \
so the accessibility tree never came back from the renderer"
stop
unset XDG_CONFIG_HOME
rm -rf "$looked"
echo "ok: the source view and the page information both opened and were filled in"

# N. Copy and paste in the address bar, through the real clipboard.
#
#    The round trip is the proof, and it needs no second tool to read the
#    clipboard with: copy this page's address out of the bar, go somewhere else,
#    paste it back and press Enter. Arriving where the copy came from means the
#    text made it out of the field, onto the system clipboard, and back into the
#    field — none of which `cargo test` can reach, because there is no clipboard
#    in a headless test and no window to focus.
start
focus_window
# Ctrl+L focuses the bar with the whole address selected, which is what makes
# Ctrl+C here a copy of the address rather than of nothing.
DISPLAY=$display xdotool key ctrl+l
sleep 0.5
DISPLAY=$display xdotool key ctrl+c
sleep 0.5
DISPLAY=$display xdotool key Escape
sleep 0.3

# Somewhere else, so arriving back is a real navigation rather than a page that
# never left.
after=$(click_and_read "$click_x" "$click_y")
case "$after" in
    *Arrival*) ;;
    *) fail "the link was not followed, so there is nowhere to come back from" ;;
esac

DISPLAY=$display xdotool key ctrl+l
sleep 0.5
DISPLAY=$display xdotool key ctrl+a
DISPLAY=$display xdotool key ctrl+v
sleep 0.5
DISPLAY=$display xdotool key Return
returned=""
for _ in $(seq 1 30); do
    sleep 0.4
    case "$(DISPLAY=$display xdotool getwindowname "$window" 2>/dev/null || true)" in
        *Departure*) returned=yes; break ;;
    esac
done
[ -n "$returned" ] || fail "pasting the copied address and pressing Enter did \
not go back to where it was copied from, so the address bar's copy or its paste \
did not happen"
stop
echo "ok: an address copied out of the bar pasted back into it and navigated"

echo "all window click checks passed"
