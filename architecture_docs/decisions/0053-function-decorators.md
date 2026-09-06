# ADR-0053: Function decorators

- Status: Accepted direction; detailed design pending
- Ratified direction: 2026-09-06, user approval of the priority roadmap
- Date: 2026-08-02
- Version target: Aura 0.4
- Implementation: Not started
- Roadmap decision: Batch S1, design-only checkpoint
- Related: ADR-0013, ADR-0015, ADR-0016, ADR-0022, ADR-0033, ADR-0037,
  ADR-0038, and ADR-0050

## Decision boundary

The user approved free-function decorators, repeatable wrappers, complete
callable-contract preservation, and explicit retry ownership. Implementation
has not started. ADR-0058 supplies the callable foundation deferred by ADR-0051.
The ratification section identifies remaining details in the design baseline.
See the [approved roadmap](../14-priority-roadmap.md).

## Context

Agent and service programs frequently register tools, routes, retry policies,
metrics, authorization, or tracing around functions. Aura already has typed
function values and repeatable closures, so the smallest coherent decorator
feature is syntax for applying ordinary callable transformations to a
definition. The transformation must preserve the complete function signature
and must not create hidden mutation, untracked ownership, or order-dependent
module behavior.

## Goals

- express `@tool`, `@route(...)`, `@retry(...)`, and similar wrappers compactly
- define decorators as ordinary typed function-value transformations
- preserve every parameter capability, parameter type, and result type exactly
- evaluate decorator expressions and module initialization deterministically
- make recursion and cross-module interface behavior unambiguous
- support repeatable read-only capturing wrappers

## Non-goals

- class, enum, field, parameter, local-binding, or arbitrary-expression
  decorators
- generic function definitions or generic decorator transformations
- ordinary decorated methods in the first implementation
- consuming or single-use decorator callables or results
- mutation of closure environments
- macro expansion, syntax rewriting, AST access, or compiler plug-ins
- runtime annotation reflection
- decorator-controlled overloads or signature changes
- using ordinary decorator rebinding to implement properties

## Source form

One or more decorator expressions may precede a module-level, non-generic
function definition:

```aura
@tool
@retry(attempts = 3)
def fetch_status(url: str) -> Result[int64, net.Error]:
    ...
```

The `@` begins at the definition's indentation and is followed by one ordinary
expression on that logical line. The expression grammar permits names, member
access, calls, explicit specialization, and grouped expressions. Assignment,
lambdas with missing context, control-flow expressions whose branches have
incompatible closure metadata, and statement forms remain invalid there.

Decorators attach only to the immediately following definition. A blank line,
another statement, a malformed expression, or end of file before the
definition is a syntax error. Comments may appear between decorator lines and
before the definition without detaching the group.

The decorated function's source name is declared exactly once. The
undecorated intermediate function has no source-visible binding.

## Desugaring and evaluation order

**Proposed baseline, not ratified; resolved in Batch 6.** The specific
decorator-expression evaluation and application order below remains open.

For:

```aura
@outer(make_config())
@middle
@inner(flag = enabled)
def run(value: int64) -> int64:
    ...
```

module initialization performs this sequence:

1. evaluate `outer(make_config())`
2. evaluate `middle`
3. evaluate `inner(flag = enabled)`
4. create the undecorated function value
5. apply the third result to that function
6. apply the second result to step 5
7. apply the first result to step 6
8. initialize the module binding `run` with the final result

Decorator expressions therefore evaluate once, top to bottom, while their
results apply bottom to top. Every subexpression follows ordinary left-to-right
evaluation. A trap stops initialization at that point; no later decorator or
definition initializer executes, and already-created temporaries are cleaned
up exactly once.

Evaluation occurs once per successful module initialization under the module
constant initialization order. Importers observe the initialized final
binding and never repeat decorator evaluation. Cyclic initialization is
diagnosed under the module-constant cycle rules.

The source definition's body resolves recursive references to the final
module binding. A recursive call therefore invokes the completely decorated
function. The compiler must use a checked initialization slot for this
resolution; it may not capture the undecorated intermediate value. Calling the
slot before initialization completes is a module-initialization cycle error.

**Proposed baseline, not ratified; resolved in Batch 6.** The preceding
recursive-reference and checked-initialization-slot rules remain open.

## Exact typing contract

Let `F` be the complete resolved structural type of the source function,
including:

- parameter count and order
- each bare, `mut`, or `own` parameter capability
- each parameter type and default-presence contract
- keyword-only positions and accepted argument names
- result type
- error/result structure and returned-view contract, if that combination is
  separately authorized

