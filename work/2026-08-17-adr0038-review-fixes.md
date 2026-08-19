# ADR-0038 implementation review fixes

## Goal

Close every confirmed defect from the independent ADR-0038 implementation
reviews, the final Sol Max review, and the Daybreak vulnerability review
without expanding the feature beyond ADR-0038's accepted
first-implementation boundary.

## Work completed

- Kept returned views non-owning at ordinary bindings, `own` parameters,
  Queue/Transfer boundaries, and structural function-value storage. Returned
  view kind, origin, exact single-projection footprint, concrete/generic trait
  dispatch, forwarded calls, and module-qualified exports now remain coherent.
- Replaced statement-header lifetime approximation with recursive exact
  last-reference spans. Reborrows retain parent identity, suspend ancestors
  while descendants are active, and resume parents after child cleanup.
- Preserved closure-loan regions across moves, canonicalized captures of
  existing views, rejected tuple/aggregate escape, rejected `own` snapshots of
  views even for Copy pointees, and retained explicit capture-list order.
- Added path-sensitive MIR loan validation to trusted, public serialized, and
  direct-native entry paths. Runtime loan registration independently rejects
  capability escalation, invalid overlap, parent-before-child teardown, and
  access through suspended parents.
- Removed direct code generation's dependence on basic-block storage order and
  lowered returned-view forwarding without a panic. MIR and direct execution
  now agree on branch-local views, closure reborrows, forwarding, and parent
  resumption.
- Made nested `if`, `match`, and `with` last uses end inherited loans only on
  the selected control-flow path, while conservatively retaining loop-carried
  loans across backedges. Returned child views may now run ancestor and managed
  cleanup after the outgoing handoff without violating MIR linearity.
- Preserved all projection alternatives when a returned view is forwarded
  through local aliases, generic trait calls, and another returned-view call.
  Conditional wrappers can no longer hide a returned view inside class or
  aggregate storage.
- Made grouped lambdas retain closure-loan metadata, made closure-held children
  suspend their parents, and canonicalized captures of expiring views to a
  stable physical source. The MIR validator now distinguishes that source
  metadata from a second direct access and rejects redirected or capability-
  escalating serialized captures.
- Treated fixed tuple indexes as borrowable places and typed tuple views without
  consuming non-Copy elements, preserving parent capability for reborrowing.
- Bounded adversarial serialized-MIR loan expansion before materialization:
  duplicate returned projections are deduplicated and unique expanded paths
  share a 4 MiB cumulative byte ceiling in both validation and execution.
- Made loan locks apply uniformly to mutable arguments, ownership moves, Copy
  pointees, enum payloads, and aggregate construction. Unsupported loan-bearing
  closure returns, branch selection, nested capture, and structural storage now
  fail in semantic analysis instead of producing snapshots or backend-specific
  behavior.
- Scoped returned-view projection handoff to the active call frame so nested
  cleanup calls cannot retarget an outgoing view. Dynamic returned-view closure
  captures snapshot the selected physical descriptor, and imported or generic
  forwarding resolves projections in the declaration owner's module, trait,
  bound, and substitution context.
- Added whole-function MIR structure checks for duplicate block labels and all
  successor targets, including unreachable blocks. Direct lowering now emits
  reachable blocks with CFG-propagated per-block view state and stable selector
  values, so neither block storage order nor unreachable metadata can change a
  live alias.
- Centralized canonical and type-valid projection checks for reborrows and all
  returned-view alternatives. Cumulative validation and active-state budgets
  also cover chained reborrow expansion before paths are materialized.
- Bound public MIR calls to authoritative function signatures, parameter
  passings, mutable writeback identities, closure-capture capabilities, and
  reachable returned-view contracts. Symbolic loan traversal is depth-bounded,
  and generic trait calls carry their declaring trait identity through
  validation, MIR execution, and direct code generation.
