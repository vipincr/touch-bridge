#!/bin/zsh
# Builds TouchNext and installs it as the copy you use daily: ~/Applications/TouchNext.app.
# Rebuilding in the project folder never touches that copy; run this script to update it.
#
# "Start at Login" is a per-user launch agent (~/Library/LaunchAgents/com.gramini.touchnext.agent.plist)
# that runs the installed executable by path, so updating the app in place keeps it working.
# The first install turns it on (the app writes the agent itself).
set -euo pipefail

cd "${0:A:h}/.."
DEST="$HOME/Applications/TouchNext.app"
LABEL="com.gramini.touchnext.agent"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"
JOB="gui/$(id -u)/$LABEL"

./scripts/bundle.sh

# Quit every running TouchNext (a normal Quit: the agent does not restart it), so the panel is free.
pkill -TERM -x TouchNext || true
for _ in {1..40}; do pgrep -x TouchNext >/dev/null || break; sleep 0.25; done
if pgrep -x TouchNext >/dev/null; then echo "TouchNext did not quit; aborting." >&2; exit 1; fi

mkdir -p "$DEST"
rsync -a --delete build/TouchNext.app/ "$DEST/"
codesign --verify "$DEST"

if [[ -f "$AGENT" ]]; then
    launchctl kickstart "$JOB" 2>/dev/null || launchctl bootstrap "gui/$(id -u)" "$AGENT"
    for _ in {1..32}; do launchctl print "$JOB" 2>/dev/null | grep -qE '^[[:space:]]*state = running' && break; sleep 0.25; done
    if launchctl print "$JOB" 2>/dev/null | grep -qE '^[[:space:]]*state = running'; then
        echo "Installed $DEST and restarted the login item."
    else
        echo "Installed $DEST, but the login item did not start; opening the app directly." >&2
        open "$DEST"
        exit 1
    fi
else
    open "$DEST" --args --enable-login-item
    echo "Installed $DEST; it turns on Start at Login on first start."
fi
