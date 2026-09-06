# Priority roadmap amendment — 2026-09-06

## Goal and authority

Apply the user's approved documentation-only amendment to commit `9fbe495`.
Preserve the ten Approved Decisions verbatim and all independent uncommitted
v0.3.3-preview release work. Do not implement scheduled compiler, runtime,
benchmark, example, library, formatter, Manual, or release changes.

## Changes by file

| File | Amendment |
| --- | --- |
| `architecture_docs/14-priority-roadmap.md` | Recorded usability-first rationale and pending release coverage; six pre-Batch-1 foundations with completion criteria; staged Batch 1, shared Batch 3 cleanup, Batch 4 formatter/API docs, focused Batch 5/11, decode eligibility, Batch 7 backend decision, nominal iterator step, unscheduled systems direction, and checkpoint dependencies |
| `architecture_docs/decisions/0038-place-based-loans-and-views.md` | Joint callable/collection lifetime ownership; cancellation/reset and temporary-origin conflicts; shared partial-cleanup extension |
| `architecture_docs/decisions/0052-anonymous-closed-union-types.md` | Inline one-member-policy markers; Batch 1 generic-member/narrowing/FFI criteria and pre-removal checkpoint |
| `architecture_docs/decisions/0053-function-decorators.md` | Inline pending evaluation/application order and recursive-initialization markers |
| `architecture_docs/decisions/0054-generators-and-iterator-protocol.md` | Nominal-enum advancement result, with names still open in Batch 9 |
| `architecture_docs/decisions/0055-display-trait-and-properties.md` | Concurrent print atomicity marked as pending in Batch 4 |
| `architecture_docs/decisions/0056-docstrings-and-documentation-metadata.md` | Byte limits/counting and associated thresholds marked as pending in Batch 4 |
| `architecture_docs/decisions/0058-first-class-callables-and-binding-contracts.md` | Owned-capture/owned-or-Copy receiver Batch 1 scope; joint Batch 1–2 loan storage; sema split and intermediate checkpoint |
| `architecture_docs/decisions/0059-custom-initialization.md` | Shared partial cleanup and computed-initializer decode conflict |
| `architecture_docs/decisions/0060-typed-context-managers.md` | Cancellation/reset, scoped header-manager origins, and shared partial cleanup |
| `architecture_docs/decisions/0061-collection-element-loans-and-slice-views.md` | Joint ownership of lifetime-bearing callable design |
| `architecture_docs/decisions/0062-typed-serialization-validation-and-schemas.md` | Field-constructed-or-explicit-factory decode rule and reuse of Batch 3 cleanup |
| `architecture_docs/decisions/0064-native-backend-strategy-and-codegen-boundary.md` | New accepted-direction record with optimization/baseline scheduling, thin boundary, backend alternatives, rationale, and evidence locations |
| `architecture_docs/decisions/README.md` | Linked ADR-0064, extended the decision range, and corrected an identity-gate phrase; preserved the pre-existing uncommitted ADR-0049 description change |
| `work/task-board.md` | Added only this task's status entry, preserving release status |
| `work/2026-09-06-roadmap-amendment.md` | Recorded authorization, changes, reconciliation, verification, tensions, and implementation follow-up |

## Reconciliation table

| ADR | Statement or conflict | Disposition |
| --- | --- | --- |
| 0052 | One-member union rejection | Proposed baseline, not ratified; resolved in Batch 1; body and test expectations marked inline |
| 0052 | Type-parameter union members, stable-place narrowing, FFI nullable replacement | Must resolve before Option removal in Batch 1; narrowing implemented in the removal family |
| 0053 | Decorator evaluation/application order and recursive binding initialization | Proposed baseline, not ratified; resolved in Batch 6; original text retained |
| 0055 | Concurrent print output atomicity | Proposed baseline, not ratified; resolved in Batch 4 |
| 0056 | Docstring/interface byte limits and counting policy | Proposed baseline, not ratified; resolved in Batch 4 |
| 0038 / 0058 / 0061 | Circular deferral of lifetime-bearing callable storage | One joint Batch 1–2 design; initial Batch 1 storage limited to owned captures and owned/Copy receivers |
| 0038 / 0060 | Cancellation cleanup after forced frame reset; entry views from header temporaries | Open Batch 3 obligations; no temporary-origin exemption or destroyed-frame cleanup invented |
| 0059 / 0060 / 0062 | Partial-construction cleanup | One Batch 3 mechanism extending ADR-0038's ordered exit-action stack, reused by all three |
| 0059 / 0062 | No general inverse for computed initializer fields | Batch 6 decoding requires automatic field construction or an explicit decode factory |
| 0054 | Item/end advancement representation | Nominal enum, not a third sum mechanism; names to be designed in Batch 9 |

## Approved-Decision tensions preserved

- Decision 6 promises general stored capturing closures and bound methods.
  The amendment stages loan-capture storage behind the joint Batch 1–2 design
  and scopes initial receivers to owned or Copy values. This changes delivery
  scope, not the eventual accepted behavior; Decision 6 is unchanged.
- Decision 4 promises cancellation cleanup and scoped entry views. ADR-0038
  forbids arbitrary source cleanup after forced generated-frame reset and
  excludes temporary view origins. These are recorded as Batch 3 conflicts;
  the promise is neither weakened nor treated as already implemented.
- Decision 10 approves typed serialization generally. The field-constructed-or-
  explicit-factory rule supplies an implementation path for custom-initialized
  classes without claiming their computed fields are invertible. Decision 10
  is unchanged.
- Decision 2's optional replacement remains approved. The pre-removal
  checkpoint, generic-member rule, narrowing, and nullable-FFI criteria stage
  completion without adding compatibility behavior. Iterator nominal enums
  refine Decision 8's already-approved distinction without changing its text.

No Approved Decision is reworded. The current-surface reference agent must use
manual typed schemas and an ordinary streaming loop until later batches add
generated schemas and generators; it adds no new library surface.

## Verification

Passed the scoped check of 16 documents and 83 relative links/anchors,
including index coverage, code fences, final newlines, whitespace, reconciliation
markers, and conflict entries. The work board is checked only for this task's
new section: an initial whole-file scan found existing historical whitespace
and a placeholder link outside the amendment, which were preserved.

The ten Approved Decisions (and their following technical-default paragraph)
are byte-identical to `9fbe495`. `git diff --check` for the amendment and the
tree excluding protected `personal/file_ops.au` passes; the unrestricted check
reports that file's pre-existing trailing whitespace, which is outside scope.

`python3 scripts/test_aura_identity.py` passes all 15 tests. Its first run
identified two pre-existing phrases in the roadmap and ADR index that violated
the current documentation-wording gate. Rephrased those lines without changing
the Approved Decisions or test code, then reran the complete identity suite.
No heavyweight compiler or release gate was run for this documentation task.

## Follow-up

Implement the scheduled pre-Batch-1 foundations in a later task, coordinated
with the v0.3.3-preview update; publish measurements only after running them.
Create `architecture_docs/15-backend-boundary.md` with the real semantic
inventory then. Continue into Batch 1 design and its staged checkpoint under
the existing parity and coverage gates. Do not switch native backend before
Batch 7 or schedule Batches 12–13 prematurely.
