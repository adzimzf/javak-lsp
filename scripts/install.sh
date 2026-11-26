#!/usr/bin/env sh
# Install or update javak-lsp from its GitHub releases.
#
#   scripts/install.sh                     newest release, release candidates included
#   scripts/install.sh --stable            newest release that is not a candidate
#   scripts/install.sh --version v0.1.0-rc.3
#   scripts/install.sh --prefix /usr/local
#
# Or without a checkout:
#   curl -fsSL https://raw.githubusercontent.com/adzimzf/javak-lsp/main/scripts/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --stable
#
# Installs <prefix>/bin/javak-lsp and <prefix>/share/javak-lsp/javak-debug-server.jar
# (the server finds the jar there). The prefix defaults to ~/.local; also
# settable with JAVAK_LSP_PREFIX. A private repository needs GITHUB_TOKEN.
set -eu

REPO="adzimzf/javak-lsp"
prefix="${JAVAK_LSP_PREFIX:-$HOME/.local}"
version=""
stable=0

usage() {
  cat <<'EOF_USAGE'
Usage: install.sh [--version <tag>] [--stable] [--prefix <dir>]
  --version <tag>  install that release (e.g. v0.1.0-rc.3)
  --stable         newest release that is not a release candidate
  --prefix <dir>   install under <dir>/bin and <dir>/share/javak-lsp (default ~/.local)
EOF_USAGE
}
fail() { echo "install.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || fail "--version needs a tag"; version="$2"; shift 2 ;;
    --version=*) version="${1#*=}"; shift ;;
    --prefix) [ $# -ge 2 ] || fail "--prefix needs a directory"; prefix="$2"; shift 2 ;;
    --prefix=*) prefix="${1#*=}"; shift ;;
    --stable) stable=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1 (see --help)" ;;
  esac
done

command -v curl >/dev/null 2>&1 || fail "curl is needed"
command -v tar >/dev/null 2>&1 || fail "tar is needed"

# GitHub requests, with the token when there is one.
fetch() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$@"
  else
    curl -fsSL "$@"
  fi
}

case "$(uname -s)" in
  Linux) os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *) fail "no release for $(uname -s); build from source with cargo build --release" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) fail "no release for $(uname -m)" ;;
esac
# An Intel shell under Rosetta still gets the native Apple Silicon build.
if [ "$os" = "apple-darwin" ] && [ "$arch" = "x86_64" ] \
  && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
  arch="aarch64"
fi
target="${arch}-${os}"

api="https://api.github.com/repos/$REPO/releases"
if [ -z "$version" ]; then
  if [ "$stable" = 1 ]; then
    json=$(fetch "$api/latest") || fail "no stable release found"
  else
    json=$(fetch "$api?per_page=1") || fail "could not list the releases"
  fi
  version=$(printf '%s\n' "$json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
  [ -n "$version" ] || fail "no release found"
fi

installed=""
if [ -x "$prefix/bin/javak-lsp" ] && [ -f "$prefix/share/javak-lsp/VERSION" ]; then
  installed=$(cat "$prefix/share/javak-lsp/VERSION")
fi
if [ "$installed" = "$version" ]; then
  echo "javak-lsp $version is already installed in $prefix"
  exit 0
fi

name="javak-lsp-${version}-${target}"
base="https://github.com/$REPO/releases/download/$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Downloading javak-lsp $version for $target"
fetch -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" \
  || fail "no $name.tar.gz in release $version"

if fetch -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" 2>/dev/null; then
  expected=$(grep " $name.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
  else
    actual=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
  fi
  [ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum of $name.tar.gz does not match"
fi

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$prefix/bin" "$prefix/share/javak-lsp"
# Replace, not overwrite: a running server keeps its old binary.
cp "$tmp/$name/javak-lsp" "$prefix/bin/.javak-lsp.new"
chmod 755 "$prefix/bin/.javak-lsp.new"
mv -f "$prefix/bin/.javak-lsp.new" "$prefix/bin/javak-lsp"
cp "$tmp/$name/javak-debug-server.jar" "$prefix/share/javak-lsp/javak-debug-server.jar"
printf '%s\n' "$version" > "$prefix/share/javak-lsp/VERSION"
# macOS quarantines downloads; the binary is not notarized.
if [ "$os" = "apple-darwin" ]; then
  xattr -d com.apple.quarantine "$prefix/bin/javak-lsp" 2>/dev/null || true
fi

if [ -n "$installed" ]; then
  echo "Updated javak-lsp $installed -> $version in $prefix/bin"
else
  echo "Installed javak-lsp $version in $prefix/bin"
fi
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) echo "Add $prefix/bin to your PATH to run javak-lsp." ;;
esac
if ! command -v java >/dev/null 2>&1; then
  echo "Debugging and decompiling libraries need Java 17 or newer (java on the PATH or JAVA_HOME)."
fi
