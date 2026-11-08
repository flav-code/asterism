#!/bin/sh
# Builds Asterism.app and a .dmg in dist/, with macOS's own tools only (iconutil-made icon, codesign, hdiutil).
# The app is signed ad hoc: it runs here and on Apple Silicon, but Gatekeeper asks to confirm it on other Macs
# until it is signed with a Developer ID and notarized.
set -eu
cd "$(dirname "$0")/.."

name=Asterism
id=io.github.flav-code.asterism
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(uname -m)

CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4} cargo build --release -p asterism

app="dist/$name.app"
rm -rf "$app" dist/dmg
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/asterism "$app/Contents/MacOS/asterism"
cp crates/app/assets/icon.icns "$app/Contents/Resources/icon.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleDisplayName</key><string>$name</string>
  <key>CFBundleIdentifier</key><string>$id</string>
  <key>CFBundleExecutable</key><string>asterism</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
codesign --force --deep --sign - "$app"

# The disk image: the app next to a link to /Applications, to drag it there.
mkdir -p dist/dmg
cp -R "$app" dist/dmg/
ln -s /Applications dist/dmg/Applications
dmg="dist/$name-$version-macos-$arch.dmg"
rm -f "$dmg"
hdiutil create -volname "$name" -srcfolder dist/dmg -ov -format UDZO "$dmg" >/dev/null
rm -rf dist/dmg
echo "$app"
echo "$dmg"
