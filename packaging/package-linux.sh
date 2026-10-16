#!/bin/sh
# Package the Linux release tarball (experimental) from the release builds (run from the repo
# root after `cargo build --release` in the root and in gui/). Used by CI (to check it) and
# Release.
set -eu
version="$1"
name="lghub-lite-$version-linux-x64-experimental"
rm -rf "$name"
mkdir -p "$name/fonts"
cp target/release/lghub-lite target/release/lghub-lite-bg gui/target/release/lghub-lite-gui "$name/"
cp -r packaging/linux/70-lghub-lite.rules packaging/linux/lghub-lite.desktop packaging/linux/icons "$name/"
cp packaging/linux/INSTALL-linux.md README.md LICENSE "$name/"
cp gui/fonts/*.txt gui/fonts/README.md "$name/fonts/"
tar czf "$name.tar.gz" "$name"
sha256sum "$name.tar.gz" > "$name.tar.gz.sha256"
ls -l "$name.tar.gz" "$name.tar.gz.sha256"
tar tzf "$name.tar.gz"
