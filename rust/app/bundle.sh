#!/usr/bin/env bash
# Build the native app in release mode and install it as ~/Applications/Agent Box.app.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p agent-box-app
app="$HOME/Applications/Agent Box.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/agent-box "$app/Contents/MacOS/AgentBox"
cat > "$app/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Agent Box</string>
  <key>CFBundleDisplayName</key><string>Agent Box</string>
  <key>CFBundleIdentifier</key><string>com.tj.agentbox</string>
  <key>CFBundleExecutable</key><string>AgentBox</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>2.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$app" > /dev/null 2>&1
echo "installed $app"
