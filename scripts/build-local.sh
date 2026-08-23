#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
cargo_bin=${CARGO:-cargo}
target_dir=${CARGO_TARGET_DIR:-$repo_dir/target}

"$cargo_bin" build --release --locked --manifest-path "$repo_dir/Cargo.toml"

mkdir -p "$repo_dir/bin"
temporary="$repo_dir/bin/.threadbeat.$$"
trap 'rm -f "$temporary"' EXIT HUP INT TERM
install -m 0755 "$target_dir/release/threadbeat" "$temporary"

# Cargo's rust-objcopy-based stripping is not available in every minimal Rust
# installation. Strip the packaged copy with the host tool when present while
# leaving the reproducible target artifact untouched.
case "$(uname -s)" in
  Darwin)
    if [ -x /usr/bin/strip ]; then
      /usr/bin/strip -S -x "$temporary"
    fi
    ;;
  Linux)
    if command -v strip >/dev/null 2>&1; then
      strip --strip-unneeded "$temporary"
    fi
    ;;
esac

mv -f "$temporary" "$repo_dir/bin/threadbeat"
trap - EXIT HUP INT TERM

"$repo_dir/bin/threadbeat" --version
printf '%s\n' "Packaged local executable: $repo_dir/bin/threadbeat"
