# ADR-0045/ADR-0049 checkpoint cleanup

## Goal

Close the remaining documentary checkpoint state by ratifying ADR-0045 and
formally recording ADR-0049's class-pattern deferment without changing Aura
language or tooling behavior.

## Work completed

- Changed ADR-0045 from Provisional to Accepted and recorded the Batch S1
  checkpoint as its acceptance point.
- Checked the final completion-matrix item and explicitly ratified all six
  testing-contract choices: registration shape, `-k` semantics, assertion
  operand bounds, lifecycle-failure precedence, phase isolation and
  capture-free cases, and JSON result schema 1.
- Changed ADR-0049's class-pattern disposition from a pending checkpoint
  question to an accepted deferment. Class-pattern implementation now requires
  a future dedicated ADR defining match exposure, visibility, property
  evaluation, ownership, mutation, and exhaustiveness behavior.
- Updated the ADR index and Manual conformance ledger, then regenerated the
  public LLM documentation derived from the Manual.
- Added reference-integrity assertions that reject a regression to
  provisional status in either ADR and pin the accepted conformance wording.

## Verification

- The new reference-integrity assertions failed against the previous
  provisional ADR state before the documentation changes were applied.
- `bash scripts/check-reference.sh` passes after the ratification updates and
  generated-document refresh.
- `python3 scripts/generate_llms.py --check` passes.
- `git diff --check` passes for every file in this checkpoint-cleanup change.
  The full-worktree form still reports trailing whitespace in the protected,
  pre-existing `personal/file_ops.au` user edit, which this task does not
  modify.

The ratification relies on the already-completed Batch S1/S1.1 proof: the full
local repository gate and forced MIR/direct parity matrix were green, and CI
run 30846511697 passed the complete gate on Ubuntu 24.04 and macOS 15 at
`b5831abb258ea836d43b6421698306679688f006`.

## Follow-up

No implementation follow-up is required. Class patterns remain outside Aura
0.3 unless a future dedicated ADR is proposed and accepted.
