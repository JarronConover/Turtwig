#!/usr/bin/env bash
# Build "Agent Box.app" into ~/Applications (ad-hoc signed).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
app="$HOME/Applications/Agent Box.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
swiftc -O -target arm64-apple-macos14.0 -o "$app/Contents/MacOS/AgentBox" "$here/main.swift"
cat > "$app/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Agent Box</string>
  <key>CFBundleDisplayName</key><string>Agent Box</string>
  <key>CFBundleIdentifier</key><string>com.tj.agentbox</string>
  <key>CFBundleExecutable</key><string>AgentBox</string>
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
