# ADR-0039: Comprehensions

> Approved next integration (2026-09-06): [ADR-0054](0054-generators-and-iterator-protocol.md)
> adds protocol-based sources with distinct item/end results. Existing eager
> comprehension evaluation and cleanup remain the implemented baseline.

- Status: Accepted
- Date: 2026-07-31
- Roadmap decision: Batch 6, Phase 7.1
- Builds on: ADR-0006, ADR-0016, ADR-0017, and ADR-0037

## Context

Aurora already has Python-shaped collection literals and bare `for`
iteration, but transforming a collection still requires either a named
callback or a statement loop with a mutable result. Phase 7 adds the familiar
list, set, and map comprehension shapes without adding lazy iterators,
implicit copying, or another ownership mode.

The syntax writes the result expression before the clauses even though the
result cannot execute until clause targets have been bound. Nested clauses,
filters, Queue iteration, move-only elements, and lambdas therefore need one
precise evaluation and ownership contract shared by both backends.

## Decision

### Forms

Aurora accepts eager list, set, and map comprehensions:

```aurora
[value * 2 for value in values if value > 0]
{value for value in values if value > 0}
{entry.key: entry.value for entry in counts.items()}
```

One or more `for` clauses are required. Any clause may be followed by zero or
more `if` filters, and another `for` clause may follow:

```aurora
[left * 10 + right
 for left in values if left < 3
 for right in values if right < 3]
```

Comprehension targets use the same single-name and recursive tuple-target
grammar as statement `for`. Clause iteration has no `mut` or `own` spelling:
every clause uses the existing bare-loop contract. Generator expressions such
as `(value for value in values)` remain unavailable and receive `AU2005`:

```text
generator expressions are unavailable; use an eager owned list comprehension or an explicit loop
```

### Type and scope

A list comprehension produces `Vec[T]`, a set comprehension produces
`Set[T]`, and a map comprehension produces `Map[K, V]`. The result expression,
or key and value expressions, determines the specialization unless an expected
collection type supplies context. Every filter must have exactly type `bool`.

Targets enter scope progressively. A clause target is visible in that clause's
filters, every later clause, and the result expression, but not in its own
iterable expression. Earlier clause targets are visible in later iterable
expressions. Targets follow Aurora's ordinary no-shadowing rule and do not
leak into the enclosing scope after the comprehension.

### Evaluation

Comprehensions are eager. Evaluating one creates a fresh result collection,
then executes its clauses as nested bare `for` loops:

1. the first iterable expression is evaluated once
2. its target is bound for one item
3. its filters are evaluated from left to right, stopping that item at the
   first `false`
4. each later iterable is evaluated once for every surviving combination of
   earlier targets
5. at the innermost surviving combination, the result expression is evaluated
   and inserted into the result

The order is outer-major: the complete inner traversal for one outer item
finishes before the next outer item begins. Although result syntax appears
first, no result expression executes before all clauses and filters that guard
it. A map evaluates and captures its key before evaluating its value.
Duplicate set values collapse, and a later equal map key replaces its value
under the ordinary collection rules.

Every comprehension returns the newly owned collection. It is not a view of
any source and has no lazy resumption state.

### Iteration and ownership

Each clause accepts the same iterable forms as a statement bare loop:

- `Vec[T]` and `Set[T]` are shared and frozen for the active traversal
- `Range` yields independent copy `int32` values
- `enumerate(...)` and `zip(...)` retain their compiler-known bare-loop
  contracts
- `Queue[T]` preserves ADR-0006's receive carve-out: the Queue handle is copied
  once for the active clause and each received item is already owned

ADR-0016 expression sequencing applies inside iterable, filter, key, value, and
element expressions. A reached expression evaluates its subexpressions in the
ordinary left-to-right order, with retained place borrows and move conflicts
checked normally.

Insertion into the result is an owned storage boundary. Copy values are
copied. A non-Copy value must already be owned and moves into the result. A
non-Copy element reached through bare shared Vec or Set iteration therefore
needs an explicit `.clone()` when it is clone-safe; Aurora never inserts a
hidden clone. Queue iteration may move each received owned item directly into
the result.

Loop-carried ownership checking applies to every clause. A move from an outer
place is rejected when another reachable iteration could use the moved place.
An abandoned partial result is cleaned up on a runtime trap or `try`
propagation.

### Closures

Lambdas evaluated in iterable, filter, key, value, or element expressions
follow ADR-0037 without an exception. Creation snapshots Copy captures, moves
owned non-Copy captures, rejects shared or mutable capability capture, derives
repeatability and Transfer structurally, and keeps capture environments
read-only in Aurora 0.2.

A comprehension target is a local binding, not a capture of a lambda that
encloses the whole comprehension. A lambda created inside a reached
comprehension step may capture a Copy target by value. Capturing a shared
non-Copy target is rejected; a Queue-received owned target may move into a
single reached closure. The existing collection-storage rule also remains:
capturing closure values cannot themselves be stored as comprehension output,
although a compiler-known callback may use a qualifying closure within an
element or filter expression.

## Consequences

Aurora gains the compact transformation form Python developers expect while
keeping allocation, order, and ownership explicit. Comprehensions do not
create a second iterator protocol, do not provide mutable or consuming source
traversal, and do not weaken closure or move checking.

Eagerness means an unbounded Queue comprehension is also unbounded work and
storage until that Queue iteration terminates. Use an explicit loop when the
program needs early exit, mutation, incremental consumption, or bounded
streaming behavior.

## Completion tests

- parser fixtures for list/set/map, filters, nested clauses, tuple targets,
  multiline layout, rejected modifiers, malformed/trailing-comma forms, and
  the exact generator-expression `AU2005`
- static fixtures for inference and expected types, exact-Boolean filters,
  scope/non-leakage, no-shadowing, every bare iterable kind, frozen sources,
  explicit clone/move rules, and loop-carried moves
- closure fixtures for enclosing-lambda capture discovery, immediate Copy
  capture, shared non-Copy target rejection, and capturing-result rejection
- runtime fixtures for eager execution, left-to-right filters, outer-major
  nesting, set deduplication, map key-before-value/replacement, Queue receive
  ownership, traps, `try`, and partial-result cleanup
- MIR/direct backend parity, compiler analysis, completion, hover, definition,
  language-server, editor, maintained-example, and source-hash-pinned Manual
  coverage

## Ratification

Batch 6 authorizes this decision as the binding Aurora 0.2 comprehension
contract. Implementation, reference, diagnostics, examples, and editor
behavior land together under the reference-freeze rule.
