#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
target=""
output="dist"
skip_build=0
while (($#)); do
  case "$1" in
    --target) target="${2:?missing target}"; shift 2 ;;
    --output) output="${2:?missing output directory}"; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

os="$(uname -s)"
if [[ -z "$target" ]]; then
  case "$os/$(uname -m)" in
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
    *) printf 'Unsupported host; use a supported native build host.\n' >&2; exit 2 ;;
  esac
fi
case "$os/$target" in
  Darwin/x86_64-apple-darwin|Darwin/aarch64-apple-darwin) export MACOSX_DEPLOYMENT_TARGET=13.0 ;;
  Linux/x86_64-unknown-linux-gnu) ;;
  *) printf 'Target does not match a supported packaging host: %s/%s\n' "$os" "$target" >&2; exit 2 ;;
esac
if ((skip_build == 0)); then cargo build --locked --release --workspace --target "$target"; fi
metadata="$(cargo metadata --locked --no-deps --format-version 1)"
version="$(printf '%s' "$metadata" | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["version"] for p in m["packages"] if any(t["name"]=="local-image-tools" for t in p["targets"])))')"
target_dir="$(printf '%s' "$metadata" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
mkdir -p "$output"
output="$(cd "$output" && pwd)"
stage="$(mktemp -d "$output/.stage-package.XXXXXXXX")"
# mktemp creates this private staging directory under the explicit output root.
trap 'rm -rf -- "$stage"' EXIT
common="$stage/common"
mkdir -p "$common"
cp LICENSE THIRD_PARTY_NOTICES.md README.md CONTRIBUTING.md CHANGELOG.md CONTEXT.md "$common/"
cp -R docs "$common/"
python3 packaging/copy-assets.py assets "$common/assets"
python3 packaging/collect-licenses.py "$common/dependency-licenses" --target "$target"
release_dir="$target_dir/$target/release"

if [[ "$os" == Darwin ]]; then
  arch=x64
  [[ "$target" == aarch64-apple-darwin ]] && arch=arm64
  base="local-image-tools-$version-macos-$arch"
  app="$stage/Local Image Tools.app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$release_dir/local-image-tools" "$release_dir/lit" "$app/Contents/MacOS/"
  cp -R "$common/." "$app/Contents/Resources/"
  cp assets/icons/app.icns "$app/Contents/Resources/app.icns"
  sed "s/@VERSION@/$version/g" packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
  plutil -lint "$app/Contents/Info.plist"
  # Linkers can apply an ad-hoc signature on arm64; there is no developer signing or notarization here.
  for artifact in "$output/$base.zip" "$output/$base.dmg"; do
    [[ ! -e "$artifact" ]] || { printf 'Package already exists: %s\n' "$artifact" >&2; exit 1; }
  done
  ditto -c -k --sequesterRsrc --keepParent "$app" "$output/$base.zip"
  dmg_root="$stage/dmg"
  mkdir "$dmg_root"
  cp -R "$app" "$dmg_root/"
  ln -s /Applications "$dmg_root/Applications"
  hdiutil create -quiet -volname 'Local Image Tools' -srcfolder "$dmg_root" -ov -format UDZO "$output/$base.dmg"
  (cd "$output" && shasum -a 256 "$base.zip" "$base.dmg" > "$base-SHA256SUMS.txt")
  printf '%s\n' "$output/$base.zip" "$output/$base.dmg"
else
  command -v dpkg-deb >/dev/null || { printf 'dpkg-deb is required.\n' >&2; exit 1; }
  base="local-image-tools-$version-linux-x64"
  deb_root="$stage/deb"
  mkdir -p "$deb_root/DEBIAN" "$deb_root/usr/bin" "$deb_root/usr/share/applications" \
    "$deb_root/usr/share/icons/hicolor/256x256/apps" "$deb_root/usr/share/doc/local-image-tools" \
    "$deb_root/usr/share/local-image-tools"
  install -m 755 "$release_dir/local-image-tools" "$release_dir/lit" "$deb_root/usr/bin/"
  install -m 644 packaging/linux/local-image-tools.desktop "$deb_root/usr/share/applications/"
  install -m 644 assets/icons/app.png "$deb_root/usr/share/icons/hicolor/256x256/apps/local-image-tools.png"
  cp -R "$common/assets" "$deb_root/usr/share/local-image-tools/"
  cp -R "$common/"* "$deb_root/usr/share/doc/local-image-tools/"
  # Keep assets in one canonical installed location, not duplicated in /usr/share/doc.
  rm -rf -- "$deb_root/usr/share/doc/local-image-tools/assets"
  cp LICENSE "$deb_root/usr/share/doc/local-image-tools/copyright"
  sed "s/@VERSION@/$version/g" packaging/linux/control.in > "$deb_root/DEBIAN/control"
  installed_size="$(du -sk "$deb_root/usr" | cut -f1)"
  printf 'Installed-Size: %s\n' "$installed_size" >> "$deb_root/DEBIAN/control"
  [[ ! -e "$output/$base.deb" ]] || { printf 'Package already exists: %s\n' "$output/$base.deb" >&2; exit 1; }
  dpkg-deb --root-owner-group --build "$deb_root" "$output/$base.deb"
  (cd "$output" && sha256sum "$base.deb" > "$base-SHA256SUMS.txt")
  printf '%s\n' "$output/$base.deb"
fi
