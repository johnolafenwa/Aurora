# ADR-0016: Retained non-copy expression borrows

> Approved next extension (2026-09-06): [ADR-0061](0061-collection-element-loans-and-slice-views.md)
> generalizes contextual element access and must preserve this ADR's sibling
> evaluation and conflict rules. Collection loans are not implemented by this
> historical decision alone.

- Status: Accepted
- Date: 2026-07-14
- Reference gap: later sibling mutation while an earlier non-copy place remains selected

> **Amended by ADR-0022 (2026-07-27).** The sequencing rules are unchanged,
> but they now apply to copy-typed bare parameters too, because bare means
> shared for every type. A copy argument that overlaps a consumed or mutable
> argument in the same call is therefore rejected where the old copy-snapshot
> rule allowed it.

## Context

Left-to-right evaluation captures copy values without ambiguity. A non-copy
place used as a binary operand, index base, method receiver, or indexed
assignment target is different: the operation retains access to that owned
object until its later inputs have been evaluated. If a later input mutates or
rebinds an overlapping place, implementing the earlier access would require
either a hidden deep clone or a live alias. Hidden cloning violates Aurora's
ownership contract. Live non-copy aliases are reserved for Phase 6, and the
current MIR and direct value representations do not give them identical
semantics.

Python retains object identity in comparable expressions and can expose the
later mutation through the earlier reference. Aurora cannot implement that
choice today without hidden cost or backend divergence.

## Decision

- Evaluating a copy place captures its copied value at that sequence point.
- An operation that produces a point-in-time representation completes that
  operation immediately. In particular, each f-string interpolation renders
  to `String` before the next interpolation begins.
- A non-copy place selected as a binary left operand, index base, method
  receiver, or indexed-assignment target remains borrowed until that operation
  consumes all of its inputs.
- A later sibling may take another shared borrow. An overlapping mutable borrow
  or consumption is rejected with `AU3002`, pointing at the conflicting access
  and the retained-borrow origin. The programmer can clone explicitly or move
  the mutation into a separate statement before beginning the retained borrow.
- Name roots and projected member places follow the same rule. No lowering path
  may obtain parity by deep-cloning a non-copy place implicitly.
- Compound assignment selects its target place once and uses exactly the
  corresponding binary operator dispatch, including an applicable user-defined
  operator trait for a root or projected target. For a copy target, it captures
  the target's current copied value before evaluating the right operand, then
  stores the operator result back into the originally selected place.
  Right-operand side effects cannot change that captured left value or retarget
  the store.
- A non-copy root or projected compound target remains borrowed across right-
  operand evaluation. An overlapping right-operand mutable borrow or
  consumption is rejected with `AU3002` under the rule above. Direct compound
  assignment to a non-copy `Vec` element or `Map` value is rejected: until live
  aliases exist, the read-modify-write cannot be implemented without a hidden
  clone or destructive move.

This is the loud contained resolution required by P1 (no plausible wrong
result), P2 (backend parity), P4 (no hidden clone), P5 (live aliases remain
available for the ratified Phase 6 design), and P6 (one rule for every retained
place form). It deliberately diverges from Python under P3 because identical
object-identity behavior is not implementable on both current backends without
hidden cost.

## Completion tests

- `crates/aurora-compiler/tests/fixtures/run-pass/left_to_right_value_snapshotting.au`
  pins copy capture, immediate non-copy f-string rendering, compound-target
  capture, and receiver-before-argument effects on both backends.
- `crates/aurora-compiler/tests/fixtures/run-pass/operator_traits.au` pins the
  corresponding user-defined binary dispatch for root and projected compound
  targets.
- `compound_noncopy_target_rejects_rhs_mutation.au` pins retained non-copy
  compound-target overlap rejection.
- `vec_compound_assignment_noncopy_element_rejected.au` and
  `map_compound_assignment_noncopy_value_rejected.au` pin the direct-index
  containment boundary.
- `binary_left_borrow_rejects_later_mutation.au` and
  `projected_binary_left_borrow_rejects_later_mutation.au` pin root and member
  overlap rejection.
- `index_base_borrow_rejects_index_mutation.au` and
  `indexed_assignment_target_rejects_index_mutation.au` pin retained read and
  write bases.
- `method_receiver_borrow_rejects_nested_argument_mutation.au` pins the
  receiver/argument boundary.
- `retained_receiver_nested_consumption_repro.au`,
  `retained_argument_nested_consumption_repro.au`,
  `method_receiver_rejects_nested_argument_consumption.au`, and
  `retained_parameter_rejects_nested_argument_consumption.au` pin B2.0-a
  containment when a nested call consumes a retained receiver or earlier
  argument.
