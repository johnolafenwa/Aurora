# ADR-0060: Typed context managers and cleanup

- Status: Accepted direction; detailed design pending
- Date: 2026-09-06
- Implementation: Not started
- Roadmap: Batch 3
- Related: ADR-0022, ADR-0038, ADR-0054, ADR-0058, and ADR-0059

## Authority and current boundary

The user approved richer context managers in the
[roadmap](../14-priority-roadmap.md). Existing `with` forms manage builtin
resources and eligible non-generic user classes through `close(mut self)`.
Generic entry/exit protocols and multiple-resource syntax are future work.

## Accepted decisions

Support generic managers, multiple resources in one `with`, and statically
typed entry/exit operations. Entry may fail. Register exit only after entry
succeeds; earlier successful entries must still be cleaned up if a later
entry fails. Resources acquired inside a failed entry remain subject to
ordinary ownership cleanup.

After successful entry, exit runs when the scope leaves through normal
completion, return, break/continue out of the scope, typed error propagation,
maintained runtime failure, or cancellation. Nested managed lifetimes unwind
in reverse entry order. Each successful registration exits exactly once.

Preserve an existing body failure as primary and attach cleanup failures as
additional information. A manager cannot silently suppress the body failure.
Cancellation must still perform the required cleanup. A manager may expose a
scoped resource or view when its lifetime is tied to the managed scope;
that resource cannot escape or be invalidated before exit.

The intended applications include files, locks, transactions, temporary
directories, and scoped configuration. This acceptance defines management
semantics, not a new shared-memory locking API or a transaction commit policy.

## Remaining detailed design

Specify protocol names/signatures, result/entry-view types, associated-type
needs, multi-resource grammar/evaluation, and interoperability with existing
close-only resources. Do not silently introduce Python exception suppression
through an exit return value.

Specify how a cleanup-only failure propagates, how typed and runtime failures
compose, and how callers observe additional cleanup failures. Define bounded
cleanup/cancellation interaction without choosing a new duration policy here.
Lock guards, commit/rollback, and restoration of scoped configuration need
their own concrete resource contracts under this protocol.

## Completion evidence required

Pin generic managers, multiple resources, failed first/later entry, partial
entry acquisition cleanup, every exit path, reverse order, exactly-once exit,
body/cleanup failure precedence, cancellation, and scoped-resource escape
rejection. Prove parity through both backends and update tooling, fixtures,
examples, and reference documentation with the implementation.
