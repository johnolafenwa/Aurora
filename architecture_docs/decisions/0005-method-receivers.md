# ADR-0005: Method receivers

> Approved next extensions (2026-09-06): [ADR-0058](0058-first-class-callables-and-binding-contracts.md)
> defines stored bound-method direction; [ADR-0059](0059-custom-initialization.md)
> records initializer-specific `self`. Ordinary implemented receiver semantics
> below remain unchanged.

- Status: Accepted
- Date: 2026-07-13
- Roadmap decision: D5

> **Amended by ADR-0022 (2026-07-27).** The receiver spellings changed: the
> shared receiver is bare `self` only (`borrow self` is retired), the mutable
> receiver is `mut self` (was `borrow mut self`), and the consuming receiver
> remains `own self`. The three receiver capabilities and their semantics are
> unchanged; only the surface syntax moved.

## Decision

Bare `self` is a shared borrow. `own self` is the consuming receiver and
`borrow mut self` remains the mutable receiver. `borrow self` remains accepted
as an explicit synonym for bare `self`. A first parameter spelled
`self: SomeType` is rejected with a diagnostic naming the receiver forms:

```text
`self: Type` is not a method receiver; use `self` or `borrow self` for shared access, `own self` to consume, or `borrow mut self` to mutate
```

The semantic model canonicalizes `self` and `borrow self` to the same shared
receiver kind. Tooling renders that kind as `self`, while retaining `own self`
and `borrow mut self` for the distinct consuming and mutable contracts.

## Rationale

Reading methods are overwhelmingly the common case in Python-shaped code. A
bare `self` that silently consumes a non-copy object makes familiar-looking
methods unexpectedly invalidate their receiver after one call. Making the
default shared preserves the object and makes the common spelling safe.

Consumption remains explicit rather than disappearing: `own self` documents
the transfer at the declaration and call boundary. Rejecting `self: Type`
prevents a method-looking declaration from silently becoming an associated
method with an ordinary parameter.

## Consequences

- Existing consuming methods migrate from `self` to `own self`.
- Existing `borrow self` methods remain source-compatible.
- Bare and explicit shared receivers satisfy the same trait contract.
- An owned receiver can move non-copy fields; a shared receiver cannot.
- `own` becomes a reserved lexer keyword.

## Completion tests

- Parser and semantic unit tests for all receiver forms.
- Check-pass/check-fail fixtures pinning move behavior and diagnostic text.
- MIR/direct method-call parity fixtures.
- LSP hover/completion/diagnostic tests plus class examples and tutorials.
