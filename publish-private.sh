#!/usr/bin/env bash
# Build fdk-aac on this machine and put the archives where only collaborators can read them.
#
# Usage:
#   ./publish-private.sh [<target>…]      default: every target this machine can build
#
# This is the whole of releasing. Fraunhofer's licence is not OSI-approved and grants no
# patent rights, so this repository — which is public — publishes the source of the build and
# no binary of it: the archives go to the releases of `PREBUILT_REPO` (fdk-aac.env), a private
# repository, and crates/fdk-aac-prebuilt-sys/build.rs reads them back through `gh` for
# whoever is logged in to an account with access. They are built here rather than in a
# workflow for the same reason: a public repository's workflow artifacts can be downloaded by
# anyone with a GitHub account, and a private repository's runners are paid for by the
# minute.
#
# A release holds the targets the machine that cut it could build, so a release cut on Linux
# has no macOS or Windows archive; a consumer on a target the latest release lacks is told to
# build its own. Every archive passes the same gate build.yml applies before it is uploaded:
# build.sh's own verification, the workspace's tests against it, and the e2e binary run and
# checked for what it links.
#
# Commit and push first. The tag is *computed*, never typed — `v<fdk-aac>-<YYYYMMDDHHMMSS>-
# <short sha>`, the version saying what is inside, the timestamp when, and the hash which
# commit — and it is created twice: on the private repository, as the release holding the
# archives, and on this one, as the plain git tag a consumer's manifest names to pin the
# crate those archives were tested with.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=fdk-aac.env
. ./fdk-aac.env

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)  buildable=(linux-x86_64 linux-x86_64-v3) ;;
  Linux-aarch64) buildable=(linux-aarch64) ;;
  Darwin-arm64)  buildable=(macos-arm64) ;;
  *) echo "no target of build.sh's builds on $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
if [ $# -gt 0 ]; then targets=("$@"); else targets=("${buildable[@]}"); fi

# Every asset filename is built from this value, so a typo in fdk-aac.env becomes a release
# of misnamed archives that no consumer's build.rs can find.
echo "$FDK_AAC_VERSION" | grep -Eq '^[0-9]+\.[0-9]+(\.[0-9]+)?$' \
  || { echo "FDK_AAC_VERSION in fdk-aac.env is not a version: '$FDK_AAC_VERSION'" >&2; exit 1; }

# The tag names a commit, so the archives must be that commit's and the commit must be one a
# consumer can fetch.
[ -z "$(git status --porcelain)" ] \
  || { echo "the working tree has uncommitted changes; a release is of a commit" >&2; exit 1; }
sha="$(git rev-parse HEAD)"
origin="$(git remote get-url origin)"
git fetch --quiet origin
[ -n "$(git branch -r --contains "$sha")" ] \
  || { echo "$sha is on no branch of origin; push it first" >&2; exit 1; }

# Before the build rather than after it.
gh release list --repo "$PREBUILT_REPO" --limit 1 >/dev/null \
  || { echo "cannot read $PREBUILT_REPO: gh auth login, with an account that has access" >&2; exit 1; }

# Seconds, so that two releases on one day sort in the order they happened, and UTC, so that
# the order does not depend on whose clock it was.
stamp="$(date -u +%Y%m%d%H%M%S)-${sha:0:7}"
tag="v${FDK_AAC_VERSION}-${stamp}"

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

for target in "${targets[@]}"; do
  echo ">> $target"
  ./build.sh "$target"
  [ -f "dist/$target/MANIFEST" ] || { echo "build.sh left no dist/$target/MANIFEST" >&2; exit 1; }
done
./sync-prebuilt.sh

# One flavour at a time: which x86_64 archive a build links is a cargo feature, so each is
# tested by the build that links it.
for target in "${targets[@]}"; do
  case "$target" in *-v3) cargo=(--features fdk-aac-e2e/x86-64-v3) ;; *) cargo=() ;; esac
  echo ">> testing $target"
  # `${a[@]+…}`: an empty array is an unbound variable to the bash 3.2 macOS ships.
  cargo test --offline --workspace ${cargo[@]+"${cargo[@]}"}
  cargo build --offline --release --workspace ${cargo[@]+"${cargo[@]}"}
  ./target/release/fdk-aac-e2e
  ./check-static.sh target/release/fdk-aac-e2e
  tar czf "$out/fdk-aac-${FDK_AAC_VERSION}-${target}.tar.gz" -C "dist/$target" .
done

# A corruption check, not a tamper check: it lives on the same release as the files it
# covers, so it proves a download arrived intact, not that the release is honest.
(cd "$out" && sha256sum -- *.tar.gz > SHA256SUMS)
cat "$out/SHA256SUMS"

# Draft first, publish last, so a failed upload leaves a deletable draft rather than a
# release build.rs resolves `latest` to and finds half of.
echo ">> releasing $tag on $PREBUILT_REPO"
gh release create "$tag" --repo "$PREBUILT_REPO" --draft \
  --title "fdk-aac ${FDK_AAC_VERSION} static archives - ${stamp}" \
  --notes "Built from ${origin%.git}/commit/${sha} for: ${targets[*]}" \
  "$out"/SHA256SUMS "$out"/*.tar.gz
gh release edit "$tag" --repo "$PREBUILT_REPO" --draft=false

git tag "$tag" "$sha"
git push origin "refs/tags/$tag"
echo ">> published $tag"