`F` is a design-level contract here. The current written `def(...)` type does
not yet encode every item in this list. ADR-0058 must define a representation
that preserves these properties across stored and passed callable values before
decorators rely on them. A wrapper cannot erase a keyword-only restriction or
lose defaults through a narrower capture-free function-pointer representation.

Every decorator expression must resolve to an exact transformation
`def(F) -> F`. This notation is a compiler rule over one concrete `F`; it does
not introduce higher-kinded types or source-level signature variables.
Application is type checked after each layer, so no decorator may add, remove,
reorder, rename, widen, narrow, or change the capability of a parameter or
change the result type.

For example, a wrapper for `def(str, own Request) -> Result[Response, Error]`
must accept that exact function type and return the same exact function type.
A transformation returning `def(str, Request) -> Result[Response, Error]` is
rejected because it loses the ownership-transfer contract.

The source definition and every decorator are fully concrete in the first
implementation. A generic decorated definition, a generic decorator awaiting
inference, or one decorator implicitly instantiated differently for multiple
uses is rejected. Explicitly specialized ordinary functions may participate
when the resulting transformation is concrete.

## Callable and ownership contract

Decorator application is a compiler-recognized callable boundary. The input
function value is supplied once to each transformation. A named capture-free
function value can be copied. A non-Copy intermediate is moved into the next
application, leaving no second source-visible owner.

Every decorator callable must be repeatable, and every returned function must
also be repeatable. A decorator may return a capturing closure when:

- the closure reads but does not consume or mutate its captures
- its exact callable signature is `F`
- every captured value follows the ordinary value-capture rules
- cleanup of its owned environment is defined for module shutdown and failed
  initialization

The decorated binding uses ADR-0058's general callable model to retain its
closure environment and repeatability metadata through storage, arguments, and
returns. It is not erased to a capture-free pointer. A
single-use result, a closure that consumes a non-Copy capture, or a mutable
capture is rejected at the decorator site.

The final decorated value is Copy only if its actual representation is Copy.
It is `Transfer` exactly when its complete environment is `Transfer` under the
structural rule. Using the binding as a task target rechecks that property.
Decoration cannot turn a shared or mutable capability into owned storage and
cannot hide a non-Transfer capture.

A decorator expression may read module constants already initialized before
the decorated definition. It may not capture a local capability because the
syntax is module-level. Calls performed by decorator evaluation use their
declared argument capabilities normally.

## Methods and properties

Ordinary decorated instance, static, associated, and trait methods are outside
the first implementation. Their binding creates receiver descriptors whose
type is not the same as a free structural function transformation.

`@property` is a separate compiler-recognized descriptor specified by
ADR-0055. It uses the decorator-shaped parser surface but does not evaluate a
name called `property`, apply `def(F) -> F`, or rebind the method to an ordinary
function value. Its parser support may land before ordinary decorator
execution. No other compiler-special decorator exists in this design.

## Agent-facing patterns

Libraries can define concrete transformations for focused APIs:

```aura
@tool
def search(query: str) -> Result[SearchResult, ToolError]:
    ...

@route("/health")
def health(request: Request) -> Response:
    ...

@retry(attempts = 3)
def call_model(request: ModelRequest) -> Result[ModelReply, ModelError]:
    ...
```

The language assigns no registration meaning to these names. A library may
register metadata when its decorator expression is evaluated or may return a
wrapper. Such effects happen under the module initialization order and the
exact signature rule. A registration API must define duplicate registration,
failure, task safety, and global-state policy independently.

### Retry ownership

Repeatability of a wrapper does not make an owned argument reusable within one
invocation. After the wrapped call consumes `own Request`, a retry must obtain
a fresh request through an explicit clone or reconstruction policy. Alternatively,
the operation may take shared input when that matches its semantics. The
compiler and library cannot silently duplicate a resource or reuse a moved value.
Retry-policy API syntax and cloning/reconstruction capabilities are detailed
library design work; a retry count alone is insufficient for consumed input.

## Backend and interface contract

Checked module interfaces record the final binding's exact callable signature,
repeatability, Copy status, Transfer derivation, and environment-bearing
representation. They do not export the undecorated intermediate function or
the list of decorator expressions as callable symbols.

MIR and direct lowering must agree on top-down evaluation, bottom-up
application, recursion through the final slot, trap cleanup, and one-time
module initialization. Native caches include decorated-binding metadata and
the initialization graph. The C FFI may export a decorated function only when
the final value is a capture-free function with an otherwise valid FFI
signature; an environment-bearing result is rejected.

