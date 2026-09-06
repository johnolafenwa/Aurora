# ADR-0051: Import aliases and keyword-only parameter disposition

> Approved next extensions (2026-09-06): [ADR-0058](0058-first-class-callables-and-binding-contracts.md)
> accepts keyword-only callability with preserved binding metadata, resolving
> the direction of the deferral below. [ADR-0063](0063-everyday-syntax-and-pattern-ergonomics.md)
> records parenthesized imports and trailing commas. Their detailed designs and
> implementation remain pending; current import/call behavior is unchanged.

- Status: Accepted
- Date: 2026-08-02
- Roadmap decision: Batch S1, S4.7
- Builds on: ADR-0015, ADR-0037, and ADR-0050

## Context

Qualified module paths can be long, and independently designed packages often
export the same concise item name. Import aliases solve both problems without
dynamic lookup or runtime indirection.

Keyword-only parameters appear syntactically small, but Aura function values
have structural callable types. A declaration-only restriction would vanish
when the function flows through a variable, callback parameter, closure, or
collection-independent higher-order API. This ADR accepts aliases and records
why keyword-only parameters require a separate callable-type design.

## Decision

### Module aliases

A module import may bind an explicit local name:

```aura
import agents.telemetry as telemetry

telemetry.record("ready")
```

The grammar is:

```text
"import" identifier_path ["as" identifier] NEWLINE
```

With `as`, only the alias is introduced into the importing module's name
space. The path's first component is not introduced by that declaration. The
alias denotes the complete resolved module and may qualify every public name
that the unaliased complete path can qualify.

### From-import aliases

Each name in a from-import may have its own alias:

```aura
from agents.telemetry import record as record_event, Event as TelemetryEvent
from settings import default_timeout as timeout
```

The grammar is:

```text
"from" identifier_path "import"
    import_name {"," import_name} NEWLINE

import_name := identifier ["as" identifier]
```

An aliased entry binds only its alias. Entries without `as` bind their
imported name. One declaration may mix the two forms. Functions, classes,
enums, traits, extern declarations, and public module constants accepted by
the ordinary visibility rules may be aliased.

`as` changes only the local binding name. The target retains its defining
module identity, nominal type identity, visibility, trait implementations,
diagnostic origin, initialization storage, and documentation target. A public
constant alias reads the one defining-module value under ADR-0050; it does not
initialize or copy the constant again.

### Resolution and name rules

Imports are resolved before function bodies and module-constant initializers
are checked. An alias occupies the same module-level name space as items,
constants, and other import bindings. Duplicate aliases, collisions, reserved
words, and `_` are rejected. Two entries may not import the same target twice
in one declaration, even under different aliases.

An alias cannot bypass visibility or package boundaries. Module filesystem
resolution uses the path before `as`; the alias is never interpreted as a
package, directory, or dependency key. Relative-dot imports, wildcard imports,
parenthesized import lists, and trailing commas are outside this contract.

Import declarations have no expression result and do not execute at their
textual location. The dependency and once-only constant initialization order
is based on the resolved module path, not the alias spelling.

### Analysis and tooling

Hover on an alias reports its local name and fully qualified target. Go to
definition navigates to the target module or declaration. Rename may rename
the local alias and its uses without editing the target declaration. Find
references for a target includes uses reached through aliases. Completion
after a module alias lists the target module's public members; unqualified
completion lists from-import aliases as local names.

Diagnostics use the written alias at the primary source span and include the
fully qualified target when it disambiguates an error.

### Keyword-only parameter disposition

Keyword-only parameters are deliberately deferred. Aura's callable type:

```aura
def(T1, T2, ...) -> R
```

records parameter types and capabilities in order, but not parameter names,
default availability, or a positional-versus-keyword calling restriction.
Closures and named functions with compatible structural signatures can flow
through the same callable value. A keyword-only marker on one declaration
would therefore be erased when the value is assigned to that structural type,
allowing a positional call that the declaration claims to forbid.

