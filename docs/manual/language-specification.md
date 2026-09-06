# Language Specification

This Manual is the normative specification of the implemented Aura 0.3
development language. It defines the source language, static rules, ownership
model, execution behavior, maintained runtime APIs, package model, and tool
contracts that a conforming implementation must provide.

The specification describes exactly the language implemented in this repository.

## Scope

The specification covers:

- UTF-8 `.au` source text, tokens, indentation, and the complete accepted grammar
- declarations, statements, expressions, patterns, names, scopes, and visibility
- types, inference, generics, traits, calls, and operator resolution
- moves, copies, borrows, mutable places, resources, and owned returns
- module loading, packages, entry modules, top-level execution, and `main`
- evaluation order, control flow, runtime failures, cleanup, tasks, cancellation, and backend equivalence
- maintained builtin functions, enums, modules, resources, and CLI/editor contracts
- implementation limits that are observable by valid or invalid Aura programs

The specification does not define the compiler's private Rust data structures, MIR encoding, native ABI, object-file layout, or internal optimization choices except where they affect an observable language or tool contract.

## Normative Language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, and **MAY** are normative:

- **MUST** and **MUST NOT** state requirements for conforming implementations or programs.
- **SHOULD** and **SHOULD NOT** state strong recommendations; a deviation needs a documented reason and must not contradict a MUST-level rule.
- **MAY** marks permitted behavior or an optional implementation technique.

Ordinary present-tense statements are normative when they describe accepted syntax, static behavior, evaluation, runtime results, or public APIs. Examples are illustrative unless a surrounding paragraph says that their exact output or diagnostic is part of the contract.

## Specification Version

This reference describes Aura 0.3 as implemented by the repository containing
it. Aura is an advanced technical preview, so source and API contracts may
change before a tagged stable release. Any behavior change MUST update the
relevant reference page, conformance tests, examples, tutorials, and work
record in the same pass.

The repository commit identifies the precise revision of the specification.
The rendered Manual is stamped with source version 0.3.3 (technical preview)
and its implementation baseline commit. Release builds supply that commit
without writing a self-referential hash into this source page; see the
[Manual overview](/manual/) for the exact precedence and local fallback.

## Authority And Conformance

The normative Manual and its executable conformance suite jointly define the maintained language:

1. This specification states the intended rule.
2. Compiler fixtures and regression tests make the rule executable.
3. The compiler, runtime, CLI, and language server are implementations of that rule.
4. Categorized examples and Learn chapters teach the rule without extending it.

If the Manual, tests, and implementation disagree, the disagreement is a project defect. It must be resolved deliberately; undocumented behavior does not silently become a language feature, and proposal-only behavior does not override the maintained reference.

See [Conformance](/manual/conformance) for the test mapping and [Status And Compatibility](/manual/status-and-compatibility) for the preview stability policy.

## Processing Model

A source file passes through the following observable phases:

1. **Decoding and lexing.** The implementation accepts UTF-8, forms tokens, and emits indentation tokens.
2. **Parsing.** Tokens form a module AST according to the [complete grammar](/manual/grammar).
3. **Module and package loading.** Imports are resolved relative to the package source root and dependency graph.
4. **Static checking.** Names, types, trait implementations, calls, ownership, borrows, patterns, control flow, and entrypoint rules are validated.
5. **Lowering and execution.** `aura run` executes checked MIR. A direct build emits native code; the default auto build may package checked MIR in a native launcher when direct emission is unavailable. All maintained representations MUST agree on program behavior.

A failure in phases 1–4 is a compile-time diagnostic. A checked program may still produce an explicit runtime error for operations such as checked integer overflow, division by zero, out-of-bounds mutation, I/O failure, recursion-depth exhaustion, or invalid resource state. Recoverable library failures use typed `Result` or outcome enums where the API specifies them.

## Terms

**Module**
: The declarations, imports, and optional top-level statements in one `.au` source file, together with its logical package-qualified name.

**Entry module**
: The source file selected by a run, build, check, test, analysis, or completion command. Entrypoint-only rules such as the `main` signature apply to this module.

**Item**
: A top-level class, enum, Aura function, extern function, extern opaque
  handle, trait, or trait implementation declaration.

**Binding**
: A name associated with a value, parameter, pattern payload, module, type parameter, or declaration.

**Place**
: A storage location that may be read, moved, assigned, or borrowed: a local binding, field path, or supported indexed location.

**Copy type**
: A type whose values are duplicated by assignment and by-value use instead of being consumed.

**Move type**
: A type whose by-value use transfers ownership and makes the source place unavailable until it is reinitialized.

**Clone-producing operation**
: An operation that creates a second owned structural value while retaining the
  original, including explicit collection copies, cloned collection reads, and
  task-result observations.

**Clone-safety obligation**
: An inferred callable requirement that a substituted type must not duplicate
  non-cloneable state through a clone-producing operation. Aura 0.3 protects
  `random.Rng` state under this contract.

**Borrow**
: Temporary access to an existing place without transferring ownership. A shared borrow permits reading; a mutable borrow permits exclusive mutation.

**Owned position**
: A source position that consumes a non-copy value, including an explicit
  `own` parameter or collection loop, a class field or enum payload
  constructor, assignment, return, and maintained storing APIs.

**Default parameter mode**
: The unmodified `value: T` spelling. Shared access for every type is the
  source contract;
  an implementation may pass copy bits directly without changing that source
  contract. The shared mode remains stable after generic specialization.

**Resource**
: A runtime-backed value with an explicit `close()` contract and, where documented, lexical cleanup through `with`.

**Diverging path**
: A control-flow path that returns, breaks, continues, propagates an error, or terminates through a runtime failure instead of reaching the next statement normally.

## Defined, Implementation-Defined, And Unspecified Behavior

Aura aims to avoid undefined behavior at the language level. Programs that violate a static rule MUST be rejected. Checked operations that fail MUST produce the documented typed outcome or runtime diagnostic rather than memory-unsafe behavior.

Some behavior is intentionally platform-dependent:

- `intsize` and `uintsize` follow the host pointer width.
- filesystem paths, process behavior, Unix sockets, available address families, and host error messages depend on the platform.
- ordering of external events and concurrently ready tasks is not a deterministic language guarantee unless an API states otherwise.
- dictionary and set iteration follow the maintained runtime's insertion-oriented representation today, but programs should rely only on ordering explicitly promised by the relevant API contract.

Implementation-defined or platform-dependent behavior MUST remain within the constraints documented by the relevant Manual page. Behavior not granted by the specification, especially dependence on object layout, task scheduling order, hash identity, native symbol names, or diagnostic byte offsets, is unspecified and must not be required for portable Aura programs.

## Reference Organization

Read the normative core in this order:

1. [Lexical Structure](/manual/lexical-structure)
2. [Grammar](/manual/grammar)
3. [Names And Scopes](/manual/names-and-scopes)
4. [Types](/manual/types) and [Static Semantics](/manual/static-semantics)
5. [Ownership And Borrowing](/manual/ownership-and-borrowing)
6. [Expressions](/manual/expressions), [Statements](/manual/statements),
   [Closures](/manual/closures), [FFI v0](/manual/ffi), and declaration chapters
7. [Execution Model](/manual/execution-model)
8. runtime/library chapters and the [API Index](/manual/api-index)
9. [Diagnostics](/manual/diagnostics), [Current Limits](/manual/current-limits), and [Conformance](/manual/conformance)

The Learn track and a future book may reorder concepts pedagogically, but they MUST not contradict these rules.
