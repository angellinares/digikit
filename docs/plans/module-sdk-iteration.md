# Fast module and SDK iteration

Status: in progress on `test/module-sdk-iteration` (2026-11-04). This plan follows the Nx workflow work merged in #285 and the SDK host fixture and zero-DSP card safety work merged in #286.

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
- Module `pack` is currently `cache: false`, although its object builder has a content-hash cache. Device tests and conformance depend on pack, but `mise run test` does not request affected pack targets directly.
- The scoped pack cache audit still recommends `cache: false`: installed compiler contents, inherited build environment, and mutable DKO intermediates are outside the proven Nx key. The content-hash object cache already avoids repeated compilation. Direct affected pack selection is implemented on this branch.

## Decisions still to prove

- Catalog validation must be selected for every manifest edit without becoming a dependency of each local module pack. A local selected pack may succeed while an unrelated manifest is broken; the merge gate must still reject the repository.
- The final runtime/SDK fixture matrix and triggers for wider shipped-module compatibility testing need assertion-level coverage evidence before any existing device checks are removed.
- A future raw card module needs explicit delivery metadata. Current Nx inference gives a pack target to dynamic, host, or service modules (and acceptance fixtures), so a plain `type = "raw"` module with no such marker would not pack in the gate. Do not overload `dynamic` to mean card delivery.
