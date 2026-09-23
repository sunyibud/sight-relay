#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
APP_VERSION="${APP_VERSION:-$(sed -n 's/^version = "\([^\"]*\)"/\1/p' crates/capture-client/Cargo.toml | head -n 1)}"
./deploy/build-roi.sh release
cargo build --quiet --release -p sight-relay-capture --bin settings --bin capture-client
rm -rf dist/SightRelay.app dist/SightRelay.dmg
mkdir -p dist/SightRelay.app/Contents/MacOS dist/SightRelay.app/Contents/Resources
cp web/capture-client.html dist/SightRelay.app/Contents/Resources/capture-client.html
cp target/release/roi-overlay dist/SightRelay.app/Contents/MacOS/roi-overlay
cp target/release/settings dist/SightRelay.app/Contents/MacOS/SightRelay
cp target/release/capture-client dist/SightRelay.app/Contents/MacOS/capture-client
cat > dist/SightRelay.app/Contents/Info.plist <<PLIST
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleName</key><string>Sight Relay Capture</string><key>CFBundleIdentifier</key><string>com.sightrelay.capture</string><key>CFBundleExecutable</key><string>SightRelay</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>${APP_VERSION}</string><key>CFBundleVersion</key><string>${APP_VERSION}</string><key>NSScreenCaptureUsageDescription</key><string>Sight Relay needs screen recording access to capture selected screens.</string><key>NSInputMonitoringUsageDescription</key><string>Sight Relay needs input monitoring access only when you enable a custom mouse capture trigger.</string></dict></plist>
PLIST
if [[ -n "${APPLE_SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" dist/SightRelay.app/Contents/MacOS/roi-overlay
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" dist/SightRelay.app/Contents/MacOS/capture-client
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" dist/SightRelay.app
  codesign --verify --deep --strict --verbose=2 dist/SightRelay.app
fi
rm -rf dist/dmg-root
mkdir -p dist/dmg-root
cp -R dist/SightRelay.app dist/dmg-root/SightRelay.app
ln -s /Applications dist/dmg-root/Applications
hdiutil create -volname SightRelay -srcfolder dist/dmg-root -ov -format UDZO dist/SightRelay.dmg
if [[ -n "${APPLE_NOTARY_PROFILE:-}" ]]; then
  xcrun notarytool submit dist/SightRelay.dmg --keychain-profile "$APPLE_NOTARY_PROFILE" --wait
  xcrun stapler staple dist/SightRelay.dmg
  xcrun stapler validate dist/SightRelay.dmg
fi
