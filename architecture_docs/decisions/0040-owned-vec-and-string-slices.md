# ADR-0040: Owned Vec and String slices

> Approved next extension (2026-09-06): [ADR-0061](0061-collection-element-loans-and-slice-views.md)
> adds slice views with explicit lifetime rules. Existing owned slices remain
> copies; this approval does not silently change them into aliases.

- Status: Accepted
- Date: 2026-07-31
- Roadmap decision: Batch 6, Phase 7.2
- Builds on: ADR-0004, ADR-0016, and ADR-0022
- Distinct from: ADR-0038

## Context

Aurora has Python-shaped Vec indexing and Unicode-aware Strings, but selecting
a contiguous subsequence still requires an explicit loop or string operation.
Phase 7 adds the familiar `value[start:end]` syntax without introducing the
place identity, lifetime, or aliasing model reserved for ADR-0038.

Python clamps slice endpoints into range. Aurora's existing negative-index
policy instead normalizes once and reports an invariant failure when the
result is invalid. Slices must keep that difference visible rather than
silently select different data. They must also state how an owned Vec result
duplicates non-Copy elements and how a UTF-8 String maps scalar positions to
byte boundaries.

## Decision

### Forms and result types

Aurora accepts half-open slices on `Vec[T]` and `String`:

```aurora
value[start:end]
value[:end]
value[start:]
value[:]
```

`Vec[T]` slicing returns a fresh owned `Vec[T]`. `String` slicing returns a
fresh owned `String`. Neither result aliases its source, and neither expression
is a place. Mutating an owned Vec result cannot mutate the source.

The selected range includes the normalized start and excludes the normalized
end. Equal endpoints produce an empty result. An omitted start means zero. An
omitted end means the source length; this conceptual endpoint does not narrow
the source length through the public `int32` endpoint type.

Slicing is unavailable on Map, Set, Range, tuples, arbitrary user types, and
other builtin values. Integer indexing on String remains unavailable.

### Endpoint types and bounds

Each written endpoint has exactly type `int32`. A contextually typed integer
literal may adopt `int32`; an already-bound `int64` value is not implicitly
narrowed.

For a source of length `len`, each written negative endpoint `i` is normalized
exactly once as `len + i`. After normalization, both endpoints must be in the
inclusive range `0..=len`, and start must not exceed end. A violation traps
with `AU4003`.

Aurora deliberately does not use Python's clamping slice semantics. An endpoint
that remains below zero or lies above the source length is an error, not an
instruction to substitute the nearest boundary. A reversed range is also an
error rather than an empty result. This makes a mistaken boundary loud.

### String units and complexity

String endpoints count Unicode scalar values, matching `String.len()`. They do
not count UTF-8 bytes or grapheme clusters. The implementation finds the
corresponding UTF-8 byte boundaries and copies that valid substring into a new
String.

String slicing is O(n) in the source text because locating scalar boundaries
requires a scan. The returned substring allocation and copy are proportional
to the selected UTF-8 byte length. Aurora still has no integer
`string[index]` operation and no distinct character type.

### Vec ownership and clone safety

Vec slicing reads the source through shared access and constructs a second
owned sequence:

- Copy elements are copied.
- Non-Copy, clone-safe elements are explicitly cloned by the slice operation.
- An element type containing non-cloneable `random.Rng` state is rejected with
  `AU3007`.
- A capturing closure environment, including one placed in a Vec through a
  generic constructor, is not clone-safe and is rejected with `AU3007`.
- An element type containing a non-repeatable `Task` observation right is
  rejected with `AU3009`.
- A generic definition that slices `Vec[T]` records the corresponding inferred
  clone-safety or task-repeatability obligation and validates it after
  specialization.

The source remains owned and usable after a successful slice. Aurora does not
destructively move elements from a shared source and does not create hidden
views.

### Evaluation and retained access

The base, written start, and written end expressions are each evaluated
exactly once, from left to right. An omitted endpoint evaluates no expression.
The selected non-Copy base is retained through endpoint evaluation under
ADR-0016, so a later endpoint may read the same source but may not mutate or
consume an overlapping place.

Bounds are normalized and checked only after the reached base and endpoint
expressions have completed. The selected values are then copied into the fresh
owned result in source order. A trap during evaluation or allocation produces
no partial result.

### Reserved syntax

A third, step component is reserved but unavailable:

```aurora
value[start:end:step]
```

It reports `AU2005` with the exact guidance `slice steps are unavailable; use
an explicit loop to select a stride`. The diagnostic is also used for
omitted-step spellings such as `value[::]`; accepting a syntactic no-op would
reserve the wrong observable contract.

Slice assignment and compound slice assignment are unavailable:

```aurora
value[start:end] = replacement
value[start:end] += replacement
```

They report `AU2005` with the exact guidance `slice assignment is unavailable
because slices are owned copies; mutate the source by index or build a new
value`. A slice is an owned value expression, not an assignable view.

## Relationship to ADR-0038

ADR-0038 designs future place-based loans and views for Aurora 0.3. This
decision does not implement any part of that design. A Phase 7 slice has no
PlaceId, source generation, lifetime, write-through behavior, reborrow, or
returned-view provenance. Future views must use their own explicit source form
and cannot reinterpret `value[start:end]` as an alias.

## Consequences

Aurora gains a familiar compact subsequence operation while preserving its
owned-result model and loud bounds policy. Code can safely retain and mutate
the source independently from the slice. The costs are also explicit:
non-Copy Vec elements are cloned, and String scalar slicing scans UTF-8 text.

Programs that require stride selection, in-place range replacement, a
zero-copy alias, grapheme segmentation, or arbitrary iterable slicing must use
an explicit operation today or wait for a separately ratified design.

## Completion tests

- parser fixtures for all four endpoint forms, nesting and postfix chaining,
  multiline layout, malformed colons, reserved steps, and slice-assignment
  guidance
- static fixtures for exact `int32` endpoint typing, contextual literals,
  Vec/String result typing, unsupported bases, String integer-index rejection,
  clone-safe and non-cloneable Vec elements, non-repeatable Tasks, and generic
  inferred obligations
- ownership fixtures for retained base access, endpoint read/mutation/move
  conflicts, source reuse, result independence, and non-place slice results
- runtime fixtures for empty/full/prefix/suffix/interior slices, every negative
  endpoint boundary, out-of-range and reversed `AU4003`, Unicode scalar String
  slicing, and evaluate-once left-to-right effects
- MIR/direct parity, compiler analysis, completion, hover, definition,
  language-server, editor, maintained-example, and source-hash-pinned Manual
  coverage

## Ratification

Batch 6 authorizes this decision as the binding Aurora 0.2 Vec and String slice
contract. Implementation, reference, diagnostics, examples, and editor
behavior land together under the reference-freeze rule.
