# ADR-0037: Expression closures and value capture

- Status: Accepted
- Date: 2026-07-30
- Accepted at: Batch 6 opening checkpoint
- Roadmap decision: Batch 5, Phase 6.3
- Amends: ADR-0013
- Implemented design amendment: ADR-0038 (explicit loan capture lists in Aura 0.3)

## Context

ADR-0013 ordered capture-free function values before closures and deferred
borrowed captures until Aurora has a first-class loan design. Phase 6.1 now
provides structural `def(...) -> R` function types and indirect calls. Phase
6.3 needs a compact callable expression without weakening the language's
ownership or task-isolation rules.

The critical choice is when capture happens. An implicit reference capture
would let a closure outlive a local capability, make mutation aliasing hidden,
and allow task transfer to launder shared state. Aurora instead needs a closure
value whose complete owned state is established when the expression is
evaluated.

## Decision

### Syntax and contextual typing

A closure is an expression with Python-shaped, expression-only syntax:

```aurora
lambda value: value + 1
lambda left, right: left + right
```

Parameters have no inline type annotations or defaults. A lambda with
parameters receives their types from one expected structural function type.
Bare, `own`, and `mut` parameter modes are written before the parameter name
and must match the corresponding contextual mode:

```aurora
lambda value: value.len()
lambda own value: value
lambda mut values: values.push(1)
```

The expected return type constrains the body when present. A zero-parameter
`lambda: expression` may infer `def() -> R` from its body without an expected
callable type; any lambda with parameters requires complete expected parameter
types. The body is one expression. Statements, suites, local declarations,
defaults, generics, and multi-statement bodies remain named-function
territory. A capture-free lambda lowers to the existing function-value
representation and is Copy and Transfer.

### Capture set and creation

Every resolved outer local or owned parameter read by the body is captured by
value. Module items, builtin names, and the lambda's own parameters are not
captures.

Capture happens when the lambda expression is evaluated:

- a Copy value is copied into closure storage
- a non-Copy owned value is moved into closure storage
- a later use of a moved source is rejected with the `AU3001` moved-value
  diagnostic and identifies closure creation as the move origin

Use `.clone()` before creating the closure when both the outer scope and the
closure need an independently owned non-Copy value.

A bare shared parameter is a capability, not an owned value, so capturing it
is rejected. The diagnostic recommends taking owned input or cloning to an
owned local before closure creation. A `mut` parameter is likewise a
caller-owned capability and cannot be captured. In-loan capture remains
implemented by ADR-0038's explicit exhaustive capture lists in Aura 0.3.

### Callability

A closure whose body only reads its captures is repeatable. Calling it borrows
its capture storage for that invocation, so a non-Copy capture can be read on
multiple calls.

A closure whose body consumes any non-Copy capture is consuming and
single-use. Its first call moves the closure value under the existing move
checker; another call or use reports `AU3001`. Copy captures do not make a
closure consuming.

Mutation of captured state is outside Phase 6.3. Assignment is already
unavailable in an expression body; passing a capture as `mut`, invoking a
`mut self` method on it, or otherwise requesting mutable capture access is
rejected. A lambda's own `mut` parameter is distinct: it writes through the
caller's explicitly mutable argument and captures no mutable outer state.

Capturing closures are non-Copy even when every capture is Copy. This avoids
silently duplicating an environment and keeps one stable callable-ownership
model. Ordinary structural function types remain the source spelling for the
call signature; callable compatibility includes every parameter mode, type,
and result type.

An arbitrary written `def(...) -> R` parameter or stored field, collection
element, or annotated return describes a capture-free code pointer and does
not retain closure environment or call-kind metadata. Capture-free lambdas may
cross those boundaries. Capturing closures are limited to immutable inferred
or contextually typed locals, direct calls, compiler-known repeatable callback
sites, and qualifying task targets. The Vec algorithms and `control.retry`
preserve repeatable closure metadata and reject consuming closures; task start
moves a qualifying closure for one invocation.

Conditional and `match` expressions cannot produce a capturing closure value
from multiple branches in Phase 6.3. Each branch can have a different capture
set, environment ownership state, and call kind, and the language has no
closure-union type that can preserve that distinction. The compiler reports
`AU2002` with guidance to call the closure inside each branch or use
capture-free lambdas or named functions. Branch-local closure creation and
invocation remain supported and use the ordinary control-flow move merge.

### Transfer

A closure is compiler-derived `Transfer` exactly when every captured value is
`Transfer`. A capture-free lambda is therefore Transfer. At a task boundary,
the closure value itself is copied or moved into task-owned storage; the task
then calls it under its ordinary callable contract.

Structural derivation does not hide a non-Transfer leaf. A closure capturing
`random.Rng`, `TaskGroup`, or live host authority is rejected at the task
boundary with the existing `AU3008` explanation. A shared or mutable
capability cannot become Transfer through closure capture because such
capture is rejected at creation.

## Consequences

Closure creation has visible ownership effects at one source location.
Read-only callbacks can retain and repeatedly inspect owned data, while a
consuming callback has one statically enforced invocation right. Task
isolation composes directly with ADR-0033 rather than relying on a special
closure escape hatch.

The phase deliberately does not provide mutable environments, reference
capture, in-loan capture, capture lists, closure trait objects, callbacks
across FFI, async closure syntax, comprehensions, or statement bodies.
Phase 7 later adds eager comprehensions under ADR-0039. Lambdas evaluated
inside them retain this ADR's capture and storage rules unchanged.

## Completion tests

- lexer/parser fixtures for zero, one, and multiple parameters; bare, `own`,
  and `mut` contextual modes; precedence; and statement-body rejection
- semantic fixtures for contextual parameter/result typing, zero-parameter
  result inference, and missing parameter context
- Copy-snapshot, non-Copy move-at-creation, explicit-clone, repeatable-read,
  consuming-single-use, and mutable-capture rejection fixtures
- conditional/`match` capturing-closure merge rejection and branch-local
  creation/move fixtures
- structural Transfer acceptance/rejection through task boundaries
- MIR and direct-native execution parity, including cleanup of never-called
  and consumed closure environments
- compiler analysis, completion, hover, definition, lambda scope, language
  server, and editor grammar regressions
- maintained examples and source-hash-pinned Manual execution

## Ratification

Batch 6 accepts this decision. The Batch 5 implementation completed the
semantic, ownership, Transfer, backend-parity, editor, reference, tutorial,
and example matrix above. Expression closures and value capture are therefore
part of the binding Aurora 0.2 language design.
