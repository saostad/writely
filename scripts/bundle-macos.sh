#!/bin/sh
# Build the release binary and package it as a macOS app bundle so macOS
# treats Writely as its own app in Privacy & Security > Accessibility.
# Writes only inside the repo (target/release/Writely.app).
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "${VERSION:-}" ]; then
  echo "bundle-macos: could not read the version from Cargo.toml" >&2
  exit 1
fi

echo "bundle-macos: building the release binary (version $VERSION)..."
cargo build --release || {
  echo "bundle-macos: cargo build --release failed (exit $?)" >&2
  exit 1
}

APP="target/release/Writely.app"
if [ ! -f "target/release/writely" ]; then
  echo "bundle-macos: expected binary target/release/writely is missing after the build" >&2
  exit 1
fi

echo "bundle-macos: creating $APP from scratch..."
rm -rf "$APP" || {
  echo "bundle-macos: could not remove the old $APP" >&2
  exit 1
}
mkdir -p "$APP/Contents/MacOS" || {
  echo "bundle-macos: could not create $APP/Contents/MacOS" >&2
  exit 1
}
cp "target/release/writely" "$APP/Contents/MacOS/Writely" || {
  echo "bundle-macos: could not copy the release binary into the bundle" >&2
  exit 1
}
chmod +x "$APP/Contents/MacOS/Writely"

printf 'APPL????' > "$APP/Contents/PkgInfo" || {
  echo "bundle-macos: could not write $APP/Contents/PkgInfo" >&2
  exit 1
}

cat > "$APP/Contents/Info.plist" <<EOF || {
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>dev.writely.Writely</string>
	<key>CFBundleName</key>
	<string>Writely</string>
	<key>CFBundleDisplayName</key>
	<string>Writely</string>
	<key>CFBundleExecutable</key>
	<string>Writely</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF
  echo "bundle-macos: could not write $APP/Contents/Info.plist" >&2
  exit 1
}

echo "bundle-macos: validating Info.plist..."
plutil -lint "$APP/Contents/Info.plist" || {
  echo "bundle-macos: plutil -lint failed for $APP/Contents/Info.plist (exit $?)" >&2
  exit 1
}

echo "bundle-macos: ad-hoc signing the bundle..."
codesign --force --sign - --identifier dev.writely.Writely "$APP" || {
  echo "bundle-macos: codesign failed for $APP (exit $?)" >&2
  exit 1
}

echo "bundle-macos: verifying the signature..."
codesign --verify --strict "$APP" || {
  echo "bundle-macos: codesign --verify --strict failed for $APP (exit $?)" >&2
  exit 1
}

echo "bundle-macos: done: $APP"
echo
echo "Next steps:"
echo "1. Run: open target/release/Writely.app (or drag it to Applications first, then open it from there)."
echo "2. Turn on Writely in System Settings > Privacy & Security > Accessibility."
echo "3. Quit and reopen Writely."
