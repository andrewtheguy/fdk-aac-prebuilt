#!/usr/bin/env bash
# Build all six archives on machines of the operator's own, and put them where only
# collaborators can read them.
#
# Usage:
#   FDK_AAC_PREBUILT_LINUX_AARCH64_HOST=<ssh host> ./publish-private.sh
#
# This is the whole of releasing. Fraunhofer's licence is not OSI-approved and grants no
# patent rights, so this repository — which is public — publishes the source of the build and
# no binary of it: the archives go to the releases of `PREBUILT_REPO` (fdk-aac.env), a private
# repository, and crates/fdk-aac-prebuilt-sys/build.rs reads them back through `gh` for
# whoever is logged in to an account with access. They are built on the operator's machines
# rather than in a workflow for the same reason: a public repository's workflow artifacts can
# be downloaded by anyone with a GitHub account, and a private repository's runners are paid
# for by the minute.
#
# Run on Linux x86_64, with the sibling `devtools` checkout beside this one (or `DEVTOOLS_DIR`
# naming it): its remote drivers copy a tree to a machine, run this repository's ci/unix/ci.sh
# or ci/windows/ci.ps1 there, and hand back the `dist/<target>` that leaves behind. Four
# builders, at once:
#
#   here                                      linux-x86_64, linux-x86_64-v3   (container)
#   $FDK_AAC_PREBUILT_LINUX_AARCH64_HOST      linux-aarch64                   (container)
#   $FDK_AAC_PREBUILT_MACOS_HOST, or macvm    macos-arm64
#   the devtools Windows CI box               windows-x86_64-msvc, -msvc-v3
#
# The Linux ARM builder has no default because it has no sandbox: it is whichever aarch64
# server can spare the cycles, it needs nothing installed but podman or docker, and naming it
# is the operator's decision each time. FDK_AAC_PREBUILT_UNIXCI_IDENTITY names a private key
# for it if its login is not in ~/.ssh/config.
#
# Every archive passes the gate build.yml applies, on the machine that built it: build.sh's
# own verification, the workspace's tests against it, and the e2e binary run and checked for
# what it links. A release is all six or it does not happen.
#
# Commit and push first. What gets built is `git archive HEAD`, not this directory, so nothing
# uncommitted or ignored can reach an archive. The tag is *computed*, never typed —
# `v<fdk-aac>-<YYYYMMDDHHMMSS>-<short sha>`, the version saying what is inside, the timestamp
# when, and the hash which commit — and it is created twice: on the private repository, as
# the release holding the archives, and on this one, as the plain git tag a consumer's
# manifest names to pin the crate those archives were tested with.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=fdk-aac.env
. ./fdk-aac.env

[ "$(uname -s)-$(uname -m)" = Linux-x86_64 ] \
  || { echo "the publisher builds the x86_64 Linux pair itself: run it on Linux x86_64, not $(uname -s)-$(uname -m)" >&2; exit 1; }
[ $# -eq 0 ] || { sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 2; }

arm_host="${FDK_AAC_PREBUILT_LINUX_AARCH64_HOST:-}"
[ -n "$arm_host" ] \
  || { echo "set FDK_AAC_PREBUILT_LINUX_AARCH64_HOST to the ssh host that builds linux-aarch64" >&2; exit 1; }
mac_host="${FDK_AAC_PREBUILT_MACOS_HOST:-macvm}"
export DEVTOOLS_DIR="${DEVTOOLS_DIR:-$here/../devtools}"
[ -f "$DEVTOOLS_DIR/ci/unix/remote.sh" ] \
  || { echo "no devtools checkout at $DEVTOOLS_DIR (set DEVTOOLS_DIR)" >&2; exit 1; }
command -v pwsh >/dev/null 2>&1 || { echo "pwsh drives the Windows builder and is not on PATH" >&2; exit 1; }

targets=(macos-arm64 linux-x86_64 linux-x86_64-v3 linux-aarch64
         windows-x86_64-msvc windows-x86_64-msvc-v3)

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

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
out="$work/assets"
tree="$work/tree"
logs="$here/tmp/publish-$stamp"
mkdir -p "$out" "$tree" "$logs"

# The commit, and only the commit: ignored leftovers of earlier builds stay behind.
git archive "$sha" | tar -x -C "$tree"

# builder NAME FETCH-TARGETS… -- DRIVER ARGS…: run the driver's `ci` on one machine, then
# fetch each target's dist/ from the workspace it left. In the background, logged per
# builder, because the four machines have nothing to wait on each other for.
pids=()
names=()
builder() {
  local name="$1"; shift
  local fetch=()
  while [ "$1" != -- ]; do fetch+=("$1"); shift; done
  shift
  (
    set -e
    "$@" ci
    for target in "${fetch[@]}"; do
      "$@" fetch "dist/$target" "$work/dist/$target"
    done
  ) > "$logs/$name.log" 2>&1 &
  pids+=("$!")
  names+=("$name")
  echo ">> $name: building ${fetch[*]} (log: $logs/$name.log)"
}

builder local linux-x86_64 linux-x86_64-v3 -- "$tree/ci/unix/remote.sh"
builder linux-aarch64 linux-aarch64 -- "$tree/ci/unix/remote.sh" -H "$arm_host"
builder macos macos-arm64 -- "$tree/ci/unix/remote.sh" -H "$mac_host"
builder windows windows-x86_64-msvc windows-x86_64-msvc-v3 -- pwsh -NoLogo -NoProfile -File "$tree/ci/windows/remote.ps1"

failed=0
for i in "${!pids[@]}"; do
  if wait "${pids[$i]}"; then
    echo ">> ${names[$i]}: passed"
  else
    echo ">> ${names[$i]}: FAILED — see $logs/${names[$i]}.log" >&2
    failed=1
  fi
done
[ "$failed" = 0 ] || { echo "no release: a builder failed" >&2; exit 1; }

# Named here rather than discovered, so that a target silently missing is a failed release
# instead of a release somebody links against six months later and finds nothing for their
# platform.
for target in "${targets[@]}"; do
  [ -f "$work/dist/$target/MANIFEST" ] \
    || { echo "no archive for $target — refusing to publish an incomplete release" >&2; exit 1; }
  tar czf "$out/fdk-aac-${FDK_AAC_VERSION}-${target}.tar.gz" -C "$work/dist/$target" .
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
  --notes "Built from ${origin%.git}/commit/${sha}" \
  "$out"/SHA256SUMS "$out"/*.tar.gz
gh release edit "$tag" --repo "$PREBUILT_REPO" --draft=false

git tag "$tag" "$sha"
git push origin "refs/tags/$tag"
echo ">> published $tag"
