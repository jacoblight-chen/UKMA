#!/bin/sh
# Publish (or dry-run) the `ukma-cli` npm launcher.
#
# `npm publish` reads package.json from the current directory and the repository
# root deliberately has none, so running it there fails with
# `ENOENT .../package.json`. This script always runs it inside `npm/`, shows the
# tarball first, and publishes only with --publish.
#
#   sh scripts/publish_npm.sh                     # dry run
#   sh scripts/publish_npm.sh --publish           # upload
#   sh scripts/publish_npm.sh --tag v0.1.0 --publish
set -eu

PUBLISH=0
TAG=""
OTP=""
PROVENANCE=0

usage() {
    cat <<'EOF'
usage: publish_npm.sh [options]

  --publish       actually upload (default: dry run)
  --tag <vX.Y.Z>  check that the tag matches crates/ukma-cli/Cargo.toml and align npm/package.json
  --otp <code>    two-factor code, when the npm account requires one
  --provenance    publish with --provenance (needs a CI OIDC token, not a local login)
  -h, --help      this text
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --publish) PUBLISH=1; shift ;;
        --tag) TAG=$2; shift 2 ;;
        --otp) OTP=$2; shift 2 ;;
        --provenance) PROVENANCE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
package_dir="$root/npm"
cargo_toml="$root/crates/ukma-cli/Cargo.toml"

step() { printf '==> %s\n' "$1"; }

command -v npm >/dev/null 2>&1 || { echo "npm is not on PATH; install Node.js 18 or newer." >&2; exit 1; }
[ -f "$package_dir/package.json" ] || { echo "npm/package.json is missing; is $root the repository root?" >&2; exit 1; }

cd "$package_dir"
step "package dir $package_dir"

if [ -n "$TAG" ]; then
    case "$TAG" in
        v[0-9]*.[0-9]*.[0-9]*) ;;
        *) echo "tag \`$TAG\` is not of the form vX.Y.Z" >&2; exit 2 ;;
    esac
    crate_version=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$cargo_toml" | head -1)
    if [ "${TAG#v}" != "$crate_version" ]; then
        echo "tag $TAG does not match ukma-cli $crate_version (crates/ukma-cli/Cargo.toml)" >&2
        exit 1
    fi
    packed=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' package.json | head -1)
    if [ "$packed" != "$crate_version" ]; then
        step "aligning npm/package.json $packed -> $crate_version"
        if [ "$PUBLISH" -eq 1 ]; then
            npm version "$crate_version" --no-git-tag-version --allow-same-version >/dev/null
        fi
    fi
fi

step 'tarball contents'
npm pack --dry-run 2>&1 | grep -v '^npm notice *$' || true

if [ "$PUBLISH" -eq 1 ]; then
    who=$(npm whoami 2>/dev/null || true)
    if [ -z "$who" ]; then
        echo "not logged in to npm; run \`npm login\` first" >&2
        exit 1
    fi
    step "publishing as $who"
    set -- publish --access public
else
    step 'dry run (pass --publish to upload)'
    set -- publish --access public --dry-run
fi

if [ "$PROVENANCE" -eq 1 ]; then
    set -- "$@" --provenance
fi
if [ -n "$OTP" ]; then
    set -- "$@" --otp "$OTP"
fi

set +e
output=$(npm "$@" 2>&1)
status=$?
set -e
printf '%s\n' "$output"
if [ "$status" -ne 0 ]; then
    case "$output" in
        *two-factor*|*2fa*|*EOTP*)
            cat >&2 <<'EOF'
npm rejected the publish because the account requires two-factor authentication.
  - interactive:    sh scripts/publish_npm.sh --publish --otp <6-digit-code>
  - CI / no prompt: create an npm token with "bypass 2FA" and use it as NPM_TOKEN
    (Automation tokens bypass 2FA; Granular tokens need "Bypass 2FA" enabled)
EOF
            ;;
    esac
    echo "npm publish failed with exit code $status" >&2
    exit 1
fi

if [ "$PUBLISH" -eq 1 ]; then
    step 'published; verify with `npm view ukma-cli version` and `npx ukma-cli@<version> --version`'
fi
