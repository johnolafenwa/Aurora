# ADR-0026: Minimal tuples

> Approved next extension (2026-09-06): [ADR-0063](0063-everyday-syntax-and-pattern-ergonomics.md)
> records tuple trailing-comma consistency and richer unpacking/rest patterns.
> Source ownership and grouping distinctions remain requirements; the new
> forms are not yet implemented.

- Status: Accepted
- Date: 2026-07-24
- Amended: 2026-07-26 (B3.0-c structural tuple equality and inequality)
- Roadmap decision: Phase 3.5 tuples

## Context

Aurora needs a small product type for returning and unpacking a fixed number of
heterogeneous values. The first tuple surface must fit Aurora's existing
copy/move and borrowed-match rules without silently introducing partial moves,
mutable projection writeback, a new collection protocol, or open-ended
indexing.

The Batch 2 tuple ticket therefore defines one deliberately narrow kernel. More
expressive tuple operations remain separate language decisions.

## Decision

- Tuple value expressions are parenthesized: `(left, right)` and the singleton
  `(value,)`. A comma is required, so `(value)` remains grouping.
- Tuple types use the same fixed-arity shape: `(T1, T2)` and `(T,)`.
- `indirect` does not apply to a tuple type. A recursive class link therefore
  cannot be hidden inside tuple storage and must remain a separately named
  `indirect` field.
- Empty `()` tuples are rejected. Tuple literals, tuple types, tuple targets,
  and tuple patterns accept exactly one trailing comma only for arity one.
  Multi-element trailing commas are rejected.
- Tuple values may be parameters, local values, class or enum payloads, and
  function results wherever their exact tuple type is accepted.
- Tuple construction evaluates and captures elements left to right.
- A tuple is copyable exactly when every element type is copyable. Otherwise
  it is a move value.
- Assignment and `for` binding targets may recursively unpack a value whose
  tuple shape and element types match exactly. Every leaf introduces a fresh
  name and cannot shadow a visible binding; member and index leaves are not
  tuple binding targets.
- Unpacking a copy tuple copies its elements. Unpacking a non-copy tuple
  consumes the whole source value once and gives owned leaf bindings. It does
  not create independently reusable partial source projections. Any later use
  of the source is a loud use-after-move diagnostic.
- A bare collection iteration keeps the collection and gives tuple leaves the
  same shared provenance as the yielded element. `own` collection iteration
  and Queue receive iteration give owned tuple leaves. Mutable-borrow
  iteration with a tuple target is rejected in this minimal surface; there is
  no recursive mutable tuple writeback.
- Tuple patterns are recursive and fixed-arity. A by-value `match` consumes a
  non-copy tuple scrutinee as one whole value and gives owned bindings.
  `match borrow` keeps the scrutinee and gives shared leaf provenance.
  `match borrow mut` with a tuple pattern is rejected; tuple-pattern mutation
  and reconstruction are not part of this decision.
- `tuple[INTEGER]` is supported only when the index is a non-negative integer
  literal known at compile time, is in bounds, and the selected element type
  is copyable. The operation returns a copy. Dynamic, negative, out-of-bounds,
  and non-copy-element tuple indexing are rejected. Unpack when ownership of a
  non-copy element is required.
- Tuple iteration, methods, named elements, rest patterns, and implicit
  tuple/collection conversions are not introduced.
- Tuple equality and inequality are defined by the 2026-07-26 amendment below.
  Tuple ordering is not introduced.

These choices are Accepted with the Batch 3 B3.0-c amendment.

## 2026-07-26 Amendment: Tuple Equality

Batch 3 B3.0-c ratifies the minimal tuple kernel and adds builtin tuple `==`
and `!=` with these rules:

- Both operands must have the same static tuple type. Tuple type identity is
  structural, so this requires equal arity and the same corresponding element
  types recursively.
- Equality compares corresponding element values from left to right using each
  element type's ordinary equality semantics. Nested tuples apply this rule
  recursively. `==` stops at the first unequal element, and `!=` is the
  logical negation of `==`.
- Both operand expressions are evaluated once, left to right. Equality reads
  both resulting tuple values and consumes neither, including when the tuple
  contains non-copy elements.
- Runtime element-type, transport, or backend metadata carried with a tuple
  value is not an additional semantic component of equality. Static typing
  establishes compatibility before the recursive value comparison.
- Tuple equality links participate in the ordinary comparison-chain contract.
  Chain operands are evaluated left to right at most once, and the chain stops
  at its first false link without evaluating later operands.
- `<`, `<=`, `>`, and `>=` remain rejected for tuples. The amendment does not
  define lexicographic or metadata-based tuple ordering.

## Rationale

Whole-source moves give one easily explained ownership event and avoid
specifying disjoint partial-move paths for positional projections. Copy-only
constant indexing provides the common read case without a hidden clone or a
destructive read through a general index expression. Shared borrowed
destructuring composes with existing collection and match provenance, while
rejecting mutable tuple writeback prevents an implicit reconstruction protocol
from becoming part of Aurora 0.1 by accident.

## Completion Evidence

- Lexer/parser/AST tests and parse fixtures pin singleton and multi-element
  values and types, nested targets and patterns, comma boundaries, and
  constant-index syntax.
- Checker fixtures pin exact shape/type matching, recursive copy
  classification, whole-source moves, shared provenance, invalid indexing,
  same-static-type equality, continued ordering rejection, and the
  mutable-writeback rejections.
- Run fixtures and CLI parity tests pin tuple returns, recursive assignment,
  `for` unpacking, tuple-pattern arms, recursive equality, non-consuming
  operands, comparison-chain short-circuiting, evaluation order, and equal
  MIR/direct output.
- `examples/basics/tuples.au` and the executable block in
  `docs/manual/tuples.md` pin the maintained user-facing surface and exact
  output.
- The Manual, Current Limits, conformance map, executable-reference gate, and
  tutorial track document the same boundary.
