#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mode="${1:-debug}"
case "$mode" in
 debug) cargo build --locked -p social-publisher ;;
 release) cargo build --locked --release -p social-publisher ;;
 *) echo 'Usage: scripts/package-macos.sh [debug|release]' >&2; exit 2 ;;
esac
icon_source='crates/desktop/assets/AirsIcon.png'
icon_tmp=$(mktemp -d "${TMPDIR:-/tmp}/airs-icon.XXXXXX")
trap 'rm -rf "$icon_tmp"' EXIT
iconset="$icon_tmp/AirsIcon.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon_source" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    retina_size=$((size * 2))
    sips -z "$retina_size" "$retina_size" "$icon_source" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns -o "$icon_tmp/AirsIcon.icns" "$iconset"

bundle='target/package/Airs.app'
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp -X "$icon_tmp/AirsIcon.icns" "$bundle/Contents/Resources/AirsIcon.icns"
# Replace through a temporary path so a running development app is not overwritten in place.
cp -X "target/$mode/social-publisher" "$bundle/Contents/MacOS/social-publisher.new"
mv "$bundle/Contents/MacOS/social-publisher.new" "$bundle/Contents/MacOS/social-publisher"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>social-publisher</string>
<key>CFBundleIdentifier</key><string>com.rustsocialpublisher.desktop</string>
<key>CFBundleName</key><string>Airs</string>
<key>CFBundleDisplayName</key><string>Airs</string>
<key>CFBundleIconFile</key><string>AirsIcon.icns</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Finder / synced folders can attach presentation metadata that codesign rejects.
# Remove only these non-security attributes from our generated bundle.
xattr -dr com.apple.FinderInfo "$bundle" 2>/dev/null || true
xattr -dr com.apple.ResourceFork "$bundle" 2>/dev/null || true
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
echo "已生成本机开发版：$bundle"
