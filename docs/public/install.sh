#!/bin/sh
set -eu

release_tag=v0.3.3-preview
prefix=${AURA_INSTALL_PREFIX:-"$HOME/.local"}
base_url=${AURA_INSTALL_BASE_URL:-"https://github.com/johnolafenwa/Aura/releases/download/$release_tag"}

fail() {
  printf 'Aura installer: %s\n' "$1" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64|Darwin-aarch64)
    target=aarch64-apple-darwin
    ;;
  Darwin-x86_64)
    target=x86_64-apple-darwin
    ;;
  Linux-x86_64|Linux-amd64)
    target=x86_64-unknown-linux-gnu
    ;;
  *)
    fail "no release archive is available for $(uname -s) $(uname -m)"
    ;;
esac

archive_root="aura-$release_tag-$target"
archive="$archive_root.tar.gz"
tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/aura-install.XXXXXX") || fail "could not create a temporary directory"
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM

printf 'Downloading Aura %s for %s...\n' "$release_tag" "$target"
curl -fsSL "$base_url/$archive" -o "$tmpdir/$archive" || fail "could not download $archive"
curl -fsSL "$base_url/SHA256SUMS" -o "$tmpdir/SHA256SUMS" || fail "could not download SHA256SUMS"

(
  cd "$tmpdir"
  awk -v name="$archive" '$2 == name { print; found = 1 } END { if (!found) exit 1 }' SHA256SUMS > checksum-entry
) || fail "$archive is missing from SHA256SUMS"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmpdir" && sha256sum -c checksum-entry >/dev/null) || fail "checksum verification failed"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmpdir" && shasum -a 256 -c checksum-entry >/dev/null) || fail "checksum verification failed"
else
  fail "sha256sum or shasum is required to verify the release"
fi

tar -xzf "$tmpdir/$archive" -C "$tmpdir" || fail "could not extract $archive"
source_root="$tmpdir/$archive_root"
test -x "$source_root/bin/aura" || fail "the archive does not contain bin/aura"
test -f "$source_root/lib/aura/libaura_compiler.a" || fail "the archive is missing the native runtime"
test -f "$source_root/lib/aura/native-link-args.json" || fail "the archive is missing native-link-args.json"

mkdir -p "$prefix/bin" "$prefix/lib/aura" "$prefix/share/aura"
rm -f "$prefix/bin/aura"
cp "$source_root/bin/aura" "$prefix/bin/aura"
chmod 755 "$prefix/bin/aura"
cp "$source_root/lib/aura/libaura_compiler.a" "$prefix/lib/aura/libaura_compiler.a"
cp "$source_root/lib/aura/native-link-args.json" "$prefix/lib/aura/native-link-args.json"

if test -d "$source_root/examples"; then
  rm -rf "$prefix/share/aura/examples"
  cp -R "$source_root/examples" "$prefix/share/aura/examples"
fi
for file in README.md AURA_CLI_README.md LICENSE; do
  if test -f "$source_root/$file"; then
    cp "$source_root/$file" "$prefix/share/aura/$file"
  fi
done

printf '\nAura is installed at %s/bin/aura\n' "$prefix"
case ":$PATH:" in
  *":$prefix/bin:"*)
    printf 'Run: aura --version\n'
    ;;
  *)
    printf 'Add Aura to this shell with:\n  export PATH="%s/bin:$PATH"\n' "$prefix"
    ;;
esac
