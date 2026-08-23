#!/bin/sh
set -eu

usage() {
  printf '%s\n' \
    'Usage: setup-service.sh --config ABS --destination ABS --plan ABS' \
    '       [--platform systemd-user|launchd-agent] [--apply-digest SHA256]' \
    '' \
    'Without --apply-digest, this only writes a reviewable plan.' \
    'Applying a matching digest creates private state/runtime directories and' \
    'writes the service definition. It never enables or starts the service.'
}

config=
destination=
plan=
platform=
apply_digest=

while [ "$#" -gt 0 ]; do
  case "$1" in
    --config|--destination|--plan|--platform|--apply-digest)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      case "$1" in
        --config) config=$2 ;;
        --destination) destination=$2 ;;
        --plan) plan=$2 ;;
        --platform) platform=$2 ;;
        --apply-digest) apply_digest=$2 ;;
      esac
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

for required in "$config" "$destination" "$plan"; do
  case "$required" in
    /*) ;;
    *) printf '%s\n' 'config, destination, and plan paths must be absolute' >&2; exit 2 ;;
  esac
done

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
binary="$repo_dir/bin/threadbeat"
[ -x "$binary" ] || {
  printf '%s\n' "Missing $binary; reinstall the matching checksum- and attestation-verified bundle." >&2
  printf '%s\n' "A pinned build via scripts/build-local.sh is a separate source-checkout fallback." >&2
  exit 1
}

if [ -n "$platform" ]; then
  "$binary" --config "$config" service plan \
    --platform "$platform" \
    --destination "$destination" \
    --output "$plan"
else
  "$binary" --config "$config" service plan \
    --destination "$destination" \
    --output "$plan"
fi

if [ -z "$apply_digest" ]; then
  printf '%s\n' "Review the plan at $plan, then rerun with --apply-digest and its exact digest."
  exit 0
fi

"$binary" --config "$config" service apply \
  --plan "$plan" \
  --plan-digest "$apply_digest"
