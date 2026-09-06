# Aura Architecture Decision Records

Pre-ADR-0042 documents use the former working name Aurora.

These ADRs record accepted language and runtime decisions, contained
provisional decisions awaiting a named checkpoint, and proposed designs that
are not yet scheduled or ratified. `Accepted` means the design is binding; it
does not by itself claim that the implementation is complete. `Provisional`
and `Proposed` decisions are not binding until ratified. Each ADR names the
test locations that must prove completion.

`Accepted direction; detailed design pending` records user-approved behavior
without treating every retained design detail as ratified. Each such ADR lists
its remaining questions and implementation status. The 2026-09-06
[roadmap decisions](../14-priority-roadmap.md#approved-decisions) are recorded
in ADR-0052 through ADR-0064 where relevant. Existing accepted ADRs retain
their implemented contracts and link to future extensions; future approval
does not change the current Manual or compiler. In particular, the global
`Option[T]` replacement belongs to ADR-0052's later implementation family.

1. [ADR-0001: Contextual `None` and `Option` equality](0001-contextual-none-and-option-equality.md)
2. [ADR-0002: Integer division and modulo](0002-integer-division-and-modulo.md)
3. [ADR-0003: Default integer type](0003-default-integer-type.md)
4. [ADR-0004: String semantics](0004-string-semantics.md)
5. [ADR-0005: Method receivers](0005-method-receivers.md)
6. [ADR-0006: Parameter and loop ownership defaults](0006-parameter-and-loop-ownership-defaults.md)
7. [ADR-0007: Duration representation](0007-duration-representation.md)
8. [ADR-0008: Task-result ownership](0008-task-result-ownership.md)
9. [ADR-0009: Borrowed-return containment](0009-borrowed-return-containment.md)
10. [ADR-0010: Chained comparisons](0010-chained-comparisons.md)
11. [ADR-0011: Typed errors and assertions](0011-typed-errors-and-assertions.md)
12. [ADR-0012: Boolean-only conditions](0012-boolean-only-conditions.md)
13. [ADR-0013: Callable sequencing and ownership](0013-callable-sequencing-and-ownership.md)
14. [ADR-0014: Map literals and indexing](0014-map-literals-and-indexing.md)
15. [ADR-0015: Explicit and default argument evaluation order](0015-explicit-and-default-argument-order.md)
16. [ADR-0016: Retained non-copy expression borrows](0016-retained-noncopy-expression-borrows.md)
17. [ADR-0017: Iteration source selection](0017-iteration-source-selection.md)
18. [ADR-0018: Fixed resource read limits](0018-fixed-resource-read-limits.md)
19. [ADR-0019: Duration conversion and timer policy](0019-duration-conversion-and-timer-policy.md)
20. [ADR-0020: Randomness algorithm and security boundary](0020-randomness-algorithm-and-security-boundary.md)
21. [ADR-0021: JSON value model and codec policy](0021-json-value-model-and-codec-policy.md)
22. [ADR-0022: Implicit shared, `mut`, and `own` capability syntax](0022-implicit-shared-mut-own-capability-syntax.md) — accepted, ratified, and implemented
23. [ADR-0023: Byte-vector codecs and hashing policy](0023-byte-vector-codecs-and-hashing-policy.md)
24. [ADR-0024: Assertion evaluation and diagnostic policy](0024-assertion-evaluation-and-diagnostic-policy.md)
25. [ADR-0025: Newline continuation and delimited layout](0025-newline-continuation-and-delimited-layout.md)
26. [ADR-0026: Minimal tuples](0026-minimal-tuples.md)
27. [ADR-0027: Conditional expressions](0027-conditional-expressions.md)
28. [ADR-0028: Membership operators and comparison chains](0028-membership-and-comparison-chains.md)
29. [ADR-0029: `enumerate` and `zip` loop forms](0029-enumerate-and-zip-loop-forms.md)
30. [ADR-0030: `len` and `str` builtins](0030-len-and-str-builtins.md)
31. [ADR-0031: CLI backend defaults](0031-cli-backend-defaults.md)
32. [ADR-0032: Guarded lightweight-task stacks](0032-guarded-lightweight-task-stacks.md) — Accepted at the Batch 4 checkpoint
33. [ADR-0033: Structural Transfer and task-result consumption](0033-structural-transfer-and-task-results.md) — Accepted at the Batch 4 checkpoint
34. [ADR-0034: Typed heterogeneous `select`](0034-typed-heterogeneous-select.md) — Accepted after the Batch 5 nested-payload closure
35. [ADR-0035: Configurable blocking-I/O pool](0035-configurable-blocking-io-pool.md) — Accepted after the Batch 5 default-parallel watchdog closure
36. [ADR-0036: Native structured runtime frames](0036-native-structured-runtime-frames.md) — Accepted at the Batch 4 checkpoint
37. [ADR-0037: Expression closures and value capture](0037-expression-closures-and-value-capture.md) — Accepted at the Batch 6 opening checkpoint
38. [ADR-0038: Place-based loans and views](0038-place-based-loans-and-views.md) — Implemented for Aura 0.3
39. [ADR-0039: Comprehensions](0039-comprehensions.md) — Accepted for Aura 0.2 in Batch 6, Phase 7.1
40. [ADR-0040: Owned Vec and String slices](0040-owned-vec-and-string-slices.md) — Accepted for Aura 0.2 in Batch 6, Phase 7.2
41. [ADR-0041: Contiguous numeric arrays and explicit integer arithmetic modes](0041-contiguous-numeric-arrays.md) — Accepted for Aura 0.2 in Batch 6, Phase 7.3
42. [ADR-0042: Aura product identity](0042-aura-product-identity.md) — Accepted before the first v0.2.0 preview publication
43. [ADR-0043: Unified `int64` index domain](0043-int64-index-domain.md) — Accepted for Aura 0.3 in Batch S1
44. [ADR-0044: Canonical collection surface](0044-canonical-collection-surface.md) — Accepted for Aura 0.3 in Batch S1
45. [ADR-0045: Testing framework and assertion introspection](0045-testing-framework-and-assertion-introspection.md) — Accepted for Aura 0.3 at the Batch S1 checkpoint
46. [ADR-0046: String literal forms and f-string format specifications](0046-string-literals-and-format-specifications.md) — Accepted for Aura 0.3 in Batch S1
47. [ADR-0047: Integer literal bases, bitwise operators, and shifts](0047-integer-literals-bitwise-and-shifts.md) — Accepted for Aura 0.3 in Batch S1
48. [ADR-0048: Power, rounding, divmod, and the math module](0048-power-round-divmod-and-math.md) — Accepted for Aura 0.3 in Batch S1
49. [ADR-0049: Match guards and or-patterns](0049-match-guards-and-or-patterns.md) — Accepted for Aura 0.3; future class-pattern direction recorded in ADR-0063
50. [ADR-0050: Module-level constants and deterministic initialization](0050-module-level-constants.md) — Accepted for Aura 0.3 in Batch S1
51. [ADR-0051: Import aliases and keyword-only parameter disposition](0051-import-aliases-and-keyword-only-parameters.md) — Accepted for Aura 0.3; future call metadata in ADR-0058 and import polish in ADR-0063
52. [ADR-0052: Anonymous closed union types](0052-anonymous-closed-union-types.md) — Accepted direction; explicit unions, optional replacement, aliases, and narrowing; detailed design pending
53. [ADR-0053: Function decorators](0053-function-decorators.md) — Accepted direction; full callable preservation and explicit retry ownership; detailed design pending
54. [ADR-0054: Generators and the iterator protocol](0054-generators-and-iterator-protocol.md) — Accepted direction; distinct item/end, persistent failure, close, and initial pinned frames; detailed design pending
55. [ADR-0055: Display trait and read-only properties](0055-display-trait-and-properties.md) — Accepted direction; independent delivery and explicit effect boundary; detailed design pending
56. [ADR-0056: Docstrings and documentation metadata](0056-docstrings-and-documentation-metadata.md) — Accepted direction; normalized presentation and field/parameter metadata; detailed design pending
57. [ADR-0057: Clean-slate pre-adoption policy](0057-clean-slate-pre-adoption-policy.md) — Accepted as the standing pre-adoption policy
58. [ADR-0058: First-class callables and binding contracts](0058-first-class-callables-and-binding-contracts.md) — Accepted direction; closures, bound methods, call kinds, and keyword-only metadata; detailed design pending
59. [ADR-0059: Custom initialization and fallible factories](0059-custom-initialization.md) — Accepted direction; `__init__`, definite initialization, and named fallible factories; detailed design pending
60. [ADR-0060: Typed context managers and cleanup](0060-typed-context-managers.md) — Accepted direction; generic/multiple managers, typed entry/exit, and failure precedence; detailed design pending
61. [ADR-0061: Collection-element loans and slice views](0061-collection-element-loans-and-slice-views.md) — Accepted direction; contextual access, explicit owned reads, and invalidation checks; detailed design pending
62. [ADR-0062: Typed serialization, validation, and schemas](0062-typed-serialization-validation-and-schemas.md) — Accepted direction; compile-time opt-in and shared metadata; detailed design pending
63. [ADR-0063: Everyday syntax and pattern ergonomics](0063-everyday-syntax-and-pattern-ergonomics.md) — Accepted roadmap direction; syntax/pattern/API details pending
64. [ADR-0064: Native backend strategy and codegen boundary](0064-native-backend-strategy-and-codegen-boundary.md) — Accepted direction; pre-Batch-1 measurements, incremental thin boundary, and Batch 7 backend decision; detailed design pending
