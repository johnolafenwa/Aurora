# ADR-0015: Explicit and default argument evaluation order

> Future integration (2026-09-06): [ADR-0058](0058-first-class-callables-and-binding-contracts.md)
> must preserve default/binding contracts through callbacks;
> [ADR-0059](0059-custom-initialization.md) must specify custom initializer
> defaults and evaluation order against this implemented baseline.

- Status: Accepted
- Date: 2026-07-14
- Reference gap: supplied/default ordering and result-capture timing

## Context

The ratified evaluation model orders supplied arguments from left to right and
evaluates an omitted default afresh, but it did not order those two groups
relative to one another. Side effects in named arguments and defaults therefore
made function calls and class construction observably underdetermined. Named
enum-variant arguments had a related gap: binding by payload name did not say
whether declaration-slot order could reorder evaluation at the call site.

Source-order evaluation also requires a capture point. If an implementation
deferred storing an earlier result until after a later expression ran, that
later expression could mutate the earlier expression's source place and change
the observed argument or field value despite the stated order.

## Decision

For function calls, method calls, class construction, and enum-variant
construction, Aurora evaluates all supplied argument expressions first in
call-site source order. Functions, methods, and classes then evaluate defaults
for omitted parameters or fields in declaration order. Each supplied expression
completes before the next begins. A copy or move result is captured into its
destination slot; a borrow-mode selection is established without cloning and
remains subject to the retained-borrow overlap rules in ADR-0016. Later side
effects therefore cannot cause an earlier captured argument or class-field
value to be re-read.

Binding arguments to declaration slots does not reorder their evaluation, and
no default is evaluated for a supplied slot. In particular, named enum-variant
arguments evaluate in their written source order and then bind by payload name
to the variant's declaration-order slots. Declaration-slot order affects the
constructed payload layout, not expression evaluation order.

This contained gap-fill follows P2 (one order on MIR and direct), P4 (evaluation
is explicit and contains no hidden replay), and P6 (one rule shared by calls and
construction). It standardizes the lowering order and adds no syntax.

## Completion tests

- `crates/aurora-compiler/tests/fixtures/run-pass/explicit_and_default_argument_order.au`
  pins reversed named supplied arguments followed by the one omitted default
  for both a function and a class constructor. It pins source-order evaluation
  with declaration-slot binding for reversed named enum payload arguments. It
  also pins that a supplied function argument and class-field value are
  captured before a later supplied expression mutates their source place. The
  forced MIR/direct parity matrix executes the fixture on both backends.
- `crates/aurora-compiler/tests/fixtures/run-pass/left_to_right_value_snapshotting.au`
  pins copy-value capture for binary operands and vector elements, immediate
  f-string rendering, compound-target capture, and receiver-before-argument
  evaluation.
