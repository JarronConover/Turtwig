#!/usr/bin/env bash
# Build the native app and this Mac's daemon in release mode and install the app as ~/Applications/Turtwig.app.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p agent-box-app -p agent-boxd
app="$HOME/Applications/Turtwig.app"
# The app was a.box (and Agent Box) before the fork; clear those bundles too.
rm -rf "$app" "$HOME/Applications/a.box.app" "$HOME/Applications/Agent Box.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/agent-box "$app/Contents/MacOS/Turtwig"
# The bundle icon comes from the settings' logo, like the Dock icon the running app shows.
iconset=$(mktemp -d)/AppIcon.iconset
"$app/Contents/MacOS/Turtwig" --iconset "$iconset" && iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Turtwig</string>
  <key>CFBundleDisplayName</key><string>Turtwig</string>
  <key>CFBundleIdentifier</key><string>com.jarron.turtwig</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleExecutable</key><string>Turtwig</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>2.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# A real (even self-signed) signature lets macOS show its notifications and Dock badge; ad-hoc can't.
identity=$(security find-identity -p codesigning 2> /dev/null | awk -F'"' '/Turtwig Local Signing/ {print $2; exit}')
# allow-jit lets wasmtime compile scripts to machine code.
codesign --force --sign "${identity:--}" --entitlements app/entitlements.plist "$app" > /dev/null 2>&1
echo "installed $app"
