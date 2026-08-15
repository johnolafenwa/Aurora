# ADR-0045: Testing framework and assertion introspection

- Status: Accepted
- Date: 2026-08-02
- Accepted at: Batch S1 checkpoint, recorded 2026-08-15
- Version target: Aura 0.3
- Implementation: Compiler, runner, reference, LSP, and editor integration complete; full local and hosted checkpoint gates green
- Roadmap decision: Batch S1, S3

## Decision boundary

This ADR defines the complete Aura 0.3 testing surface. The compiler and runner
implementation is complete, the completion matrix is green, assertion
diagnostics have passed the forced MIR/direct parity matrix, and the Batch S1
checkpoint has ratified the registration API and external runner contract.
Skip markers, expected-failure markers, and property-based generation are
design topics for a later release and are not recognized syntax or runner
options in Aura 0.3.

## Goals

- Make ordinary comparison failures explain the values that disagreed.
- Preserve Aura's once-only, left-to-right expression semantics.
- Let developers select tests and consume stable machine-readable results.
- Provide deterministic per-test setup, teardown, and parametrized cases with
  no new declaration syntax.
- Keep introspected assertion diagnostics equivalent across the MIR and direct
  backends.

## Assertion introspection

The compiler introspects these ungrouped top-level assertion conditions:

```aura
assert left == right
assert left != right
assert left < right
assert left <= right
assert left > right
assert left >= right
assert item in collection
```

Each operand is evaluated exactly once, from left to right, before the
comparison. The comparison uses those captured values. A successful assertion
discards the captures. A failed assertion renders each capture with the same
`str()` contract used by ordinary Aura output.

Parentheses around the whole comparison do not disable introspection.
Comparison chains, `not in`, boolean combinations, calls returning `bool`, and
all other conditions retain the ordinary assertion diagnostic without operand
captures. This boundary avoids inventing a misleading two-operand account of a
condition with different evaluation semantics.

The optional assertion message remains lazy. It is evaluated exactly once only
after the condition fails. A trap in either condition operand or the message
remains primary. Active-resource cleanup follows ADR-0024.

Human diagnostics append these notes in operand order:

```text
left = 41
right = 42
```

Membership uses `item` and `collection` labels. Each rendered value is bounded
to 4,096 UTF-8 bytes. A longer rendering is cut at a valid UTF-8 boundary and
ends with `... (truncated)`. Truncation changes presentation only; it never
changes comparison semantics.

Introspection applies only when the effective comparison or membership
dispatch uses both operands non-consumingly. Builtin comparisons and
membership operations satisfy this rule, as does a custom operator whose
selected receiver and ordinary operand contracts are shared. When the
selected custom operator consumes either operand, the assertion evaluates the
condition normally and retains the ordinary assertion diagnostic without
operand captures. The compiler does not clone an operand, render it before the
operator call, or otherwise preserve a second observation of a value that the
operator consumes. Dispatch selection therefore determines whether an
otherwise supported top-level condition is introspected.

Structured diagnostics add an optional `assertion_operands` array. Each entry
contains `label`, `type`, `value`, and `truncated`. The field is absent for
diagnostics without captured operands. It is additive to diagnostic schema 1,
and native diagnostic-channel records enforce the existing total record-size
limit after bounded rendering.

## Discovery and selection

`aura test` discovers each parameterless module function whose name starts
with `test_`, in declaration order. A source file with no such function keeps
the existing single file-level case.

`-k <substring>` selects cases whose canonical reported name contains the
given substring. Matching is literal and case-sensitive. Selection occurs
after parametrized cases have been expanded, so a parameter label may match.
The option may appear once. A missing value, an empty substring, or a repeated
`-k` is command-usage error status 2. A valid filter matching no cases succeeds
with a zero-case summary.

Canonical names are:

- `path::test_name` for an ordinary discovered function;
- `path::test_name[label]` for a registered parameter;
- `path` for a file-level case.

Paths use the runner's normalized display path. Names do not depend on backend
or execution order.

## Setup and teardown

A test file may declare these ordinary parameterless functions:

```aura
def setup():
    pass

def teardown():
    pass
```

For every selected case, the lifecycle is:

1. run `setup`, when present;
2. run the case only if setup succeeded;
3. run `teardown`, when present, after every setup attempt and every case
   attempt, including a trap or non-zero test result.

Setup, the case, and teardown are isolated entry invocations of the same
checked source module. This boundary lets the runner execute teardown after a
case trap without exposing catchable language exceptions. Aura values and
module-runtime state do not flow between phases or cases; externally visible
effects such as files and processes follow their ordinary host lifetime.
Discovery and parameter registration do not run either hook.

The first lifecycle failure is primary. A teardown failure is reported as
secondary when setup or the case already failed; it becomes primary when the
earlier stages succeeded. The per-case timeout covers the complete lifecycle.
If a timed-out runner worker cannot be stopped, teardown cannot be promised;
the runner reports the timeout and applies the documented worker limitation.

A hook with parameters, a result type other than `None`, or a name collision
with a non-function declaration is a check-time error for `aura test`.

## Parametrized registration

Parametrized cases use existing tuples, lists, and function values:

```aura
def test_parser_cases() -> list[(str, def() -> None)]:
    return [
        ("empty", lambda: check_empty()),
        ("unicode", lambda: check_unicode())
    ]
```

A discovered `test_*` function is either:

