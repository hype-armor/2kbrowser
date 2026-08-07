#!/usr/bin/env bash
#
# Opens a window on a virtual display and checks it survives.
#
# The window is the one part of this browser with no automated coverage: CI has
# no display server, so event handling and blitting have always been exercised
# by hand and by nothing else. Xvfb closes most of that gap for the price of one
# CI step — not "does it look right", but "does it open, render a page in a
# child process, and stay up", which is what actually breaks.
#
# Linux only, and skipped rather than failed where Xvfb is missing: a check that
# cannot run must not look like a check that passed.
set -euo pipefail

BROWSER="${1:-./target/debug/2kbrowser}"
PAGE="${2:-tests/ref/fixtures/era-page.html}"
SECONDS_UP="${3:-6}"

if ! command -v Xvfb >/dev/null 2>&1; then
    echo "SKIP: Xvfb is not installed"
    exit 0
fi

DISPLAY_NUMBER=99
export DISPLAY=":${DISPLAY_NUMBER}"
Xvfb "$DISPLAY" -screen 0 1024x768x24 >/tmp/xvfb.log 2>&1 &
XVFB_PID=$!
trap 'kill "$XVFB_PID" 2>/dev/null || true' EXIT
sleep 3

"$BROWSER" open "$PAGE" --width 800 --height 600 >/tmp/window.out 2>/tmp/window.err &
APP_PID=$!
sleep "$SECONDS_UP"

# Still running is the whole assertion. A panic in the event loop, a renderer
# child that never answers, or a blit that indexes out of bounds all end with
# this process gone.
if ! kill -0 "$APP_PID" 2>/dev/null; then
    echo "FAIL: the window exited within ${SECONDS_UP}s"
    echo "--- stderr ---"
    cat /tmp/window.err
    exit 1
fi

# A renderer child should be alive alongside it, since the page is rendered out
# of process (ADR-0012). Its absence would mean the window fell back to
# something, which is exactly the regression worth catching.
if ! pgrep -f -- "--render-child" >/dev/null 2>&1; then
    echo "FAIL: no renderer child process is running"
    echo "--- stderr ---"
    cat /tmp/window.err
    exit 1
fi

kill "$APP_PID" 2>/dev/null || true
wait "$APP_PID" 2>/dev/null || true

# Anything on stderr is a panic message or a warning worth seeing; the renderer
# child inherits stderr on purpose.
if [ -s /tmp/window.err ]; then
    echo "FAIL: the window wrote to stderr"
    cat /tmp/window.err
    exit 1
fi

echo "ok: the window opened, rendered out of process, and stayed up for ${SECONDS_UP}s"
