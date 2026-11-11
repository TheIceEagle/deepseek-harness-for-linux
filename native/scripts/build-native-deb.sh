#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
native_dir="$(CDPATH= cd -- "$script_dir/.." && pwd)"
repo_dir="$(CDPATH= cd -- "$native_dir/.." && pwd)"
package_name=deepseek-harness-native
binary_name=deepseek-harness-native
desktop_id=io.github.theiceeagle.DeepSeekHarnessNative

for build_tool in cargo python3 dpkg-deb dpkg-shlibdeps dpkg-architecture; do
  command -v "$build_tool" >/dev/null || {
    printf 'Missing build tool: %s\n' "$build_tool" >&2
    exit 1
  }
done

architecture="$(dpkg-architecture -qDEB_HOST_ARCH)"
if [ "$architecture" != amd64 ]; then
  printf 'The native preview package currently targets Linux amd64.\n' >&2
  exit 1
fi

metadata="$(cargo metadata --manifest-path "$native_dir/Cargo.toml" --no-deps --format-version 1 --locked)"
app_version="$(python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "harness-native-app"))' <<< "$metadata")"
target_dir="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<< "$metadata")"
package_version="$(python3 -c 'import sys; base, separator, prerelease = sys.argv[1].partition("-"); print(base + ("~" + prerelease.replace(".", "") if separator else ""))' "$app_version")"

cargo build --manifest-path "$native_dir/Cargo.toml" --release --locked -p harness-native-app
cargo run --manifest-path "$native_dir/Cargo.toml" --locked -p harness-native-plugins --example build-plugin
binary="$target_dir/release/$binary_name"
test -x "$binary"

work_dir="$(mktemp -d -t deepseek-native-package-XXXXXXXX)"
trap 'rm -rf -- "$work_dir"' EXIT
staging="$work_dir/debian/$package_name"
install -Dm755 "$binary" "$staging/usr/bin/$binary_name"
install -Dm644 "$repo_dir/LICENSE" "$staging/usr/share/doc/$package_name/copyright"
install -Dm644 "$native_dir/README.md" "$staging/usr/share/doc/$package_name/README.md"
install -Dm644 "$native_dir/THIRD_PARTY_NOTICES.md" "$staging/usr/share/doc/$package_name/THIRD_PARTY_NOTICES.md"
for documentation in "$native_dir"/docs/*.md; do
  install -Dm644 "$documentation" "$staging/usr/share/doc/$package_name/docs/$(basename -- "$documentation")"
done
install -Dm644 "$native_dir/compat/README.md" "$staging/usr/share/doc/$package_name/compat/README.md"
python3 "$script_dir/collect-licenses.py" "$staging/usr/share/doc/$package_name/third-party-licenses"
install -Dm644 "$repo_dir/src-tauri/icons/icon.png" "$staging/usr/share/icons/hicolor/128x128/apps/$package_name.png"
for example_file in plugin.json plugin.wasm; do
  install -Dm644 "$native_dir/examples/uppercase/$example_file" "$staging/usr/share/$package_name/examples/uppercase/$example_file"
done
for compat_file in "$native_dir"/compat/*.mjs; do
  install -Dm644 "$compat_file" "$staging/usr/share/$package_name/compat/$(basename -- "$compat_file")"
done
mkdir -p "$staging/usr/share/applications" "$staging/DEBIAN" "$work_dir/debian"

cat > "$staging/usr/share/applications/$desktop_id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=DeepSeek Harness for Linux (Native Preview)
Comment=Unofficial native Rust chat client for DeepSeek
Exec=$binary_name
Icon=$package_name
Terminal=false
Categories=Office;Utility;
StartupNotify=true
StartupWMClass=$desktop_id
EOF

# dpkg-shlibdeps needs a source-control context even for this standalone package.
cat > "$work_dir/debian/control" <<EOF
Source: $package_name
Section: utils
Priority: optional
Maintainer: DeepSeek Harness for Linux contributors <noreply@github.com>

Package: $package_name
Architecture: any
Description: Unofficial native Rust chat client for DeepSeek
EOF
dependencies="$(cd "$work_dir" && dpkg-shlibdeps -O -e"$staging/usr/bin/$binary_name")"
dependencies="${dependencies#shlibs:Depends=}"
test -n "$dependencies"
installed_size="$(du -sk "$staging" | cut -f1)"

cat > "$staging/DEBIAN/control" <<EOF
Package: $package_name
Version: $package_version
Architecture: $architecture
Section: utils
Priority: optional
Maintainer: DeepSeek Harness for Linux contributors <noreply@github.com>
Installed-Size: $installed_size
Depends: $dependencies
Description: Unofficial native Rust chat client for DeepSeek
 Native GTK4 preview with a Rust streaming backend and bounded WASM commands.
 This independent application is not affiliated with or endorsed by DeepSeek.
EOF

output_dir="$native_dir/dist"
mkdir -p "$output_dir"
output="$output_dir/${package_name}_${package_version}_${architecture}.deb"
dpkg-deb --root-owner-group --build "$staging" "$output"
(cd "$output_dir" && sha256sum -- "$(basename -- "$output")" > SHA256SUMS)
printf 'Built %s\n' "$output"
