# Fast module and SDK iteration

Status: in progress on `test/module-sdk-iteration` (2026-11-04). This plan follows the Nx workflow work merged in #285 and the SDK host fixture and zero-DSP card safety work merged in #286.

## Checkpoint: selected module work

- Scoped patchkit pack/gen and selected TypeScript conformance now read explicit manifests. The separate `patchkit:catalog-check` validates the repository catalog; its named Nx target passed on 69 manifests. Core generation with all 21 core manifests reproduced 24 generated files byte for byte against the former full scan.
- A hypothetical Analog manifest edit now selects six requested targets: its pack and conformance, the catalog check, and three host/Rust checks. It selects no other shipped module conformance or quick runtime capacity test. Pluck C source selects three requested targets. These are selection proofs, not device run times.
- A runtime-owned machine/FX fixture passed four coupled cells (DT2 1.17 and DN2 1.12) through its named Nx target: 23 prerequisite/test tasks in 3m 27s, with 2m 34s in the fixture target. The existing zero-DSP service fixture is a separate owner. Adding fixtures does not reduce gate time until their scheduling policy replaces repeated unchanged-module acceptance.
- A separate empty-store menu fixture passed two zero-DSP cells, including explicit zero-peer/no-audio assertions, through its named Nx target in 24.6s. The DSP-dependent menu-capacity test remains coupled and unchanged.
- The whole branch plans more than 80 requested tasks because it changes Nx plugin and gate files. The already agreed broad-suite exception is being used during implementation; no full device gate has run on this branch.

## Outcome

`mise run test:plan` must explain a small, correct affected task set before any build or test runs. A changed shipped module must produce its own `.dkm` and run its own load and behavior checks for the supported devices. Runtime and SDK work must use shared fixtures for common behavior, while real changed modules still prove that their artifacts load. Builds and repeat tests should use caching only after the input and output boundary is demonstrated.

## Sequence

1. **Scope selected module commands.** Selected patchkit `pack` and `gen --modules` should read the selected manifests and their actual dependencies; selected conformance should read its owning manifest. Keep unselected whole-repository modes. Add a fast host-only catalog validation for repository-wide manifest errors, including duplicate IDs and broken `requires`. Make affected `pack` a direct target of `mise run test`, so changed packable modules build even if they have no device-test target. Preserve failures for malformed selected manifests, unknown IDs, missing requirements, and cycles. Do not narrow Nx inputs until the command read closure is proven.
2. **Clarify the Nx graph.** Split `tools/nx/modules.mjs` by discovery, target inference, and dependency inference. Derive the runtime identity from its manifest role and validate uniqueness. Keep the physical `modules/core/runtime` directory while the graph and ownership become clear; a move has no measured feedback-loop benefit. Replace broad manifest inputs with precise owner and dependency inputs only after step 1.
3. **Move common acceptance to fixtures.** Map every existing assertion to its owner first. SDK host fixtures cover declaration and API contracts; zero-DSP DT2/DN2 runtime fixtures cover boot, card, and menu behavior that does not use DSP; coupled fixtures cover machine/FX registration and routes. Keep current coupled menu-capacity cases where DSP state is the subject. Each changed module retains pack validity, real-artifact load on supported devices, declared contract/version checks, and its own DSP/audio/UI behavior. Fixture coverage does not imply that all shipped artifacts are compatible after a public ABI or loader change; define those wider-change triggers explicitly before reducing their gate selection.
4. **Prove caching and selection.** For each cache change, inspect declared inputs; prove a relevant edit misses, an unrelated edit hits, and `dist/` producer outputs restore. Keep native read-closure requirements. Assert representative affected plans for module source, manifest, SDK header, runtime, menu, and harness edits. Run only focused host and fixture checks during implementation, then one complete affected `mise run test` before merge. Use `test:full` only if generated core changes require it.

## Current observations

- At the #286 baseline, a Pluck C source edit selected a small set while a Pluck `module.toml` edit selected 82 requested tasks: pack/gen and conformance read every manifest, and Nx declared those reads truthfully. The scoped command and input work in this branch narrows that selection.
- Nine shipped modules have conformance cells on both devices: six coupled DSP modules (12 cells) and three zero-DSP services (six cells). Conformance proves upload, stored bytes and reboot persistence, registration, contract/DSP status, and fault containment. It does not prove rendered audio correctness.
- Module `pack` is currently `cache: false`, although its object builder has a content-hash cache. Device tests and conformance depend on pack, and this branch adds affected pack targets directly to `mise run test`.
- The scoped pack cache audit still recommends `cache: false`: installed compiler contents, inherited build environment, and mutable DKO intermediates are outside the proven Nx key. The content-hash object cache already avoids repeated compilation. Direct affected pack selection is implemented on this branch.

## Decisions still to prove

- Catalog validation must be selected for every manifest edit without becoming a dependency of each local module pack. A local selected pack may succeed while an unrelated manifest is broken; the merge gate must still reject the repository.
- The final runtime/SDK fixture matrix and triggers for wider shipped-module compatibility testing need assertion-level coverage evidence before any existing device checks are removed.
- Nx 24 affected selection propagates through declared producer outputs and `^production`; a truthful cache key alone cannot express "hash this runtime but do not schedule its consumers." Runtime and SDK fixture-first scheduling therefore needs an explicit, documented gate policy rather than deleting real inputs. Manual per-module conformance keeps its full runtime/image cache key.
- The full-tier inferred-needs cases now have their own `core/runtime-inferred-needs-tests:test-device-full` target. Its 12 real modules and fixtures do not select the quick capacity target for a module edit. One named DT2 `test/c-demo` case passed; the other 22 cases were filtered out, so the full matrix remains unverified on this branch.
- A future raw card module needs explicit delivery metadata. Current Nx inference gives a pack target to dynamic, host, or service modules (and acceptance fixtures), so a plain `type = "raw"` module with no such marker would not pack in the gate. Do not overload `dynamic` to mean card delivery.
