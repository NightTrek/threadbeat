#!/bin/sh
set -eu

usage() {
  printf '%s\n' \
    'Usage: release.sh VERSION [--publish]' \
    '' \
    'Without --publish, run all local gates and build the native host bundle.' \
    'With --publish, additionally create and push a new, non-reused vVERSION tag;' \
    'the GitHub release workflow then rebuilds all four supported targets.'
}

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  usage >&2
  exit 2
fi
version=$1
mode=${2:-}
[ -z "$mode" ] || [ "$mode" = "--publish" ] || { usage >&2; exit 2; }

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
cd "$repo_dir"
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

python3 scripts/check-release-version.py "$version" --require-clean
cargo_proxy_dir=$(dirname -- "$cargo_bin")
env PATH="$cargo_proxy_dir:$PATH" "$cargo_bin" fmt --all -- --check
env PATH="$cargo_proxy_dir:$PATH" "$cargo_bin" clippy --all-targets --all-features --locked -- -D warnings
"$cargo_bin" test --all-targets --all-features --locked
cargo_deny_bin=${CARGO_DENY:-cargo-deny}
case "$cargo_deny_bin" in
  */*)
    if [ -x "$cargo_deny_bin" ]; then
      "$cargo_deny_bin" check
    elif [ "$mode" = "--publish" ]; then
      printf '%s\n' "cargo-deny is not executable: $cargo_deny_bin" >&2
      exit 1
    else
      printf '%s\n' 'cargo-deny is not installed; CI will run the pinned dependency-policy gate.' >&2
    fi
    ;;
  *)
    if command -v "$cargo_deny_bin" >/dev/null 2>&1; then
      "$cargo_deny_bin" check
    elif [ "$mode" = "--publish" ]; then
      printf '%s\n' 'cargo-deny is required for --publish.' >&2
      exit 1
    else
      printf '%s\n' 'cargo-deny is not installed; CI will run the pinned dependency-policy gate.' >&2
    fi
    ;;
esac

host=$("$cargo_bin" -vV | awk '$1 == "host:" { print $2 }')
mkdir -p "$repo_dir/dist"
output_dir=$(mktemp -d "$repo_dir/dist/local-$version-$host.XXXXXX")
./scripts/build-release.sh "$version" "$host" "$output_dir"

[ "$mode" = "--publish" ] || {
  printf '%s\n' 'Local release gate passed. No tag was created or pushed.'
  exit 0
}

command -v gh >/dev/null 2>&1 || { printf '%s\n' 'GitHub CLI is required for --publish.' >&2; exit 1; }
gh auth status --hostname github.com >/dev/null
[ "$(git branch --show-current)" = "main" ] || { printf '%s\n' 'Publish must run from main.' >&2; exit 1; }
origin=$(git remote get-url origin)
case "$origin" in
  https://github.com/NightTrek/threadbeat|https://github.com/NightTrek/threadbeat.git|git@github.com:NightTrek/threadbeat.git|ssh://git@github.com/NightTrek/threadbeat.git) ;;
  *) printf '%s\n' "Refusing unexpected origin: $origin" >&2; exit 1 ;;
esac

git fetch --no-tags origin main:refs/remotes/origin/main
head=$(git rev-parse HEAD)
remote_head=$(git rev-parse origin/main)
[ "$head" = "$remote_head" ] || { printf '%s\n' 'HEAD must exactly match origin/main.' >&2; exit 1; }
tag="v$version"
[ -z "$(git tag --list "$tag")" ] || { printf '%s\n' "Local tag already exists: $tag" >&2; exit 1; }
[ -z "$(git ls-remote --tags origin "refs/tags/$tag")" ] || { printf '%s\n' "Remote tag already exists: $tag" >&2; exit 1; }

git tag -a "$tag" -m "Threadbeat $tag"
git push origin "refs/tags/$tag"
printf '%s\n' "Pushed $tag; GitHub Actions is building and verifying all release targets."
