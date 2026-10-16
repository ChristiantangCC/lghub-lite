#!/bin/sh
# Package the macOS release tarball from the release builds (run from the repo root after
# `cargo build --release` in the root and in gui/). The app goes into a minimal lghub-lite.app
# bundle so it opens from Finder without a Terminal window. Used by CI (to check it) and
# Release.
set -eu
version="$1"
arch="$(uname -m)"
name="lghub-lite-$version-macos-$arch"
rm -rf "$name"
mkdir -p "$name/fonts" "$name/lghub-lite.app/Contents/MacOS"
cp target/release/lghub-lite target/release/lghub-lite-bg "$name/"
cp gui/target/release/lghub-lite-gui "$name/lghub-lite.app/Contents/MacOS/"
sed "s/VERSION/${version#v}/g" packaging/macos/Info.plist > "$name/lghub-lite.app/Contents/Info.plist"
cp packaging/macos/INSTALL-macos.md README.md LICENSE "$name/"
cp gui/fonts/*.txt gui/fonts/README.md "$name/fonts/"
tar czf "$name.tar.gz" "$name"
shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256"
ls -l "$name.tar.gz" "$name.tar.gz.sha256"
tar tzf "$name.tar.gz"
