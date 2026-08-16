# Closures

Aura closures use `lambda parameters: expression`. They are small
expression-bodied callable values. Parameter types come from context; a
zero-parameter lambda may infer its result type from its body. A lambda without
a capture list captures by value. An explicit exhaustive capture list can
instead request shared, mutable, or owned access to named outer locals.

```aura
def main():
    factor: int32 = 2
    scale: def(int32) -> int32 = lambda value: value * factor

    name = "Aura"
    length: def() -> int64 = lambda: name.len()

    token = "owned"
    take: def() -> str = lambda: token

    print(scale(21))
    print(scale(6))
    print(length())
    print(length())
    print(take())
```

This prints `42`, `12`, `6`, `6`, and `owned` on separate lines. `factor` is
copied into `scale`; `name` moves into a read-only, repeatable closure; and
`token` moves into a consuming closure that is called once.

## Grammar

The closure productions are:

```ebnf
lambda-expression
    = "lambda", [ lambda-capture-list ],
      [ lambda-parameter,
      { ",", lambda-parameter } ], ":", expression ;

lambda-capture-list
    = "[", lambda-capture,
      { ",", lambda-capture }, "]" ;

lambda-capture
    = [ "mut" | "own" ], identifier ;

lambda-parameter
    = [ "mut" | "own" ], identifier ;
```

A lambda is the lowest-precedence expression form. The body is exactly one
expression; the colon does not introduce an indented suite. Parameter lists do not accept
types, defaults, or a trailing comma. Zero parameters use `lambda:
expression`.

`lambda` is a contextual expression introducer: the lexer still produces an
identifier token, but the spelling always begins a lambda at the start of an
expression. Member and named-argument positions may use the same identifier
spelling. A lambda may appear anywhere an expression is accepted, subject to
the contextual typing rule below. There is no arrow spelling, statement body,
`async` form, or nested `def`.

## Typing Rules

A lambda with parameters requires a complete expected parameter contract from
a structural function type such as `def(T1, mut T2, own T3) -> R`. The
expected type fixes the parameter count and each parameter's capability and
type. An expected result type also constrains the body. A zero-parameter
`lambda: expression` may instead infer `def() -> R` from the body when no
expected callable type is present.

```aura
shared: def(str) -> int64 = lambda text: text.len()
owned: def(own str) -> str = lambda own text: text
push_one: def(mut list[int32]) -> None = lambda mut values: values.append(1)
```

A bare lambda parameter matches a bare shared parameter, `own name` matches an
owned parameter, and `mut name` matches a mutable parameter. Modes cannot be
silently changed. The body must have exactly the expected result type.
Parameters are in scope only in the body and follow the ordinary no-shadowing
rules.

The compiler does not guess parameter types from body operations. Generic
lambdas and lambda parameter type annotations are unavailable. A capture-free
lambda uses the ordinary function-value representation and is Copy and
Transfer. It may appear anywhere an ordinary function value can appear,
including arguments, fields, collections, and returns.

A capturing closure retains semantic environment and call-kind metadata that
an arbitrary written `def(...) -> R` storage type does not describe. It may be
held in an immutable inferred or contextually typed local, called directly,
passed directly to compiler-known repeatable callback sites such as the list
algorithms and `control.retry`, or moved into a qualifying task start. It
cannot be coerced through an arbitrary written `def` parameter, stored in a
`def` field or collection element, or returned through an annotated `def`
result. Those metadata-erasing boundaries report `AU2002`.

A conditional or `match` expression also cannot merge capturing closure
values from different branches. The branches may have different capture
sets, ownership states, and call kinds, and Phase 6.3 has no closure-union
type that preserves those differences. Call the closure inside each branch,
or return capture-free lambdas or named functions with one structural
`def(...) -> R` type. Creating and calling a closure wholly inside a branch
remains supported.

A resolved name in a lambda without a list is a capture only when it denotes
an outer owned local or an `own` parameter. Lambda parameters, module
functions, types, builtins, and imported items are resolved normally and are
not stored in the environment.

An explicit list is exhaustive. Every resolved outer local used by the body
must appear exactly once, and every entry must be used. Entries are acquired
left to right:

| Entry | Contract |
| --- | --- |
| `value` | Shared live loan of `value`. |
| `mut value` | Exclusive mutable live loan; `value` must be a mutable place or mutable view. |
| `own value` | By-value Copy snapshot or move under ADR-0037. |

A projected place must first be named by a `view` binding. A shared or mutable
view can be reborrowed but cannot be captured with `own`. A bare capture of a
Copy local is intentionally live; write `own value` for a snapshot.

## Runtime Semantics

Evaluating a lambda constructs its callable value immediately. For an ordinary
lambda, each captured Copy value is snapshotted and each captured non-Copy
owned value moves. An explicit list instead acquires its declared loans or
owned captures left to right. Loan releases belong to the closure environment
and run exactly once when its final use or scope ends.

Calling the closure evaluates arguments under its contextual structural
function signature and then evaluates the body. A closure whose body only
reads captures borrows its environment for the call and can be invoked
repeatedly. A body that consumes a non-Copy capture consumes the closure on
its first call. The existing move checker rejects another call or use.

Capture-free lambdas dispatch as ordinary function values. Capturing closures
carry an owned environment and are non-Copy, including when their captures
are individually Copy. They are also not clone-safe: a clone-producing
generic specialization that would duplicate the environment reports
`AU3007`. Use a named function or capture-free lambda when a callable must be
copied or cloned.