A complete design must choose one of two coherent directions: enrich callable
type identity with argument-binding metadata and define variance/equality for
that metadata, or make keyword-only behavior a property of a distinct nominal
callable interface. It must also cover defaults, generic functions, methods,
closures, trait requirements and implementations, callback invocation,
diagnostics, and ABI metadata. Batch S1 does not select that design.

Function declarations continue to use their canonical ordered parameter
lists. Named arguments follow ADR-0015's declaration-name binding and ordering
rules, and every parameter remains positionally bindable. A `*` parameter-list
marker is rejected with `AU1101`, with guidance that keyword-only callability
is not part of Aura 0.3's structural callable model.

## Diagnostics

- `AU1101` reports malformed `as` placement, a missing alias/name, a trailing
  comma, parenthesized/wildcard import forms, and a keyword-only marker.
- `AU2001` reports an unresolved module or imported name and includes the
  pre-alias qualified path.
- `AU2999` reports an alias collision, duplicate target entry, reserved alias,
  private target, or package-boundary violation, with target and competing
  declaration spans where available.
- Diagnostics after successful resolution use the normal type, capability,
  argument-binding, and visibility codes of the referenced declaration.

No runtime diagnostic is specific to aliasing; aliases are statically erased
to resolved module/declaration identities.

## Backend requirements

The resolver produces one canonical target identity for both backends. MIR,
direct code generation, package dependency discovery, module initialization,
monomorphization, and trait lookup consume that identity. The local spelling
has no backend role. Alias choice therefore cannot affect generated behavior,
symbol identity, cache correctness, or initialization count.

Compiler analysis, the language server, and the bundled editor consume the
same alias-to-target map as compilation.

## Limits

There are no wildcard, relative-dot, parenthesized, conditional, function-
local, runtime, or re-export declarations in this decision. An alias cannot
rename individual members after a module import; use a from-import alias for
that binding shape. Keyword-only parameters and callable binding metadata are
not part of the Aura 0.3 callable surface.

## Consequences

Package users can choose concise and collision-free local vocabulary while
Aura retains static identity, visibility, and initialization semantics.
Deferring keyword-only parameters avoids a restriction that would disappear
through ordinary function values and leaves room for one sound callable-type
decision.

## Completion test matrix

- lexer/parser tests for module aliases, from-import aliases, mixed aliased and
  direct entries, contextual `as`, multiline surroundings, missing names,
  duplicate separators, trailing commas, wildcard/relative/parenthesized
  forms, and keyword-only marker rejection
- resolver tests for complete-module binding, aliased-entry binding, absence
  of path-component bindings when aliased, every importable declaration kind,
  public constants, private targets, duplicate targets, collisions, reserved
  names, `_`, package paths, and dependency keys
- semantic tests proving target nominal identity, generic substitution, trait
  lookup, capability, visibility, and named-argument behavior are unchanged by
  a local alias
- multi-module runtime tests for function calls, type construction, constant
  reads, dependency order, diamond initialization once, and identical behavior
  under several local aliases
- codegen/cache tests proving alias spelling does not change resolved symbols
  or duplicate generated declarations
- language-server tests for diagnostics, hover with qualified target,
  completion, go-to-definition, local rename, target references, semantic
  tokens, and real package files
- byte-identical MIR/direct fixtures, formatter idempotence, bundled-extension
  packaging, maintained example, and executable Manual coverage
- callable tests pinning that names/defaults are absent from structural
  `def(...)` identity, ordinary positional and named calls remain valid, and a
  keyword-only marker receives the focused diagnostic

## Ratification

Batch S1 accepts import aliases for Aura 0.3 and defers keyword-only
parameters until callable identity can preserve their binding restriction.
Parser, resolver, packages, initialization, both backends, analysis, reference,
examples, and tooling land together for the implemented alias surface.
