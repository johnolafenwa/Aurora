# Approved roadmap ADR reconciliation — 2026-09-06

## Goal and authorization

Record the user's acceptance of all ten roadmap recommendations and the
follow-up callable explanation. The requested work is updating the ADRs;
language implementation and release preparation belong to separate work.

## Changes

- Amended ADR-0052–0056 with accepted-direction status, the approval date,
  implementation status, and a separation of settled behavior from retained
  detailed proposals. Removed questions asking the user to approve settled
  product choices again.
- ADR-0052 now records aliases, optional narrowing, the clean-slate optional
  migration, and audits of APIs needing distinct present-`None`/absent cases.
- ADR-0053 depends on a general callable contract and specifies explicit retry
  input ownership. ADR-0054 uses distinct item/end results, persistent failure,
  explicit close, and initial frame pinning with future scheduler integration.
- ADR-0055 separates shared receiver capabilities from an effect/purity
  guarantee and allows independent Display/property delivery. ADR-0056
  normalizes presentation while preserving source content, adds field/parameter
  metadata, and permits documentation delivery before decorators.
- Added ADR-0058 callables, ADR-0059 initialization, ADR-0060 context managers,
  ADR-0061 collection loans, ADR-0062 typed schemas/validation, and ADR-0063
  everyday syntax. These specify accepted direction and required implementation
  evidence without inventing final syntax or unapproved policy details.
- Added future-extension notes to directly affected accepted ADRs, updated the
  index and roadmap, and preserved existing implementation/historical contracts.

## Verification

Passed a scoped check of 29 documents and 97 relative links/anchors, including
index coverage, future-implementation status, code fences, final newlines, and
whitespace. `git diff --check` passes for the edited architecture documents;
a broader initial check found only pre-existing whitespace in protected
`personal/file_ops.au`, which was left unchanged. The superseded generator
exhaustion/failure, exact-indentation presentation, and proposed-status wording
sweep is clear in ADR-0052–0056.

Compiler/runtime tests and the full CI gate are unnecessary for this design
change. No Manual examples, executable fixtures, or reference hashes changed.
The task-board addition is isolated from the existing uncommitted release
status changes; those changes remain owned by the release task.

## Follow-up

Finish detailed callable/type contracts first. Resolve only listed technical
details, including syntax/layout, stream error observation, metadata attachment,
and schema policies; do not reopen the ten approved product decisions. Future
implementation must add behavioral tests and update the maintained language,
tooling, and reference surfaces in the same feature family.