## Diagnostics

Focused diagnostics must identify:

- a decorator group not followed by an eligible function definition
- a decorated generic function, method, or other ineligible declaration
- the exact decorator layer whose resolved type is not `def(F) -> F`
- the first parameter mode, type, name, keyword-only contract, default
  contract, or result mismatch between expected and returned `F`
- an unresolved or generic decorator transformation
- a consuming decorator callable or consuming returned closure
- a forbidden mutable or capability capture in the returned wrapper
- a non-Transfer decorated task target, with the complete capture path
- recursion or observation through an uninitialized final module slot
- an environment-bearing final value at an FFI export

The primary span is the failing `@` expression. Related spans point to the
function signature and, for a returned-wrapper mismatch, the decorator's
return declaration. Diagnostics do not execute or discard a decorator to
recover a different binding.

## Consequences

Decorators remain ordinary, statically typed function composition. Libraries
can build agent-tool and service registration surfaces without a macro system,
and readers can recover exact execution order from source.

The concrete-signature and repeatability restrictions deliberately limit
abstraction in the first implementation. They avoid higher-kinded typing,
receiver descriptor transformations, and callable-state ambiguity while the
core feature establishes one stable model.

## Implementation adoption

Function decorators are an additive source feature. Existing Aura programs
need no rewrite. The parser, formatter, semantic model, module initializer,
MIR, direct backend, language server, reference, examples, and tutorials adopt
the `@expression` surface in one coordinated family.

Adoption depends on ADR-0058's implemented general callable model, exact
callable signatures, repeatability and Transfer analysis, deterministic module
initialization, and checked cross-module bindings. It also supplies the syntax
dependency used by ADR-0055 properties and the metadata association required
by ADR-0056 docstrings.

Decorator groups and final decorated bindings become explicit checked-interface
records. The AST/semantic schema, interface format, module-initialization graph,
native cache identity, language-server index, and generated-reference identity
are bumped when implementation lands. Cached analysis, interfaces, and native
artifacts are invalidated and rebuilt under the new versions.

## Completion-test matrix

- lexer and parser: one and multiple decorators, comments, multiline grouped
  expressions, malformed `@` lines, detachment, and eligible declarations
- evaluation: expression evaluation top to bottom, application bottom to top,
  once-only subexpressions, traps at every layer, and exact cleanup
  (**Proposed baseline, not ratified; resolved in Batch 6.** Specific order.)
- module behavior: single initialization across multiple imports, declaration
  ordering, cycle rejection, failed initialization, and cache invalidation
- typing: exact `F`, every parameter capability, defaults, keyword-only names,
  result types, explicit specializations, and mismatch diagnostics per layer
- exclusions: generic definitions, unresolved generic transformations,
  methods, non-function declarations, consuming callables, and consuming
  results
- closures: repeatable capture-free results, repeatable capturing results,
  Copy and non-Copy environments, mutable-capture rejection, and destruction
- retry: shared-input repetition, explicitly cloned/reconstructed consumed
  input, moved-input rejection, and cleanup on failed request reconstruction
- storage: decorated callbacks retain defaults, argument names, keyword-only
  restrictions, environments, capabilities, and lifetime/Transfer checks
- recursion: direct, mutual, and decorator-wrapper recursion all resolve the
  final initialized bindings; early observation is rejected
  (**Proposed baseline, not ratified; resolved in Batch 6.** Recursive initialization.)
- ownership and Transfer: copied named functions, moved intermediates,
  non-Transfer capture paths, task targets, and no hidden capability capture
- interfaces: imports expose only the final exact signature and preserve
  environment metadata; incremental/native cache keys change correctly
- FFI: capture-free qualifying export and environment-bearing rejection
- tooling: formatting, completion after `@`, hover/definition through the
  decorator expression and final binding, semantic tokens, rename, docs, and
  reference examples for `@tool`, `@route`, and `@retry`
- parity: byte-identical MIR/direct output, diagnostics, initialization, trap
  cleanup, recursion, and forced-backend results

## Ratification and remaining design

Accepted on 2026-09-06: preserve the complete callable contract; begin with free
functions and repeatable wrappers; defer ordinary method decoration until
receiver binding is settled; retain source documentation after decoration;
and require explicit ownership policies for consumed retry input.

Remaining details include decorator expression/application order, recursive
binding initialization, generic specialization limits, decorator provenance,
and the retry library API. The original order and recursion rules above remain
the proposed baseline. Callable representation and binding metadata are owned
by ADR-0058; registration/schema behavior belongs to ADR-0062.
