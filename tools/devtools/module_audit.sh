#!/bin/sh
# The ColdFire memory audit of one run-time module: tools/devtools/module_audit.sh NAME [--device dt2|dn2]  (mise run module:audit NAME)
# NAME is a module id or its last part. Runs modules/core/runtime/tests/memaudit-module.device.test.ts (docs/MODULE-AUTHORING.md).
set -eu
if [ $# -lt 1 ] || [ "$1" = --help ] || [ "$1" = -h ]; then
  echo "usage: module_audit.sh NAME [--device dt2|dn2]" >&2
  [ $# -ge 1 ] && exit 0
  exit 2
fi
name=$1
device=
if [ $# -ge 3 ] && [ "$2" = --device ]; then device=$3; fi
cd "$(dirname "$0")/../.."
# the patchkit CLI, plusdrive and the daemons (cached when current)
pnpm nx:pilot run-many -t cli,daemons -p patchkit,plusdrive-format,digi-control
DIGI_AUDIT_MODULE=$name DIGI_AUDIT_DEVICE=$device DIGIKIT_TIER=full REGRESSION_JOBS=1 \
  exec pnpm --filter @digi/regression test:runtime ../../modules/core/runtime/tests/memaudit-module.device.test.ts
