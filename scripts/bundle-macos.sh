#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo 'Usage: scripts/bundle-macos.sh [--universal] [--sign "identity"] [--zip] [--dmg]'
}

die() {
    echo "Error: $*" >&2
    exit 1
}

universal=false
zip=false
dmg=false
identity=-
while (($#)); do
    case "$1" in
        --universal) universal=true ;;
        --zip) zip=true ;;
        --dmg) dmg=true ;;
        --sign)
            (($# >= 2)) && [[ -n "$2" ]] || die '--sign needs a signing identity.'
            identity=$2
            shift
            ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "Unknown option: $1" ;;
    esac
    shift
done

[[ $(uname -s) == Darwin ]] || die 'Run this script on macOS.'
cd "$(dirname "$0")/.."
for tool in cargo rustc plutil sips iconutil codesign ditto; do
    command -v "$tool" >/dev/null || die "Required tool not found: $tool"
done

if $dmg; then
    command -v hdiutil >/dev/null || die 'Required tool not found: hdiutil'
fi

export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
[[ $MACOSX_DEPLOYMENT_TARGET =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] ||
    die 'MACOSX_DEPLOYMENT_TARGET must be a version such as 13.0.'
version=$(awk '
    /^\[workspace.package\]$/ { package = 1; next }
    /^\[/ { package = 0 }
    package && /^version *=/ { split($0, parts, "\""); print parts[2]; exit }
' Cargo.toml)
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die 'Expected a numeric workspace version in Cargo.toml.'

client=${TINYTRAC_GOOGLE_CLIENT:-$HOME/.config/tinytrac/google-client.json}
# An explicit environment pair wins over the local JSON file. Reject partial pairs.
if [[ -n ${TINYTRAC_GOOGLE_CLIENT_ID:-} || -n ${TINYTRAC_GOOGLE_CLIENT_SECRET:-} ]]; then
    [[ -n ${TINYTRAC_GOOGLE_CLIENT_ID:-} && -n ${TINYTRAC_GOOGLE_CLIENT_SECRET:-} ]] ||
        die 'Set both Google client environment variables, or neither.'
    export TINYTRAC_GOOGLE_CLIENT_ID TINYTRAC_GOOGLE_CLIENT_SECRET
    echo 'Building with Google sync credentials from the environment.'
elif [[ -f "$client" ]]; then
    TINYTRAC_GOOGLE_CLIENT_ID=$(plutil -extract installed.client_id raw -o - "$client")
    TINYTRAC_GOOGLE_CLIENT_SECRET=$(plutil -extract installed.client_secret raw -o - "$client")
    [[ -n "$TINYTRAC_GOOGLE_CLIENT_ID" && -n "$TINYTRAC_GOOGLE_CLIENT_SECRET" ]] ||
        die 'Google client ID and secret must both be nonempty.'
    export TINYTRAC_GOOGLE_CLIENT_ID TINYTRAC_GOOGLE_CLIENT_SECRET
    echo 'Building with Google sync credentials from JSON.'
else
    unset TINYTRAC_GOOGLE_CLIENT_ID TINYTRAC_GOOGLE_CLIENT_SECRET
    echo 'No Google credentials; building without Google sync.'
fi

host=$(rustc -vV | sed -n 's/^host: //p')
targets=("$host")
if $universal; then
    command -v rustup >/dev/null || die 'Universal builds require rustup.'
    command -v lipo >/dev/null || die 'Universal builds require lipo (Xcode Command Line Tools).'
    targets=(aarch64-apple-darwin x86_64-apple-darwin)
    installed=$(rustup target list --installed)
    for target in "${targets[@]}"; do
        if ! grep -qx "$target" <<< "$installed"; then
            die "Missing Rust target $target. Run: rustup target add $target"
        fi
    done
fi

# Keep build and bundle paths predictable even with CARGO_TARGET_DIR configured.
export CARGO_TARGET_DIR="$PWD/target"
for target in "${targets[@]}"; do
    cargo build --release --locked -p tinytrac --target "$target"
done

mkdir -p target/bundle
stage=$(mktemp -d "$PWD/target/bundle/.tinytrac.XXXXXX")
trap 'rm -rf "$stage"' EXIT
app="$stage/tinytrac.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
if $universal; then
    lipo -create target/aarch64-apple-darwin/release/tinytrac \
        target/x86_64-apple-darwin/release/tinytrac -output "$app/Contents/MacOS/tinytrac"
else
    cp "target/$host/release/tinytrac" "$app/Contents/MacOS/tinytrac"
fi

# assets/icon.png was rendered once with Pillow: the tray's opaque green clock
# and white hands, with ~10% transparent padding and supersampled smooth edges.
iconset="$stage/tinytrac.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/tinytrac.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>tinytrac</string>
    <key>CFBundleDisplayName</key><string>tinytrac</string>
    <key>CFBundleIdentifier</key><string>net.expertstudio.tinytrac</string>
    <key>CFBundleExecutable</key><string>tinytrac</string>
    <key>CFBundleIconFile</key><string>tinytrac.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign "$identity" "$app"
# Verify integrity and the designated requirement, without a trust/Gatekeeper policy.
# A self-signed identity is valid here; do not add an "anchor trusted" requirement.
codesign --verify --deep --strict --verbose=2 "$app"
echo 'Code signature verification passed.'

destination="$PWD/target/bundle/tinytrac.app"
rm -rf "$destination"
mv "$app" "$destination"
echo "Created $destination"
if $zip; then
    archive="$PWD/target/bundle/tinytrac-$version-macos.zip"
    ditto -c -k --keepParent "$destination" "$archive"
    echo "Created $archive"
fi

if $dmg; then
    image_root="$stage/dmg"
    mkdir -p "$image_root"
    ditto "$destination" "$image_root/tinytrac.app"
    ln -s /Applications "$image_root/Applications"
    archive="$PWD/target/bundle/tinytrac-$version-macos.dmg"
    hdiutil create -ov -format UDZO -volname tinytrac -srcfolder "$image_root" "$archive"
    echo "Created $archive"
fi

cat <<'EOF'
Next steps:
  1. Copy tinytrac.app to /Applications on each Mac.
  2. Open it. If quarantined, use System Settings > Privacy & Security > Open Anyway.
  3. Add tinytrac to System Settings > General > Login Items.
Reopen the window from the menu bar; the app has no Dock icon.
EOF