- parameterless and returning `None`, which is one ordinary case; or
- parameterless and returning `list[(str, def() -> None)]`, which is a
  registration function expanded into one case per list entry.

Registration runs once per file before selection. Entries preserve list order.
Labels must be non-empty and unique within the registration function. The case
function must be capture-free, repeatable, parameterless, and return `None`.
This lets each case run as an isolated named entry with the same backend and
timeout contract as an ordinary discovered test. Registration failure is a
file-level discovery failure and no case from that registration runs. The
runner never executes a returned case during discovery.

Standard output produced by a successful registration is captured and
reported exactly once. Human mode emits it before case results. JSON mode
places it in an ordered top-level `discovery` record containing the
registration name, file, and captured `stdout`.

This API deliberately uses no decorator, attribute, reflection, or hidden
global registry. It composes with the existing function-value model and keeps
each reported case independently selectable.

## JSON results

`aura test --format json` writes exactly one JSON document to standard output
and no human progress lines. Its top-level form is:

```json
{
  "schema_version": 1,
  "summary": {"selected": 1, "passed": 1, "failed": 0},
  "tests": [
    {
      "name": "tests/math.au::test_addition",
      "file": "tests/math.au",
      "outcome": "passed",
      "duration_ms": 2
    }
  ]
}
```

`outcome` is `passed` or `failed`. A trapped failure carries `diagnostic` using
the existing structured diagnostic schema. A runner-originated failure such as
a timeout carries `reason`. Exactly one of those fields appears on a failed
record. A non-empty captured `stdout` field is included on the corresponding
test record; an empty field is omitted. Durations are non-negative integer
milliseconds measured across the complete lifecycle. Test records remain in
canonical discovery order even if execution later becomes concurrent. The
optional top-level `discovery` array uses the registration-output contract
above and preserves registration order.

Invalid command usage still reports to standard error and exits with status 2.
A completed JSON test run exits 0 when every selected case passes and 1 when
any case or discovery step fails.

## Backend and ownership contract

Assertion capture, hook calls, and registered function values obey ordinary
Aura ownership and capability rules. The testing surface inserts no clone,
move, truthiness conversion, or backend-specific representation.

`aura test` continues to execute through its current runner backend. This ADR
does not add an `aura test --backend` option and does not require the test
runner to execute each case on both backends. Assertion runtime diagnostics
are separately exercised through forced MIR and direct fixtures, which must
produce byte-identical human and structured failure data. Runner durations are
measured only by `aura test` and are not part of that backend-parity
comparison.

## Completion matrix

- [x] Parser, checker, MIR, and direct tests cover every introspected operator,
  grouped comparisons, the explicitly non-introspected forms, operand order,
  once-only evaluation, lazy messages, operand traps, Unicode truncation, and
  bounded structured records.
- [x] Operator-dispatch tests cover builtin comparisons and membership plus
  shared custom comparisons as introspected forms, and custom receiver-consuming,
  operand-consuming, and both-consuming operators as ordinary assertions with
  no captures. They prove that the non-introspected path performs no clone,
  pre-render, or second observation of either operand.
- [x] Human and JSON diagnostics pin labels, types, rendered values, truncation,
  source spans, cleanup precedence, and MIR/direct parity.
- [x] CLI tests cover `-k` matching, no-match success, every usage error, ordinary
  and parameterized names, and filtering after expansion.
- [x] JSON runner tests pin schema 1, ordering, summaries, durations, captured
  case and discovery output, diagnostic failures, runner failures,
  stdout/stderr separation, and exit status.
- [x] Hook tests cover all presence combinations, isolated phase state, external
  side-effect ordering, setup failure, case failure, teardown failure, dual
  failures, timeouts, and declaration errors.
- [x] Registration tests cover order, empty and duplicate labels, capture
  rejection, registration traps, invalid signatures, empty lists, and
  independent case reporting.
- [x] Manual, CLI reference, conformance map, examples, tutorials, LSP metadata,
  and editor packaging describe and validate the same surface.
- [x] Assertion diagnostic fixtures pass through the complete forced MIR/direct
  parity matrix. The `aura test` runner suite remains on its current backend,
  and the complete corpus passes the full local and hosted gates. The
  checkpoint-wide matrix is byte-identical across MIR/direct execution, and
  one complete hosted CI run passed on both supported hosted platforms.

## Ratification

The Batch S1 checkpoint accepts this decision as Aura 0.3's binding testing
and assertion-diagnostic contract. It explicitly confirms:

1. parameter registration remains `list[(str, def() -> None)]`, expanded once
   in registration order into independently reported cases;
2. `aura test -k` uses literal case-sensitive substring matching after
   parameter expansion, and selecting zero cases succeeds;
3. each rendered assertion operand has an independent 4,096-byte UTF-8 bound
   with explicit truncation state;
4. the first lifecycle failure remains primary, teardown failure is reported
   secondarily, and teardown runs after a case failure or trap;
5. setup, case, and teardown remain isolated lifecycle phases, and registered
   case function values must be capture-free; and
6. JSON result schema 1 is the stable external contract described above,
   including its ordered summary, test, discovery, output, diagnostic, and
   runner-failure fields and completed-run exit statuses.

The completed checkpoint evidence includes the full local repository gate,
the forced-backend parity matrix, and one complete green hosted CI run on
Ubuntu and macOS. No new testing syntax or runner behavior is introduced by
ratification.