## Ownership And Evaluation Order

Implicit capture is by value and happens at closure creation, not on the first call.
Copy captures leave their sources usable. Non-Copy captures move, so using the
outer source afterward reports `AU3001`. Clone before creation when both
owners are required:

```aura
def main():
    name = "Aura"
    kept = name.clone()
    length: def() -> int64 = lambda: kept.len()
    print(name)
    print(length())
```

A bare or mutable enclosing parameter may be named in an explicit capture
list, which creates a loan bounded by the closure's live region. Without a
list, those capabilities are not captured; take the input as `own` or clone it
into an owned local for by-value capture.

An inner lambda without a list cannot capture a bare parameter of its enclosing
lambda. An explicit bare entry creates a shared contained reborrow; a `mut`
entry requires a mutable outer capability. When an independent snapshot is
needed, make the outer parameter `own` and use an `own` capture, or pass the
value to a named helper that creates the owned closure.

An ordinary by-value closure environment is read-only. A body may mutate only
a `mut` entry from an explicit capture list. Such a closure is
mutable-repeatable: it must be stored in a `mut` local and called sequentially
through that mutable place. A shared-loan closure is shared-repeatable. A body
that consumes a non-Copy `own` capture remains a consuming, single-use closure,
even when the environment also contains loans.

A by-value closure is Transfer exactly when all of its captures are Transfer. Moving a
qualifying closure into `TaskGroup.start`, `start_soon`, or an explicit-stack
variant transfers the complete environment to child-owned storage. A
non-Transfer leaf retains the ordinary `AU3008` boundary explanation. A
closure containing any shared or mutable loan is always non-Transfer and
cannot cross a task, Queue, supervisor, detached-work, or FFI boundary.

### Comprehension Interaction

Comprehensions do not change closure capture. A lambda enclosing a
comprehension captures outer names used by iterable, filter, and output
expressions, while comprehension targets are local bindings in the lambda body
and are not captures.

A lambda expression reached inside a comprehension is created at that runtime
position. It may snapshot a Copy target, and it may move a Queue-received owned
target when the surrounding use permits one consuming closure. A shared
non-Copy list/set target may be captured only through an explicit loan list
whose lifetime remains inside the synchronous containing task; otherwise pass
an explicit clone to a named helper or arrange another owned value outside the
comprehension.

The ordinary storage boundary also remains. A capturing closure cannot itself
be inserted as a list, set, or dictionary comprehension result because collection
storage erases its environment/call-kind metadata. It may be used immediately
at a compiler-known callback or direct-call site inside an iterable, filter,
key, value, or element expression. Such creation happens only when preceding
clauses and filters reach it, and repeatable callback sites still reject a
consuming closure.

## Diagnostics

`AU1101` reports malformed lambda parameter or body syntax. `AU2002` reports a
missing or mismatched parameter context, parameter capability, result type,
metadata-erasing storage boundary, or a consuming closure supplied where a
repeatable callback is required. `AU3001` reports use after a non-Copy value
moved into a closure and use after a consuming closure call. `AU3002` rejects
overlapping capture loans and source accesses. `AU3003` rejects capability
escalation or a mutable-repeatable call through an immutable closure place.
`AU3008` reports a closure whose
captured environment cannot cross a task boundary because some captured value
is not Transfer. `AU3010` reports a loan closure escaping into storage or a
metadata-erasing callable boundary.

The shared-capability diagnostic recommends cloning to an owned local or
taking owned input. Move diagnostics identify closure creation or the
consuming call as the ownership origin.

## Backend Support

Contextual checking, capture and loan analysis, move checking, MIR lowering,
and direct native lowering implement the same closure contract. Both maintained
backends copy, move, or loan captures at creation; preserve shared- and
mutable-repeatable calls; enforce single-use consumption; write mutable loans
through immediately; and clean up an environment exactly once. Compiler
analysis and the language server expose lambda parameter scope,
captured-name definitions, callable hover, completions, and the compiler-owned
diagnostics.

## Limits And Implementation-Defined Behavior

Closures are expression-only and contextually typed. They do not support
statement bodies, inline parameter types, defaults, generics, implicit
reference capture, method values, trait objects, FFI callbacks, asynchronous
syntax, returned loan closures, or lifetime-bearing structural callable types.
Explicit lists accept local identifiers; project a field into a named view
before capturing it.

Arbitrary structural `def` parameters and stored `def` fields, collection
elements, and annotated returns currently carry only capture-free code
pointers. Compiler-known callback sites preserve repeatable closure metadata;
`control.retry` and the list callbacks reject consuming closures. Task start
accepts a qualifying closure by move for one invocation.

Conditional and `match` expressions cannot merge capturing closures from
multiple branches. This is an explicit closure-union boundary, not an
implementation-defined coercion.

The capture, callability, ownership, and Transfer rules are language-defined;
the implementation does not choose a reference-versus-value capture mode.

## Status

Expression closures and by-value capture are implemented under Accepted ADR-0037
(`architecture_docs/decisions/0037-expression-closures-and-value-capture.md`)
after ratification at the Batch 6 opening checkpoint. Capture-free function
values remain governed by [Functions](/manual/functions), and task-boundary
Transfer remains governed by Accepted ADR-0033. Comprehensions preserve this
contract under Accepted ADR-0039. Explicit shared/mutable/owned capture lists,
mutable-repeatable closure calls, and loan cleanup are implemented under
ADR-0038.
