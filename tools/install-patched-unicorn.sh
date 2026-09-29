#!/usr/bin/env bash
# Build the m68k SR-read, code-hook CCR-sync, EMAC MAC-with-load, EMAC
# fractional-mode and flush-flags CC_OP fixes, and the count-hook fast path,
# from official Unicorn 2.1.4.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
commit=8028ec436f2d9376525352dd38ed9ed6b9f6be10
# Applied in this order, each pinned by SHA-256.
patches=(
  "$root/patches/unicorn-2.1.4-m68k-hook-ccr-sync.patch"
  "$root/patches/unicorn-2.1.4-m68k-emac-mac-load.patch"
  "$root/patches/unicorn-2.1.4-m68k-emac-fractional.patch"
  "$root/patches/unicorn-2.1.4-count-hook-fast-path.patch"
  "$root/patches/unicorn-2.1.4-m68k-flush-flags-sync.patch"
)
patch_shas=(
  56de71acf2adbd5ca2f448095478e65e49fd79d378aeb5b5e4217d2c90f52f4e
  ac128dd6836997de55e0d2ad70d7a2978639168090f552c5634da50fddc70bfe
  8f497c939b7b772a0a00a7d16ad85e8a1a6865b772191ca5ca131fb477824603
  5f7523267ed0a6324496c7ea665d6ea22d84f30d82199218f66594291adc3e17
  9f76167940573e1e33582f6e4fbef10d95528dfa26090406a9b40a4b45712807
)
python=${PYTHON:-$root/.venv/bin/python}
dry_run=false
if [[ ${1:-} == --dry-run ]]; then
  dry_run=true
  shift
fi
[[ $# == 0 ]] || {
  echo "usage: $0 [--dry-run]" >&2
  exit 2
}
[[ -x $python ]] || {
  echo "error: project Python not found: $python (set PYTHON=...)" >&2
  exit 2
}
[[ $("$python" -c 'import unicorn; print(unicorn.__version__)') == 2.1.4 ]] || {
  echo "error: interpreter must have unicorn==2.1.4; run uv sync first" >&2
  exit 2
}
for i in "${!patches[@]}"; do
  [[ $(shasum -a 256 "${patches[i]}" | awk '{print $1}') == "${patch_shas[i]}" ]] || {
    echo "error: unexpected patch SHA-256: ${patches[i]}" >&2
    exit 2
  }
done
command -v git >/dev/null || {
  echo "error: git is required" >&2
  exit 2
}
command -v cmake >/dev/null || {
  echo "error: cmake is required" >&2
  exit 2
}

# These are the exact dynamic-library names selected by Unicorn's bindings.
# Do not glob: official wheels also contain libunicorn.a.
target=$(
  "$python" - <<'PY'
import os
import sys
import unicorn

name = {"darwin": "libunicorn.2.dylib", "linux": "libunicorn.so.2"}.get(sys.platform)
if name is None:
    raise SystemExit("unsupported Unicorn platform: " + sys.platform)
path = os.path.join(os.path.dirname(unicorn.__file__), "lib", name)
if not os.path.isfile(path):
    raise SystemExit("unsupported Unicorn native-library layout: expected " + path)
print(path)
PY
)
case $(uname -s) in
Darwin) built_name=libunicorn.2.dylib ;;
Linux) built_name=libunicorn.so.2 ;;
*)
  echo "error: unsupported platform $(uname -s)" >&2
  exit 2
  ;;
esac
if $dry_run; then
  printf 'dry-run: target=%s\n' "$target"
  printf 'dry-run: expected-build-payload=build/%s\n' "$built_name"
  exit 0
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/digitakt2-unicorn.XXXXXX")
trap 'rm -rf "$work"' EXIT

git clone --quiet --branch 2.1.4 --depth 1 https://github.com/unicorn-engine/unicorn.git "$work/src"
[[ $(git -C "$work/src" rev-parse HEAD) == "$commit" ]] || {
  echo "error: tag 2.1.4 did not resolve to expected commit" >&2
  exit 2
}
for p in "${patches[@]}"; do
  git -C "$work/src" apply --check "$p"
  git -C "$work/src" apply "$p"
done
cmake -S "$work/src" -B "$work/build" -DCMAKE_BUILD_TYPE=Release -DUNICORN_ARCH=m68k -DUNICORN_BUILD_TESTS=OFF
cmake --build "$work/build" --config Release --target unicorn
built=$work/build/$built_name
[[ -f $built && ! -L $built ]] || {
  echo "error: expected real m68k dynamic-library payload not produced: $built" >&2
  exit 2
}
# Replacement is atomic in the library directory, so an interrupted install never leaves a partial dylib.
tmp_target=$(mktemp "$(dirname "$target")/.${built_name}.XXXXXX")
cp "$built" "$tmp_target"
mv -f "$tmp_target" "$target"
printf 'unicorn commit=%s\n' "$commit"
for i in "${!patches[@]}"; do
  printf 'patch=%s sha256=%s\n' "$(basename "${patches[i]}")" "${patch_shas[i]}"
done
printf 'library=%s\nlibrary_sha256=%s\n' "$target" "$(shasum -a 256 "$target" | awk '{print $1}')"
"$python" -m emu.unicorn_compat