- Preserved returned-view descriptor ancestry through nested reborrows and
  closure capture, composed call-rooted child projections explicitly, retained
  class/tuple suffixes in `ReturnLoan` contracts, and made specialized generic
  projection summaries independent of same-named trait implementation order.
- Made mutable receiver, argument, and closure-capture writes observable before
  traps and cleanup on both backends. Direct closure metadata now reaches a
  fixed point across loop backedges, while dead loop-local descriptors are
  pruned without weakening live selector/writeback identity checks.
- Made direct ownership bookkeeping path-sensitive for match variants and
  dynamic trait dispatch, eliminating projected opaque-value leaks. Generated
  receiver metadata follows the materialized operand across calls, indexing,
  iteration, operators, slices, membership, and `len`.
- Kept collection indexes as explicit MIR operations instead of spelling them
  as stable tuple/class places. Runtime place typing now traverses tuple
  positions before class fields, so projected `BorrowMut` operator writebacks
  agree on MIR and direct backends.
- Separated an alias's pointee type from its conservative conflict footprint,
  added non-consuming nested tuple/class place typing, made `if`/`match` expiry
  path-local, and made returned-view callee resolution lexical-local-first and
  transparent through grouping and specialization.
- Corrected analysis provenance and go-to-definition, recorded capture-list
  identifier occurrences, recovered completion in partial capture lists, made
  boundary cursors in multiline capture lists work, made bounded generic trait
  methods navigable, made `view`/`from` TextMate scopes contextual under a real
  tokenizer, and repaired the maintained architecture, Manual, and tutorial
  documentation inconsistencies found by review.
- The Daybreak review's checked-source forwarding panic remained a correctness
  defect in the submitting one-shot process. Follow-up adversarial review found
  an availability risk in cumulative serialized-MIR loan-path expansion; the
  new pre-materialization ceiling closes that path while preserving ordinary
  and duplicate-projection inputs.

## Verification

- All 137 ADR-0038 compiler tests pass, including forged public MIR,
  forwarding, module-qualified metadata, trait identity and implementation
  ordering, closure moves, aggregate escape, exact lifetime, projected
  operator writeback, and direct-codegen validation regressions.
- The complete compiler library suite passes 1,819/1,819 tests. The broad CLI
  integration run passed 371/374 cases under parallel load; its three
  cache/scheduler timing failures each passed an exact isolated rerun. The
  subsequently added trait-order and projected-operator regressions pass on
  both backends. CLI unit tests pass 31/31 and native-codegen acceptance passes
  1/1.
- The complete language-server suite passes 111/111 and the extension suite
  passes 27/27, including real-tokenizer coverage for tuple returned-view
  contracts and ordinary calls named `view`.
- A combined exact MIR/direct runtime probe produces identical output for
  selected-path cleanup, returned child and forwarded aliases, managed cleanup,
  tuple reborrowing, shared/mutable closure capture, dynamic projection
  writeback, and reversed CFG storage order.
- The complete forced MIR/direct runtime-fixture matrix passes with fallback
  disabled and local loopback enabled (1/1 aggregate test, 1,186.97 seconds).
- The serialized-MIR availability regression remains below 64 KiB, is rejected
  before cumulative reborrow paths are materialized, and an ordinary serialized
  control still executes. Common validation rejects malformed projections and
  CFG structure before either backend.
- All 340 tutorial fences, all 129 verified Aura Manual blocks, reference
  integrity, generated LLM-document freshness, the production documentation
  build, formatting, and compiler Clippy with warnings denied pass. Coverage
  was not repeated; the earlier implementation baseline remains the current
  coverage evidence.

## Follow-up

No confirmed review defect remains after independent post-fix semantic,
returned-view, public-MIR, native-parity, and TextMate rechecks. Indexed/keyed views, view-bearing
aggregates, multi-origin returns, returned loan closures, and structural
lifetime-bearing callable types remain intentionally deferred by ADR-0038.
