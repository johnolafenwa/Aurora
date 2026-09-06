# ADR-0049: Match guards and or-patterns

> Roadmap disposition (2026-09-06): [ADR-0063](0063-everyday-syntax-and-pattern-ergonomics.md)
> now records the approved class-pattern workstream. Detailed pattern syntax,
> ownership, and visibility remain pending; class patterns are not implemented.

- Status: Accepted; class patterns deferred
- Date: 2026-08-02
- Accepted at: Batch S1 checkpoint, class-pattern deferment recorded 2026-08-15
- Roadmap decision: Batch S1, S4.4
- Builds on: ADR-0022 and ADR-0026

## Context

Enum, tuple, and scalar matching needs two common refinements: a Boolean
condition after a structural match, and one arm that accepts several
structural alternatives. Both features must preserve exhaustiveness, ownership
provenance, delayed extraction for consuming matches, and mutable-match
writeback on every path.

Call-shaped class patterns are a separate design problem. They would commit
Aura to representation exposure, positional-field rules, property evaluation,
and capability behavior that are not implied by enum patterns.

## Decision

### Syntax

A statement or expression match arm may add one guard:

```aura
match response:
    case Result.Ok(value) if value > 0:
        use(value)
    case _:
        recover()
```

An or-pattern joins two or more alternatives with `|`:

```aura
match code:
    case 200 | 201 | 204:
        accept()
    case Result.Err(Timeout) | Result.Err(Cancelled):
        retry()
    case _:
        reject()
```

The grammar is conceptually:

```text
match_arm  := "case" or_pattern ["if" expression] ":" arm_body
or_pattern := closed_pattern {"|" closed_pattern}
```

`|` has the lowest precedence inside a pattern. Parenthesized and nested
patterns may contain an or-pattern wherever all alternatives are valid for
the same expected subpattern type. The same token remains the integer bitwise
operator in expression grammar; the `case` pattern context makes the parse
unambiguous.

### Alternative bindings

Every alternative in one or-pattern must bind exactly the same set of names.
For each name, every alternative must produce the same exact static type and
the same capability: Copy value, shared non-Copy access, mutable non-Copy
access, or owned non-Copy value. Binding order in source does not affect this
comparison. A mismatch rejects the complete or-pattern with related spans for
each conflicting alternative.

After an alternative succeeds, its bindings become the single binding set
visible to the guard and arm body. Alternatives are tested left to right, and
the first structurally successful alternative supplies those bindings. Pattern
tests themselves do not execute user code.

### Top-level binding patterns

An unqualified lowercase name may bind the complete scrutinee at the top level
of an arm. `case name:` is an irrefutable catch-all and must be the final arm.
`case name if condition:` binds the complete scrutinee for the guard and body,
but the guarded arm does not contribute to exhaustiveness. The binding has the
same copy, shared, mutable, or owned capability that the complete scrutinee
would have under the match mode.

Duplicate or subsumed alternatives are unreachable and rejected. Coverage of
an unguarded or-pattern is the union of its alternatives. Existing literal,
Boolean, enum, and tuple reachability rules apply recursively.

### Guard typing, scope, and evaluation

The guard must have exactly type `bool`; Aura performs no truthiness
conversion. Pattern bindings are in scope throughout the guard. Arm-local
bindings do not escape the guard or body.

A match evaluates its scrutinee exactly once. For each arm in source order:

1. its alternatives are probed left to right without consuming the scrutinee
2. if none matches structurally, selection continues with the next arm
3. after one alternative matches, its guard evaluates once
4. a `true` guard selects the arm; a `false` guard continues with the next arm
5. a trap or propagated failure from the guard remains primary and does not
  continue arm selection

A guarded arm does not contribute to exhaustiveness or make later structural
patterns unreachable, because its condition may be false. An unguarded arm
retains the ordinary coverage rules. A final wildcard with a guard is not a
catch-all.

### Ownership and consuming matches

Structural probing never extracts an owned payload. In `match own`, a
non-Copy binding visible to a guard is a candidate view of the private
scrutinee. The guard may inspect it but may not move it, return it as owned,
store it in an owned destination, or capture it in a task or retaining
closure. A `true` guard commits the selected alternative, after which owned
payload extraction occurs exactly once before the arm body. A `false` guard
leaves the scrutinee intact for later arms.

Copy candidate bindings are ordinary values in a guard. Bare matches retain
shared provenance throughout the guard and body.

### Mutable matches and guard writeback

In `match mut`, guard bindings have the same mutable capability as the
candidate arm. Mutations performed while evaluating a guard are observable
even when the guard evaluates to `false`. The implementation reconstructs and
writes the matched value back before testing a later arm. A later arm observes
the updated payload state.

Writeback also occurs before a guard propagates failure or raises a runtime
trap as part of cleanup. A guard failure remains primary if writeback cleanup
also fails. After a `true` guard, the selected arm retains the established
mutable binding and the ordinary normal, `return`, `break`, `continue`, and
`try` writeback paths apply to the arm body. Existing overlap, invalidation,
root-reassignment, and disjoint-sibling rules apply while the guard or arm is
active.

