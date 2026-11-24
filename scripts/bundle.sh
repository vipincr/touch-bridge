#!/bin/zsh
# Builds TouchNext.app into ./build and signs it.
# Signing with a stable identity keeps macOS privacy grants (Accessibility, Input Monitoring) across rebuilds;
# ad-hoc signing works but macOS forgets the grants after every build.
set -euo pipefail

cd "${0:A:h}/.."
IDENTITY="${TOUCHNEXT_SIGN_IDENTITY:-TouchNext Dev}"
APP="build/TouchNext.app"

swift build -c release --product TouchNextApp

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp .build/release/TouchNextApp "$APP/Contents/MacOS/TouchNext"
cp Resources/Info.plist "$APP/Contents/Info.plist"
mkdir -p "$APP/Contents/Library/LaunchAgents"
cp Resources/com.gramini.touchnext.plist "$APP/Contents/Library/LaunchAgents/"

if security find-identity -p codesigning | grep -qF "\"$IDENTITY\""; then
    codesign --force --sign "$IDENTITY" "$APP"
    echo "Signed with \"$IDENTITY\"."
else
    # An ad-hoc build looks like a different app to the privacy system: installed over a signed copy,
    # it takes the Accessibility and Input Monitoring grants away from it.
    echo "error: signing identity \"$IDENTITY\" is not in the keychain, so nothing was signed." >&2
    echo "Set TOUCHNEXT_SIGN_IDENTITY to one of the identities below and run this again:" >&2
    security find-identity -v -p codesigning | sed -E 's/^ *[0-9]+\) [0-9A-F]+ //' >&2
    exit 1
fi

codesign --verify --verbose=1 "$APP"
echo "Built $PWD/$APP"
