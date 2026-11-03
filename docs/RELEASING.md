# Releasing digikit

This guide covers version metadata with Nx's `release version` subcommand. It does not publish artifacts,
create tags or commit changes. For verification before release, follow [Testing](TESTING.md).

## Version ownership

Shipped non-core modules own independent `[module].version` values in `module.toml`, including raw
modules. Fixtures and patched-in core adapters are excluded. Nx derives their `release-module` tag
from the manifest category and shipped root; the `modules` release group uses those tags.

The runtime release also defines the SDK release: `[runtime].version` in `sdk/sdk.toml` is canonical,
and `modules/core/runtime/module.toml` mirrors it. The `runtime` group versions that one identity;
the adapter refuses a missing or mismatched mirror. SDK contract numbers are a separate internal check.
Versioning one module or the runtime does not bump dependents or rewrite dependency declarations.

## Choose and preview a version

While experimental at `0.x`, choose minor for breaking changes and patch for features/fixes; `1.0.0`
is an explicit future milestone. Bump names are literal (`minor` changes `0.5.4` to `0.6.0`).
Packed versions must be plain `major.minor.patch`, each part at most 255; prerelease and build suffixes
are rejected.

Run from the repository root, replacing `MODULE_ID` with the module's manifest ID. Choose the bump
explicitly; these examples preview a module patch and a runtime minor:

```sh
pnpm nx:pilot release version patch --projects=MODULE_ID --dry-run
pnpm nx:pilot release version minor --groups=runtime --dry-run
```

An explicit version also works; for example, preview a future runtime milestone with:

```sh
pnpm nx:pilot release version 1.0.0 --groups=runtime --dry-run
```

## Apply reviewed metadata

After reviewing the preview and choosing the intended bump, omit `--dry-run`:

```sh
pnpm nx:pilot release version patch --projects=MODULE_ID
pnpm nx:pilot release version minor --groups=runtime
```

The adapter changes only quoted version values through Nx's Tree, preserving TOML comments and
formatting. Inspect the resulting diff. Git commits, staging, tags, pushing and changelogs are disabled.
Use only `nx release version` for this workflow; combined release and publishing are not configured
or part of these commands. Generated SDK sources and core objects still need their normal
generation/build checks before release verification.

## Compatibility follow-up

The planned public runtime requirement is `>=0.5.4 <0.6.0`. Declaring ranges and changing module-loading
enforcement are much-later follow-up work. Current loading still checks SDK contracts, with packed
runtime version information unchanged; this versioning setup does not implement a new compatibility gate.
