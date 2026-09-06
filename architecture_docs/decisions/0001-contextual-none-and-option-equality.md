# ADR-0001: Contextual `None` and `Option` equality

> Future design amendment (2026-09-06): [ADR-0052](0052-anonymous-closed-union-types.md)
> approves replacing `Option[T]` with `T | None`, without compatibility support.
> The contract below remains the implemented baseline until that migration;
> APIs distinguishing present-`None` from absence need distinct tagged cases.

- Status: Accepted
- Date: 2026-07-13
- Roadmap decision: D1

## Decision

Bare `None` coerces to `Option.None` whenever the surrounding position expects
`Option[T]`. This applies symmetrically to equality and inequality, grouped
expressions, annotated bindings, returns, and arguments. Unit `None == None`
is true. Unconstrained qualified `Option.None` remains an inference error.
Aurora rejects `is` and `is None` with a fix suggesting `== None` or `match`.

## Completion tests

- `crates/aurora-compiler/tests/fixtures/check-pass/` and `check-fail/` for all contextual and diagnostic forms.
- `crates/aurora-compiler/tests/fixtures/run-pass/` for observable equality results.
- `crates/aurora-compiler/src/sema_tests.rs` and `mir_tests.rs` for contextual typing and lowering.
- `crates/aurora-compiler/src/native_codegen_tests.rs` for unit equality.
- Parser unit tests and `parse-fail/` fixtures for the rejected `is` spelling.
- `crates/aura/tests/backend_parity.rs` for forced MIR/direct parity.