The implementation may use direct write-through when storage is stable, but
its observable order must equal the reconstruction rule. It may not discard a
mutation merely because the guarded arm body did not run.

### Class-pattern deferment

The current implementation does not accept class patterns such as
`case Point(x=0):`. Aura has no accepted positional-class-pattern contract,
per-class match metadata, or rule for whether a named component reads storage
directly or invokes a property-like operation. Adding call-shaped
destructuring would also need answers for private fields, inheritance, generic
classes, partial moves, mutable write-through, computed properties, and
exhaustiveness of an open class domain.

Classes can be matched by explicit enum/tag representations or by a wildcard
followed by ordinary code. A later class-pattern ADR must define the exposure
mechanism and capability behavior before syntax is accepted. Batch S1
formally defers class patterns from Aura 0.3 and does not reserve their
call-shaped syntax as an accepted future contract. Implementation requires a
separate accepted ADR that resolves the design questions above.

## Diagnostics

- `AU1101` reports malformed guard or or-pattern grammar, a missing
  alternative, or more than one `if` clause on an arm.
- `AU2002` reports a non-`bool` guard and alternative binding type mismatch.
- `AU2999` reports binding-set or capability mismatch, duplicate/subsumed
  alternatives, non-exhaustive coverage, unreachable arms, class patterns,
  and unsupported pattern positions. Binding mismatches include related spans
  and the complete expected/actual binding summary.
- `AU3001` reports an attempted move from an owned candidate binding before a
  guard commits the arm.
- `AU3002`/`AU3003` retain their ordinary overlap and mutation meanings for
  shared or mutable guard bindings.

Diagnostics produced by evaluating a guard retain their operation-specific
runtime code and point through the guard expression's normal source spans.

## Backend requirements

The checker lowers both backends from one decision-tree representation that
separates structural probe, candidate binding, guard evaluation, commit, and
owned extraction. MIR and direct execution must agree on alternative order,
guard count, selected arm, delayed extraction, guard effects, and all
writeback paths.

A direct backend optimization may merge pure discriminant tests, but cannot
reorder alternatives, hoist a guard, speculate guard execution, extract an
owned payload before commit, or delay mutable guard writeback past the next
arm probe.

## Limits

This decision does not add class, collection, mapping, range, rest, arbitrary
predicate, or user-overloadable patterns. Guards are ordinary synchronous
expressions under the operations otherwise legal in their enclosing function;
they add no await or suspension syntax. Pattern alternation does not define a
union type.

## Consequences

Common classification logic becomes concise without weakening ownership or
determinism. Guarded mutable matching deliberately exposes guard side effects,
including on a false result, which follows Aura's general rule that evaluating
an expression does not roll back its mutations.

Leaving class patterns outside the current implementation keeps field
visibility and capability design open until Aura has a deliberate
match-exposure protocol. This is an accepted deferment, not an authorization
to implement a particular class-pattern spelling or representation model.

## Completion test matrix

- parser tests for statement and expression arms, guard placement, multiline
  bodies, nested or-patterns, precedence, missing alternatives, duplicate
  guards, and separation from expression `|`
- static tests for exact-`bool` guards, binding scope/non-leakage, identical
  binding sets/types/capabilities, ordering-independent binding comparison,
  nested expected types, top-level guarded and unguarded catch-all bindings,
  duplicate/subsumed alternatives, reachability, and guarded-arm exhaustiveness
- runtime tests for left-to-right alternative probing, one guard evaluation,
  false continuation, true selection, guard traps/propagation, first-match
  behavior, and no evaluation of later guards
- ownership tests for shared bindings, Copy candidates, delayed non-Copy
  extraction, every prohibited candidate move, false-guard scrutinee
  preservation, and exactly-once extraction after commit
- mutable tests proving false-guard writeback, guard trap/propagation cleanup,
  selected-arm writeback on normal exit, `return`, `break`, `continue`, and
  `try`, later-arm observation after false, overlap rejection, invalidation,
  and disjoint-sibling mutation
- focused class-pattern rejection tests covering positional and named shapes
- byte-identical MIR/direct output and diagnostics, decision-tree/codegen
  tests, compiler analysis, completion, hover, definition, language-server,
  bundled-editor, maintained example, and executable Manual coverage

## Implementation and checkpoint status

Guards, or-patterns, and top-level catch-all binding patterns are accepted and
implemented as Aura 0.3's pattern-polish surface. Parser, checker, decision
tree, both backends, diagnostics, reference, examples, and tooling land
together. Class patterns remain unimplemented. The 2026-09-06 roadmap approval
and ADR-0063 schedule that direction; detailed match exposure, field visibility,
property evaluation, ownership, mutation, and exhaustiveness still need design.
