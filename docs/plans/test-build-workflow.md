# Test and build workflow

Objective: fast, accurate Nx selection with explicit build and cache boundaries, while retaining
coverage. Nx owns scheduling; mise remains a thin entry point rather than a second router.

## Sequence and checkpoints

0. **Done: upgrade Nx first.** `80135ab5` pins Nx/devkit 24.0.3. All 120 project definitions and
   edges match Nx 22; representative task selection matches. Frozen install, 15 preflight tasks,
   nine Nx audit unit tests and formatting passed. Native `summary` output works; `--graph` plans
   tasks and `--explain` reports task-selection reasons without execution.
1. **Active: prove selection before changing defaults.** Completed sub-checkpoint: narrowed the
   global desktop UI host-test input, preserved actual web imports, registered standalone runtime
   unit tests, and added real Nx selection regression cases. These check runtime source/dependency
   changes, UI edits and leaf-module conformance under both affected modes. The first unit run exposed
   stale host-test compilation/link fixtures (page parameter wrappers, removed step allocations,
   optional firmware symbols); repaired test-only setup without changing production behavior.
   Host input audits also exposed a stale plusdrive digest path: aligned it with the actual
   `dist/plusdrive/plusdrive` binary read by regression helpers, retaining its content digest.
   Completed evidence: ten real selection cases pass across both modes, including
   SDK/fit edits selecting runtime unit tests. The runtime target passes all 32 host unit tests
   without daemon prerequisites. All five cached host targets pass input audits with successful
   tests; 16/16 affected preflight tasks and formatting pass. No device runs or full gate were run.
   Legacy UI selection still includes web's host suite through web's other UI inputs; task-level
   selection selects only desktop. Stage 1 remains active for the cycle fix and affected-mode decision.
   Keep `affected.legacy=true` until the broader matrix is accepted.
   Next, break runtime acceptance cycles: proposed nested `modules/core/runtime/tests` project
   `core/runtime-tests` owns acceptance metadata and depends on runtime plus its fixtures. The
   plugin must avoid parent-to-child edges for tagged test-only projects, infer their targets and
   include their metadata when discovering packs. Retain `core/runtime:test-device` as a task
   alias; update the layercheck exception. Prove no runtime/content strongly connected component.
   This boundary is a design checkpoint, not part of the current implementation batch.
2. **Next: prove build-cache boundaries.** Early win `16594f8b` excludes task definitions from the
   daemon key while hashing the Rust pin and actual compiler identity. Six focused tests, mypy,
   Python lint and eight preflight tasks passed; 88 existing depfiles contain no mise input.
   The new scheme causes one initial daemon-store miss. Next audit module-pack and daemon
   outputs, dependency hashes, firmware/ready/card identity, and cache restore behavior. Any new
   cached device target requires input audit and coverage parity evidence before enabling cache.
3. **Next: separate execution boundaries.** Provide independent unit, component, ColdFire-only,
   coupled and device targets. Establish SDK contract fixtures and retain each module's own
   conformance. Runtime edits may skip consumer conformance only when contract evidence proves
   that boundary; folder names alone do not establish independence.
4. **Next: share session infrastructure.** Consolidate TypeScript fixtures, reset and event
   helpers; use bounded concurrency after session isolation is proven. ColdFire waits cannot
   depend on DSP-frame progress. Define a fresh card baseline, epoch/protocol checks and a rule
   to discard dirty daemons before reuse.
5. **Optional, last: move directories.** Consider runtime outside content after dependency and
   execution boundaries work. Create a raw folder only for an actual contract or module.

After the task graph is correct, simplify commands and docs around one default test command,
native Nx `--explain` planning and concise `summary` output. Current names are confusing; resolve
them without introducing another selection router.

## Acceptance and remaining decisions

- Keep a real-file selection matrix with task IDs, dependencies and reasons; check positive
  coverage and unwanted selection. Leaf changes retain their own conformance.
- Audit narrowed inputs against meaningful tests. Cached outputs must restore correctly and
  invalidate for source, dependency, toolchain and device-identity changes.
- Decide the minimum SDK contract fixtures before reducing cross-module conformance.
- Decide the fresh-card/reset/epoch contract and ColdFire progress signal before pooling sessions.
- Preserve coverage on DT2 OS 1.17 and DN2 OS 1.12. Delete scenarios only after both-device parity.
- Iterate with scoped owner checks, never full suites. Run the merge gate before merging unless
  the user explicitly approves an exception. Record checkpoint commits and actual checks here.

## Limits of current evidence

No measured speedup or full gate is claimed. The Nx upgrade preserves project-level affected
semantics. Task-level selection is an explicit test override until its matrix is reviewed.
No device coverage has been removed, and no new device-test cache boundary has been established by
these host-only checks.
