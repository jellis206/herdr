#!/usr/bin/env bash
# Keep vendor/libghostty-vt on Ghostty main and the Zig pin in .tool-versions
# current. A refresh is due when Ghostty main needs a Zig newer than our pin
# and asdf can install it. The refresh runs in a new worktree on branch
# ghostty-refresh/zig-<version>: re-vendor, reapply patches, regenerate
# bindings, bump the pin, test, commit. Nothing is pushed or merged.
#
# Usage: scripts/refresh_libghostty_vt.sh [--force]
#   --force  refresh even when the Zig pin is already current
set -euo pipefail

repo="$(cd -- "$(dirname -- "$0")/.." && pwd)"
force=false
[[ "${1:-}" == "--force" ]] && force=true

pinned="$(awk '$1 == "zig" {print $2}' "$repo/.tool-versions")"
latest="$(asdf latest zig)"
wanted="$(curl -fsSL https://raw.githubusercontent.com/ghostty-org/ghostty/main/build.zig.zon |
  sed -n 's/.*minimum_zig_version = "\(.*\)".*/\1/p')"
[[ -n "$pinned" && -n "$latest" && -n "$wanted" ]] || {
  echo "refresh: could not read Zig versions (pinned='$pinned' latest='$latest' ghostty='$wanted')" >&2
  exit 1
}

newest() { printf '%s\n' "$@" | sort -V | tail -1; }
if [[ "$wanted" == "$pinned" && "$force" == false ]]; then
  echo "refresh: up to date (Zig $pinned; latest Zig $latest)"
  exit 0
fi
if [[ "$(newest "$wanted" "$latest")" != "$latest" ]]; then
  echo "refresh: Ghostty main needs Zig $wanted, but asdf only offers $latest; waiting"
  exit 0
fi

branch="ghostty-refresh/zig-$wanted"
"$force" && branch="ghostty-refresh/$(date +%Y%m%d-%H%M%S)"
if git -C "$repo" show-ref --quiet "refs/heads/$branch"; then
  echo "refresh: $branch already exists; review or delete it"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp/ghostty"' EXIT
work="$tmp/herdr"
step() {
  echo "refresh: $1"
}
fail() {
  echo "refresh FAILED at: $1. Worktree left for repair: $work (branch $branch)" >&2
  exit 1
}

step "creating worktree $work on $branch"
git -C "$repo" worktree add -q -b "$branch" "$work" master || fail "worktree"
cd "$work"

step "installing Zig $wanted"
asdf install zig "$wanted" >/dev/null || fail "asdf install zig $wanted"
printf 'zig %s\n' "$wanted" >.tool-versions

step "re-vendoring libghostty-vt from Ghostty main"
git clone -q --depth 1 https://github.com/ghostty-org/ghostty.git "$tmp/ghostty" || fail "clone ghostty"
python3 scripts/vendor_libghostty_vt.py --source-repo "$tmp/ghostty" || fail "vendor"
git add -A vendor

for patch in vendor/patches/libghostty-vt/*.patch; do
  step "applying ${patch##*/}"
  # Plain apply: the patches carry no blob ids, so --3way only adds noise.
  git apply "$patch" || fail "patch ${patch##*/} (see vendor/libghostty-vt.patches.md)"
done

step "checking build tools"
bindgen --version 2>/dev/null | grep -q '0\.72\.1' ||
  cargo install --locked bindgen-cli --version 0.72.1 || fail "install bindgen-cli 0.72.1"
command -v just >/dev/null || cargo install --locked just || fail "install just"
command -v cargo-nextest >/dev/null || cargo install --locked cargo-nextest || fail "install cargo-nextest"

step "regenerating bindings"
scripts/generate_libghostty_bindings.sh || fail "bindings"

step "running tests"
just test || fail "just test"

git add -A
git commit -q -m "chore(vendor): refresh libghostty-vt to Ghostty main (Zig $wanted)" || fail "commit"
echo "refresh: done. Review $branch in $work, merge it into master, then run: devtools herdr"
