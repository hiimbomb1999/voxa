#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
root=$(pwd)
manifest="$root/src-tauri/Cargo.toml"
export MACOSX_DEPLOYMENT_TARGET=14.2
cargo build --locked --release --features custom-protocol --manifest-path "$manifest"
target_dir="${CARGO_TARGET_DIR:-$root/src-tauri/target}"
app="$root/release/Voxa.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target_dir/release/voxa" "$app/Contents/MacOS/Voxa"
cp src-tauri/Info.plist "$app/Contents/Info.plist"
plist="$app/Contents/Info.plist"
plutil -insert CFBundleName -string Voxa "$plist"
plutil -insert CFBundleDisplayName -string Voxa "$plist"
plutil -insert CFBundleIdentifier -string local.voxa.translate "$plist"
plutil -insert CFBundleExecutable -string Voxa "$plist"
plutil -insert CFBundlePackageType -string APPL "$plist"
plutil -insert CFBundleShortVersionString -string 0.1.0 "$plist"
plutil -insert CFBundleVersion -string 1 "$plist"
plutil -insert LSMinimumSystemVersion -string 14.2 "$plist"
plutil -insert NSHighResolutionCapable -bool true "$plist"
cp src-tauri/icons/icon.png "$app/Contents/Resources/Voxa.png"
plutil -insert CFBundleIconFile -string Voxa.png "$plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
printf '%s\n' "$app"
