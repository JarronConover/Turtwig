#!/usr/bin/env bash
# Build "Turtwig.app" into ~/Applications (ad-hoc signed).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
app="$HOME/Applications/Turtwig.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
swiftc -O -target arm64-apple-macos14.0 -o "$app/Contents/MacOS/Turtwig" "$here/main.swift"
cat > "$app/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Turtwig</string>
  <key>CFBundleDisplayName</key><string>Turtwig</string>
  <key>CFBundleIdentifier</key><string>com.jarron.turtwig</string>
  <key>CFBundleExecutable</key><string>Turtwig</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppTransportSecurity</key>
  <dict><key>NSAllowsLocalNetworking</key><true/></dict>
</dict>
</plist>
EOF
codesign --force --sign - "$app" > /dev/null 2>&1
echo "built $app"
