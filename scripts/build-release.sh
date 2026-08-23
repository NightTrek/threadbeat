#!/bin/sh
set -eu

usage() {
  printf '%s\n' 'Usage: build-release.sh VERSION TARGET OUTPUT_DIR'
}

[ "$#" -eq 3 ] || { usage >&2; exit 2; }
version=$1
target=$2
output_dir=$3

case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin) ;;
  *) printf '%s\n' "Unsupported release target: $target" >&2; exit 2 ;;
esac

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
cargo_bin=${CARGO:-cargo}
case "$cargo_bin" in
  */*) [ -x "$cargo_bin" ] || { printf '%s\n' "Cargo is not executable: $cargo_bin" >&2; exit 1; } ;;
  *) cargo_bin=$(command -v "$cargo_bin") || { printf '%s\n' 'Cargo is not installed.' >&2; exit 1; } ;;
esac
rustc_bin=${RUSTC:-rustc}
case "$rustc_bin" in
  */*) [ -x "$rustc_bin" ] || { printf '%s\n' "rustc is not executable: $rustc_bin" >&2; exit 1; } ;;
  *)
    if command -v "$rustc_bin" >/dev/null 2>&1; then
      rustc_bin=$(command -v "$rustc_bin")
    elif [ -x "$(dirname -- "$cargo_bin")/rustc" ]; then
      rustc_bin=$(dirname -- "$cargo_bin")/rustc
    else
      printf '%s\n' 'rustc is not installed.' >&2
      exit 1
    fi
    ;;
esac
export CARGO="$cargo_bin"
export RUSTC="$rustc_bin"
host=$("$cargo_bin" -vV | awk '$1 == "host:" { print $2 }')
[ "$host" = "$target" ] || {
  printf '%s\n' "Release builds must be native: host=$host target=$target" >&2
  exit 1
}

python3 "$script_dir/check-release-version.py" "$version" --require-clean
cd "$repo_dir"

case "$output_dir" in
  /*) ;;
  *) output_dir=$repo_dir/$output_dir ;;
esac
archive="$output_dir/threadbeat-plugin-v$version-$target.tar.gz"
[ ! -e "$archive" ] || {
  printf '%s\n' "Refusing to overwrite existing artifact: $archive" >&2
  exit 1
}

umask 077
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/threadbeat-release.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

target_dir=${CARGO_TARGET_DIR:-$repo_dir/target}
case "$target_dir" in
  /*) ;;
  *) target_dir=$repo_dir/$target_dir ;;
esac
export CARGO_TARGET_DIR="$target_dir"
export CARGO_INCREMENTAL=0
if [ "${target##*-}" = "darwin" ]; then
  export MACOSX_DEPLOYMENT_TARGET=11.0
fi

"$cargo_bin" fetch --locked --target "$target"
"$cargo_bin" test --all-targets --all-features --locked --offline --target "$target"
"$cargo_bin" build --release --locked --offline --target "$target"
python3 "$script_dir/check-release-version.py" "$version" --require-clean

built="$target_dir/$target/release/threadbeat"
[ -f "$built" ] || { printf '%s\n' "Missing release binary: $built" >&2; exit 1; }
install -m 0755 "$built" "$work_dir/threadbeat"

case "$target" in
  *-apple-darwin) /usr/bin/strip -S -x "$work_dir/threadbeat" ;;
  *-linux-gnu) strip --strip-unneeded "$work_dir/threadbeat" ;;
esac

python3 "$script_dir/check-release-binary.py" \
  --binary "$work_dir/threadbeat" \
  --target "$target" \
  --version "$version"
python3 "$script_dir/smoke-mcp.py" --binary "$work_dir/threadbeat"

epoch=$(git -C "$repo_dir" show -s --format=%ct HEAD)
python3 "$script_dir/package-release.py" \
  --binary "$work_dir/threadbeat" \
  --target "$target" \
  --version "$version" \
  --output-dir "$output_dir" \
  --source-date-epoch "$epoch"
python3 "$script_dir/smoke-package.py" \
  --archive "$archive" \
  --version "$version"

trap - EXIT HUP INT TERM
rm -rf "$work_dir"
