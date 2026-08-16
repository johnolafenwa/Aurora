# Aura Language Server

This package contains the Aura Language Server Protocol implementation.

Current LSP features:

- completion items
- member completion after `.`
- document symbols
- hover
- go-to-definition
- document diagnostics

Current compiler-backed analysis covers:

- completion items
- document diagnostics
- document symbols
- hover
- go-to-definition
- contextual lambda parameter scope, captured-name navigation, callable hover,
  and closure ownership diagnostics
- progressively scoped comprehension targets, including hover, exact
  go-to-definition, nested-clause completion, and owned result-type inference
- incomplete comprehension clauses and filters retain exact `AU1101`
  diagnostics, broad recovery completions, and safe empty hover responses
- owned list/str slices use compiler-owned result types, exact endpoint
  diagnostics, retained-source ownership analysis, and hover/navigation for
  names inside base and endpoint expressions
- incomplete or reserved slice forms preserve the compiler's exact `AU2005`
  step/assignment guidance without JavaScript-side reinterpretation
- global numeric `Array[T]` constructors, members, multidimensional indexing,
  first-axis slices, operator result types, and exact compiler-owned
  dtype/shape diagnostics
- extern C and opaque-handle symbols, hover, definitions, completions, and
  package-authorization diagnostics

The server starts one persistent compiler service:

- `aura lsp`

Requests and responses are newline-delimited JSON and carry compiler-owned
`semantic_interface_version: 6`. Every request must include the exact field:

```json
{"id":1,"semantic_interface_version":6,"method":"analyze","path":"/absolute/app.au","source":"print(1)\n"}
```

This identity is distinct from the public diagnostic document's numeric schema
version. The transport rejects and disposes a compiler with a missing or
different semantic identity, invalidates all cached document analysis, and uses
lexical recovery for the failed request. Responses remain bounded to 16 MiB.
With a matching compiler, the server caches analysis per document version,
debounces changes, cancels obsolete completion work, guards asynchronous
responses by document version, and invalidates only changed documents and their
dependents.

Compiler diagnostics keep the stable `AU####` code, related source spans,
notes, help, and machine-applicable edits through the LSP mapping. The bridge
does not classify or recreate semantic diagnostics independently.
`Diagnostic.data` also preserves the compiler-owned `call_frames` and
`task_ancestry` arrays. Their frame spans use zero-based `line`,
`start_character`, and `end_character` coordinates and retain each frame's
optional `file_path`; the bridge neither parses human backtrace notes nor
reconstructs paths or ancestry. Compiler responses always include both arrays.
Compile-time diagnostics normally carry empty frame arrays today, while the
populated shape is ready for editor workflows that present runtime diagnostics.
Failed structured assertions may also carry an optional `assertion_operands`
array in `Diagnostic.data`. Each operand preserves the compiler-owned `label`,
`type`, rendered `value`, and `truncated` flag. The field is absent when the
diagnostic has no captured assertion operands; the bridge does not infer or
re-render values.

If the compiler process cannot be started, the lexical recovery layer provides only:

- recovered top-level declarations, extern C functions, opaque handles, and
  nested method declarations
- top-level keywords, builtins, and recovered declaration completions
- same-file hover and definition for recovered declarations

The recovery path deliberately has no semantic diagnostics or member inference.
Incomplete buffers are normally handled by compiler recovery; JavaScript is a
recovery layer, not a second Aura type system.

## Development

From the repo root:

- `npm ci`
- `cargo build -p aura`
- `npm run check:lsp`
- `npm run test:lsp`
- `npm run coverage:lsp`

The build command provides `target/debug/aura` while working in this checkout.
To install the actual compiler-owned server binary on `PATH` for use from any
Aura workspace, run:

```bash
cargo install --path crates/aura --locked --force
```

That installs the `aura` executable; the editor launches its `aura lsp`
subcommand over stdio. You do not need to run `aura lsp` separately.

The VS Code extension bundles this package's JavaScript transport, which then
starts `aura lsp` for compiler-owned semantic analysis. After installing or
building `aura` as described above, rebuild, package, and force-install the
transport so VS Code does not keep an older local VSIX:

```bash
npm run package:extension
code --install-extension tools/vscode-aura/aura-language.vsix --force
```

Run **Developer: Reload Window** afterward. For a workspace outside this
repository, put `aura` on `PATH` or set `AURA_LSP_AURA_PATH` to its absolute
path before launching VS Code.

## Architecture

- `src/server.js`
  - LSP transport and request handlers
- `src/compiler_bridge.js`
  - owns the persistent compiler process and machine-readable request lifecycle
- `src/recovery.js`
  - lexical compiler-unavailable recovery only

The current direction is:

- keep diagnostics and navigation on compiler-owned analysis
- keep recovery lexical so semantic behavior has exactly one implementation
