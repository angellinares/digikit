#!/usr/bin/env bash
# Prepare a git worktree for an agent: fast-forward it to BASE (default
# work/sharc-emulator) and link the main tree's untracked inputs into it --
# firmware-derived data, generated Ghidra/SLEIGH language files, and the
# virtualenv with the patched Unicorn and the dev tools.
#
#   tools/worktree-setup.sh [BASE]      # run from inside the worktree
#
# The links are ignored through the repository's info/exclude. Do not change
# dependencies in a worktree: the virtualenv is shared with the main tree.
set -euo pipefail

base=${1:-work/sharc-emulator}
here=$(git rev-parse --show-toplevel)
# Read the whole listing (no early `exit`): with many worktrees an early exit
# closes the pipe, git gets SIGPIPE and `pipefail` aborts the script (141).
main=$(git worktree list --porcelain | awk '/^worktree / && !n {print $2; n=1}')
if [[ $here == "$main" ]]; then
  echo "error: run this inside a worktree, not the main tree ($main)" >&2
  exit 2
fi

git -C "$here" merge --ff-only --quiet "$base"

links=(
  out
  sections
  snapshots
  .venv
  tools/ghidra/SHARC
  tools/ghidra/ColdfireEMAC/data/languages/coldfire_emac.sla
  tools/ghidra/ColdfireEMAC/extension.properties
  tools/sharcspec/ghidra/SHARC_VISA
)
exclude="$(git rev-parse --git-common-dir)/info/exclude"
mkdir -p "$(dirname "$exclude")"
touch "$exclude"
for path in "${links[@]}"; do
  grep -qxF "/$path" "$exclude" || echo "/$path" >>"$exclude"
  if [[ -e $main/$path && ! -e $here/$path && ! -L $here/$path ]]; then
    mkdir -p "$(dirname "$here/$path")"
    ln -s "$main/$path" "$here/$path"
  fi
done

echo "worktree: $here"
echo "base:     $(git -C "$here" log --oneline -1)"
for path in "${links[@]}"; do
  [[ -L $here/$path ]] && echo "linked:   $path"
done
if [[ -f $here/sections/.source-sha256 ]]; then
  echo "sections: $(cat "$here/sections/.source-sha256")"
fi
