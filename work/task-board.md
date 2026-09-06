# Task Board

Last updated: 2026-09-06

## Priority roadmap scheduling amendment (complete)

- Authorized scope: documentation-only ordering, pre-Batch-1 foundations,
  reconciliation markers, cross-ADR conflicts, and ADR-0064. Preserve the ten
  Approved Decisions and independent v0.3.3-preview release work.
- Verification passed: 16 scoped documents, 83 relative links/anchors, ten
  byte-identical Approved Decisions, scoped whitespace, and 15 identity tests.
- Commit only this amendment; scheduled compiler, benchmark, reference-agent,
  and release-profile implementation belongs to a later task.
- Work note: `work/2026-09-06-roadmap-amendment.md`.

## Approved roadmap ADR reconciliation (complete)

- Authorized scope: update design records to reflect the user's approved
  roadmap decisions; no compiler/runtime implementation or release changes.
- Updated ADR-0052–0056 and added ADR-0058–0063 for the previously unrecorded
  callable, initialization, context-manager, collection-loan, schema, and
  everyday-syntax directions. Related historical ADRs link to future extensions.
- Verification: 29 documents and 97 relative links/anchors checked; index,
  future-implementation status, fences, whitespace, and superseded wording
  checks pass. Detailed feature design and implementation remain future work.
- Work note: `work/2026-09-06-approved-roadmap-adrs.md`.

## ADR-0038 loans and views (complete)

- Implemented place identity for local/parameter/receiver roots, fixed class
  fields, fixed tuple positions, and contained reborrows, with conservative
  inferred last-use regions and disjoint-projection analysis.
- Implemented shared and mutable local views, returned receiver/parameter views,
  immediate write-through, trait origin-slot checking, early-exit/scope loan
  cleanup, and the `AU3010` view escape/provenance diagnostic family.
- Returned-view declarations permit control flow to select different fixed
  projections of one origin. MIR and direct execution hand off the exact
  selected projection while the checker conservatively locks the origin.
- Implemented exhaustive shared/mutable/owned lambda capture lists, shared- and
  mutable-repeatable call kinds, consuming mixed environments, task/Transfer
  rejection, and exact-once closure-loan teardown.
- MIR execution and direct native builds share explicit loan lowering and pass
  local, returned, method, tuple, reborrow, and closure write-through probes.
  Analysis/LSP, TextMate syntax, snippets, and semantic-interface schema 6 are
  updated.
- The ADR is ratified. The Manual, tutorials, maintained example, generated
  agent documentation, language server, and editor tooling are updated in the
  same change family.
- Independent implementation reviews, a final Sol Max review, and a Daybreak
  vulnerability review are closed. Review fixes cover returned-view ownership/storage, exact nested
  last use, reborrow suspension, trait and module-qualified contracts, precise
  footprints, closure-loan moves/aggregate escape/canonical capture sources,
  capture acquisition order, MIR authority validation, returned-view
  forwarding, direct control-flow parity, LSP provenance/completion, contextual
  TextMate scopes, and maintained documentation drift. Follow-up fixes also
  cover selected-path cleanup in nested control flow, outgoing-return cleanup,
  grouped closures, closure-held child suspension, tuple-index reborrowing,
  non-consuming non-Copy tuple views, multi-alternative forwarding, wrapped
  aggregate escape, canonical mutable-view capture writeback, call-frame-scoped
  projection handoff, dynamic capture descriptors, lexical-local-first callee
  resolution, Copy-view escape, loan-bearing closure flow, and path-local
  branch expiry.
- The checked-source forwarding panic is repaired as a correctness defect. A
  follow-up availability finding in adversarial serialized-MIR loan-path
  expansion is fixed by deduplicating projections and enforcing a 4 MiB
  cumulative expanded-path ceiling, including chained reborrows and active CFG
  state, before allocation in both validation and execution. A compact
  serialized regression under 64 KiB proves rejection while an ordinary
  serialized control still executes.
- Whole-function MIR validation now covers duplicate labels, every successor,
  canonical/type-valid projections, and unreachable blocks. Direct lowering
  uses reachable CFG-propagated view state instead of storage-order-global
  metadata, and returned-view handoff is scoped to the active call frame.
- Final review-fix verification is green: 137 focused ADR-0038 compiler tests,
  all 1,819 compiler library tests, CLI integration coverage with 371/374
  passing under parallel load and all three timing-sensitive cases passing
  exact isolated reruns, 31/31 CLI unit tests, native-codegen acceptance,
  111 LSP tests, and 27 extension tests. The two subsequently added CLI
  regressions for trait-order identity and projected operator writeback pass on
  both backends. The complete forced MIR/direct runtime-fixture matrix passes
  with fallback disabled and local loopback enabled (1/1 aggregate,
  1,186.97 seconds). Reference integrity, all 340 tutorial fences, all 129
  verified Aura Manual blocks, generated LLM-document freshness, the
  production docs build, formatting, and Clippy with warnings denied pass.
- The earlier implementation baseline also recorded a green forced MIR/direct
  fixture matrix, 100% LSP coverage, compiler coverage of 94.78% regions,
  97.23% functions, and 96.37% lines, plus zero npm vulnerabilities after the
  compatible `nanoid` update. Coverage was not repeated for this review-fix
  pass; the full forced-backend matrix was repeated and passed.
- Fresh independent post-fix semantic, returned-view, public-MIR,
  native-parity, and TextMate audits confirm their original repros are closed;
  the one order-sensitive trait-summary residual found during recheck and the
  projected `BorrowMut` operator mismatch exposed by the exhaustive parity
  matrix were fixed and regressed before completion.
- Protected user files remain untouched: `personal/file_ops.au`, the untracked
  ADR-0022 draft, and the untracked `.swp` file.
- Work notes: `work/2026-08-16-adr0038-loans-and-views.md` and
  `work/2026-08-17-adr0038-review-fixes.md`.

## ADR-0045/ADR-0049 checkpoint cleanup (complete)

- Ratified ADR-0045 as Aura 0.3's binding testing and assertion-introspection
  contract after the completed local matrix and one complete green hosted CI
  run on Ubuntu and macOS.
- Explicitly accepted all six checkpoint choices: parameter registration,
  `-k` selection, operand rendering bounds, lifecycle-failure precedence,
  isolated/capture-free cases, and JSON result schema 1.
- Formally recorded ADR-0049's class-pattern deferment. Aura 0.3 continues to
  reject positional and named class patterns; any implementation requires a
  future dedicated ADR covering match exposure and capability behavior.
- Updated the ADR index, conformance ledger, generated LLM documentation, and
  reference-integrity regression. No compiler or runtime behavior changed.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-15-adr-checkpoint-cleanup.md`.

## Compiled-language positioning sweep (complete)

- Authorized target: lead every maintained user-facing surface with Aura's
  current identity as a compiled, statically typed programming language; place
  the systems-language ambition exclusively in a clearly labeled long-term
  direction that names operating systems and device drivers.
- Updated the root README, VitePress homepage and metadata, Why Aura page,
  tutorial overview, ML infrastructure roadmap, VS Code listing, extension
  metadata, and generated AI-agent documentation source.
- Added positioning regressions for the homepage, site metadata, extension
  listing, and generated `llms.txt` summary. Focused tests are green.
- Verification is green: 19 VitePress component/positioning tests, 15 identity
  tests, LLM generation and freshness checks, all 24 extension tests, the
  production documentation build, Manual reference integrity, all 336 tutorial
  fences, and VSIX packaging.
- The change is documentation and listing metadata only, so the focused gates
  replace an unrelated full compiler/runtime CI run. The user authorized a
  direct push to `main`.
- Historical proposals, ADRs, work notes, and CHANGELOG entries remain
  unchanged. Protected user files remain untouched: `personal/file_ops.au` and
  the untracked ADR-0022 draft.
- Work note: `work/2026-08-05-compiled-language-positioning.md`.

## Aura v0.3.2 repair and extension 0.3.3 release (complete)

- Authorized target: fix every compiler, tutorial, and editor defect found by
  the 2026-08-04 tutorial/manual/compiler audit; publish the corresponding Aura
  and VS Code extension releases; do not pause for further authorization.
- Compiler work: regress and repair module-constant collection algorithms that
  pass checking but fail in MIR/direct execution, plus module-constant
  comprehensions that currently panic both execution paths.
- Editor work: preserve member completion through incomplete or otherwise
  diagnostic-bearing buffers, highlight builtin functions, and correct the
  extension-client disposal lifecycle.
- CLI work: add `aura upgrade` as the supported command for upgrading an
  installed Aura toolchain to the latest published release.
- Documentation work: align all tutorial claims and inline examples with the
  normative Manual and current compiler, and add executable tutorial-fence
  integrity coverage so the drift cannot recur.
- Release target: Aura `v0.3.2-preview` and extension `0.3.3`, because the
  currently published identities are Aura `v0.3.1-preview` and extension
  `0.3.2`.
- Verification: module-constant fixes pass on both backends; the
  `aura upgrade` tests are green and `aura update` is rejected; 111 compiler
  analysis tests, 109 LSP tests, 24 extension tests, 45 release tests, all 336
  tutorial fences, Manual reference integrity, generated LLM artifacts, and
  the production documentation build are green. The clean complete local gate
  is green, including forced backend parity, compiler coverage at 96.31% lines,
  97.22% functions, and 94.74% regions, plus LSP coverage at 100%.
- Exact-candidate hosted CI run `30962972180` is green on Ubuntu 24.04 and
  macOS 15. PR #5 merged at `504bb06fcc96`; annotated unsigned tag
  `v0.3.2-preview` targets that exact commit.
- Release workflow `30968287811` is green. The GitHub prerelease, three native
  archives, docs archive, VSIX, and `SHA256SUMS` are public and verified. The
  public installer reports `aura 0.3.2-preview (504bb06fcc96)` and exposes
  `aura upgrade`.
- Visual Studio Marketplace and Open VSX serve the byte-identical extension
  version 0.3.3 package at SHA-256
  `0e4a7922bd5fc54862fd468e54e12696bb040d459e0c1fb0ae05c5840f830799`.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-04-tutorial-editor-runtime-repair-release.md`.

## F-string interpolation highlighting (0.3.2 published)

- Authorized target: correct the reported VS Code syntax-highlighting defect
  where `f"Lang: {lang}"` colored the interpolation braces and expression as
  ordinary string text.
- The canonical `source.aura` grammar now assigns standard format-placeholder
  scopes to `{` and `}`, an embedded-expression scope to the interpolation,
  and expression scopes to identifiers and operators. Doubled literal braces
  remain string escapes.
- A Dark+ Shiki regression pins visibly distinct colors for string text,
  braces, and the embedded identifier. Extension structure tests pin the same
  scopes, and the packaged VSIX contains the corrected grammar.
- Focused extension tests and packaging are green. The user authorized an
  extension-only 0.3.2 publication to both marketplaces. The compiler and CLI
  remain Aura 0.3.1-preview.
- Extension-only Release run `30954401440` is green at `4fb9b5d`; the CLI and
  GitHub Release jobs were skipped. Docs run `30954403228` built and deployed
  the shared grammar successfully.
- Visual Studio Marketplace and Open VSX both publicly report version 0.3.2.
  Their downloadable VSIX packages are byte-identical at SHA-256
  `9ded50ce24dcf599938fd40bdd09ed3f664c5435db10aaf5a93320a0bd8db66e`
  and contain the corrected interpolation scopes. The local VS Code
  installation was upgraded from 0.3.1 to 0.3.2 through the public listing.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-04-fstring-interpolation-highlighting.md`.

## Aura v0.3.1-preview release (complete)

- Authorized target: publish Aura v0.3.1-preview, including compiler archives,
  documentation, checksums, and version 0.3.1 of the VS Code extension to both
  public marketplaces. No new language feature is in scope.
- Coordinated crate, npm, LSP, extension, Manual, installer, download, and
  release-process identities are now 0.3.1 / v0.3.1-preview.
- The changelog records the top-level script-local fix, direct integer-call
  equality parity, required LSP semantic-interface documentation, `.au` file
  icon, and the maintained documentation improvements since v0.3.0-preview.
- Focused release metadata, packaging, extension, and reference checks are
  green. Exact-candidate hosted CI run `30943450655` is green on Ubuntu 24.04
  and macOS 15 at `fcf8af9c`; documentation run `30943450659` is also green.
- Annotated unsigned tag `v0.3.1-preview` targets
  `fcf8af9c39713d15e6a1e4a872b38db23995b02b`. Release workflow
  `30951424421` is fully green and published the GitHub prerelease, three CLI
  archives, documentation, `SHA256SUMS`, and the VSIX.
- Every public checksum verifies. The downloaded Apple-silicon CLI archive and
  deployed installer report `aura 0.3.1-preview (fcf8af9c3971)` and pass the
  packaged direct-backend smoke. Visual Studio Marketplace and Open VSX both
  publicly report `JohnOlafenwa.vscode-aura-lang` version `0.3.1`.
- The tag is unsigned by choice because no signing identity is configured in
  this checkout.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-04-v0.3.1-preview-release.md`.

## Compiler review fixes, LSP docs, and VS Code icon (local verification complete)

- Authorized target: fix plain reassignment of an existing top-level `mut`
  script local and investigate the reported `own self` call that produced an
  `AU2001` unknown-name diagnostic.
- Parser and execution regressions pin `mut count = 0`,
  `count = count + 1`, and `count += 1` as one mutable script local. The
  program now prints `2` on the MIR and direct backends.
- The `own self` report crosses Aura's module-constant initialization boundary:
  `c` is a top-level script local, while a fresh bare `x = ...` declares a
  module constant that initializes first. It now receives a focused `AU2001`
  diagnostic with the valid `mut x = ...` and `main` repairs. It is not
  classified as a move from immutable module storage.
- Compiler unit, fixture, backend, CLI parity, and LSP regressions are green.
  Manual, Learn, generated LLM documentation, reference replay, docs build,
  formatting, Clippy, extension packaging, and hygiene checks are green.
- The VS Code language contribution now assigns Aura's existing mark to `.au`
  files in both light and dark Explorer themes. A manifest regression pins the
  icon paths and verifies the packaged source asset exists.
- Direct lowering now preserves `int32` and `uint64` function-call result types
  when `==` or `!=` supplies a contextual literal. Plain, reversed, chained,
  and assertion comparisons are pinned on both backends.
- The Manual and language-server README now show the required
  `semantic_interface_version: 5` field in JSON-lines requests. A reference
  regression parses the examples and pins the exact version.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Hosted CI run `30934099472` is green on Ubuntu 24.04 and macOS 15. The local
  full-gate attempt exhausted disk on stale generated artifacts; those were
  cleaned and the affected forced-backend parity stage passed on retry.
- Work note: `work/2026-08-04-top-level-script-binding-fixes.md`.

## Landing-page language positioning (complete)

- Authorized target: describe Aura as a simple and safe compiled systems
  language for agents and frontier ML systems, with syntax similar to Python,
  ownership similar to Rust, and task-based concurrency similar to Go.
- The hero now leads with “Simple, safe systems programming.” and states all
  three language influences directly. Supporting benefits explain familiar
  syntax, deterministic ownership without garbage collection, and scoped task
  concurrency.
- The compact Python/Rust comparison remains in place. Go's influence is now
  explicit in the hero, the task-concurrency benefit, and the supporting copy,
  alongside Aura's scoped `TaskGroup` contract.
- The landing-page regression suite, generated LLM artifacts, production docs
  build, desktop render, and 390-pixel render are green. No page-level
  horizontal overflow was introduced.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-04-landing-positioning-refinement.md`.

## Platform installation, Aura highlighting, and agent docs (complete)

- Authorized target: publish detailed macOS, Linux, Windows-through-WSL, and
  VS Code extension installation documentation; replace Python highlighting
  on Aura examples with a native VitePress Aura grammar; push and merge without
  user intervention. No language feature or compiler behavior is in scope.
- Test-first contracts pin all five installation pages, current release
  commands, platform prerequisites, VS Code registry/CLI/VSIX/WSL paths, the
  `source.aura` grammar, canonical fence labels, and navigation.
- VitePress now consumes the VS Code extension's maintained TextMate grammar
  directly. The documentation site and editor therefore use one syntax source.
- All 619 maintained `python` fence labels across docs and tutorials are now
  `aura`; 673 canonical Aura fences remain and the historical proposal is
  untouched. Reference tooling no longer interprets Python as Aura, and an
  identity guard prevents reintroduction.
- The installation hub and platform guides cover architecture checks,
  prerequisites, checksum-verified installation, PATH persistence, native
  build toolchains, upgrades, WSL 2 setup, WSL filesystem placement, remote
  extension installation, and troubleshooting.
- The homepage now gives AI agents a prominent entry point for `llms.txt` and
  `llms-full.txt`, explains when to use each file, and provides a copyable
  instruction that treats the linked Manual as normative. The hero links
  directly to this section.
- A dedicated installation rail directly below the homepage hero actions links
  macOS, Linux, Windows-through-WSL, and VS Code users to the right guide. The
  Learn installation chapter repeats those platform routes and now includes a
  complete VS Code setup section, including `aura lsp` and remote WSL setup.
- Focused tests, generated LLM artifacts, the `/Aura/` production docs build,
  complete reference replay, 22 extension tests, six local HTTP routes,
  desktop rendering, and 390-pixel rendering are green. PR #4 merged at
  `d9cac8a`; its documentation workflow is green. The direct-to-main homepage
  follow-ups were authorized by the user. The installation navigation has
  focused route/responsiveness tests, rendered desktop and mobile verification,
  and current generated LLM documentation.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-04-installation-and-aura-highlighting.md`.

## Aura v0.3.0-preview release (complete)

- Authorized target: prepare and publish Aura v0.3.0-preview without adding
  language features, run one complete verification, publish the GitHub
  prerelease and version 0.3.0 of `JohnOlafenwa.vscode-aura-lang` to Visual
  Studio Marketplace and Open VSX, then verify every public artifact.
- The prerequisite merged-main CI run `30854235277` is green on Ubuntu 24.04
  and macOS 15. Release preparation completed on
  `codex/v0.3.0-preview-release`; exact candidate `8dd97c59` is merged to
  `main`.
- Product manifests already agree on 0.3.0. Release-only work stamps packaged
  binaries as `0.3.0-preview`, moves the public installer and maintained
  documentation to the 0.3 preview, and retains `0.3.0-dev` for ordinary
  source builds.
- The tag-triggered workflow built and smoke-tested three CLI archives,
  packaged the documentation and VSIX, generated and verified `SHA256SUMS`,
  published a GitHub prerelease, and published the same VSIX to both
  configured registries.
- No tag-signing identity is configured, so the annotated tag will be unsigned
  by choice. One complete hosted CI run on the exact candidate is the required
  broad verification; focused release and packaging checks run locally.
- Focused release, identity, reference, documentation, workflow, shell,
  formatting, extension, native archive, and installed archive-smoke checks are
  green. The one complete exact-candidate hosted gate, run `30860733864`, is
  green on Ubuntu 24.04 and macOS 15 at `8dd97c59`.
- Annotated unsigned tag `v0.3.0-preview` targets `8dd97c59`. Coordinated
  release run `30866580337` is green. The public GitHub prerelease contains all
  six expected assets, every checksum verifies, the deployed installer and
  Manual identify the exact release, and extension version 0.3.0 is available
  from Open VSX and the Visual Studio Marketplace package endpoint.
- Protected user files remain untouched: `personal/file_ops.au` and the
  untracked ADR-0022 draft.
- Work note: `work/2026-08-03-v0.3.0-preview-release.md`.

## Batch S1: Aura 0.3 Python-surface program (complete; merged)

- Authorized target: complete the coordinated 0.3 breaking migration in the
  ratified order S2 → S1 → S3 → S4 → five design-only ADRs, obtain the required
  local and hosted evidence, report the checkpoint, and stop before ADR-0038
  loans, the P1 performance batch, or any 0.4 implementation.
- Preconditions are verified. Hosted runs `30738666230`, `30738666191`, and
  `30738666169` each passed Ubuntu 24.04 and macOS 15 at `cd93f221`; extension
  publication is complete. Batch work is isolated on the local-only
  `codex/batch-s1-python-surface` branch and will not be pushed without the
  user's authorization.
- The workspace, compiler, CLI, language server, VS Code extension, locks,
  Manual stamp, and CLI version output are moving to Aura 0.3.0 development
  identity before semantic migration. The published 0.2.0 preview release and
  its historical packaging checks remain intact.
- Compiler coverage floors stayed frozen during implementation at 96.28%
  lines, 97.20% functions, and 94.62% regions. The one-time checkpoint
  re-ratchet is complete at 96.30% lines, 97.21% functions, and 94.71%
  regions.
- User amendment: Aura has no users and carries no backward-compatibility
  burden. The prior surface is treated as though it never existed: no aliases,
  shims, reserved spellings, specialized retirement diagnostics, fix-its,
  grace periods, or public migration guidance. `list.remove(x)` activates for
  integer lists in the same migration as every other element type; the
  internal repository rewrite changes old index-removal calls to `pop(index)`.
  Focused tests run continuously, while the expensive full local gate is
  reserved for completed migration families and checkpoint. The user confirmed
  this as the standing policy on 2026-08-03; Accepted ADR-0057 is binding.
- Current phase: S2's unified `int64` index domain and S1's canonical
  `list`/`dict`/`set`/`str` surface are implemented across the compiler, both
  backends, fixtures, diagnostics, examples, tutorials, Manual, LSP, and editor
  tooling. The coordinated S1/S2 family is committed and merged locally with
  the landed documentation/CI pull request. S3 is implemented: the 17-test
  runner suite covers discovery, filtering, JSON, lifecycle hooks, and
  parametrized registration; assertion introspection now preserves once-only
  evaluation and produces typed, bounded operand records with byte-identical
  focused MIR/direct human output. The focused compiler assertion partition is
  11/11 green, the MIR-runtime and native-runtime partitions are 130/130 and
  182/182 green, the complete compiler library suite is 1,512/1,512 green, and
  the CLI once-only/backend-parity and JSON-runner tests pass. Both new
  run-fail fixtures are byte-identical across forced MIR/direct execution. The
  LSP is 102/102 at 100% coverage, the extension is 21/21, and the 38-page
  executable reference gate is green.
  The checkpoint-wide forced-backend matrix and full local/hosted gates have not
  run for S3. S4 feature implementation is otherwise substantially complete.
  Import aliases and focused keyword-only rejection, integer bases/separators,
  power, bitwise and checked/wrapping/saturating shifts, `round`, `divmod`, raw
  and triple-quoted strings, practical static f-string specifications, match
  guards, or-patterns, and generic module constants are implemented across the
  applicable checker, analysis, MIR, direct backend, fixtures, examples,
  tutorials, Manual, LSP, and editor surfaces. Float power executes at its
  destination width; exact wide-integer formatting does not round through
  binary64; mutable match guards write back on false, trap, propagation, and
  selected-arm exits; non-Copy module constants retain one shared defining
  storage identity. The semantic tooling/cache schema is version 5. The public
  example and fixture path inventory uses canonical list/dict/set/shared/mut
  names with a permanent clean-slate filename gate. ADR-0052 through ADR-0056
  contain implementation-adoption sections with no compatibility or migration
  surface. The `math` module's eleven exact float64 functions and four exact
  constants are implemented over the generic constant foundation. The testing
  reference freeze, robust wrapped reference assertions, and focused
  warning-denied Clippy are complete. ADR-0045 remains Provisional until its
  P1-P6 answers pass the final matrix/gates, and ADR-0049's class-pattern
  disposition remains provisional;
  guards and or-patterns themselves are accepted and implemented. Generated
  `llms.txt`/`llms-full.txt`, combined reference/editor/cache verification, and
  the pre-closure forced-parity/documentation gates are green. The exact
  closure-tree coverage gate passes 361 CLI and 1,659 compiler tests. The
  final exact-tree `npm run ci` gate is green at `5ee64cd`, including forced
  backend parity, editor tooling, reference integrity, docs, audits, Clippy,
  hygiene, and coverage at 96.304488163144% lines, 97.216796875% functions,
  and 94.717811553593% regions. No synthetic tests were added. S1.1 closed the
  remaining checkpoint findings, and pull request 2 merged the completed
  branch at `7d641f0`. The user made one complete hosted run the standing
  completion policy in place of repeated consecutive reruns.
- Protected user files remain untouched: `personal/file_ops.au`, the untracked
  ADR-0022 draft, and `fc2_direct.out`.
- Work note: `work/2026-08-02-batch-s1-python-surface.md`.

## Batch S1.1: checkpoint findings closure (complete; merged)

- Authorized target: close equality gating, format zero-padding and literal
  diagnostics, root match binding patterns, the 0.3 reference restamp,
  ADR-0045's example, bounded-blocking-pool classification, and the Aug 3
  Safety workflow failures before the S1 merge.
- Standing clean-slate policy is confirmed and recorded in Accepted ADR-0057.
- S1.1-a equality gating is implemented at the common equality-obligation
  boundary. AU2008 now blocks callables, `random.Rng`, opaque FFI handles, and
  containing aggregates across list search, membership, set insertion,
  dictionary-key use, and direct equality. Twelve closure/Rng surface fixtures
  and the forced MIR/direct diagnostic-parity test are green; the complete
  semantic checker partition is 331/331 green. The stale Rng identity run-pass
  fixture is replaced by a rendering fixture and a direct AU2008 rejection.
- S1.1-b implements Python-compatible sign-aware numeric zero padding. Focused
  runtime tests and the MIR/direct fixture are green. Raw `rf`/`fr` and
  triple-quoted f-string prefixes now receive precise AU1002 diagnostics, and
  the reference records the explicit-type requirement for comma grouping.
- S1.1-c implements root binding patterns for statement and expression
  matches. Guarded bindings expose the whole scrutinee without contributing to
  exhaustiveness; unguarded bindings are final catch-alls. Copy, shared,
  `own`, and `mut` capabilities execute identically in MIR and direct mode.
- S1.1-d inspected all 39 Manual pages and migrated all 91 stale 0.2
  references across 32 affected pages to the 0.3 development contract. The
  CLI version example now matches `aura 0.3.0-dev`, generated LLM artifacts
  are current, and the identity gate pins the 39-page inventory plus absence
  of stale 0.2 Manual prose without misclassifying third-party versions.
- S1.1-e's ADR-0045 trailing-comma example is corrected. The two bounded-pool
  watchdogs passed three independent isolated reruns apiece across MIR,
  direct, and standalone paths, classifying the checkpoint reports as mutual
  host-contention flakes rather than product failures; the established
  narrow shared guard remains the correct disposition.
- The Aug 3 Safety failure is locally dispositioned. Fuzz now selects its
  pinned nightly and explicit GNU target; ASan/TSan sanitizer flags apply to
  target crates without contaminating host build tools; and a coverage-only
  live-value guard found and closed the test-only 18-value and 53-value FFI
  harness leaks. Hosted repair run `30802829724` proved the directly
  instrumented TSan scheduler partition 274/274 and the ASan native-runtime
  FFI partition 7/7, then exposed nested Aura/Cargo integration builds
  inheriting the parent sanitizer ABI without its `-Zbuild-std` contract. The
  final scripts retain those complete directly instrumented boundaries and no
  longer launch a second Cargo process. Both fuzz targets explicitly use the
  installed `x86_64-unknown-linux-gnu` target. Hosted run `30819818749` proved
  all four fuzz/sanitizer jobs and exposed an independent workflow-budget
  defect: the stress job reached iteration 16/50 before its 45-minute timeout,
  while one stale exact filter ran zero tests. The filter now resolves to the
  maintained one-worker fairness regression, the hosted count is ten complete
  repetitions of all three tests, and a regression pins both the resolved
  names and budget. Focused metadata, shell-syntax, and workflow checks are
  green. Hosted Safety run `30823933687` is fully green, including ASan, TSan,
  both fuzz targets, scheduler stress, the scheduler model, and the benchmark.
- The final local gate is green. It covers 31 Aura unit tests, 362 CLI tests,
  1,663 compiler tests, the complete forced MIR/direct parity matrix, 107 LSP
  tests at 100% coverage, 22 extension tests, all 39 Manual pages and 270
  fenced blocks, the documentation build, both dependency audits,
  warnings-denied Clippy, and hygiene. Exact compiler coverage is
  96.303181376484% lines (91,176/94,676), 97.23532281671817% functions
  (5,979/6,149), and 94.722310489708% regions (134,141/141,615). Hosted CI
  attempt 2 of run `30823914449` then found macOS's variable runtime profile
  two lines below the 96.30% floor after every behavioral test passed. Two
  observable runtime-value regressions now pin exact Duration-to-binary64 carry
  normalization and the `Array` numeric-cast diagnostic source type. The exact
  corrected coverage gate is green at 96.306350078161302% lines
  (91,179/94,676), 97.235322816718167% functions (5,979/6,149), and
  94.723722769480631% regions (134,143/141,615). No synthetic coverage test or
  justified exclusion was added; the frozen floors did not change. Hosted run
  `30841047064` proved the correction on macOS at 96.305293844268874% lines,
  then encountered newly published npm advisories at the later audit stage.
  The lockfile now selects fixed transitive releases `brace-expansion` 5.0.9
  and `postcss` 8.5.25; a clean `npm ci` and complete npm/Rust audit gate are
  green with zero npm vulnerabilities.
- The one required complete hosted proof, run `30846511697`, is green at final
  code/lock commit `b5831ab`. Ubuntu 24.04 completed in 1:40:17 with
  96.306350078161302% lines (91,179/94,676), 97.235322816718167% functions
  (5,979/6,149), and 94.725135049253268% regions (134,145/141,615). macOS 15
  completed in 1:06:21 with 96.304237610376447% lines (91,177/94,676),
  97.235322816718167% functions (5,979/6,149), and 94.722310489708008% regions
  (134,141/141,615). Both full gates and the companion Docs run `30846511698`
  are green with zero npm vulnerabilities. No consecutive reruns were
  requested under the user's one-run policy.
- Push and pull-request authorization is explicit. The user's standing policy
  now requires one complete green hosted CI run on both operating systems;
  repeated consecutive reruns are not required. That run completed, and pull
  request 2 merged the checkpoint at `7d641f0`.
- Protected user files remain untouched: `personal/file_ops.au`, the untracked
  ADR-0022 draft, and `fc2_direct.out`.
- Work note: `work/2026-08-03-s1.1-checkpoint-findings-closure.md`.

## Hosted CI path filtering (complete)

- Authorized target: prevent documentation, README, work-note, and passive
  repository-metadata changes from launching the complete Linux/macOS gate.
- The full CI workflow now runs for pull requests with code-bearing paths and
  for matching pushes to `main`. Feature-branch pushes no longer duplicate the
  pull-request run.
- Pure Markdown, `docs/**`, `work/**`, the license, Git metadata, editor
  metadata, issue/PR templates, and CODEOWNERS are excluded from the full
  matrix. Compiler, runtime, examples, tutorials with non-Markdown assets,
  packages, tooling, scripts, dependencies, and workflow changes remain in
  scope.
- The Docs workflow covers maintained Markdown and VitePress changes while
  excluding internal work notes, GitHub templates, and AGENTS.md. It builds the
  site and validates the Manual inventory, hashes, page roles, and normative
  structure without compiling the Aura toolchain.
- Verification: 34 release/workflow regressions, Manual inventory over 38
  pages and 261 fences, the production VitePress build, and
  `github-actionlint` are green.
- Work note: `work/2026-08-02-ci-path-filtering.md`.

## Landing page, installer, and documentation voice (complete)

- Authorized target: make the landing page explain Aura's benefits around
  democratized systems programming, Python-like readability, Rust-style
  safety, native compilation, static typing, deterministic ownership, no
  garbage collector, and reliable ML/agent infrastructure; add a top-level
  curl installation path and Python/Rust/Aura comparison; remove the landing
  benchmark table; move performance evidence and forward optimization work to
  the Manual; and remove unnecessary contraction-heavy comparison prose.
- The hero now leads with “Python-like code with Rust-style safety,” followed
  by the complete compiled/static/no-GC/ownership pitch for ML systems and
  agents. The landing body explains the democratization goal, compares Python,
  Rust, and Aura across seven dimensions, and names concrete model-serving,
  agent-runtime, worker, tool, process, queue, and networking use cases.
- The hero contains a responsive, accessible curl command bar with a tested
  copy interaction. `docs/public/install.sh` is a real POSIX installer for
  Linux x64, macOS x64, and macOS arm64. It selects the release archive,
  downloads `SHA256SUMS`, verifies with `sha256sum` or `shasum`, and installs
  the CLI plus its native runtime under `~/.local` or `AURA_INSTALL_PREFIX`.
- Performance measurements are absent from the landing and root README. The
  new Manual Performance chapter centralizes the current protocol, integer,
  and Array evidence, calls out the measured task and Array gaps, and records
  the optimization direction for later releases. It is classified as a
  structural Manual page in the reference-integrity sidecar.
- Voice sweep: the maintained reader surfaces began with two literal English
  contractions and 81 `rather than` / `instead of` constructions. README,
  landing, positioning, Downloads, Learn, tutorials, and the examples guide
  now contain zero of either class. Normative Manual contrasts remain where
  they distinguish exact semantic outcomes.
- Verification is green: all 32 release-packaging/installer regressions,
  `npm run docs:build`, full reference integrity over 38 Manual pages and 261
  fences, identity/reference supporting tests, and owned-file diff hygiene.
  Browser checks at 1280x720 and 390x844 covered the hero, copy state,
  responsive comparison table, and Performance route/navigation.
- Work note: `work/2026-08-02-landing-page-and-documentation-pitch.md`.
- Protected user files remain untouched: `personal/file_ops.au`, the untracked
  ADR-0022 draft, and `fc2_direct.out`.

## Extension marketplaces and hosted-CI singleton reliability (complete)

- Authorized target: prepare `JohnOlafenwa.vscode-aura-lang` for the Visual Studio
  Marketplace and Open VSX; publish only from the Release workflow with
  visible secretless skips and a tag-based manual dispatch; close the hosted
  Linux diagnostic-channel test race and the macOS timing-margin flaky class;
  then prove three consecutive hosted CI runs green on both operating systems.
- E1-E4 implementation: the extension has the public publisher/display name,
  preview metadata, exact requested categories/keywords and repository links,
  standalone listing README, packaged MIT license, and a 256 px PNG rendered
  from the byte-identical deployed `aura-mark.svg`. The release workflow now
  validates the plain `0.2.0` VSIX identity and conditionally publishes to both
  registries, with a dispatch-only path for the existing preview release.
  Downloads and release-process pages are added and linked from the docs site.
- F1 diagnosis: test-only. Hosted Ubuntu logs show Dash's `Bad fd number` from
  the test helper's `eval ... >&$FD`; concurrent tests merely made the inherited
  channel descriptor exceed Dash's single-digit redirection syntax. Production
  writes to the inherited descriptor directly. The test now deliberately holds
  descriptors above 9 and its POSIX shell helper writes through `/dev/fd`; the
  focused high-thread macOS run and 100/100 Linux high-thread loop are green.
- F2 policy: preserve calibrated local margins and use four-times hosted
  discrimination windows across the complete wall-clock family, scaling the
  deliberately slow comparison operation with the limit so real regressions
  remain distinguishable. Bounded-queue ordering now uses an explicit
  host/program release handshake. Both hosted systems retain normal parallel
  Rust execution. Corrective run `30717422681` proved that per-binary guards
  plus whole-suite single-thread execution still failed four macOS timing
  cases, which is why that attempted policy was removed rather than extended.
- Focused proof is green: `vsce ls`, `vsce package`, extension/workflow tests,
  actionlint, both safepoint probes, all 1,499 compiler-library tests at 64
  test threads, and 100/100 Linux high-thread runs of the F1 family. Both
  required repository secret names are configured.
- Pre-correction exact full `npm run ci` was green: 336 CLI/runtime tests, 1,499 compiler
  tests, the forced parity matrix, 101 LSP tests at 100% coverage, 20 extension
  tests, compiler coverage at 96.28% lines / 97.21% functions / 94.62%
  regions, reference integrity, docs, audits, warning-denied Clippy, and
  hygiene.
- First hosted attempt at `24e048c` was correctly treated as evidence, not a
  completion: F1 passed, but Ubuntu exposed an x86-64-only direct-backend ABI
  limit for flattened mutable writeback and macOS exposed whole-suite timing
  contention. The x86 correction enables Cranelift's internal stack-return
  area and pins the three-result receiver shape; focused regressions are green.
- Corrective branch run `30717422681` proved the x86-64 regression green, then
  exposed a real package-command timeout defect on Ubuntu: killing the direct
  shell left its sleeping helper alive with inherited output pipes, so reader
  joins waited for the descendant. Timed package commands now run in a fresh
  Unix process group and timeout/error cleanup kills and reaps the whole tree;
  the regression uses a ten-second descendant and completes in about 60 ms.
- Corrective run `30718486470` exposed a Linux-only benchmark-monitor race
  before Rust tests: a naturally completed child remained momentarily in
  `/proc` as a zombie, whose status intentionally has no `VmRSS`. Zombie
  samples now use the same natural-completion path as a disappeared process;
  malformed live-process RSS remains a hard error. All 56 harness tests pass,
  including the exact zombie record and natural-completion regression.
- Current focused proof: the complete compiler-library suite passes under
  `GITHUB_ACTIONS=true` with 64 test threads; the prior macOS failures, both
  CLI safepoint probes, deterministic queue ordering, x86 codegen pin, package
  process-tree timeout, 21 release-workflow tests, and 56 runtime-harness tests
  are green.
- Standard-capacity proof run `30719290315` passed all 337 macOS CLI tests,
  including the complete timing family, then exposed an unrelated FFI
  test-isolation race. Parallel FFI tests could receive the same process-ID /
  clock-tick directory name and overwrite one another's `src/main.au`.
  Temporary packages now add an atomic sequence, claim the root exclusively,
  and have a deterministic same-timestamp isolation regression; all five FFI
  acceptance tests pass with 16 test threads.
- The first exact corrective-tree local gate passed every behavioral stage and
  stopped only on the coverage floor at 96.28% lines / 97.14% functions /
  94.61% regions. Coverage closure consolidated the hard-coded Cranelift flag
  error paths, removed an unreachable class-name-only trait-dispatch fallback,
  folded timeout cleanup into its caller, and replaced a child-side `pre_exec`
  closure with direct process-group configuration. The exact instrumented
  replay is green: 337 CLI tests, five FFI tests, 1,500 compiler tests, and
  96.29% lines / 97.21% functions / 94.62% regions. No synthetic coverage test
  or coverage exclusion was added.
- The final exact `npm run ci` replay is green: 337 CLI tests, five FFI tests,
  1,500 compiler tests, the 752.92-second forced parity matrix, 101 LSP tests at
  100% coverage, 20 extension tests, compiler coverage at 96.29% lines /
  97.21% functions / 94.62% regions, reference integrity, docs, audits,
  warning-denied Clippy, and hygiene.
- The first `f795eab` three-run streak was invalidated when proof-3 macOS run
  `30727321064` failed before Rust. The idle benchmark test fixture lived for
  only 50 ms after `READY`; hosted process-stat collection could consume that
  whole interval before its 10 ms stability assertion began. The fixture-only
  lifetime is now one second without weakening the asserted window. Twenty
  consecutive `GITHUB_ACTIONS=true` focused runs and all 56 harness tests pass.
- The remaining old-commit jobs exposed two more infrastructure limits. Four
  healthy jobs reached CI's 45-minute wall-clock cap during the complete cold
  gate, so the standard-runner job budget is now 90 minutes and is pinned by a
  workflow regression. Proof-2 Ubuntu run `30727320065` also found that the
  package tests mutated `XDG_CACHE_HOME`, `HOME`, and `AURA_GIT_TIMEOUT_MS`
  concurrently; the entire environment-mutating family now shares one lock.
  All 16 package tests pass 100/100 high-thread repetitions, and the 22-test
  workflow/packaging suite plus `github-actionlint` are green.
- The next streak ran all six standard jobs beyond the former 45-minute cap.
  Proof-2 macOS run `30730386579` then reached reference integrity after its
  Rust, parity, extension, and coverage gates passed, but the hosted image did
  not provide the `rg` command required by the checker. CI now installs pinned
  `ripgrep@14.1.1` on every matrix OS; a workflow regression and
  `github-actionlint` pin the environment prerequisite.
- Primary macOS observation job `91449588741` found a separate test-only race
  under single-threaded coverage: the Phase-5.8 select registration test used
  a 20 ms sleep to keep a losing task pending, but instrumented setup could
  consume the sleep before selection began. Deadline and cancellation losers
  now use explicit release channels. The test passes 100/100 hosted-mode runs
  and the exact LLVM coverage invocation without weakening its assertions.
- Proof-3 Ubuntu job `91449593795` completed all instrumented library tests and
  then found that the `native_runtime_ffi` ELF test executable did not export
  its no-mangle C helpers for the adapter's real `RTLD_DEFAULT` lookup. The
  compiler build script now applies `-Wl,--export-dynamic` only to Linux test
  targets. A cross-platform regression pins the link contract and all seven
  instrumented FFI tests pass locally; product binaries are unchanged.
- The first exact-SHA macOS proof jobs completed every substantive gate and
  then exposed a shallow-checkout hygiene defect: with only `HEAD` available,
  `git show --check HEAD` treated it as a root commit and scanned legacy
  whitespace across the entire tracked tree. CI now fetches depth two so the
  existing commit-level hygiene check sees the parent. A workflow regression,
  all 25 packaging tests, and `github-actionlint` pin the correction without
  rewriting historical examples or user-owned personal files.
- Ubuntu job `91459871235` passed every pre-coverage stage and was progressing
  through the instrumented compiler library when the initial 90-minute budget
  canceled it without a test failure. The complete cold gate now has a
  120-minute job allowance on the same standard runner class; individual test
  contracts, coverage floors, parity, and reference checks are unchanged.
- Standard-capacity sign-off is complete at `cd93f221`: exact-SHA runs
  `30738666230`, `30738666191`, and `30738666169` each passed Ubuntu 24.04 and
  macOS 15, and main run `30742009895` passed the same tree. The release asset
  preflight then found that the previously uploaded VSIX predates the publisher
  migration. The extension-only workflow path is being corrected to build the
  current VSIX from an explicit `source_ref` before the authorized
  Marketplace/Open VSX dispatch; the tag and GitHub Release remain unchanged.
  Run `30746127573` built and validated that fresh package, then Visual Studio
  Marketplace rejected the globally occupied internal name `vscode-aura`;
  Open VSX therefore did not run. The user selected `vscode-aura-lang` as the
  permanent replacement ID. The manifest, registry URLs, workflow guard, and
  regressions now pin `JohnOlafenwa.vscode-aura-lang` for the retry.
- Publication is complete. Release workflow run `30746408847` built the VSIX
  from immutable commit `d13acd7`, preserved the existing GitHub Release, and
  published version `0.2.0` successfully to both Visual Studio Marketplace and
  Open VSX. Both public listing pages and registry metadata endpoints resolve
  `JohnOlafenwa.vscode-aura-lang`.
- Work note: `work/2026-08-01-extension-publishing-and-hosted-ci-reliability.md`.
- Protected user files remain untouched: `personal/file_ops.au`, the untracked
  ADR-0022 draft, and `fc2_direct.out`.

## Hosted CI hotfix and voice cleanup (source complete; local release handoff in progress)

- Authorized target: close ANSI-contaminated native-link capture under hosted
  `CARGO_TERM_COLOR=always`, make the archive smoke test portable to Dash,
  harden workflows, record hosted-runner results as a definition-of-done gate,
  and remove defensive disclaimer riders from maintained user-facing prose.
- Hosted audit: every CI push run from 29 July through the preview-tag push was
  red while local gates were reported green. H1 affected the hosted macOS
  matrix and all release CLI archives; H2 affected Ubuntu's archive-smoke test.
  The release publish job was skipped and GitHub has no published release.
- H1-H3 focused status: Aura-spawned and packaging Cargo captures force
  terminal color off; ANSI parsing and control-token rejection are pinned;
  the smoke test double is POSIX `sh` and Dash-checked; CI/release workflows
  set color off; Docs uses the official Node 24 deploy-pages v5 pin.
- H5 sweep: 59 disclaimer sentences were removed or rewritten while retaining
  measurement provenance and factual scope: README 2, docs 36, tutorials 10,
  CHANGELOG 4, benchmark pages 7, and zero in examples, release notes, or
  llms/marketplace copy. The README heading qualifier was also removed.
- Focused compiler, packaging, identity, reference, docs, workflow, formatting,
  and diff gates are green. The exact full local CI gate is also green: 336
  CLI/runtime tests, 1,499 compiler-library tests, the complete forced
  MIR/direct matrix, 101 LSP tests, 19 extension tests, compiler coverage at
  96.28% lines / 97.21% functions / 94.62% regions, 100% LSP coverage,
  reference integrity, docs, audits, warning-denied Clippy, and hygiene.
  Remaining local handoff steps: one commit, stale remote-tag deletion, local
  retag, artifact/checksum rebuild, cleanup, and the final report.
- Hosted verification of the fixed commit remains pending because this task
  stops before pushing. After a future branch push, every expected run must be
  watched and audited with `gh run list` before completion is reported.
- Work note: `work/2026-08-01-hosted-ci-hotfix-and-voice-cleanup.md`.
- Protected user files and untracked files remain untouched.

## Aura identity migration (completed)

- Authorized target: atomically rename the product and language from Aurora to
  Aura before the first public `v0.2.0-preview` release. The `aura` CLI and
  `.au` extension stay unchanged; compiler crate/library/ABI names, cache and
  environment contracts, manifests, diagnostics, editor tooling, docs, URLs,
  and release artifacts move to the single Aura identity.
- Current documentation describes the implemented Aura surface directly.
  Removed-feature narratives and speculative-version commentary have been
  removed from maintained README, Manual, Learn, tutorial, example, and tooling
  prose. Existing ADR bodies, work logs, the proposal, and CHANGELOG retain
  truthful history; ADR-0042 and its index note are the explicit rename bridge.
- Pre-flip inventory is recorded in
  `work/2026-08-01-aura-identity-migration.md`: 6,174 identity-style source
  identifiers, 2,881 runtime symbols, 329 environment-variable tokens, 194
  manifest references, 10 manifest paths, 711 diagnostic-oracle tokens, 1,568
  documentation tokens, three old repository URLs, 1,746 identity-bearing
  paths, and 52 prose-review candidates.
- The compiler crate/library/ABI, cache and environment contracts, package
  manifests, diagnostics, editor tooling, docs/site, URLs, and release metadata
  now use Aura. A failing-first repository identity/content gate covers all
  maintained public Markdown; focused compiler, CLI, package, reference, LSP,
  extension, docs, release, formatting, and hygiene checks are green.
- The exact full `npm run ci` replay is green: 336 CLI/runtime tests, 1,499
  compiler-library tests, the forced MIR/direct matrix, 101 LSP tests, 19
  extension tests, compiler coverage at 96.28% lines / 97.21% functions /
  94.62% regions, LSP coverage at 100%, reference integrity over 37 pages and
  260 fences, all 683 manifests, docs, audits, warning-denied Clippy, and
  hygiene.
- Completed at `5d181e1`: atomic commit, local tag movement, native macOS/Linux
  archives, VSIX/docs artifacts, smoke validation, and `SHA256SUMS`. The hosted
  hotfix above supersedes that tag target before publication.
- Nothing was published. Protected user files remained untouched.

## v0.2.0-preview pre-publish patch

- Authorized target: close the queue-iteration oversubscription livelock if
  the scheduler repair is contained; stamp preview binaries with their source
  commit; mark GitHub publication as a prerelease with a generated checksum
  asset; close the listed release-truth polish; run exact full CI; move the
  still-local tag; rebuild and verify every local artifact; then stop without
  pushing or publishing.
- Queue-iteration diagnosis and disposition: fixed. Registered-producer
  iteration kept completed tasks in every scheduler wait. Once the first
  producer finished, its permanently-ready result made consumers loop through
  `wait_for_runtime_scheduler` without parking while other producers still
  needed workers. Iteration now snapshots only still-running producers before
  each wait. A failing-first CLI regression creates four more CPU burners than
  the host's actual default worker count and verifies the iteration consumer
  result on MIR and direct without overriding `AURORA_WORKERS`.
- Release identity: `aura version`, `--version`, and `-V` print
  `aura 0.2.0-preview (<12-hex-commit>)`. The build script derives the Git
  commit or accepts an explicit validated `AURORA_BUILD_COMMIT`; the hosted
  release job supplies its already-resolved immutable commit.
- Workflow and truth polish: publication sets `prerelease: true`, generates
  and verifies `SHA256SUMS` from the downloaded runner artifacts, and attaches
  it with the release assets. Handoff commands require `gh auth login` and
  `gh auth status`. The cold-cache concurrency test budget is 120 seconds.
  Backend parity reads the actual compiler archive path from Cargo's JSON
  artifact message. AU3002 says “shared values” with an explicit stable code.
  The historical proposal points to the maintained 0.1/0.2 contract.
- Tag signature decision: the repository has no configured signing key, GPG
  secret key, or SSH-agent identity, so the replacement tag is annotated and
  unsigned by choice.
- Focused verification is green: the failing-first livelock regression on
  both backends; version flags; the named cache-concurrency test; the
  Cargo-reported parity package helper; check-fail diagnostics; 16 release
  packaging/metadata tests; warning-denied Clippy; reference integrity over
  37 pages, 260 fences, and 126 verified blocks; docs build; workflow YAML;
  shell syntax; formatting; and scoped diff hygiene.
- The exact clean full-CI replay, local tag movement, and clean-tag artifact
  rebuild necessarily run after this commit exists. Their immutable SHA,
  checksums, and smoke results belong in the final user handoff rather than a
  self-referential source note. Nothing is pushed or published by this task.
- Work note: `work/2026-08-01-v0.2.0-preview-prepublish.md`.
- Protected user state remains outside the patch:
  `personal/file_ops.au` and the untracked ADR-0022 draft.

## Batch 6 of 6 (completed)

- Authorized target: close B6.0-a through B6.0-d, then implement Phase 7
  comprehensions, owned Vec/String slices, and contiguous numeric arrays;
  conduct the final fresh-eyes, performance, claims, and positioning audits;
  prepare and locally verify the 0.2.0 technical-preview release; and stop
  after the final report without pushing or publishing.
- Entry state: Batch 5 is accepted at `8131ebe`. ADR-0037 is Accepted.
  ADR-0038 is Accepted as a design whose implementation targets Aurora 0.3
  and is not authorized in the 0.2 cycle; all ten ratification questions are
  answered yes as recommended. Compiler coverage floors are frozen at
  `96.18/96.97/94.62` until the one-time final downward-truncated re-ratchet.
- Current stage: B6.0 is complete. Dedicated `AU2008` rejects
  equality and inequality over named functions and both closure kinds before
  backend selection; exact semantic, fixture, registry, and forced-backend
  parity checks pass. The first full gate found and closed two stale FFI
  closure assertions: callable equality now pins `AU2008` there while direct
  opaque-handle equality retains `AU2003`. ADR-0037 is recorded as Accepted,
  ADR-0038 has its exact design-only 0.3 status plus ten yes answers, and the
  retry, Vec callback, and nested-lambda documentation is synchronized. Every
  benchmark workload now uses a verified owned process-group cleanup guard;
  all 54 harness tests pass across success and abnormal exits. The baseline
  Mac rebooted at
  `2026-07-30 23:02:25 +0100`; the clean contractual schema-4 replay measured
  V6 whole-process medians of `36.691666 ms` / `14.837417 ms`, reproducing the
  accepted reactor-era baseline within `1.99%` / `1.12%`. The slower dirty
  pair is not a HEAD regression. All maintained runtime gates pass; only the
  already-withdrawn massive-concurrency RSS claim remains red. The first exact
  clean full-CI replay passed all behavior, parity, LSP, extension, and
  instrumented-test stages, then missed only the frozen compiler line/function
  floors at 80,456/83,656 lines (96.174811%), 5,345/5,513 functions
  (96.952657%), and 117,328/123,996 regions (94.622407%). Coverage inspection
  found one now-unreachable closure-capture recursion after callable equality
  gained precedence; it is replaced by an explicit non-structural `Closure`
  case rather than a synthetic test. Two exact malformed-lambda parameter
  diagnostics now provide observable parser coverage. Focused parser and FFI
  equality tests pass. The exact clean full-CI replay at `49ae8bb` is green:
  54 benchmark tests, 320 CLI tests, 1,386 compiler tests plus all integration
  targets, 6 retry tests, 4 FFI acceptance tests, 2 closure acceptance tests,
  659.88 seconds of forced MIR/direct parity, 97 LSP tests, 15 extension tests,
  both coverage gates, reference integrity, docs, both audits, warning-denied
  Clippy, and hygiene. Compiler coverage is 80,466/83,652 lines (96.191364%),
  5,345/5,512 functions (96.970247%), and 117,334/123,989 regions
  (94.632588%); LSP coverage is 100%. No synthetic coverage test or exclusion
  was added.

  Phase 7.1 comprehension implementation is complete across feature commit
  `c7170b5`, completion/coverage closure `e8c7af1`, and deterministic
  ADR-0035 coverage stabilization `5609d74`. The parser and AST accept eager
  list, set, and map
  comprehensions with progressive nested clauses, filters, recursive tuple
  targets, and multiline layout; rejected capability modifiers, generators,
  mixed literals, malformed clauses, and trailing commas have teaching
  diagnostics. Static semantics infer or contextually check `Vec`, `Set`, and
  `Map` results, require exact-Boolean filters, preserve progressive
  no-shadowing/non-leaking scope, and apply bare-loop ownership to Range,
  Vec, Set, Queue, enumerate, and zip. Owner-qualified semantic metadata
  records the result type plus each clause's binding type and Queue
  receive-ownership status. MIR lowering allocates one fresh owned result,
  reuses the statement-loop machinery for nested clauses, branches filters in
  execution order, and performs the ordinary Vec append, Set insertion, or
  key-before-value Map replacement operation. The direct backend consumes the
  same MIR contract, and dedicated runtime parity covers eager ordering,
  nesting, deduplication, replacement, Queue receive, and `try` cleanup.
  Compiler analysis, the language server, and the bundled extension expose
  checked result types and progressively scoped completion, hover, and exact
  target definitions without leaking targets after the expression.

  Integration exposed an imported-module defect: MIR correctly looked for
  comprehension metadata in the defining module namespace, but namespace
  export omitted that metadata. Export now carries owner-qualified
  comprehension records and qualifies nominal result and clause-binding
  types; an imported public-helper regression runs as `2\n6` on both
  backends. ADR-0039, the normative Manual, Learn/tutorial material, indexes,
  maintained example, editor guidance, and source-hash reference inventory
  are updated with the same eager owned contract.

  The final independent audit found a second metadata defect before commit:
  accepted comprehensions in function-parameter and class-field defaults
  passed checking but panicked during MIR lowering. Failing semantic and
  MIR/direct regressions now pin both positions. Field-default metadata is
  retained, hidden default lowerers carry the lexical owner, and lookup
  resolves the owner-qualified record in the defining module.

  The first exact clean full-CI replay at `c7170b5` passed every behavior,
  forced-backend parity, LSP, extension, reference, documentation, audit,
  warning-denied Clippy, and hygiene stage before the frozen compiler-
  coverage check. Its initial report reached 96.01% lines, 96.90% functions,
  and 94.40% regions. The retained log is
  `/private/tmp/aurora-comprehension-ci-c7170b5.log`, SHA-256
  `f4e8bb8fe140277ce5a9362389fe28fe71a6fb29dd334d29156548b457c4036d`.
  Per the standing rule, that is a coverage-only closure rather than an
  escalation or permission to start Phase 7.2.

  Two rounds of behavior-focused coverage work pin parser spans and exact
  diagnostics, nested default dependencies, contextual result mismatches,
  lambda captures and target shadowing, every supported iterable family,
  owner-qualified defaults, and MIR/direct ownership parity. The first round
  reached 81,690/84,978 lines (96.13%), 5,405/5,577 functions (96.92%), and
  119,105/125,951 regions (94.56%). The second reached 81,734/84,979 lines
  (96.18%), 5,412/5,581 functions (96.97%), and 119,176/125,954 regions
  (94.61865%): the displayed region value rounded to 94.62%, but the exact
  fraction remained microscopically below the frozen 94.62% floor.

  Coverage audits also found and fixed three observable completion defects.
  A target was hidden immediately after its comprehension `if`; raw source
  scanning then let `if` in comments displace the keyword and compared byte
  columns with UTF-16 LSP positions; and a multiline comprehension used as a
  function's final statement fell outside function scope because statement
  extents ignored contained multiline expressions. Exact regressions now
  cover comments, non-BMP Unicode, f-string comprehensions, final multiline
  assignments, returns, assertions, calls, and nested blocks.

  Focused verification is green: 14/14 comprehension compiler unit tests,
  60/60 parser tests plus parse fixtures and the retired Python hint,
  check-pass/check-fail fixtures, imported nominal-metadata MIR and dual-
  backend runtime regressions, both main and `try` runtime fixtures on MIR and
  direct, the dedicated three-test CLI parity matrix, 91/91 analysis tests,
  99/99 LSP tests at 100% coverage, 17/17 extension tests, reference integrity
  (36 pages, 258 blocks, 9 reference tests, 59 integrity tests, and all 683
  migration manifests), the docs build, and formatting. The latest complete
  instrumented compiler-library replay passed 1,416/1,416 and the process
  integration suite passed 5/5; the new default-expression semantic,
  run-pass-fixture, and MIR/direct regressions are green.

  No synthetic coverage test or exclusion was added. The settled full-access
  replay after the completion fixes passed every test at 81,766/85,015 lines
  (96.178321%), 5,410/5,579 functions (96.970783%), and
  119,241/126,026 regions (94.616190%); only 2 covered lines and 5 covered
  regions remained. Its log is
  `/private/tmp/aurora-comprehension-coverage-settled-full-access.log`,
  SHA-256
  `087286b9d38f3da7b1d72e616fb401e1849882ea3f02065243631480fb857fd0`.
  A final observable regression now pins function-local completion inside a
  multiline indexed assignment used as the final statement, covering the
  indexed-target extent path. The only other new uncovered path is the
  defensive no-filter-token fallback: a checked comprehension filter can
  exist only after the parser consumes `if`, so exercising it would require a
  synthetic AST/source mismatch. It remains deliberately unforced and is the
  justified-invariant list for this closure. The definitive full-access
  coverage replay is green: 81,768/85,015 lines (96.180674%),
  5,410/5,579 functions (96.970783%), and 119,248/126,026 regions
  (94.621745%), with 324/324 CLI and 1,416/1,416 compiler-library tests plus
  every integration target passing. The exact log is
  `/private/tmp/aurora-comprehension-coverage-final-full-access.log`, SHA-256
  `bd4ac540e1b20e52925c885d4b23611cd9de2c56661538810b0a85379198d77e`.
  The coverage/completion closure is committed at `e8c7af1`. Its exact clean
  full-CI replay passed the 54-test benchmark harness, 324 CLI tests, 1,416
  compiler-library tests and all integration targets, 697.85 seconds of
  forced MIR/direct parity, 99 LSP tests, and 17 extension tests. The only red
  stage was again the coverage ratchet: scheduling chose the submitter-side
  cleanup of a blocking-I/O admission deadline rather than the worker-side
  cleanup in `runtime_value.rs`, leaving one otherwise reachable line
  unexecuted. The clean totals were 81,767/85,015 lines (96.179498%),
  5,410/5,579 functions (96.970783%), and 119,247/126,026 regions
  (94.620951%). The retained log is
  `/private/tmp/aurora-comprehension-ci-e8c7af1.log`, SHA-256
  `5f049f2aa5def066c698fbd1122061cb4c598d55609e29236777dd4b3273583e`.

  No runtime bug exists: the mutex linearizes either correct cleanup path.
  A deterministic ADR-0035 unit regression now proves the full observable
  contract when a slot opens behind an expired FIFO head: the expired job
  times out and never executes, the next live waiter is accepted into the
  released slot, both completion signals close exactly once, and no waiter or
  capacity leaks. The focused test, formatting, diff check, and warning-denied
  production Clippy pass.

  The exact clean full-CI replay at `5609d74` is green end to end: 54
  benchmark-harness tests, 324 CLI tests in 733.47 seconds, 1,417 compiler-
  library tests in 377.84 seconds plus every integration target, the
  683.02-second forced MIR/direct fixture matrix, 99 LSP tests, and 17
  extension tests. Compiler coverage is 81,768/85,015 lines
  (96.180673999%), 5,410/5,579 functions (96.970783294%), and
  119,248/126,026 regions (94.621744719%), above the frozen
  `96.18/96.97/94.62` floors. LSP coverage remains 937/937 lines, 49/49
  functions, and 251/251 branches. Reference integrity covers 36 pages, 258
  fenced blocks, 125 verified blocks, 9 reference tests, 59 integrity tests,
  and all 683 migration manifests. Docs, npm audit, the allowed
  `rustls-pemfile` RustSec warning, warning-denied Clippy, and hygiene pass.
  The retained CI log is
  `/private/tmp/aurora-comprehension-ci-5609d74.log`, SHA-256
  `878710a6d88a79e9a0ae0993edbb4f8a2fe9dc4e551fb7f78d1db23255bc56c1`.
  The detached proof worktree is clean. No synthetic coverage test or
  exclusion was added; the sole justified defensive comprehension invariant
  remains the unreachable no-filter-token fallback described above.

  Phase 7.1 is signed off. Phase 7.2 owned Vec/String slicing is implemented
  across the compiler, both backends, editor tooling, fixtures, examples, and
  the reference. All four one-colon forms produce fresh owned values. Written
  endpoints are exact `int32`, omitted endpoints carry explicit presence
  flags, negative bounds normalize once without clamping, and invalid or
  reversed bounds use `AU4003`. String slicing uses Unicode-scalar positions
  in O(n); Vec slicing clones only the selected clone-safe elements while
  retaining the source. Steps and slice assignment use the ratified `AU2005`
  guidance, and String integer indexing remains unavailable.

  Independent audit found and closed three observable gaps. Completion after
  a call-based slice receiver lost its callee, and a `]` inside an endpoint
  string corrupted raw bracket matching; the receiver scanner is now
  delimiter-stack and string aware, with compiler, LSP, and extension
  regressions. The Manual conformance row claimed executable slice evidence
  while its executable Collections block did not slice; that source-hash-
  pinned block now runs Vec and Unicode String slices. A generic constructor
  could form `Vec[consuming closure]`, after which slicing shared the
  single-use closure environment between source and result; structural
  clone-safety now rejects direct or nested capturing closure environments
  with `AU3007`.

  Focused verification is green: 19 slice compiler-library tests, both
  forced-MIR/direct CLI parity tests, all 9 fixture gates, 94 compiler-analysis
  tests, 100 language-server tests, 18 bundled-extension tests, the exact
  maintained slice example, warning-denied compiler/CLI Clippy, formatting,
  and diff hygiene. Reference integrity passes over 36 Manual pages, 258
  fenced blocks, 125 verified blocks, 9 reference tests, 59 integrity tests,
  and all 683 migration manifests; the docs build passes. Boundary fixtures
  now pin valid `-len`, negative-end underflow, String integer-index
  rejection, generic clone-safety specialization, and retained-base
  consumption. No synthetic coverage test or exclusion has been added.

  The exact clean full-CI replay at `1903aae` is green end to end: 54
  benchmark-harness tests, 326 CLI tests, 1,436 compiler-library tests plus
  every integration target, 725.84 seconds of forced MIR/direct parity, 100
  language-server tests, and 18 extension tests. Compiler coverage is
  82,477/85,734 lines (96.201040427%), 5,448/5,617 functions
  (96.991276482%), and 120,411/127,229 regions (94.641158855%), above the
  frozen `96.18/96.97/94.62` floors. LSP coverage remains 100%. Reference,
  docs, npm and Rust audits, warning-denied Clippy, and hygiene pass. The
  retained log is `/private/tmp/aurora-slice-ci-1903aae.log`, SHA-256
  `a3088d808902694863e7109be4b518d8f3f1d114d9fc4a435570d4ecdef770a0`.
  The detached proof worktree is clean. No synthetic coverage test or
  exclusion was added.

  Phase 7.2 is signed off. Phase 7.3 contiguous arrays, arithmetic modes,
  native kernels, editor/reference material, and the controlled benchmark
  harness are implemented on the working tree. The converged focused proof
  passes 29 Array tests, 2 scalar integer-mode tests, all 9 fixture categories,
  4 forced MIR/direct Array matrices, fixed-width parity, 2 exported native
  ABI tests covering all four dtypes, 10 benchmark-harness tests, 10
  reference-integrity tests, the complete reference gate, 101 LSP tests, 19
  extension tests, and the docs build.

  Two independent audits found and closed shared-operand Array snapshots,
  infallible Array and nested-container copies, wrong-rank `get` behavior,
  incomplete dtype/shape/native coverage, `from_vec` diagnostic precedence,
  recursive equality containment (including generics and trait dispatch),
  the incomplete diagnostic registry, and benchmark provenance/quiet-host
  classification gaps. The root rerun also removed one obsolete
  source-unreachable `Set[Array]` runtime test after the recursive equality
  rule correctly rejected it. `cargo clean` removed 72.3 GiB of disposable
  build artifacts before the final focused rebuild.

  The implementation checkpoint is `0511adf`. Its clean detached contractual
  11-pair run on the post-reboot Mac14,9 M2 Pro host measured median
  one-million-element `float64` operations of `1.142461 ms` for Aurora add
  versus `0.251602 ms` for NumPy 2.0.2, and `1.150392 ms` for Aurora sum
  versus `0.174065 ms` for NumPy. The ratios of medians are `4.540751×` and
  `6.608975×`; these are exact-workload measurements, not portable claims.
  The raw/summary evidence hashes are `f51b9799…` / `f6fc84c1…`. Release
  disassembly showed scalar float kernels, so no float-SIMD claim is made.

  The aggregate compiler-coverage proof is green after 334 instrumented CLI
  tests, 1,498 compiler-library tests, and every integration target. Coverage
  is 86,645/89,983 lines (96.290410411%), 5,704/5,866 functions
  (97.238322537%), and 126,842/134,034 regions (94.634197293%), above all
  frozen floors. The closure found and fixed specialized Array analysis
  inference, builtin associated-call MIR result inference, `None`
  impl-parameter inference, typed scalar-on-left Array MIR metadata,
  clean-target runtime-archive resolution in CLI installed/cache tests, and
  cancellation precedence for empty task-group Queue iteration. No synthetic
  coverage test or exclusion was added; genuine host-OOM cleanup and
  compiler-enforced Array/checked-MIR invariants remain deliberately
  unforced. The retained log and JSON report have SHA-256
  `99359fa3f8da…` and `246765c9ffff…`.

  Phase 7.3 is signed off at coverage-closure commit `465d0a0`. Its exact
  detached full-CI replay passed 54 scalable-runtime and 10 numeric-Array
  harness tests, 334 CLI tests in 474.09 seconds, 1,498 compiler-library
  tests in 184.85 seconds plus every integration target, the complete forced
  MIR/direct matrix in 764.95 seconds, 101 language-server tests, 19
  extension tests, compiler and 100% LSP coverage, reference integrity, docs,
  audits, warning-denied Clippy, and hygiene. Compiler coverage is
  86,645/89,983 lines (96.290410411%), 5,704/5,866 functions
  (97.238322537%), and 126,842/134,034 regions (94.634197293%). The retained
  log is `/private/tmp/aurora-array-ci-465d0a0.log`, SHA-256
  `9eb63c28c882c418a87470ea6fe348b3ea76b03652bee41626465cc035966b08`.
  The detached proof tree was clean and its 11 GiB build output was removed.

  The Part 3 fresh-eyes corpus is complete. Thirty new programs were written
  from the maintained reader documentation without consulting fixtures or
  examples; all 30 pass `check` and `fmt --check`, and all 60 forced MIR/direct
  executions pass with byte-identical program stdout. The corpus found one
  compiler defect: contextual `int16` operands could panic in MIR wrapping
  and saturating methods. The test-first repair reapplies the checked receiver
  width and returns controlled `AU4001` on invariant failure; all six `int16`
  boundary methods now have parity coverage. The corpus also closed the
  missing String-ordering documentation and corrected cold native progress
  from the misleading `rebuilding native runtime` to `building native
  program`. Cache-key/MIR determinism is sound; the apparent repeat was 30
  distinct cold programs plus concurrent Cargo runtime-archive SHA changes.
  Focused CLI regressions, reference integrity, docs, formatting, the exact
  production Clippy gate, and scoped diff hygiene pass.

  The consolidated performance harness is committed at `18c45ac`; all 54
  scalable-runtime, 10 Array, and 23 release-performance harness tests pass.
  Its clean detached post-reboot run is contractual with 11/11 pairs, empty
  quiet-host inventories, unchanged inputs after timing, and Xcode CPython
  3.9.6 on the Mac14,9 M2 Pro host. Primary medians are `93.875250 ms` versus
  `158.491666 ms` for naive `fib(30)`, `101.743042 ms` versus `51.950667 ms`
  for 10,000 tasks, `104.505375 ms` versus `108.605459 ms` for 20-client TCP
  fan-out, `429.291292 ms` versus `520.447791 ms` for the retrying worker,
  and whole-process V6 `36.620333 ms` / `13.724042 ms` versus CPython
  `321.096625 ms`. Separately qualified one-million-element `float64` Array
  medians remain Aurora/NumPy `1.142461/0.251602 ms` for add and
  `1.150392/0.174065 ms` for sum. The new raw/summary SHA-256 values are
  `06cc1223…` / `4490e0d1…`.

  The claims and positioning audit is complete. The root README and new
  `docs/positioning.md` use the evidence-bounded agent-control-plane wedge,
  distinguish ownership determinism from scheduling, remove the unsupported
  Rust-equivalence claim, and publish only the qualified post-reboot
  measurements. All product manifests and locks now report `0.2.0`;
  maintained Manual, Learn, tutorial, support, and changelog release wording
  is synchronized; and rendered Manual pages carry the release version plus
  an injected exact implementation commit.

  Release packaging is prepared but not published. Manual dispatch defaults
  to build-only, separates source from release identity, validates archive
  names, and refuses `publish=true` unless the release tag resolves to the
  checked-out source commit. Packaged CLI smoke runs from copied release
  examples outside the checkout with Cargo unavailable, a fresh isolated
  native cache, exact `aura 0.2.0`, basic-output, and retry-worker-output
  checks, bounded owned process groups, and descendant cleanup. Nine focused
  release-packaging tests plus the metadata and release-stamp suites pass.

  The release-preparation tree is committed at `b6230af`. Its first exact
  final compiler replay passed all 334 CLI and 1,498 compiler-library tests
  plus every integration target at 86,650/90,002 lines (96.275638%),
  5,706/5,870 functions (97.206133%), and 126,854/134,075 regions
  (94.614208%). Only the frozen 94.62% region floor was red. Per the standing
  coverage-only rule, grouped and tuple lambda-type diagnostics plus the
  generic treatment of slice and literal lambda bodies now pin observable
  parser behavior. The exact clean replay at `b2fdfdc` covers eight more
  regions and passes all 334 CLI tests, 1,498 compiler-library tests, and
  every integration target at 86,655/90,002 lines (96.28119375124997%),
  5,706/5,870 functions (97.206132879046%), and 126,862/134,075 regions
  (94.62017527503264%). Its log/JSON SHA-256 values are
  `61bb557e…`/`9c199436…`. No synthetic test or exclusion was added. The
  single final downward-truncated re-ratchet is now
  `96.28/97.20/94.62`.

  Batch 6 is complete. The one-time final re-ratchet is committed at
  `003ca88`. Its exact clean full CI is green across behavior, 772.00 seconds
  of forced backend parity, 101 LSP and 19 extension tests, compiler and 100%
  LSP coverage, reference, docs, audits, warning-denied Clippy, and hygiene.
  The final CI log/coverage JSON SHA-256 values are
  `a111b1e4…`/`7a8ddfb2…`; the final replay covered
  86,655/90,002 lines, 5,706/5,870 functions, and 126,863/134,075 regions.

  The annotated local `v0.2.0-preview` tag object is `093ef98e…` and peels to
  `003ca885…`. All three supported CLI archives were built from that clean
  tag: native macOS arm64, macOS x86_64 under Rosetta, and Linux x86_64 in an
  Ubuntu 24.04 amd64 container. Each passed architecture/layout checks plus
  installed cold-cache smoke outside the checkout with Cargo unavailable.
  The tag-built VSIX and commit-stamped docs archive also pass integrity and
  content checks. The checksum manifest is under
  `release/v0.2.0-preview-003ca8850207/`.

  The single final report is
  `work/2026-07-31-batch6-final-report.md`. No commit or tag was pushed and no
  release was published. The next step is the user's hosted-runner
  verification and publish decision.
- Work notes: `work/2026-07-30-batch6-phase7-release.md`,
  `work/2026-07-31-batch6-fresh-eyes-corpus.md`, and
  `work/2026-07-31-batch6-consolidated-benchmarks.md`,
  `work/2026-07-31-batch6-final-coverage-closure.md`, and
  `work/2026-07-31-batch6-final-report.md`.
- Standing rules: strict B6.0 then Phase 7 sequence; test-first changes;
  reference pages land with each feature; behavior-focused coverage only;
  post-reboot provenance for release performance; user-owned
  `personal/file_ops.au` and the untracked ADR-0022 draft remain outside
  Batch 6; no push and no publication.

## Batch 5 of 6 (completed)

- Authorized target: close B5.0-a through B5.0-f before Phase 6, then implement
  capture-free function values, the callable-powered standard-library
  additions, value-capturing expression lambdas, manifest-gated FFI v0, and a
  proposal-only loan/view ADR. Stop at the Batch 5 checkpoint without
  beginning Phase 7 or release work.
- Entry state: Batch 4 is accepted at settled commit `4c9d9a2`. ADR-0032,
  ADR-0033, and ADR-0036 are accepted by the Batch 4 checkpoint ruling.
  ADR-0034 is Accepted after B5.0-b closed and passed independent audit;
  ADR-0035 is Accepted after B5.0-c passed repeated default-parallel runs.
  Compiler coverage floors remain frozen at
  `96.13/96.90/94.46` until the single checkpoint re-ratchet.
- Current stage: B5.0 is complete and fully gated in seven isolated commits.
  Reachability-based TaskGroup joins retain reachable
  producer/consumer work under artificial CPU load and still cancel true
  deadlocks; queue-iteration producer lifetime remains distinct from general
  handle reachability. Nested select payload typing, default-parallel
  blocking-pool watchdogs, the Linux nightly TSan scheduler job, schema-4 V6
  startup/loop reporting, `aura build` wait progress, MIR contention
  documentation, and diagnostic wording are complete. The exact isolated
  `npm run ci` gate passes at `14f2b8b`: 308 CLI tests, 1,157 compiler tests,
  forced MIR/direct parity, 90 LSP tests, 13 extension tests, both coverage
  gates, reference integrity, docs, audits, warning-denied Clippy, and hygiene.
  B5.0 compiler coverage is 71,457/74,328 lines (96.137391%), 4,800/4,953
  functions (96.910963%), and 104,919/111,056 regions (94.473959%), above the
  frozen `96.13/96.90/94.46` floors. No synthetic coverage test or exclusion
  was added. B5.0 is closed.

  Phase 6.1 capture-free function values are implemented across parser, AST,
  static semantics, generic specialization, callable-contract flow, MIR,
  direct native ABI/runtime, TaskGroup targets, analysis/LSP, fixtures,
  examples, tutorials, and the normative Manual. Concrete inferred function
  values preserve names and defaults; written structural types and mutable
  storage intentionally erase them. Bare/`mut`/`own` capabilities retain
  shared access, mutable writeback, and transfer semantics on indirect calls.
  Method values remain out of scope with teaching diagnostics. The first exact
  instrumented replay passed 317/317 CLI tests, 1,220/1,220 compiler tests,
  all fixtures and integration targets. Its behavior-focused function-gap
  closure adds a public invalid-UTF-8/sorted-directory regression and an
  `i64::MAX` native Set boundary pin, while redundant or unreachable compiler
  closure boundaries use equivalent explicit control flow. The final exact
  compiler gate passes 317/317 CLI tests and 1,221/1,221 compiler tests at
  74,018/76,980 lines (96.15%), 4,975/5,127 functions (97.04%), and
  108,683/114,967 regions (94.53%), above the frozen
  `96.13/96.90/94.46` floors. No synthetic coverage test or exclusion has
  been added. The full repository gate is green through formatting, all Rust
  and forced-backend tests, LSP and extension tests, both coverage gates,
  reference integrity, docs, audits, and warning-denied Clippy. The final
  global hygiene command reports only whitespace in the excluded user-owned
  `personal/file_ops.au`; the complete Phase 6.1 diff and every remaining
  hygiene rule pass independently. The isolated Phase 6.1 commit is now the
  settled `8a6dbd9` stage.

  Phase 6.2 callable-powered standard-library work is complete. The maintained
  reference, Learn/tutorial track, README indexes, source-hash-pinned Manual
  examples, `examples/collections/vec_algorithms.au`, and
  `examples/agents/retry_with_backoff.au` now describe stable mutable
  `sort`/`sort_by`, eager shared `map`/clone-producing `filter`, exact shared
  callback capabilities, and `control.retry` attempt/backoff/final-error/trap/
  cancellation semantics. The full gate passed 49 benchmark checks, 318 CLI
  tests, 6 retry integration tests, 1,231 compiler tests, forced-backend
  parity, 92 LSP tests, 13 extension tests, reference integrity, docs, audits,
  Clippy, and all maintained hygiene checks. Compiler coverage is
  75,389/78,414 lines (96.14%), 5,019/5,176 functions (96.97%), and
  110,433/116,803 regions (94.55%); LSP coverage remains 100% in every metric.
  No synthetic coverage test or exclusion was added. The global hygiene
  command reports only the excluded user-owned `personal/file_ops.au`.

  Phase 6.3 lambda and closure implementation is complete and fully gated.
  Provisional ADR-0037, the
  source-hash-pinned Closure Manual page, cross-reference/Learn/tutorial/example
  updates, compiler-backed lambda scope/hover/definition/completion, and VS
  Code grammar/snippet support are implemented. Focused compiler-analysis,
  LSP, recovery, extension, reference-integrity, and both-backend maintained
  example checks pass. The explicit Phase 6.3 closure-union boundary now
  rejects conditional/`match` merging of distinct capturing closures with a
  teaching AU2002 diagnostic, while branch-local creation/calls, same-site
  distinct environments, nested capture, and never-called cleanup pass on MIR
  and direct. The combined semantic, ownership, Transfer, backend, fixture, and
  exact instrumented suites are green: 319 CLI tests, 6 retry tests, 2 closure
  acceptance tests, 1,306 compiler tests, and every remaining integration
  target pass. Compiler coverage is 77,482/80,598 lines (96.133899%),
  5,135/5,298 functions (96.923367%), and 113,378/119,923 regions
  (94.542331%), above the frozen `96.13/96.90/94.46` floors. Coverage closure
  used only observable semantic, diagnostic, runtime, cleanup, and parity
  assertions; no synthetic test, production coverage edit, or exclusion was
  added. The first full-CI replay then identified six remaining 15-second
  outer process watchdogs as load-sensitive under default-parallel direct
  execution; every case passes focused, and the isolated cross-join case took
  17.41 seconds. Those six test-only watchdogs now use the existing 30-second
  load-tolerant margin without changing semantic timings or assertions. The
  corrected full repository replay passes formatting, 49 benchmark checks,
  all default-parallel Rust tests, the forced MIR/direct parity matrix, 94 LSP
  tests, 14 extension tests, both coverage gates, reference integrity across
  all 683 historical capability-migration entries, docs, dependency audits,
  and warning-denied Clippy. LSP coverage remains 100% in every metric. The
  global hygiene command reports only the excluded user-owned
  `personal/file_ops.au`; the staged Phase 6.3 tree passes whitespace checks
  and every other hygiene invariant independently. The isolated Phase 6.3
  commit is settled as `e1feb04`.

  Phase 6.4 FFI v0 implementation and final-audit hardening are complete.
  Its provisional surface uses explicit-result `extern "C" def`
  declarations, `extern "C" opaque class` handles, build-wide
  `[package] allow_ffi = true`, and an exact root `[ffi] dependencies`
  report for direct and transitive FFI-enabled dependencies. The frontend,
  canonical nominal semantics, shared libffi engine, MIR/direct backends,
  opaque runtime values, manifest policy, analysis/LSP, extension, reference,
  tutorial, and maintained `ffi_getpid` package are integrated. The final
  audit closed basename-based opaque-type confusion, undefined opaque
  equality/address operations, public raw-checker authorization bypasses,
  forged-FFI execution through safe public arbitrary-MIR APIs, an optional
  extern-result parser mismatch, fallback extern-name spans, and incomplete
  TextMate scopes. Public arbitrary-MIR runners now reject extern targets;
  authorized path APIs, embedded MIR, and `aura test` use crate-private
  trusted execution after package validation. Focused verification passes 58
  FFI compiler tests, 12 frontend/engine tests, all four CLI acceptance
  tests, 15 public-surface tests, focused LSP/extension/reference/docs gates,
  production Clippy, formatting, and whitespace checks. The first final clean
  replay passed every behavior target and reached 80,437/83,647 lines
  (96.162444558681%) and 117,301/123,988 regions (94.606736135755%), but
  5,342/5,513 functions (96.898240522402%) was exactly one function below the
  unrounded 96.90% floor. Observable import, MIR, object-lowering,
  fixed-width-boundary, and direct process-capture tests close that final gap;
  an instrumented proof reaches 80,453/83,647 lines (96.181572560881%),
  5,346/5,513 functions (96.970796299655%), and 117,329/123,988 regions
  (94.629318966352%). No synthetic test, production coverage edit, or
  exclusion was added. The clean canonical full-CI replay passes 49 benchmark
  checks, 320 CLI tests, 6 retry tests, 4 FFI acceptance tests, 2 closure
  acceptance tests, the 712.40-second forced MIR/direct parity matrix, 1,385
  compiler tests, all remaining Rust integration targets, 97 LSP tests, 15
  extension tests, executable reference integrity, the documentation build,
  both dependency audits, and warning-denied Clippy. LSP coverage remains
  exactly 100% at 937/937 lines, 49/49 functions, and 251/251 branches.
  Canonical compiler coverage is 80,452/83,647 lines
  (96.18037706074335%), 5,346/5,513 functions (96.97079629965536%), and
  117,328/123,988 regions (94.62851243668743%), above the frozen floors.
  Global hygiene reports only whitespace in the excluded user-owned
  `personal/file_ops.au`; the Phase 6.4 diff and every other hygiene rule pass
  independently. Obsolete coverage artifacts were cleaned at the
  disk-hygiene threshold before the replay.

  Phase 6.5 is complete at its proposal-only boundary. Proposed,
  explicitly-unimplemented ADR-0038 defines place-based shared/mutable views,
  one-origin returned-view contracts, inferred last-use regions, exclusivity
  and escape rules, explicit in-loan lambda capture lists, mutable-repeatable
  closure calls, non-Transfer/task containment, typed MIR loan operations,
  stable backend storage, immediate write-through, and a unified exit-action
  model. It preserves current ADR-0009 containment and ADR-0037 by-value
  capture until implementation, does not revive `borrow` or return labels,
  keeps proposed syntax out of the normative Manual, and recommends Aurora
  0.3 rather than 0.2. No compiler or tooling implementation was added. The
  final checkpoint gate is green through formatting, 49 benchmark-harness
  tests, all Rust targets (320 CLI, 1,385 compiler, 6 retry, 4 FFI acceptance,
  and 2 closure acceptance tests), forced MIR/direct parity, 97 LSP tests, 15
  extension tests, both coverage gates, executable reference integrity, docs,
  audits, and warning-denied Clippy. LSP coverage is 100% at 937/937 lines,
  49/49 functions, and 251/251 branches. Final compiler coverage is
  80,453/83,647 lines (96.18157256088085%), 5,346/5,513 functions
  (96.97079629965536%), and 117,329/123,988 regions
  (94.62931896635159%). The one-time downward-truncated coverage floors are
  now `96.18/96.97/94.62`. No synthetic coverage test, production
  coverage-only edit, or exclusion was added. The checkpoint report is
  `work/2026-07-30-batch5-checkpoint.md`; ADR-0037 remains Provisional pending
  the next authoritative ruling, and ADR-0038 remains Proposed and
  unimplemented with Aurora 0.3 recommended. Phase 7 and release work have
  not started. Batch 5 is complete.
- Standing rules: test-first implementation; behavior-focused coverage only;
  reference pages land with each new language surface; each Phase 6 stage is a
  separately gated commit family; user-owned `personal/file_ops.au` and the
  untracked ADR-0022 draft remain outside Batch 5 changes.

## Batch 4 of 6 (completed)

- Authorized target: close B4.0-a through B4.0-d, then implement Phase 5's
  scalable runtime in the ratified strict order: reactor, public
  `yield_now()`, compiler safepoints, stack diet, scheduler soundness,
  structural Transfer rules, pinned-worker multicore, typed select,
  configurable blocking pool, and native structured frames. Stop at the Batch
  4 checkpoint without beginning Phase 6.
- Entry state: Batch 3 is accepted and committed at `1c249ab`. The Batch 4
  worktree opened from that clean checkpoint with compiler coverage floors
  frozen at `96.13/96.89/94.35`; see
  `work/2026-07-27-batch4-scalable-runtime.md`.
- Current stage: Phase 5.10 native structured frames is complete through
  observable coverage-closure commit `181204b`, after provisional ADR-0036
  landed independently at `ad6bef6`. The compiler diagnostic model, MIR
  capture, native task-local call stacks and ancestry, codegen/runtime ABI,
  two-pipe direct JSON transport, analysis/LSP forwarding, and maintained
  documentation are implemented. Both diagnostic-note parity masks are
  removed, and all 53 maintained run-fail oracles pin their full Aurora call
  chains. Three independently found transport defects are fixed and
  regression-tested: post-launch automatic fallback, descendant
  descriptor/environment leakage, and child human output after a signalled
  record failure.
  The exact-clean `181204b` CI is green: 45 benchmark-runner tests, 300 CLI
  tests, 1,150 compiler-library tests, the serialized forced-backend parity
  matrix in 623.99 seconds, 90 LSP tests, 13 extension tests, both coverage
  gates, reference integrity, docs, audits, warning-denied Clippy, formatting,
  and hygiene. Its log SHA-256 is
  `0776403c16bd356cb46d42b1e3dcc19c0c09a0ebf29be0e0cf2e405f6fa6c910`;
  the coverage JSON SHA-256 is
  `e66a3db6c7f94f5cd2ea966c19717538b646ea6c001d9fa873b03bf44f219ddd`.
  Final compiler coverage is 71,153/74,016 lines (96.131917%),
  4,757/4,909 functions (96.903646%), and 104,478/110,598 regions
  (94.466446%). The one-time Batch 4 re-ratchet is complete at the
  downward-truncated `96.13/96.90/94.46` floors. LSP coverage remains 100%:
  897/897 statements and lines, 245/245 branches, and 49/49 functions. No
  synthetic coverage test or exclusion was added.
  The clean contractual report at `181204b` uses release `aura` SHA-256
  `50503389792f7f86efb8f021f983a3917855bad82e4fbc90b99414695331142a`;
  report SHA-256
  `8ba448a06a8efb505af723ed00b8248fc1aa44ed270b46df5c15d74ecb9bd986`.
  The 10,000-sleeper peak is 207,798,272 bytes; standalone timers pass at
  6 ms arm span and 1 ms p99; idle CPU is 0.001675461%; starvation is 14 ms;
  and all seven multicore pairs pass at a 1.039673x paired median ratio and
  396.73% median four-task CPU. The massive-run peaks are 1,170,735,104,
  1,921,531,904, and 2,001,305,600 bytes, while all timer checks pass at
  3 ms arm span and 2 ms p99. The raw 1.5 GiB RSS ceiling is therefore
  recorded under the ratified escape hatch: 101,000 stackful children on this
  16 KiB-page host have a one-resident-page floor of 1,654,784,000 bytes
  before metadata. The bounded 100,000-sleeper claim is withdrawn; all other
  contractual gates pass.
  Phase 5.10 implementation and benchmark/coverage evidence are settled.
  Checkpoint documentation is committed at `77c999d`; exact-clean
  settled-tree CI passed on that commit. Phase 6 has not started.
  Earlier B4.0 implementation and its exact repository gates are complete.
  Cross-process runtime-identity and per-content-key locks give
  concurrent cold direct runs one builder plus verified consumers without
  blocking established warm hits. Human mode flushes the exact wait/rebuild
  notices before the long operation; JSON mode provisionally buffers those
  notices to preserve one structured stderr document and retains them with the
  direct failure when `auto` falls back to MIR. Installed immutable runtimes
  remain able to build with caching disabled or unavailable.
  Capability diagnostic polish is committed at `4f0461e`, and suite-count
  precision is committed at `5cb4476`.
- B4.0 verification so far: all five `native_run_cache_*` tests, the full
  274-test CLI integration suite, and the complete Rust workspace pass under
  default parallelism. The
  deterministic four-process regression proves one rebuild, four successful
  results, one published entry, and a later verified hit with both `CC` and
  `CARGO` unavailable. Broad serialization has been removed from the ordinary
  Rust gate. The compiler-coverage gate retains a narrow single-threaded
  constraint after default-parallel instrumentation passed behavior but
  undercounted function coverage at 96.86%; the serialized run restored the
  stable pre-closure result to 4201/4336 functions (96.886531%) while
  retaining the 15 known LLVM profile-data warnings. Dedicated parity, stress,
  and sanitizer ordering also remains. Behavior-focused AU2999 and canonical
  generic-analysis tests then produced final exact coverage of 96.142124%
  lines, 96.909594% functions, and 94.360014% regions, clearing the frozen
  96.13/96.89/94.35 floors with no synthetic coverage test.
  The timed warm-hit regression uses an installed immutable runtime so
  parallel Cargo activity cannot change its runtime identity while it holds an
  exact key lock; production identity remains strict and content-derived. The
  exact final-tree `npm run ci` is green across format, default-parallel Rust,
  the 529.82-second forced-backend matrix, all 79 LSP tests at 100% coverage,
  all 13 extension tests, compiler coverage, reference and stale-syntax
  integrity, docs, both audits, warning-denied Clippy, and hygiene. This
  checkpoint change lands B4.0-a/b and its behavior-focused coverage closure
  as one isolated commit family.
- Benchmark host: Mac14,9 Apple M2 Pro, 10 logical CPUs, 16 GiB RAM, macOS
  26.5.2 (25F84). Contractual measurements require the dedicated quiet-machine
  protocol and per-stage before/after evidence. B4.0 is committed at `665d540`,
  and the dedicated harness is committed at `850e906`. The contractual
  before-reactor baseline is complete from a clean tree with both process
  inventories empty: 10,000 sleepers pass at 189.641 MiB worst peak RSS; idle
  passes at 0.018886% worst CPU; all five timer runs fail the overlap gate at
  13–15 ms arm spans with diagnostic raw p99 overshoot of 8–10 ms; V6 medians
  are 32.734250 ms for int32 and 10.248625 ms for int64. Raw JSON and hashes are
  recorded in `work/2026-07-27-phase5-runtime-benchmarks.md`. No Phase 5.1
  runtime implementation edit preceded the baseline.
- Phase 5.1 reactor implementation: persistent `mio` descriptor
  registrations, a compacting versioned timer heap, durable keyed wake
  subscriptions, and direct Queue/task-completion/cancellation/blocking-pool
  wakeups now replace the rebuilt `pollfd` array and 1 ms scan. Every wait uses
  an epoch key and check-subscribe-recheck registration; losing sources and
  scheduler teardown remove all subscriptions. Nonblocking admission prevents
  a continuously yielding task from starving inbox, timer, or descriptor
  readiness. Reactor failures become scheduler diagnostics rather than false
  descriptor readiness.
- Phase 5.1 verification so far: 970 compiler library tests, all 22 reactor
  primitive tests, five adversarial scheduler models, the mixed-wakeup
  MIR/direct fixture, warning-denied product Clippy, reference integrity, docs
  build, and expanded fairness/cancellation/mixed scheduler stress are green.
  Audit regressions pin stale epochs, stale timer-heap compaction,
  transactional descriptor cleanup, source-subscription cleanup, wake
  precedence, and continuously-ready fairness. The accepted clean-tree
  benchmark at `1de9cf7` passes every reactor-stage gate: 204,128,256-byte
  worst sleeper RSS; 4-5 ms timer arm spans with 3-4 ms p99; and
  0.000012315% worst idle CPU. A corrected-workload replay against the
  pre-reactor binary measured 18 ms arm spans and 11-12 ms raw p99. Exact
  provenance and report hashes are recorded in
  `work/2026-07-27-phase5-runtime-benchmarks.md`. Frozen compiler coverage is
  green at 65,732/68,369 lines (96.142989%), 4,333/4,472 functions
  (96.891771%), and 97,052/102,827 regions (94.383771%), above the unchanged
  96.13/96.89/94.35 floors. No synthetic test or coverage exclusion was added;
  duplicated unreachable scheduler error closures were consolidated into the
  tested reactor diagnostic helper. LSP coverage remains 100%. Exact
  `npm run ci` is green: 275 CLI tests, 970 compiler library tests, forced
  backend parity, 79 LSP tests, 13 extension tests, both coverage gates,
  reference execution, docs, audits, warning-denied Clippy, and hygiene. The
  affected native-build and scheduler test watchdogs were widened only after
  isolated and CPU-saturated replays proved default-parallel cold compilation,
  rather than product deadlock, caused the earlier expirations. Phase 5.1 is
  committed through `df104fa`.
- Phase 5.2 `yield_now() -> None` is implemented through the existing
  ready-tail scheduler requeue on both backends, with a void direct ABI.
  Source diagnostics pin zero arguments and builtin-name reservation; the
  future-multicore-safe fairness fixture requires an already-runnable sibling
  to progress without specifying global task order. Focused Rust tests,
  fixtures, MIR/direct example execution, 80 LSP tests at 100% coverage, 13
  extension tests, reference integrity, and docs are green. The complete
  forced-backend matrix passed in 761.72 seconds and the contractual
  clean-tree benchmark passed all gates at `d22ae10`: 205,799,424-byte worst
  sleeper RSS; 4-9 ms timer arm spans with 2-5 ms p99; and 0.000020959% worst
  idle CPU. Exact report provenance is recorded in
  `work/2026-07-27-phase5-runtime-benchmarks.md`.
  Frozen compiler coverage passes without additional closure at
  65,767/68,407 lines (96.140746%), 4,335/4,474 functions (96.893160%), and
  97,103/102,880 regions (94.384720%). No synthetic test or exclusion was
  added. Exact full `npm run ci` is green with 275 CLI tests, 971 compiler
  library tests, forced MIR/direct parity, 80 LSP tests, 13 extension tests,
  both coverage gates, reference integrity, docs, audits, warning-denied
  Clippy, and hygiene.
- Phase 5.3 automatic loop safepoints are implemented on both backends. Every
  loop shape lowers through one explicit latch; MIR yields every eight latch
  traversals, while native code uses a 4,096-iteration unboxed fuel counter
  and statically elides checks for modules with no possible sibling task.
  Behavioral tests prove timer, Queue, and socket progress during a hot loop;
  structural tests pin `continue`/`break`, nested loops, mutable Vec writeback
  ordering, the void native yield ABI, and sequential-native elision. Focused
  gates are green: 277 CLI tests, 979 compiler tests, the full forced-backend
  parity matrix, 25 benchmark tests, reference integrity, docs, formatting,
  and hygiene. Frozen coverage is green at 65,842/68,478 lines (96.150589%),
  4,337/4,476 functions (96.894549%), and 97,258/103,032 regions (94.395916%)
  with no synthetic test or exclusion. The clean `a339c61` contractual
  benchmark passes all five gates: 204,193,792-byte worst sleeper RSS; 4 ms
  timer arm spans and 2 ms p99; 0.000011333% worst idle CPU; 18 ms worst
  starvation result; and a 16.793333 ms native int64 median, 23.377% faster
  than the accepted Phase 5.2 baseline. Exact full CI is green: 277 CLI tests,
  979 compiler tests, full forced-backend parity in 547.42 seconds, 80 LSP
  tests, 13 extension tests, both coverage gates, reference/docs, audits,
  warning-denied Clippy, and hygiene. Phase 5.3 is complete; Phase 5.4 stack
  diet is next.
- Phase 5.4 stack diet is complete through `f72fd2f`. The contractual
  pre-change report was
  captured from clean baseline commit `5af134a` at
  `/tmp/aurora-phase54-before.json` (SHA-256
  `405f3acb61126aed87ee6bebdb0d2abb3e98feef9f3992f6f0d42e32bffdfb2f`).
  The 10,000-sleeper control passes at 204,193,792 bytes worst whole-process
  peak RSS and 196,935,680 bytes worst incremental peak RSS. The new
  100,000-sleeper plus 1,000-timer gate is intentionally red before the diet:
  1,980,628,992 bytes worst whole-process peak RSS and 1,972,830,208 bytes
  incremental, above the 1.5 GiB limit, while its 4 ms arm span and 5 ms p99
  still pass. Existing controls also pass at a 5 ms timer arm span, 3 ms timer
  p99, 0.000019655072722165167% worst idle CPU, 12 ms worst starvation
  latency, and a 14.373750 ms native int64 median.
- The implementation now uses guarded 512 KiB default coroutine stacks.
  `TaskGroup.start_with_stack` and `start_soon_with_stack` provide explicit
  guarded requests from 256 KiB through 64 MiB; the 256 KiB floor is an opt-in
  for measured shallow work, not the default. Deep HTTP, rustls, and WebSocket
  host-library steps run through a dedicated bounded two-worker protocol
  service with 2 MiB worker stacks, while reactor readiness, deadlines,
  cancellation, and protocol-state ownership remain explicit. Dynamic
  `json.parse` runs through a separate two-worker, two-in-flight service with
  2 MiB stacks and admission before source cloning; iterative JSON conversion,
  writing, rendering, and canonical-value cloning avoid moving depth-bounded
  language work back onto the coroutine stack. The legacy `json.is_valid` and
  `json.parse_string_map` helpers remain bounded caller-side compatibility
  operations. Exact full `npm run ci` is green: 280 CLI tests, 1,007 compiler
  tests, the complete forced MIR/direct parity matrix in 543.05 seconds, 81
  LSP tests at 100% coverage, 13 extension tests, compiler coverage,
  reference/migration/docs, audits, warning-denied Clippy, and hygiene.
  Frozen compiler coverage passes at 67,159/69,851 lines (96.146082%),
  4,446/4,587 functions (96.926095%), and 99,186/105,100 regions
  (94.372978%), with no synthetic coverage test or exclusion. The clean
  `0dddb43` post-change benchmark passes the 10,000-sleeper, timer, idle,
  starvation, and V6 controls. Ten thousand parked sleepers use 205,389,824
  bytes worst whole-process and 197,836,800 bytes worst incremental RSS, an
  amortized 19,784-byte (19.32 KiB) upper bound per requested sleeper.
  The 100,000-sleeper plus 1,000-timer run passes 3 ms timer gates but reaches
  1,978,384,384 bytes worst RSS, so the explicit escape hatch applies: one
  16 KiB resident page for each of 101,000 stackful children already exceeds
  1.5 GiB before metadata. The massive-concurrency claim remains unavailable.
  Raw report `/private/tmp/aurora-phase54-after.json`, SHA-256
  `5245595a6675dba0cc1e39383dda505e50d7333cb59fbc3afea4c648fcca0ab4`.
- Stack evidence is scope-qualified: the complete compiled Aurora HTTP
  example, including MIR/direct language-execution frames, `SIGBUS`ed with an
  experimental 256 KiB global default and completed at 512 KiB. The isolated
  Rust runtime HTTP round trip now succeeds when only its protocol-calling
  children are forced to 256 KiB; that proves deep host frames are offloaded
  to protocol workers, not that 256 KiB is a safe whole-program default.
- Phase 5.5 scheduler soundness is complete in implementation commit
  `ea92897`. The raw
  `*mut LightweightTaskScheduler` and reconstructed `&mut *scheduler`
  nested-spawn path has been replaced by an owned FIFO request broker, leaving
  the scheduler driver as the only mutable scheduler owner. FIFO describes
  broker admission only; ready-task ordering remains unspecified. Guarded
  stack allocation and task-state construction happen synchronously before a
  nested start is published, so failure is immediate and enqueues nothing.
  The scheduler drains requests after each resume, after cleanup/unwind, and
  throughout teardown. Unbounded-wait state is now published atomically on
  `TaskState` after successful registration and cleared on disarm, avoiding
  shared inspection of scheduler internals.
- Phase 5.5 teardown work disarms waits, drains pending starts, retires all
  admitted and prepared tasks, publishes `Cancelled` completion, and wakes
  task/group/reactor observers. Pure Rust/MIR frames use force-unwind.
  Generated direct children and roots use stack reset plus exact-once release
  of scheduler-owned argument storage, claim flags, retained opaque values,
  and task-local direct-runtime state; unstarted direct tasks drop their entry
  closure and external state once. This abandonment fallback contains
  host/runtime state only. It does not run arbitrary Aurora cleanup code, so
  it is not a source-level cleanup guarantee.
- Phase 5.5 verification so far: the 1,017-test compiler library suite is
  green; focused scheduler and native-runtime tests cover synchronous
  preparation failure, nested immediate waits, wait publication, teardown
  cancellation/wakeup, pure-Rust unwind, direct child/root containment, and
  no double release. `scheduler_nested_spawns.au` passes the targeted CLI
  parity test on MIR and forced-direct backends. The hygiene gate was first
  proven red against the old aliases and now rejects reintroduction of either
  raw scheduler pointers or `&mut *scheduler` reconstruction.
- Phase 5.5 exact full CI is green on the settled implementation tree: 281 CLI
  tests, 1,017 compiler-library tests, the 547.91-second forced MIR/direct
  matrix, 81 language-server tests, 13 extension tests, reference integrity,
  documentation build, both audits, warning-denied Clippy, and hygiene.
  Frozen compiler coverage passes at 67,266/69,957 lines (96.153351%),
  4,454/4,596 functions (96.910357%), and 99,304/105,216 regions
  (94.381083%); LSP coverage remains 100%. No synthetic coverage test or
  justified exclusion was added.
- The clean contractual Phase 5.5 benchmark from `ea92897` passes the 10,000
  sleeper, timer, idle, and starvation gates. The 100,000-sleeper RSS gate
  remains the sole failure under the accepted Phase 5.4 escape hatch, while
  its timer arm-span and p99 controls pass. Full provenance, raw-report hash,
  and before/after measurements are recorded in
  `work/2026-07-27-phase5-runtime-benchmarks.md`. The soundness refactor shows
  no material performance regression and makes no new performance claim.
- Phase 5.6 structural Transfer and static single-consumer task-result
  enforcement has passed its complete implementation gate. The checker derives
  Transfer from fully resolved specialized types: copy values, `String`, and
  recursively transferable collections, tuples, classes, and enums pass;
  capability views, `random.Rng`, `TaskGroup`, and live host resources fail.
  `Queue[T]` and `Task[T]` handles are transferable independently of their
  payload, while Queue construction and send operations require a transferable
  payload. All four TaskGroup start surfaces check captured arguments and task
  results before scheduling and issue nested-path `AU3008` diagnostics.
- Static result repeatability is enforced at the same boundary. `Task[T]` is
  copyable only for copy results, Queue handles, and recursively repeatable
  Task handles. Observing a non-repeatable result consumes the Task binding;
  `wait_any` and `wait_all` consume the complete task vector. `AU3009` covers
  clone/container operations that would duplicate a result right, while the
  existing moved-value and shared-access diagnostics cover later use and
  borrowed consumption. MIR and direct lowering carry the repeatability bit
  into task creation, and both runtimes use an atomic one-winner claim as
  defense in depth for every observation surface.
- Phase 5.6 maintained surfaces now include provisional ADR-0033, amendments
  to ADR-0008 and ADR-0020, semantic/runtime architecture notes, the language
  manual and diagnostics, compiler-service/LSP expectations, and an extensive
  positive/negative fixture matrix for structural aggregates, generic
  specialization, Queue payloads, capability/host-resource leaves, repeated
  observations, aliases, branches, loops, container access, and both
  multi-wait helpers. Final review also made `Range` explicitly Transfer
  without making it Copy, and removed prose that prematurely called Queue/Task
  internals synchronized before the multicore implementation. Focused evidence
  is green for 9/9 compiler fixture
  harness tests, 19/19 call-metadata tests, 85/85 LSP tests at 100% coverage
  (895 statements and lines, 246 branches, 49 functions), 13/13 MIR
  `task_group_` tests plus four specialization/move tests, imported-class,
  associated-method, and native-object tests, CLI structural-Transfer
  MIR/direct parity, the single-observer JSON MIR/direct cleanup regression,
  direct TCP/Unix ownership smoke tests, reference integrity, and the docs
  build.
- Exact Phase 5.6 full CI is green: 282 CLI tests, 1,056 compiler-library
  tests, the 565.37-second forced MIR/direct matrix, 85 LSP tests at 100%
  coverage, 13 extension tests, both coverage gates, executable reference
  integrity, all 683 migration manifests, docs, audits, warning-denied Clippy,
  and hygiene. Frozen compiler coverage passes at 68,580/71,330 lines
  (96.144680%), 4,525/4,670 functions (96.895075%), and 101,189/107,171
  regions (94.418266%). New tests pin observable behavior; no synthetic
  coverage test or exclusion was added. Redundant checked-MIR and
  validated-type defensive branches were restructured into explicit
  invariants instead of receiving artificial tests.
- Phase 5.6 is complete at implementation commit `7dcdd70` plus its benchmark
  evidence commit. Its clean contractual report passes the 10,000-sleeper,
  timer, idle, starvation, and V6 controls; only the already-accepted
  100,000-sleeper RSS gate remains red, while that workload's timer controls
  pass. Raw report `/private/tmp/aurora-phase56-after-transfer.json`, SHA-256
  `209baaf5264fe469db9f88c2c7aa235fce2d2505e3d233eb0baad69fbe060bb7`.
  Phase 5.7 pinned-worker multicore is next, and no Phase 5.6 text or test
  claims parallel task execution.
- Phase 5.7 pinned-worker multicore is implemented at `6fb5efb`. The runtime
  now creates one scheduler/reactor per OS worker,
  defaults to `available_parallelism`, accepts a strict positive
  `AURORA_WORKERS` override, assigns prepared tasks round-robin, and never
  moves a coroutine after assignment. Queue and Task handles use synchronized
  cross-worker state; task IDs are coordinator-global; fatal errors, root
  completion, shutdown, inbox draining, cancellation, and generated direct
  cleanup are coordinated without reintroducing a periodic idle poll.
- Task startup now follows prepare, register, publish in both MIR and direct
  paths. TaskGroup membership and every captured Queue producer are visible
  before a prepared task can enter a remote worker inbox, closing the
  immediate-completion/early-Queue-close race. Four-worker behavior tests pin
  Queue/Task parity, no loss or duplication, producer-local FIFO, complete
  atomic output lines, cancellation, distinct simultaneous failures, result
  claim atomicity, worker affinity across yield/timer/Queue waits, and
  shutdown-spawn cleanup.
- The mandatory multicore calibration is green on the Batch 4 host. The final
  clean-tree contractual run's seven alternating paired repetitions measured
  a four-task/one-task median ratio of `1.077123x` against the `1.6x` ceiling,
  with `393.61%` median four-task process CPU and low MAD. The first
  calibration exposed a benchmark-only macOS
  defect: `proc_pid_rusage` CPU values are mach absolute-time ticks, not
  nanoseconds. The runner now applies `mach_timebase_info` (`125/3` on this
  host), and a behavioral host-runner test pins the conversion instead of
  weakening CPU corroboration.
- Phase 5.7 focused evidence is green: 1,072 compiler-library tests
  under default parallelism, 118 native-runtime tests twice under default
  parallelism, 166 runtime-value tests, all four new four-worker CLI tests on
  MIR and direct, AU4006 override diagnostics on both backends, the explicit
  one-worker fairness proofs, 45 benchmark-runner tests, warning-denied
  Clippy, formatting, and diff hygiene. Frozen compiler coverage passes at
  69,108/71,883 lines (96.139560%), 4,581/4,726 functions (96.931866%), and
  101,829/107,849 regions (94.418122%), above the unchanged
  96.13/96.89/94.35 floors. The closure uses only observable fault,
  shutdown, AU4006, and direct Queue-registration tests; no synthetic test or
  exclusion was added.
- Exact full Phase 5.7 `npm run ci` is green: 45 benchmark-runner tests; 288
  CLI and 1,072 compiler-library tests under default-parallel Rust; the full
  forced MIR/direct fixture matrix in 559.03 seconds; 85 LSP tests; 13
  extension tests; compiler and 100% LSP coverage; reference integrity over 34
  pages, 247 fences, and 118 verified blocks; all 683 migration manifests;
  docs; audits; warning-denied Clippy; and hygiene. The allowed
  `rustls-pemfile` unmaintained warning remains.
- Phase 5.7's clean-tree contractual report is complete at `6fb5efb`, with an
  empty dirty-file list and no competing processes. The 10,000-sleeper,
  standalone timer, idle, starvation, V6, and mandatory multicore gates pass.
  The multicore paired median ratio is `1.077123x`, ratio of medians is
  `1.056700x`, and all seven pairs pass with `393.61%` median four-task CPU.
  The 100,000-sleeper plus 1,000-timer workload is the sole red gate under the
  accepted Phase 5.4 RSS escape hatch at 1,989,033,984 bytes worst RSS; its
  5 ms arm span and 3 ms p99 pass. Raw report
  `/private/tmp/aurora-phase57-after-pinned-worker-multicore.json`, SHA-256
  `6d47c90d3dd9eb85421245c92aa3d12b01cb58ddf9ac0819b0e210c14123531d`.
  The benchmark evidence is committed at `f601fc7`.
- Phase 5.8 typed select is active. Provisional ADR-0034 landed alone at
  `ec3fd61` before implementation. It specifies a variadic positional builtin
  over Queue, Task, and relative-Duration sources; the typed
  `SelectOutcome[Q, T]` result; cancellation-first then lowest-index
  arbitration; common-base deadlines; non-repeatable Task observation-right
  consumption and loser abandonment; and check-subscribe-recheck with
  idempotent loser cleanup. No statement syntax is added.
- Phase 5.8 implementation and focused verification are complete. MIR and
  direct execution share one composite scheduler wait with cancellation-first,
  original-index arbitration, post-validation common-base deadlines, atomic
  Queue receive, Task observation claims, direct cross-worker wakeups, and
  rollback-safe loser cleanup. Both adapters validate typed source metadata
  and malformed descriptors with `AU4001`; the direct backend uses the owned
  internal `aurora_direct_select(tuple_ptr)` ABI. Focused gates are green for
  40 compiler select tests, all fixture families, four forced-backend
  four-worker CLI parity tests, 89 LSP tests, 13 extension tests, 119 verified
  reference blocks, docs, formatting, and hygiene. Frozen compiler coverage is
  green at 69,985/72,794 lines (96.141165%), 4,634/4,779 functions
  (96.965892%), and 103,033/109,068 regions (94.466755%), above the unchanged
  96.13/96.89/94.35 floors. The coverage closure contains only observable
  semantics, diagnostics, recovery, parity, and malformed external-ABI tests;
  no synthetic coverage test or exclusion was added. Exact full CI is green:
  45 benchmark-runner tests, 292 CLI tests, 1,105 compiler library tests, the
  forced MIR/direct matrix in 820.09 seconds, 89 LSP tests, 13 extension tests,
  both coverage gates, reference integrity, docs, audits, warning-denied
  Clippy, and hygiene over the Phase 5.8 snapshot. A pre-existing MIR TCP test
  ordered `peer_addr` after `shutdown_write`; contention reproduced the race
  and the test now proves live addresses before shutdown without changing
  runtime code or timeouts. The user-owned `personal/file_ops.au` remains
  byte-identical and excluded.
- Phase 5.8's clean-tree contractual report at `3e15b8a` has empty dirty and
  competing-process inventories. The 10,000-sleeper, timer, idle, starvation,
  and mandatory multicore gates pass. All seven multicore pairs pass with a
  1.020775x paired median ratio, 1.021596x ratio of medians, and 398.54%
  median four-task CPU. Massive concurrency remains the sole red gate under
  the accepted escape hatch at 1,720,057,856 bytes, with its 4 ms arm span and
  2 ms p99 passing. Raw report
  `/private/tmp/aurora-phase58-after-typed-select.json`, SHA-256
  `f72889aa83b8a222517808ef39df91d62a175109bc8806c3628602884a8c9ea2`.
- Phase 5.9's provisional ADR-0035 is committed independently at `cc450c9`.
  It specifies a documented default worker derivation, exact positive
  `AURORA_BLOCKING_WORKERS` overrides, optional positive pending-queue
  capacity through `AURORA_BLOCKING_QUEUE_CAPACITY`, FIFO scheduler-aware
  admission, a precise pre/post-acceptance timeout and cancellation boundary,
  lazy all-or-nothing worker startup, fatal pre-user-code `AU4006`
  configuration diagnostics across MIR/direct/standalone execution, and the
  deterministic resolver-saturation completion matrix. The implementation
  now provides exact configuration decoding, lazy process-lifetime
  all-or-nothing startup, bounded FIFO scheduler-aware admission, atomic
  acceptance handoff, late-result disposal, panic containment, and
  resolver/TCP recovery on the shared pool. The final independent audit found
  no product, concurrency, parity, test-matrix, documentation, or reference
  defect. Exact full CI is green with 296 CLI tests, 1,130 compiler tests, the
  serialized parity matrix in 837.22 seconds, 89 LSP tests, 13 extension
  tests, reference/docs/audits/Clippy/hygiene, and both coverage gates. Frozen
  compiler coverage is 70,514/73,341 lines (96.14540298059748%),
  4,683/4,830 functions (96.95652173913044%), and 103,704/109,782 regions
  (94.4635732633765%). No synthetic coverage test or exclusion was added;
  three unreachable defensive admission branches were restructured away.
  The clean-tree contractual benchmark at `d921313` is fully green on the
  Mac14,9 M2 Pro: 10,000 sleepers peak at 206,962,688 bytes; standalone timer
  p99 is at most 3 ms; idle CPU peaks at 0.001075%; starvation latency peaks
  at 18 ms; all seven multicore pairs pass with a 1.020214x paired median
  ratio and 398.49% median four-task CPU. The 100,000-sleeper plus
  1,000-timer gate also passes for the first time at 1,457,848,320 bytes,
  4 ms arm span, and 3 ms p99. Raw report
  `/private/tmp/aurora-phase59-after-configurable-blocking-pool.json`, SHA-256
  `d9947ddc4c65c7ff7f592585d85530f92f10045b73fa66f25dfd5a1b2dabf21a`.
  Phase 5.9 is complete.
- Phase 5.10's provisional ADR-0036 is committed independently at `ad6bef6`.
  Its implementation adds typed, always-present `call_frames` and
  `task_ancestry` arrays with innermost-first call order, youngest-first task
  order, exact per-frame paths, and once-only capture before cleanup or task
  state reset. MIR and direct execution share the model; native JSON runs use
  an exact trap-intent marker plus a separate bounded structured-record pipe,
  while ordinary nonzero program statuses stay ordinary and no post-launch
  protocol outcome may trigger automatic fallback. Compiler analysis and the
  LSP forward the additive records without changing semantic interface
  version 2. Both diagnostic-note parity normalizers are gone. All 53
  maintained run-fail oracles pin a full call chain: 47 were regenerated and
  the 6 already framed remain exact. Maintained README, tutorial, Manual,
  architecture, changelog, ADR-index, and reference-integrity surfaces are
  updated. Independent audit defects covering post-launch fallback,
  inherited internal descriptors/environment variables, and child human
  output after a signalled record failure are fixed and regression-tested.
  Compact metadata/storage at `1e1263d` and boxed, spawning-side task-state
  construction at `c3278c4` reduce retained metadata and child scope stack
  pressure without changing observable frame semantics. Observable closure
  commit `181204b` pins restoration of outer task ancestry after a nested
  runtime scope. Exact-clean CI, the contractual benchmark, final compiler
  coverage, and the one-time `96.13/96.90/94.46` re-ratchet are complete;
  their exact counts and hashes are recorded in the current-stage summary
  above. The massive RSS result uses the documented 16 KiB-page escape hatch,
  and the 100,000-sleeper claim is withdrawn. Exact-clean settled-tree CI at
  `77c999d` passed the 45 benchmark-harness tests, 300 CLI tests, 1,150
  compiler-library tests, forced MIR/direct parity in 685.76 seconds, 90 LSP
  tests, 13 extension tests, compiler and 100% LSP coverage, reference and
  stale-syntax integrity, docs, audits, warning-denied Clippy, formatting, and
  hygiene.
- Follow-up found during Phase 5.3: pre-existing `try` propagation inside
  mutable Vec iteration can bypass writeback on both backends; explicit
  `return`, `break`, and `continue` are correct. Track separately from the
  safepoint stage.
- Follow-up found during Phase 5.7: the direct backend rejected `int32 !=
  int32` when one operand came from a function result while MIR accepted the
  expression. The four-worker failure fixture uses equivalent positive
  equality so this unrelated inference/lowering discrepancy does not expand
  the scheduler ticket.
- Phase 5.7 worker-thread spawn and per-worker reactor initialization failures
  are now deterministically fault-injected. Recovery terminalizes pending
  handles and runs cleanup exactly once even when cleanup itself panics. A
  persistent kernel-level `mio::Waker` failure still has no portable recovery
  without a second control primitive; the runtime preserves durable control
  state, retries shutdown notification, reports the fatal error, and keeps the
  ratified no-periodic-tick idle contract.
- Standing rules: behavior-focused coverage only; the one-time Batch 4
  re-ratchet is complete at `96.13/96.90/94.46`; contained semantic gap-fills
  may proceed provisionally, but larger language/runtime questions stop for
  review; reference and parity surfaces move with behavior.

## Batch 3 of 6 (complete)

- Authorized target: close B3.0-a through B3.0-e in separate test-first
  commits, then perform the ratified ADR-0022 capability-syntax migration,
  complete the post-migration reference/parity/coverage/full-CI checkpoint, and
  stop without beginning Phase 5.
- Required order: artifact-cache integrity; heterogeneous `enumerate`/`zip`
  direct lowering; tuple equality; `int64` length-surface unification; the
  diagnostic/comment polish packet; ADR-0022 inventory and migration; final
  checkpoint gates and one coverage re-ratchet.
- Entry state: clean at `4929bab`. Old coverage-only build output was cleaned
  under the repository hygiene rule before the repeated Batch 3 gates.
  Prerequisite hygiene repair `18b7f00`, Part-0 ratification commit `19a10f4`,
  completed B3.0-a commit `6afe47c`, and completed B3.0-b commit `fc22696` are
  isolated. B3.0-c is exact-tree green and isolated in `e05c5e6`; B3.0-d and
  B3.0-e are both exact-tree green and committed in isolation. B3.0 is closed,
  and the first ADR-0022 capability-syntax migration landed across §1-§7.
  A line-by-line audit after checkpoint commit `91e0d5f` found binding
  ADR-0022 gaps; the corrective pass closed them and a fresh exact-tree
  `npm run ci` is green. Batch 3 implementation and verification are complete
  at the requested checkpoint; see `work/2026-07-27-batch3-checkpoint.md`.
  Post-gate coverage cleanup leaves `target/` at 6.8 GiB with 193 GiB free.
  The corrective tree is committed at `1c249ab`; nothing is pushed.
- Batch 2 ADR disposition: ADR-0018, ADR-0019, ADR-0020, ADR-0021, ADR-0023,
  ADR-0024, ADR-0025, ADR-0027, and ADR-0028 are Accepted as implemented.
  ADR-0026 and ADR-0030 become Accepted with their required B3.0 amendments;
  ADR-0029 is Accepted with the B3.0-b function-wide per-loop binding-slot
  isolation amendment. ADR-0031 remains Accepted. Acceptance does not by itself
  claim implementation or gate completion.
- ADR-0022 is Accepted with all ten binding answers and the Range ruling.
  ADR-0009 is superseded in part; ADR-0005, ADR-0006, ADR-0013, ADR-0016,
  and ADR-0017 are amended. The first coordinated source flip and cache-v4
  invalidation are committed. The corrective worktree adds independent
  semantic-interface schema version 2, a complete manifest-v2 preservation
  ledger, strict builtin inventory, and the missed binding semantics.
- B3.0-a implementation: cached native entries now atomically publish a
  platform-native `program`, its `program.sha256`, and a key-bound unique
  `entry-id`; every hit uses bounded no-follow reads and verifies identity,
  digest, regular-file/execute state, size, and native launch shape. Aurora
  materializes the verified bytes into a private per-launch executable and
  invokes it with raw `execv`, preventing both cache-path substitution and the
  macOS ENOEXEC shell fallback. Exact-entry quarantine makes corruption and
  executable-format failure rebuild without racing a concurrent replacement;
  environmental launch failures preserve valid cache state. Private cache-root
  trust, exact `0700` creation under a permissive umask, lease-protected stale
  launch cleanup, owner-aware cache-stage cleanup, and runtime-archive memo
  invalidation are pinned. Cold publication is keyed by the exact archive
  bytes and ordered native link arguments used by the link, so an immediate
  warm run no longer needs the old settle-and-delete workaround. The cache
  format tag is bumped to v3, and behavioral regressions cover the verified
  hit, corruption, non-regular members, cleanup, and preservation paths.
- B3.0-a first-pass evidence: all behavioral, parity, LSP, extension, coverage,
  reference, docs, audit, and Clippy gates passed. Final hygiene exposed
  committed trailing whitespace in `personal/file_ops.au`; prerequisite commit
  `18b7f00` repairs that non-semantic baseline so a pre-commit full gate can
  genuinely pass. The cache review then strengthened launch isolation and the
  regression matrix.
- B3.0-a disposition: complete and exact-tree decision-gate green. `npm run ci`
  passed 265 CLI tests, 897 compiler tests, forced MIR/direct parity, all 70
  LSP tests, all 13 extension tests, reference integrity, docs, audits, Clippy,
  hygiene, and both coverage gates. Compiler coverage is `64,410/67,039`
  lines (96.08%), `4,158/4,295` functions (96.81%), and
  `94,473/100,184` regions (94.30%); LSP coverage remains 100%. No synthetic
  coverage test, exclusion, or coverage-only branch was added. The exact
  network cases also passed outside the restrictive sandbox. Post-gate
  `target/` size was 14 GiB with 157 GiB free, so no cleanup threshold was
  crossed.
- B3.0-b disposition: complete. ADR-0029 now records function-wide target-slot
  isolation for every
  `for` branch, so later loops may reuse source names with different types.
  `zip(numbers, words)` followed by `zip(words, numbers)` reusing
  `number, word` is the mandated acceptance case. The run fixture extension is
  the required red repro: MIR succeeds while the pre-fix forced direct path
  traps with `AU4001`. Fresh typed target slots are implemented for lockstep,
  Range, Queue, Vec, Set, and recursive tuple targets, with iterable evaluation
  outside the target scope and the same physical slot threaded through
  mutable-Vec writeback. All 55 focused MIR tests pass, and forced MIR/direct
  runs match the `enumerate_and_zip`, `tuple_for_pattern_queue`, and
  `vec_borrow_mut_iteration` oracles. Compiler coverage is green at
  `64,476/67,106` lines (96.08%), `4,162/4,299` functions (96.81%), and
  `94,558/100,270` regions (94.30%), with no synthetic coverage test or
  exclusion. The exact `npm run ci` decision gate passed 265 CLI tests, 900
  compiler tests, forced backend parity, all 70 LSP tests, all 13 extension
  tests, both coverage gates, reference integrity, docs, audits, Clippy, and
  hygiene.
- B3.0-c disposition: complete. Structural `==` and `!=` are implemented for
  tuples whose elements are equatable, while tuple ordering remains rejected.
  Symmetric recursive tuple-literal context, exact same-static-type checking,
  non-consuming retained operands, first-false comparison-chain behavior,
  chain mutation-conflict diagnostics, and metadata-independent runtime
  equality all pass focused compiler tests.
  Forced MIR and direct runs match the nested `Option`/`float32`, non-copy
  `(String,)`, generic-float32, `==`/`!=`, once-only, and short-circuit fixture
  oracle. ADR-0026 is Accepted; the Manual, tutorial, maintained example,
  analysis/LSP regression, reference gate, and executable reference are
  aligned. Compiler coverage is green at `64,588/67,216` lines (96.09%),
  `4,176/4,313` functions (96.82%), and `94,731/100,444` regions (94.31%),
  above the frozen `96.07/96.81/94.29` floors with no synthetic test or
  exclusion. The exact `npm run ci` decision gate passed 265 CLI tests, 905
  compiler tests, forced backend parity, all 71 LSP tests, all 13 extension
  tests, both coverage gates, reference integrity, docs, audits, Clippy, and
  hygiene. Post-gate `target/` size is 19 GiB with 149 GiB free, below both
  cleanup thresholds.
- B3.0-d disposition: complete. `String.len()`,
  `String.byte_len()`, `Vec.len()`, `Map.len()`, and `Set.len()` must return
  `int64` consistently with builtin `len(...)`, with compatibility narrowing,
  LSP, examples, tutorials, reference, and resource-cap wording updated in the
  same test-first decision commit. Implementation, focused behavior, both
  backends, all 72 LSP tests at 100% coverage, all 13 extension tests,
  reference/docs gates, and compiler coverage are green. Coverage is
  `64,612/67,239` lines (96.09%), `4,179/4,315` functions (96.85%), and
  `94,761/100,470` regions (94.32%), above the frozen floors without synthetic
  tests or exclusions. An earlier gate attempt passed all code, parity, LSP,
  extension, and coverage stages before finding a line-wrap-sensitive reference
  assertion; that guard is repaired to pin the same normative statement without
  depending on its wrapping. The exact full-repository `npm run ci` decision
  gate is now green end to end: formatting, 916 compiler tests, 265 CLI tests,
  every fixture and package suite, the 516.80-second forced MIR/direct parity
  matrix, all 72 LSP tests, all 13 extension tests, compiler coverage, 100% LSP
  coverage, reference integrity, docs build, npm and Rust audits, Clippy with
  warnings denied, and hygiene.
- B3.0-e closed the four polish items in one isolated commit: clone-safety-aware
  `AU3005` guidance, the dedicated `AU2007` builtin-redefinition code,
  access-kind-specific `AU3002` recovery help, and the stale pre-selector
  comment in `backend_parity.rs`. Its full `npm run ci` was green end to end:
  918 compiler unit tests, 265 CLI tests, the forced parity matrix in 529.30s,
  73 language-server tests, 13 extension tests, coverage at 96.12/96.85/94.32
  against the frozen 96.07/96.81/94.29 floors, reference integrity, the docs
  build, both audits, Clippy with warnings denied, and hygiene.
- Batch 3 corrective disposition: complete and exact-tree gate green. The
  Range modifier ruling, capability-position diagnostics, retained
  shared-match places, mutable-source alias rejection, borrowed-return
  containment docs, semantic-interface schema version 2, retired-syntax gate,
  manifest-v2 preservation ledger, strict builtin inventory, and release notes
  are integrated.
- Migration accounting: 1,260 semantic occurrences and 832 findings are
  recorded with zero unresolved. All 773 pre-flip bare matches were reviewed:
  416 of 417 place matches became `match own` and one fixture was deleted;
  among 356 temporary matches, 22 became `match own` and 334 stayed bare.
  All 468 bare copy parameters were reviewed: 466 remain bare shared, 2 were
  deleted, and none required `own`. Of 19 borrowed returns, 11 copy returns
  became ordinary owned returns; 8 non-copy/unresolved redesign findings were
  resolved through 6 maintained-fixture redesigns and 2 obsolete deletions.
  The final manifest spans 683 files and all 59 migrator tests pass.
- Strict inventory status: zero rendered-signature/metadata mismatches, zero
  missing sibling-retention applications, zero missing rendered builtin
  variants, zero missing structured call shapes, and zero unlinked signatures.
  The retired-syntax gate has no active finding outside the four exact
  retirement fixtures.
- Final verification: one fresh `npm run ci` passes after exposing and fixing
  a TaskGroup named-argument forwarding regression. It includes 23 Aura unit
  tests, 268 CLI tests, 928 compiler tests, the 732.74-second forced
  MIR/direct parity matrix, 79/79 LSP tests, 13/13 extension tests, reference,
  docs, audits, warning-denied Clippy, hygiene, compiler coverage, and 100% LSP
  coverage. The 928/268 suite counts are gate-condition observations from the
  debug profile with Rust tests run single-threaded; alternate invocations can
  report 927 compiler and 265 CLI tests. Compiler coverage is 64,645/67,244
  lines (96.134971%),
  4,200/4,335 functions (96.885813%), and 94,962/100,649 regions
  (94.349671%); final floors are `96.13/96.89/94.35`. No synthetic coverage
  test, exclusion, or coverage-only branch was added.
- Remaining mechanical closeout: post-gate disposable-artifact cleanup and
  the corrective commit. The initial `cargo clean` removed 56.0 GiB and raised
  free space to 199 GiB. Phase 5 remains unstarted.

## Batch 2 Checkpoint (complete)

- Result: Batch 2 of 5 is complete at its requested checkpoint. Phase 5 was not
  started. B2.0 is fully closed, Phase 3 was already complete on entry, Phase
  3.5 is complete through conditional expressions, membership and comparison
  chains, `enumerate`/`zip`, and `len`/`str`, and Phase 4 is complete through
  the `aura run` backend selector, the content-addressed artifact cache, and
  function-level `aura test` discovery. V6 is diagnosed and halved.
- The full checkpoint report is `work/2026-07-25-batch2-checkpoint-report.md`.
  It carries the B2.0 disposition with repro results, per-phase evidence, the
  Provisional ADR list, the retired-hint list, the worker example path, the
  backend-default decision and its measurements, the V6 findings, coverage per
  logical decision commit, the re-ratcheted floors, and the recommended
  movements between Batches 3 to 5.
- Coverage floors are re-ratcheted once, by downward truncation from the final
  measurement, to lines/functions/regions `96.07/96.81/94.29`. The
  language-server gate remains enforced at 100%.
- The Batch 3 entry ruling accepts ADR-0018, ADR-0019, ADR-0020, ADR-0021,
  ADR-0023, ADR-0024, ADR-0025, ADR-0027, and ADR-0028 as implemented.
  ADR-0026, ADR-0029, and ADR-0030 advance with their named B3.0 amendments.
- Accepted checkpoint amendment: ADR-0031 ratifies `mir` as the `aura run`
  default for the edit-run path and retains `auto` as the `aura build` default.
  It explicitly amends the original interim-`auto` roadmap clause without
  weakening forced-backend parity. The blocker for a native `run` default is
  binary size, not correctness or compile time; a direct hello-world executable
  is about 57 MB, so a first launch costs about 0.8s even on a cache hit.
- Corrected checkpoint coverage is 64,409/67,039 lines, 4,158/4,295 functions,
  and 94,472/100,184 regions. The enforced floors remain
  `96.07/96.81/94.29`, with LSP coverage at 100%.

## Batch 2 Implementation Record (completed)

- Result: Phase 3 is complete and committed through `9ff7e82`, including Duration, deterministic and secure Randomness, recursive JSON, Bytes/codecs/SHA-256, assertion statements, and the maintained application-level retry worker. The editor completion/package repair remains committed at `f34b4de`, the ownership tutorial correction at `6665090`, and proposed future capability-syntax ADR-0022 at `929c0b8`; ADR-0022 is not implemented or mixed into Batch 2 semantics. Phase 3.5 newline continuation is complete and decision-gate green: the lexer tracks and validates nested `()`, `[]`, and `{}`, suppresses ordinary continuation layout while retaining physical spans, preserves delimited expression-`match` layout islands, and reports source-related pairing diagnostics. Parser coverage pins multiline signatures, calls, type arguments, grouping, indexing, and collection literals without changing the trailing-comma, backslash, or single-line string/f-string boundaries. Compiler analysis, the language server, and VS Code newline indentation preserve editor behavior across continued and incomplete source. The normative reference, now-Accepted ADR-0025, maintained example/tutorial, executable-reference gate, frozen coverage floors, forced-backend parity, and exact full-CI gate are aligned.
- Current verification: focused Bytes tests, all fixture categories, language-server regression, executable reference integrity, docs build, `git diff --check`, and the complete exact-tree `npm run ci` gate pass. The exact Bytes-era `npm run coverage:compiler:check` gate passes all 251 instrumented CLI tests, 781 compiler library tests, and supporting suites at 60,768/63,252 lines (96.072851451337%), 3,968/4,091 functions (96.993400146663%), and 88,637/94,027 regions (94.267603986089%), above the frozen 96.06/96.79/94.15 floors. The Bytes coverage gap was closed with observable behavior/diagnostic/backend tests plus removal of unreachable validated-decoder and duplicate adapter branches; no synthetic test or coverage exclusion was added. For `assert`, all nine fixture categories, the focused 12-test compiler assertion suite, the CLI behavior matrix, the full 60-test language-server suite, the full 10-test extension suite, the 33-page executable reference-integrity gate, the docs build, the maintained example smoke, and the complete exact-tree `npm run ci` decision gate pass. The focused compiler coverage includes a source-starting lazy-message ownership regression, and editor coverage pins invalid `assert` diagnostics at the keyword. The exact `assert` coverage gate passes all 256 instrumented CLI tests, 795 compiler library tests, and supporting suites at 60,904/63,399 lines (96.06460669726651%), 3,976/4,099 functions (96.99926811417419%), and 88,875/94,275 regions (94.27207637231504%). Its five-line-only first-pass shortfall was closed with observable exported-runtime diagnostic and refcount tests; no synthetic test or coverage exclusion was added. The retry-worker CLI regression passes its exact 15-line oracle through both MIR execution and a forced-direct binary; it pins recovery, terminal `429`, final-attempt no-sleep/no-RNG ordering, explicit timeouts, and seven real loopback requests. Its exact coverage gate passes all 257 instrumented CLI tests, 795 compiler library tests, and supporting suites at 60,904/63,399 lines (96.06460669726651%), 3,976/4,099 functions (96.99926811417419%), and 88,875/94,275 regions (94.27207637231504%). No synthetic-coverage test, exclusion, or coverage-only production restructuring was added. Lightweight reference and diff checks plus the complete exact-tree `npm run ci` decision gate pass.
- The exact newline-continuation compiler coverage gate passes at
  61,133/63,639 lines (96.06216313895567%), 3,992/4,116 functions
  (96.98736637512148%), and 89,215/94,642 regions
  (94.26575938800956%). Its initial four-line floor gap was closed with an
  observable typed-completion recovery test for an escaped quote inside a
  continued call. No synthetic-coverage test, exclusion, or coverage-only
  production restructuring was added.
- Completed tuple ticket (`1380b8d`): the parenthesized fixed-arity tuple implementation,
  Provisional ADR-0026, normative Manual, maintained example/tutorial, and
  executable-reference packet are integrated. Compiler, fixture, exact
  MIR/direct behavior, language-server, reference-integrity, docs-build, and
  diff checks pass. Product-aware boolean/enum/nested-tuple pattern
  exhaustiveness and teaching diagnostics for recursive tuple fields are
  included. Mutable tuple writeback, equality/order, tuple iteration/methods,
  empty tuples, multi-element trailing commas, and dynamic indexing remain
  outside this minimal ticket.
- Exact tuple coverage passes at 62,917/65,489 lines
  (96.072622883232299%), 4,097/4,225 functions (96.970414201183431%), and
  92,077/97,666 regions (94.277435340855575%). The closure uses observable
  diagnostic, exhaustiveness, ownership, import/generic, runtime, and native
  dispatch behavior only. No synthetic-coverage test or exclusion was added;
  parser/checker/MIR-validation-proven defensive branches were collapsed with
  their invariant checks retained and justified in the dated work note.
- Its complete `npm run ci` decision gate passed before commit, including
  forced MIR/direct parity, 100% LSP coverage, compiler coverage, reference,
  docs, audits, Clippy, and hygiene.
- Completed conditional-expression ticket: Python-style `a if condition else b`
  is integrated with now-Accepted ADR-0027, exact-bool checking, contextual arm
  unification, lazy one-arm execution, conservative ownership-state merging,
  MIR/direct lowering, compiler analysis/LSP coverage, fixtures, maintained
  example/tutorial, and the normative reference packet.
- A full-suite audit of the corrected pre-expression ownership replay found and
  closed three reachable regressions before admission: enum-variant and
  module-qualified paths such as `io.Error.NotFound` and `json.Value.Null` were
  rejected as field reads; module-rooted namespace paths were resolved as
  call-argument places; and copy-typed `mut ` arguments lost their
  retained access while the new source-ordered rejection displaced the
  parameter-aware same-level overlap diagnostic. The complete compiler,
  fixture, and 259-test CLI product suites are green on the repaired tree.
- Exact conditional coverage passes at 63,752/66,360 lines
  (96.06992163954189%), 4,137/4,268 functions (96.9306466729147%), and
  93,478/99,158 regions (94.27176828899331%). The closure uses observable
  semantic, diagnostic, ownership, editor, runtime, and backend-parity tests;
  no synthetic test or exclusion was added. Two duplicated ownership walks
  introduced by the replay repair were collapsed with their invariants retained
  and stated in the source.
- Completed B2.0-b generalization: the ratified builtin no-shadowing rule now
  covers every builtin target rather than the four named in the original repro.
  `impl Sized for Vec[int32]` with a `len` method and `impl Probe for String`
  with a `contains` method were reproduced as accepted programs whose trait body
  was silently unreachable on both backends; both are now `AU2006` at check
  time. The message generalizes to "builtin method", the direct backend's
  existing `BuiltinMember` precedence guard already covers every receiver base,
  and a noncolliding trait method still dispatches on a builtin target.
  Fixtures, a cross-target compiler regression, the maintained example and
  smoke oracle, the traits tutorial, the normative rule, the `AU2006` category
  text, the conformance map, and the reference guard ride the same commit. Its
  exact coverage gate passes at 63,750/66,358 lines (96.06980318876398%),
  4,137/4,268 functions (96.9306466729147%), and 93,474/99,154 regions
  (94.27153720475219%); no synthetic coverage test or exclusion was added.
- Completed membership/comparison-chain ticket: `in`, `not in`, and Python-style
  chained comparisons are integrated with now-Accepted ADR-0028. Equality,
  ordering, and membership now share one precedence level and chain rather than
  left-folding; membership delegates to `contains` or `contains_key` over
  `Vec`, `Set`, `Map` keys, and `String`; chains evaluate every operand at most
  once and short-circuit at the first false link. Five `AU2005` hints are
  retired to pass-through acceptance through a new `.accept` fixture marker.
  Fixtures, the maintained example and tutorial, the normative Manual and
  Grammar, the conformance map, verified reference blocks, and the
  language-server bridge ride the same commit. Its exact coverage gate passes at
  64,028/66,649 lines (96.06745787633723%), 4,145/4,281 functions
  (96.82317215603831%), and 93,930/99,630 regions (94.27883167720566%); the
  closure is observable behavior only, and two branches the replay walk could
  never take were removed with their invariants stated in the source.
- Decision condition: the complete `npm run ci` gate is the final pre-commit
  proof for each ticket.
- Completed `enumerate`/`zip` ticket: both are compiler-known `for` iterable
  forms with Provisional ADR-0029, restricted to `Vec[T]` and `Set[T]` operands
  over the bare-loop borrow default. `enumerate` yields `(int64, element)` and
  `zip` stops at the shorter operand. Both lower to one lockstep loop over the
  position-indexed member the ordinary collection loop already uses, so the
  direct backend needed no change. Fixtures, the maintained example and
  tutorial, the normative Statements and Grammar rules, the conformance map, a
  verified reference block, and the language-server bridge ride the same commit.
  Its exact coverage gate passes at 64,313/66,939 lines (96.07702535143937%),
  4,154/4,291 functions (96.80727103239339%), and 94,351/100,058 regions
  (94.29630814127806%); no synthetic coverage test or exclusion was added.
- Completed `len`/`str` ticket: both are maintained builtin functions with
  Provisional ADR-0030. `len` delegates to the value's own `len()` member and
  produces `int64`, with its domain defined by that member rather than a list;
  `str` is total over the renderable surface and produces the same `String` as
  `print` and f-string interpolation. Both lower by delegation, so the direct
  backend needed no change. Both names are now reserved, which is recorded as a
  source-compatibility change on the status page. This completes Phase 3.5. Its
  exact coverage gate passes at 64,387/67,014 lines (96.07992359805414%),
  4,154/4,291 functions (96.80727103239339%), and 94,439/100,150 regions
  (94.29755366949576%); no synthetic coverage test or exclusion was added.
- Completed Phase 4 selector ticket: `aura run --backend mir|direct|auto` is
  implemented, with `direct` reporting build and launch failures rather than
  degrading and `auto` degrading visibly. Both MIR legs of `backend_parity.rs`
  now pass `--backend mir` explicitly. The default stays `mir`: `auto` pays a
  full compile and link on every run, measured at 1.385s against 0.012s for
  hello-world, so the artifact cache is the precondition for changing it. The
  default lives in one named constant with that reasoning attached. Its exact
  coverage gate passes at 64,388/67,014 lines (96.08141582355925%), 4,154/4,291
  functions (96.80727103239339%), and 94,440/100,150 regions
  (94.29855217174239%); no synthetic coverage test or exclusion was added.
- Completed native artifact cache ticket: `aura run`'s direct path is
  content-addressed on compiler version, host target, backend, runtime archive
  content, and the complete lowered program, which already covers the entry
  source and every dependency. The runtime identity is a content hash memoized
  against a cheap stamp, because a direct build can restamp an unchanged
  archive. Entries publish atomically under `programs/`, `AURORA_CACHE_DIR`
  overrides the location, and a cache failure degrades to an ordinary build.
  Benchmarks: MIR 0.00s, cold compile+link 1.31s, warm first touch 0.81s, warm
  resident 0.01s, with a 57 MB hello-world binary. The default stays `mir`; the
  remaining blocker for a native default is binary size, not compile time. Its
  exact coverage gate passes at 64,396/67,022 lines (96.08188356062189%),
  4,155/4,292 functions (96.80801491146319%), and 94,455/100,165 regions
  (94.29940598013278%); no synthetic coverage test or exclusion was added.
- Completed function-level `aura test` discovery: a file declaring
  parameterless `def test_*()` functions reports one result per function through
  a new named-entry path in the MIR runtime, so each test uses the same runtime,
  scheduler, and trap handling as an ordinary run. Helpers and parameterized
  functions are not discovered, a failing assertion reports its message and span
  against the file, and a file declaring no test functions keeps the file-level
  model unchanged. Its exact coverage gate passes at 64,413/67,042 lines
  (96.07857760806658%), 4,158/4,295 functions (96.81024447031432%), and
  94,490/100,201 regions (94.30045608327262%); no synthetic coverage test or
  exclusion was added.
- Completed V6: the direct backend's narrow-width range check was a two-sided
  signed comparison costing five instructions plus a branch on the result of
  every `int32` operation, against `int64`'s single overflow-producing
  instruction plus a branch. The check is now one biased unsigned comparison,
  which took the ten-million iteration loop from 0.0697s to 0.0327s with
  `int64` unchanged at 0.0111s, so the ratio moved from 6.05x to 2.95x. The
  residual gap is the separate branch itself; closing it means giving narrow
  widths their own arithmetic width, which is a backend representation change
  rather than a contained fix. Both numbers are recorded in
  `benchmarks/direct_integer_loops/README.md` and the benchmark is runnable as
  `npm run bench:direct-integer-loops`. Its exact coverage gate passes at
  64,409/67,039 lines (96.07691045510822%), 4,158/4,295 functions (96.81024447031432%),
  and 94,472/100,184 regions (94.29849077697038%); no synthetic coverage test or
  exclusion was added.
- Resume point: Batch 2 is complete at its checkpoint and every landed ticket is
  full-gated. The only untracked files are the two user-created files
  `hello.text` and `personal/file_ops.au`, which were never staged.
- Phase 4 note: the prepared Phase 4/V6 scratch history under `/private/tmp` was
  based on a commit predating every ticket landed in this batch. It was treated
  as reference material rather than applied; the selector, cache, function-test
  discovery, and V6 work in this batch were derived against the current tree and
  gated here.
- Freeze rule: every semantic addition or correction must update its ADR/reference, fixtures, examples, and tutorials in the same logical commit; full `npm run ci` must be green before each commit.
- Coverage rule: floors stayed frozen at lines/functions/regions `96.06/96.79/94.15` through the batch, with behavior-focused tests only. The one downward-truncated re-ratchet at sign-off has been applied, raising them to `96.07/96.81/94.29`.

## Batch 1 Reference-Freeze Checkpoint (historical)

- Result: Batch 1 of 5 is complete at the reference-freeze checkpoint: P1-P5, the shared structured diagnostic system, MIR call/task backtraces, the executable normative Manual, four provisional semantic gap-fill ADRs, and the one-time coverage re-ratchet all landed together. No Batch 2 or Phase 3 work started.
- Checkpoint disposition (historical): ADR-0014 through ADR-0017 are Accepted.
  ADR-0014, ADR-0015, and ADR-0017 were accepted at the Batch 2 entry gate;
  ADR-0016's text was accepted there and its status became Accepted when
  B2.0-a closed the recorded implementation defect.
- Final compiler coverage: 53,769/55,971 lines (96.065820%), 3,324/3,434 functions (96.796738%), and 78,212/83,064 regions (94.158721%). Enforced floors are 96.06% / 96.79% / 94.15% by downward truncation; LSP coverage remains 100%.
- Quality result: the exact full `npm run ci` gate passes, including the 242-test CLI product suite, 552-test compiler library suite, forced MIR/direct parity matrix, LSP and extension suites, instrumented tests, 29-page reference integrity, docs build, audit, Clippy, and hygiene. No synthetic-coverage test or coverage exclusion was added.
- Historical next step (completed): the ADR-0014 through ADR-0017 disposition
  gate was completed before Batch 2 implementation continued. V6 remains in
  Batch 2 with Phase 4 native work; native backtraces remain in Batch 3 frame
  work.

## Previous Completed Milestone

- Result: Phase 1.5 D3 -> D2 -> D4 -> D5 -> D6 is complete with one full-gated decision commit each. D6 is `683b0cf`; the one-time sign-off coverage ratchet is included in the sign-off commit.
- Final compiler coverage: 51,977/54,114 lines (96.050930%), 3,217/3,326 functions (96.722790%), and 75,590/80,357 regions (94.067723%). Enforced floors are now 96.05% / 96.72% / 94.06% using the established two-decimal downward-truncation policy.
- Quality result: no synthetic-coverage test or coverage exclusion was added. All behavior, backend parity, LSP, extension, instrumented, reference, docs, audit, Clippy, and hygiene gates pass.
- Next: investigate V6's int32/int64 direct-loop inversion before or with Phase 4; Phase 2 has not started.

## Earlier Work Record (stale record recovered 2026-07-10)

- Target: Continue the v1 release-readiness push by auditing the current repo state, then fixing the next concrete gap in coverage, CI, docs/book, release packaging, or hygiene; current pass has validated CI/release/docs workflows, fixed package-example lockfile drift in tests, passed the exact full repo `npm run ci` gate after the latest runtime/native-runtime/integer coverage batch, raised package-manager/runtime/native-runtime/integer coverage, trimmed unused runtime task-join scaffolding, fixed exact integer-to-float conversion for saturating wide integer casts, raised LSP statements/functions/lines to enforced 100%, raised the LSP branch gate to 97%, then closed the remaining LSP fallback-analysis branch gaps and raised the LSP coverage gate to enforced 100% across statements/branches/functions/lines, removed the stale tracked LSP coverage summary artifact from the maintained surface, raised the compiler coverage gate from `80/82/80` to `81/83/81`, added public compiler-surface coverage for escape diagnostics, call arity diagnostics, builtin member mutability metadata, stdout-sink wrappers, builtin `from` imports, imported-function entrypoint handling, and lexer escape/f-string/float edge paths, fixed imported parameterized `main` handling so imported functions are not treated as entrypoints, fixed a runtime-scheduler lost-wakeup race exposed by the compiler coverage gate, passed the exact full repo `npm run ci` gate after those fixes, revalidated compiler coverage after the duplicate builtin import regression, added focused builtin file/network/process member metadata and binding coverage, raised `call.rs` to 99.48% line coverage, added no-manifest symlink import escape coverage and parser edge coverage, raised `parser.rs` to 97.98% line coverage, raised `runtime_value.rs` source-type/wrapper coverage to 78.25% line coverage, raised `native_runtime.rs` resource metadata, cleanup/diagnostic guard, direct opcode wrapper, direct resource type-match/metadata, and arithmetic diagnostic coverage to 72.65% line coverage, raised `builtin_modules.rs` to 100% function / 99.92% line coverage and `integer.rs` to 96.50% line coverage, passed the exact full repo `npm run ci` gate with compiler coverage at 81.97% regions / 83.74% functions / 82.16% lines and LSP coverage at enforced 100%, raised the compiler line coverage gate to `82/83/81`, resolved the npm audit transitive `brace-expansion` advisory by updating the lockfile to 5.0.6, passed the exact full repo `npm run ci` gate with compiler coverage at 82.06% regions / 83.74% functions / 82.24% lines and LSP coverage at enforced 100%, completed a Clippy hygiene pass so the repo Clippy command is now quiet, passed full `npm run ci` again with compiler coverage at 82.07% regions / 83.77% functions / 82.22% lines and LSP coverage at enforced 100%, strengthened `check:clippy` to fail on all warnings with `-D warnings`, passed full `npm run ci` again under that stricter lint gate with compiler coverage at 82.06% regions / 83.77% functions / 82.22% lines and LSP coverage at enforced 100%, fixed the incorrect integer-to-nonnumeric runtime cast diagnostic, added direct-codegen named builtin argument binding coverage and runtime-value resource source-type/wrapper coverage, passed `npm run coverage:compiler:check` at 82.20% regions / 84.22% functions / 82.41% lines, and is continuing through compiler coverage/readiness gaps.
- Latest verified status: added semantic member-call success coverage for the maintained String, Vec, Map, Set, Queue, Task, and `fs.File` builtin method surfaces, then regenerated `target/compiler-coverage.json`, `target/compiler-coverage.txt`, `target/compiler-coverage.lcov`, and `target/compiler-coverage-missing.txt`. The focused semantic test, full serialized `npm run coverage:compiler:check`, exact coverage floor report, `cargo fmt --all --check`, and `git diff --check` pass; `coverage:compiler:check` is now raised to lines/functions/regions `96.01/96.71/93.94`. Current exact compiler coverage is 93.9438% regions / 96.7186% functions / 96.0184% lines, with 3 remaining llvm-cov mismatched-function warnings.
- Previous coverage checkpoint: extended MIR runtime collection helper coverage, analysis source-range and enum inference coverage, package resolver coverage, MIR receiver-mutation detection, defensive MIR builtin collection/queue return-type fallbacks, MIR pattern/function/specialization fallbacks, imported namespace aggregate-map fallback resolution, nested borrow-mut vector return redirection, runtime-value WebSocket host-header edge coverage, runtime-value task-group wake-flag registration coverage, lightweight scheduler external-event fd polling coverage, Rustls WebSocket raw-fd/nonblocking coverage, lightweight scheduler completion/waiter/unbounded-wait coverage, runtime-value process-pipe stderr read plus closed-pipe edge coverage, runtime-value HTTP bad-request/root-path/split-body stream coverage, MIR mutable member-call receiver/borrowed-param writeback coverage, MIR channel `get_or*` / internal queue-iteration member helper coverage, maintained direct-codegen object emission for supported IO/process/network examples, direct native runtime queue wrapper coverage for closed/nonblocking/timeout channel paths, direct native runtime diagnostic coverage for invalid arg buffers, cleanup registrations, queue receivers/timeouts, wait timeouts, and task-group receiver types, native-codegen cleanup-place type resolution coverage for receivers, params, locals, inferred values, unknown-field diagnostics, native-codegen opaque file/process member success-surface coverage, runtime-value lightweight scheduler missing-result defensive-exit coverage, MIR task `result_or_none` / `result_or` nonblocking shortcut coverage for ready, cancelled, and already-cancelled runtime paths, MIR process builtin spawn-failure, timeout, and cancelled-context coverage, MIR filesystem/network builtin `Result.Err` coverage for write, directory, open, connect, listener, UDP, and Unix-socket errors, MIR process-child timeout, cancellation, `wait_or_none`, `wait_ok`, kill, terminate, close, and unsupported-method coverage, MIR process-supervisor start/default-argument, duplicate-name, event wait, empty `wait_or_none`, stop/close, and cancellation coverage, and MIR process-supervisor explicit optional-argument plus `wait_or_none(Some(event))` coverage; regenerated `target/compiler-coverage.json`, `target/compiler-coverage.txt`, `target/compiler-coverage.lcov`, and `target/compiler-coverage-missing.txt`; `cargo fmt --all` passes; the focused MIR, runtime-value, native-codegen, and native-runtime tests pass; and `npm run coverage:compiler:check` passes under the newly ratcheted lines/functions/regions gate `94.33/92.53/92.70`. Current exact compiler coverage is 92.7024% regions / 92.5392% functions / 94.3326% lines, with 3 remaining llvm-cov mismatched-function warnings.
- Current coverage checkpoint: extended MIR runtime helper/operator coverage, native-runtime cleanup/task-boundary coverage, Unix/TLS test socket-path hygiene, runtime-value lightweight-scheduler edge coverage, package graph/cache edge coverage, integer helper simplification, analysis recovery no-progress coverage, parser closure flattening and public lex-error coverage, native-codegen fallback/binder coverage, semantic checker coverage across command/byte/header/timeouts/range/type-substitution helpers, native-codegen positional wait, HTTP bytes timeout, match/branch, receiver-helper, scalar-coercion, boolean-lowering edges, runtime-value process-pipe/process-child/HTTP parser/WebSocket host plus package cache-root edge coverage, native-codegen required-argument/helper thunk coverage, direct cleanup-pop diagnostics, builtin enum payload/type helper coverage, semantic builtin argument helper coverage, native-codegen lookup/default helper closure cleanup, runtime mutex/opaque-value guard closure cleanup, MIR stdout-lock poison-recovery closure cleanup, MIR duration diagnostic edge coverage, MIR runtime helper closure cleanup, runtime-value condvar poison-recovery coverage, semantic grouped/negated const-bool loop coverage, semantic task-start callable-resolution coverage, semantic member-object type-resolution coverage, semantic module namespace/task-call wrapper coverage, semantic borrowed-return diagnostic coverage, semantic generic-enum/module-qualified member fallback coverage, semantic direct plural specialization coverage, MIR inference fallback coverage, and semantic builtin member-call success-surface coverage. Regenerated `target/compiler-coverage.json`, `target/compiler-coverage.txt`, `target/compiler-coverage.lcov`, and `target/compiler-coverage-missing.txt`; `cargo fmt --all`, the focused semantic/native-codegen/runtime-value/package/parser/MIR-runtime/MIR inference tests, full `npm run coverage:compiler:check`, and exact coverage floor reports pass; the focused coverage-only `native_runtime_ffi` target passes from the prior resource-wrapper batch; the normal non-coverage `native_runtime_ffi` target compiles to zero tests; and the compiler coverage floor is report-verified at lines/functions/regions `96.01/96.71/93.94`. Current exact compiler coverage is 93.9438% regions / 96.7186% functions / 96.0184% lines, with 3 remaining llvm-cov mismatched-function warnings.

## In Progress

- Stabilize the frozen 0.1 technical-preview surface through compatibility fixes, parity regressions, and preview-user feedback rather than adding more language syntax.
- Keep the categorized example library, manual, and `tutorials/` synchronized with the implemented language subset whenever behavior changes.
- Preserve the Batch 2 checkpoint compiler lines/functions/regions floor at `96.07/96.81/94.29` and the LSP at 100%; add behavior-focused regression tests without treating marginal compiler coverage growth as the product roadmap.

## Todo

- V6 is complete: the narrow-width range check was halved and both measurements are retained in `benchmarks/direct_integer_loops/README.md`. The remaining lead is narrow-width arithmetic in its own width, which is a backend representation change.
- In Batch 3 frame work, add native direct-backend Aurora call-chain and task-ancestry backtraces, then remove the temporary parity normalization for the three supplemental MIR backtrace note families; primary trap code/message/span parity remains mandatory meanwhile.
- In the same Batch 3 frame work, replace the current flat prose call-chain/task-ancestry `notes` entries with explicit structured frame lists in the diagnostic schema and its CLI/LSP bridges.
- Publish signed 0.1 preview archives for every supported platform after the release workflow has passed on each target.
- Use the host-array / tensor-lite layer as the next ML systems milestone, starting with a small dtype and shape surface before tensor or accelerator syntax.
- Expand control-plane serialization and networking from the current honest baseline only when real agent-service examples require nested schemas, pooling, redirects, HTTP/2, or server-side TLS.

## Done

- Fixed the reported VS Code completion crash on temporarily incomplete function parameter annotations: proved the installed 0.1.0 VSIX contained a stale language-server bundle, added a real stdio protocol regression for the exact editing state, bumped and rebuilt extension 0.1.1 with the current compiler-backed server, force-installed that package locally, and documented the exact compiler build, VSIX packaging, installation, and reload workflow. The 57-test LSP suite, 100% LSP coverage gate, extension checks, 9-test extension suite, packaged-server regression, and installed-server regression all pass.
- Corrected `process.Completed.stdout()` and `stderr()` invalid-UTF-8 traps to the runtime I/O band `AU4005` on both MIR and direct backends; the parity-focused regression executes both methods through both products and pins the unchanged primary message.
- Added MIR runtime trap backtraces with function names and source spans, exact TaskGroup child entry and spawn-site ancestry, and once-only diagnostic annotation; focused compiler and `aura run` regressions pin both structured notes and human rendering. Native backtraces remain explicitly deferred to the Batch 3 frame work, with the forced parity gate temporarily ignoring only the three supplemental MIR note prefixes while still requiring exact primary trap parity.
- Closed Batch 1 P3's previously untracked Queue/Task trait-dispatch parity gap through the preferred contained path: MIR runtime member dispatch now falls back to the sema-resolved user trait implementation for non-builtin `Queue[T]` and `Task[T]` member names, while generic run-pass fixtures keep both handles in the forced MIR/direct parity matrix; also recorded P4's intentional parameter-versus-loop `own` spelling asymmetry in ADR-0006 and the normative Manual.
- Completed the July 13 ratified trust-recovery Phase 1 tickets 1-8: recorded accepted ADRs for D1-D13; added forced-MIR/forced-direct runtime-fixture parity with fallback disabled; implemented contextual `None` and unit equality; contained non-copy borrowed returns; replaced dotted semantic places with root-plus-projection paths; isolated direct-runtime call depth, diagnostics, cancellation fallback, and cleanup state per task with a 1,000-suspended-task regression; moved DNS/connect setup to the bounded blocking service under one deadline; removed environment spoofing from `sys.args()`; corrected runtime, architecture, tutorial, example, and manual claims; independently reviewed and fixed nested-`None`, operator-trait borrowed-return, projected-borrow sibling, diagnostic-ordering, and direct generated-stack unwind regressions; measured the Phase 1.5 migration surface and confirmed `own` is cleanly reservable; and passed the exact full `npm run ci` gate at 96.02% compiler lines / 96.90% functions / 93.96% regions and 100% LSP coverage.
- Completed the July 12-13 language-reference pass: established the Manual as the normative Aurora 0.1 specification; added the complete grammar, names/scopes, static semantics, execution model, diagnostics, and conformance chapters; expanded the declaration, ownership, package, CLI, runtime, limit, and API contracts enough to derive a future language book; added the reference-integrity CI gate; corrected stale Learn/tutorial/backend/API claims; fixed parser, checker, bounded-read, metrics, and hover-contract defects exposed by the audit with unit, fixture, MIR, and direct-backend regressions; and passed the exact full `npm run ci` gate at 96.05% compiler lines / 96.87% functions / 93.95% regions and 100% LSP coverage.
- Completed the July 10 directions 1-5 pass: froze the 0.1 syntax and compiler coverage floor; established a relocatable technical-preview release, CI, documentation, and hygiene surface; added parity, fuzz, scheduler-model/stress, sanitizer, audit, and benchmark safety gates; replaced per-request LSP compiler processes with a persistent, cancellable, dependency-aware service and a small lexical recovery layer; added the ML/agent control-plane foundations (`sys`, `path`, JSON/TOML string maps, logs/traces/metrics, HTTPS/chunked HTTP, and `new`/`fmt`/`test` workflows); fixed all parity and TLS-close regressions discovered by the new gates; eliminated llvm-cov ABI map collisions without changing shipped symbols; and passed the exact full `npm run ci` gate at 96.05% compiler lines / 96.86% functions / 93.94% regions and 100% LSP coverage.
- Finished the April 24 book correctness pass: validated the external first-time-developer review against the VitePress book; removed invalid call-site `` / `mut ` from examples; collapsed runnable Aurora calls and collection literals to current single-line syntax; replaced fragile short-form `Some` / `None` examples with qualified `Option.Some` / `Option.None`; rewrote top-level `try` snippets into function or match shapes; corrected `Vec.insert` / `Vec.swap` contracts; expanded install, current limits, homepage positioning, detached-task wording, and syntax-highlighting tags; and reverified with representative snippet run/checks, `npm run docs:build`, `npm audit --audit-level=moderate`, and `git diff --check`.
- Finished the April 24 native trap-parity follow-up: validated the native direct-backend divergences where cleanup traps replaced the original body trap diagnostic and recursive `with` frames unwound one extra cleanup compared with `aura run`; added failing-first CLI regressions; fixed the direct runtime to preserve the primary runtime diagnostic while draining cleanup and to skip the saturated recursion-depth cleanup registration; and reverified with focused direct-backend cleanup/recursion tests, `cargo fmt --all --check`, and `git diff --check`.
- Finished the April 24 VitePress book depth pass: rewrote the Aurora book toward deeper, human-written language documentation with a stronger home page, a project-driven Learn track, expanded ownership/data modeling/collections/concurrency/I/O/package case-study lessons, richer process/log/worker-pool case studies, and contract-style Manual pages for types, functions, classes, ownership, collections, concurrency, I/O, filesystem, networking, process, packages, CLI/tooling, and the full API index; scrubbed the rendered docs and related proposal text of "new language" framing; corrected stale CLI/API claims such as `run-mir` and `WebSocketListener.close`; and reverified with `npm run docs:build`, `npm audit --audit-level=moderate`, `git diff --check`, `cargo run -p aura -- help`, and a local preview smoke test at `http://127.0.0.1:5173/`.
- Finished the April 24 VitePress book pass: added a maintained VitePress documentation book under `docs/` with a use-case-driven Learn track, a Python-docs-style Manual/API reference track for the current Aurora surface, local search, navigation/sidebar configuration, docs scripts, root README guidance, and clean package metadata; pinned the docs toolchain to the VitePress 2 alpha line to avoid the stable Vite/esbuild audit advisory; and reverified with `npm run docs:build`, `npm audit --audit-level=moderate`, `git diff --check`, and a local preview smoke test at `http://127.0.0.1:5173/`.
- Finished the April 24 post-Round-8 regression fix pass: fixed `for value in queue:` without an active `with TaskGroup` so standalone `TaskGroup()` producers are registered with queues they receive as arguments and queue iteration waits for those producers instead of exiting immediately; fixed direct-backend `with` cleanup registrations so mutated resources are refreshed before callee-propagated traps unwind; added focused CLI regressions for both bugs; and reverified with formatting, compiler check, compiler fixtures, the full compiler lib suite, focused queue/cleanup CLI tests, and the full serialized aura CLI suite.
- Finished the April 24 Round 8 review fix pass: validated and fixed native direct-backend `with` cleanup for callee-propagated runtime traps and recursion-limit traps, zero-producer `Queue[T]` iteration shutdown, `process.Completed.stdout_bytes()` / `stderr_bytes()` short-form `Some` / `None` match inference, non-empty `{...}` Set literals with compiler and LSP fallback support, and the maintained set examples/tutorial text; added focused CLI/compiler/LSP regressions; and reverified with compiler fixtures, the compiler lib/integration suite, the serialized aura CLI suite, LSP tests/checks, `cargo fmt --all --check`, `git diff --check`, and Clippy correctness.
- Finished the April 23 Round 7 review fix pass: validated and fixed the remaining reported defects around direct-backend `with` cleanup on runtime traps, clean-return queue iteration wakeups, annotated empty Set literals, direct recursion diagnostics, streamed `aura run` stdout before external termination, and raw `process.Completed.stdout_bytes()` / `stderr_bytes()` access; added focused CLI/compiler/LSP regressions; updated maintained process examples/tutorials/README text and LSP fallback metadata; and reverified with compiler fixtures, the compiler lib suite, the serialized aura CLI suite, LSP tests, `cargo check`, `git diff --check`, and Clippy correctness.
- Finished the April 23 audit hardening fix pass: fixed the reported `self` non-copy move hole, runtime-error `with` cleanup skipping, duplicate supervisor child leak, stdin editor-analysis lockfile writes, stale LSP completion test, under-validated `main` return types, MIR typed-empty collection lowering, and hyphenated package-name mismatch; added focused regressions for each defect; hardened bounded runtime reads and package git command execution with timed, drained output collection; refreshed affected examples, tutorials, LSP fallback metadata, VS Code syntax coverage, and package lock state; and reverified with focused compiler/CLI/runtime tests, `cargo test -p aura -- --nocapture`, Node LSP/extension tests and checks, `git diff --check`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 22 latest-review follow-up pass 3: added failing-first CLI and runtime regressions for queue iteration hanging when a sibling task panics without closing the queue; taught queue-iteration receives in both MIR and direct runtimes to wake on unobserved sibling task failure through the active `TaskGroup`; added group-failure wake-flag tracking plus a missed-wake fix in the lightweight scheduler wait-registration path; tightened task-group cleanup probing so fresh child spawns settle before blocked cleanup waits are cancelled; aligned MIR/native no-timeout `Queue.get_or*` and `Task.result_or*` helpers with the documented immediate fallback semantics without scheduler-yield side effects; fixed direct-runtime fallback-value handling to clone defaults instead of consuming them; manually revalidated the new sibling-panic repro on both `aura run` and a direct-built binary; reverified the compiler fixture suite with `cargo test -p aurora-compiler --test fixtures -- --nocapture`; and reverified the focused runtime regression with `cargo test -p aurora-compiler queue_iteration_wait_wakes_for_unobserved_task_group_failure -- --nocapture`.
- Finished the April 22 latest-review follow-up pass 2: added failing-first coverage for the remaining match-expression move-tracking false positive, the queue-iteration cancellation hang, and swap out-of-bounds message parity; fixed match-expression value-scrutinee first use without reintroducing move-state leaks; removed the abandoned scope-wide task-group cancellation rewrite; added a targeted queue-iteration receive path that threads the active `TaskGroup` cancellation into `for value in queue:` for both MIR and direct backends; aligned the direct-runtime swap diagnostic with MIR; narrowed the concurrency/resource/current-surface tutorials back to the exact supported cancellation behavior; and reverified the tree with `cargo fmt --all`, the targeted CLI regressions, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, and `cargo test -p aura --test cli -- --nocapture`.
- Finished the April 22 latest-review fix pass: validated the new review cluster around native bare-`None` coercions, wait-site inconsistencies, checker false positives, and `fs.write_bytes([], ...)` parity; fixed direct-backend coercion for bare `None` across collection literals/member calls and nested `Option[...]` class fields; aligned MIR/native no-timeout `Queue` and `Task` helper semantics with the documented immediate non-blocking behavior; made cancelled `sleep(...)` wake so tasks can observe `cancelled()`, made `wait_any([])` return `TimedOut` immediately, accepted empty byte vectors in `fs.write_bytes(...)`, removed the move-type collection-literal checker false positive, refreshed the maintained concurrency examples/tutorials, aligned the queue fairness CLI regression with the new `Task.result_or(..., timeout=...)` contract, and reverified the tree with `cargo fmt --all`, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, and `cargo test -p aura --test cli -- --nocapture`.
- Finished the April 21-22 live eleven-defect fix pass: closed the still-live `/tmp/aurora_review` issues around unannotated `Queue.get_or_none()` / `Task.result_or_none()` match inference, `aura run` partial-stdout loss on runtime errors, `TaskGroup` scope shutdown semantics, cooperative cancellation in CPU-bound lightweight tasks, surfaced task failures via `TaskResult.Error(...)`, literal-`match` and `with` move-state leaks, the self-receiver bound-call false positive, and `Vec.insert(...)` / `Vec.swap(...)` out-of-bounds silent no-ops; added and updated failing-first compiler/runtime/CLI regressions plus maintained examples/tutorial text for the changed structured-concurrency behavior; repaired the broad compiler test harnesses that were still overflowing default test-thread stacks; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, `cargo test -p aurora-compiler --lib -- --nocapture`, and `cargo test -p aura --test cli -- --nocapture`.
- Finished the April 21 twelve-finding fix audit: rechecked the specific review-finding list covering MIR fs read caps, compiler-backed LSP invalidation, UNC file URI parsing, architecture-doc concurrency syntax, stale `match mut` bindings, builtin module enum identity, malformed HTTP listener recovery, Unix/non-Unix TLS listener backlog handling, and the non-Unix TLS wait policy; confirmed the current tree already contains those fixes; and reverified them through targeted Rust and Node regression tests plus direct source inspection, so no additional production-code changes were required for that finding set.
- Finished the April 21 Claude review validation pass: replayed the external harness repros under `/tmp/aurora_review` against the current `target/debug/aura`, confirmed that the headline correctness bugs around unannotated `Option` matches from `Queue.get_or_none`, buffered stdout loss on `aura run` runtime errors, missing `TaskGroup` join-at-scope-exit behavior, non-firing `cancelled()` in CPU-bound loops, unrecoverable task runtime failures, literal-`match` and `with` move-tracking leaks, `run`/direct-backend divergence caused by the ownership hole, the self-receiver bound-call false positive, and silent `Vec.insert`/`Vec.swap` OOB no-ops still reproduce, and recorded that the task-failure claim is slightly overstated because the Aurora program terminates with a surfaced diagnostic rather than a host-process panic. 
- Finished the April 21 follow-up fix pass 3: added failing-first regressions for builtin module enum identity across `aura run` and direct-built binaries plus the non-Unix TLS listener wait policy; preserved qualified builtin enum names through sema canonicalization, MIR constructor lowering, and MIR match-pattern lowering so `io.Error.*` / `process.Error.*` round-trip consistently through construction, printing, equality, and `match`; replaced the non-Unix TLS listener fixed-sleep wait path with a readiness wait backed by `mio` plus a shared timeout-policy helper that blocks until real listener progress when the handshake queue is empty; and reverified the tree with `cargo fmt --all`, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, and `cargo test -p aura --test cli -- --nocapture`.
- Finished the April 21 post-follow-up review: re-read the latest checker/runtime diffs after the follow-up fix pass, replayed targeted repros for builtin module enum constructors and their interaction with matching/equality, inspected the non-Unix TLS accept helper added by the shared backlog refactor, and recorded the remaining constructor-identity and non-Unix polling issues.
- Finished the April 21 follow-up fix pass 2: added failing-first regressions for stale `match mut` binding use after `mut ` helper calls and module-qualified builtin `io.Error` constructors; invalidated overlapping `match mut` bindings from actual `mut ` call sites without reintroducing dead-branch writeback fallout; unified builtin module-type canonicalization so `io.Error.NotFound` type-checks as `io.Error`; reworked `TlsListenerValue::accept()` onto the shared pending-handshake queue so the non-Unix branch no longer keeps the old inline one-peer-at-a-time handshake path; and reverified the tree with `cargo fmt --all`, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, and `cargo test -p aura --test cli -- --nocapture`.
- Finished the April 21 follow-up review: re-read the current post-fix checker/runtime tree, replayed targeted repros against `match mut` mutation-through-call semantics and module-qualified builtin enum constructors, confirmed broad compiler and CLI suites still pass, and recorded the remaining uncovered issues around stale bindings after `mut ` calls, module-qualified enum type identity, and the still-inline non-Unix TLS accept path.
- Finished the April 21 review-finding fix pass: added failing-first compiler/runtime/CLI regressions for the remaining stale `match mut`, TLS accept backlog, and malformed HTTP listener defects; fixed checker-side stale pattern-binding invalidation without regressing dead branches; reworked the TLS listener accept loop so queued stalled peers no longer linearly delay the next valid client while preserving in-runtime scheduler progress; made malformed HTTP requests return `400 Bad Request` and continue the listener loop; updated the supporting compiler test helpers and fixture expectations; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, `cargo test -p aura --test cli -- --nocapture`, plus direct targeted example/runtime regressions.
- Finished the April 21 post-fix review of the fifth-pass change set: re-read the landed compiler/runtime diffs, replayed targeted adversarial repros against the current tree, and recorded the remaining semantic and listener-path issues that still survive after the fifth-pass fixes.
- Finished the April 21 fifth-review fix pass: validated the fifth-pass external review, added failing-first regressions, fixed the broad `match mut` dead-branch writeback regression, corrected direct-backend bare `None` enum emission, made TLS and HTTP listeners continue past per-connection handshake/request failures, raised the maintained `read_all` ceiling to `64 MiB`, added `431` handling, enabled `Self` in trait/impl parameter positions, restored user-class precedence over builtin variant names, added `io.Error.Cancelled` plus explicit `io.Error.Closed`/`Cancelled` runtime mapping, hardened websocket transport fallback errors, updated the maintained traits and I/O tutorials plus examples, and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, `cargo test -p aura --test cli -- --nocapture`, and `./target/debug/aura run examples/traits/self_parameters.au`.

- Finished the April 20 fourth-review fix pass: closed the fourth-pass externally reviewed defects across recursive indirect enum construction, nested generic trait-bound direct-backend dispatch, `match mut` writeback and nested-aliasing holes, managed `with` resource field moves, MIR/runtime filesystem read-cap parity, supertrait syntax and inherited bounds, `Option.Some(...)` inference, expression-form `match` positions, unreachable enum-arm detection, nested missing-pattern diagnostics, TLS handshake deadline handling, oversized HTTP request `413` responses, websocket error-kind preservation, compiler-backed LSP cache invalidation, and UNC `file://` URI handling; added failing-first compiler, runtime, CLI, and LSP regressions for those paths; aligned the maintained architecture docs, tutorials, and examples with the fixed surface; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler --lib -- --nocapture`, `cargo test -p aurora-compiler --test fixtures -- --nocapture`, `cargo test -p aura --test cli -- --nocapture`, `npm test`, `npm run check`, and direct `aura run` smoke runs for the new examples.

- Finished the April 20 third-review fix pass: closed the externally reviewed third-pass correctness and soundness defects around `match mut` rebinding, nested borrow-vs-move aliasing through sibling expression ordering, live Unix-socket listener hijacking, inferred generic-class field arithmetic in MIR, native trait-specialization order dependence, nested-pattern exhaustiveness over the same outer variant, annotation-directed `Option.None` resolution, direct-backend filesystem read caps, TLS server handshake completion and timeout handling, stricter HTTP header validation, and supervisor restart-loop throttling; added failing-first checker/runtime/CLI regressions for the new ownership, match, process, TLS, HTTP, direct-backend, and inference cases; updated the maintained I/O tutorial for the restart-backoff and TLS handshake behavior; and reverified the final tree with `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.

- Finished the April 20 second-review fix pass: closed the second-pass externally reviewed correctness and soundness defects around inferred enum `match` fallthrough, nested consume-plus-borrow aliasing, reassignment during borrowed iteration, `net.unix_listen(...)` regular-file clobbering, generic-class field arithmetic in MIR, namespace-qualified enum variants, imported-module syntax diagnostic attribution, `match mut` writeback, duplicate nested match-arm discrimination, direct-backend multi-payload enum support, finite-only float parsing, builtin shadowing rejection, and the lightweight-task stack/runtime regressions that were crashing websocket and Unix/TLS examples; restored the maintained 256-frame recursion contract; updated the affected runtime/sema/analysis/MIR/native tests and maintained diagnostics; and reverified the final tree with `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 20 review-hardening fix pass: closed the externally reviewed ownership, concurrency, parser/runtime, HTTP/process, and I/O/networking defects end to end; added failing-first regressions for consume-plus-borrow call arguments, borrowed-vector iteration mutation, explicit non-copy vector indexing, overlapping trait impl ambiguity, large left-associative expression chains, blocked `TaskGroup` cleanup, sleep cancellation propagation, queue fairness, HTTP header injection, large TCP/HTTP payload handling, `read_all` caps, filesystem directory error precision, websocket runtime stability, recursion-depth diagnostics, and the updated compiler-bridge editor surface; fixed the checker, parser, MIR runtime, direct runtime, native backend, lexer, test harness stack sizing, websocket handshake path, and docs/examples to match the hardened semantics; and reverified the finished tree with `cargo fmt --all`, targeted compiler/CLI regressions, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 process-supervisor pass: added the maintained `process.supervisor()` surface with `process.Supervisor`, `process.RestartPolicy`, `process.SupervisorEvent`, and `process.SupervisorWait`; implemented named supervised child processes with restart policy, restart backoff, max-restart limits, and group-aware shutdown across the shared runtime, MIR runtime, direct runtime, and direct backend; added compiler typing plus direct regressions for supervised restart and stop behavior; updated CLI direct-backend product coverage, fallback LSP metadata/completions/return-type inference, the maintained supervisor example, the I/O and current-surface tutorials, the root and CLI READMEs, and the examples index; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler --test process`, `cargo test -p aura direct_backend_build_supports_process_module_surface -- --nocapture`, `npm run test:lsp`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 process-groups pass: added maintained `group=true` support to `process.start(...)` and `process.run(...)`; implemented Unix process-group creation plus group-aware `kill()`, `terminate()`, and `close()` semantics in the shared runtime child lifecycle; made grouped child cleanup wait for the full process group to disappear before returning; added a regression that proves grouped `close()` tears down descendant processes rather than only the leader PID; threaded the new argument through MIR execution, direct native execution, direct-codegen lowering, CLI integration coverage, examples, tutorials, root/example READMEs, and fallback LSP metadata/tests; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 Pythonic convenience API pass: added `Queue.get_or_none(...)`, `Queue.get_or(...)`, `Task.result_or_none(...)`, `Task.result_or(...)`, `process.Child.wait_or_none(...)`, `process.Child.wait_ok(...)`, and `process.Completed.check()` across the checker, MIR runtime, direct runtime, native backend, compiler analysis/completions, and fallback LSP analysis; added failing-first compiler fixtures and process regressions for the new queue/task/process helpers; rewrote the maintained concurrency and process examples plus the concurrency/I/O tutorials to lead with the new linear helper style while keeping `QueueReceive`, `TaskResult`, and `process.Wait` documented as the lower-level surface; updated the root README and examples index to describe the new default queue/task/process style; and reverified the finished tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 process-module pass: added the first maintained shell-free `process` builtin module with `process.start(...)`, `process.run(...)`, `process.inherit()`, `process.null()`, `process.pipe()`, `process.Child`, `process.Pipe`, `process.ExitStatus`, `process.Completed`, and `process.Error`; implemented timeout-aware child waiting plus explicit `terminate()` / `kill()` / `close()` behavior across the checker, MIR runtime, direct runtime, native backend, compiler-owned analysis/completions, and LSP fallback analysis; added maintained compiler, CLI, example-smoke, and LSP regression coverage for subprocess execution, stdio piping, and builtin member completions; added the runnable `examples/io/process_run.au` and `examples/io/process_pipes.au` examples; aligned the root README, CLI README, examples index, and tutorials with the new process surface and its current limits; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 concurrency Pythonic surface reset: removed the legacy `spawn`, `spawn detached`, `select`, `after(...)`, `queue()`, `queue[T]()`, and `tasks()` surface; kept only the structured concurrency model centered on `Queue[T]()`, `TaskGroup()`, `TaskGroup.start(...)`, `TaskGroup.start_soon(...)`, `Task.result(timeout=...)`, `wait_any(...)`, and `wait_all(...)`; renamed and rewired maintained fixtures/examples around the new queue/task semantics; updated the fallback LSP analysis metadata and payload inference to the maintained `QueueReceive`, `TaskResult`, `WaitAny`, and `WaitAll` enums; aligned tutorials, READMEs, and VS Code syntax/snippets with the new surface; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Refined the April 19 ML systems roadmap so the near-term plan now centers Aurora on subprocess supervision, structured serialization, observability, and a host-side array or tensor-lite layer for NumPy-style local data processing, while scoping full tensor/device placement and distributed runtime support as explicit later phases in the same roadmap.
- Finished the April 19 ML systems roadmap pass: added `docs/ml_systems_support_plan.md` as a forward-looking plan for making Aurora a strong ML systems language without replacing Python training workflows, covering process supervision, tensor/device handle interop, zero-copy/shared-memory transport, structured serialization, observability, cross-cutting compiler/runtime implications, and staged delivery milestones; linked the roadmap from the root README; verified the new markdown links; and recorded the pass in the dated work log.
- Finished the April 19 async file I/O and bounded-queues pass: added bounded `Queue[T]` capacity support with scheduler-aware blocked send wakeups, cancellation-aware `SendError.Cancelled(value)`, and shared send-readiness handling across the MIR runtime and direct backend; routed maintained file I/O through the lightweight-task scheduler via the blocking-I/O pool so ordinary file reads and writes no longer pin a scheduler task on a blocking host thread; added maintained regressions for bounded-queue blocking and scheduler-friendly FIFO file reads; added the runnable `examples/concurrency/bounded_queue.au` example; aligned fixtures, examples, tutorials, root/CLI READMEs, compiler smoke coverage, and LSP fallback docs with `queue(capacity=...)`, `SendError.Cancelled(...)`, and scheduler-aware file I/O; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 lightweight-tasks runtime pass: replaced MIR and direct-runtime task spawning so Aurora tasks now run on the shared coroutine scheduler instead of one OS thread per task; added scheduler task-local cancellation propagation for the direct runtime; changed the direct native main wrapper to execute through `aurora_direct_run_root(...)`; added maintained regressions for thousands-of-tasks thread-count scaling and preserved recursion-limit diagnostics on the coroutine runtime; aligned the maintained concurrency and I/O tutorials plus example index with the scheduler-backed task model; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 async scheduler and HTTP runtime pass: replaced the remaining `sleep(...)`, queue-wait, and `select` polling paths with the shared runtime scheduler; routed the maintained HTTP listener/request helpers onto the same nonblocking evented runtime as the rest of networking; fixed select-cancellation semantics in both MIR and direct runtime paths so cancelled waits fall through promptly instead of waiting for timeout arms; added targeted regressions for scheduler wakeups, nonblocking HTTP resource invariants, and select cancellation; aligned the maintained tutorials/README surface with the new scheduler-backed model; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, `npm run check:extension`, and `cargo clippy -p aurora-compiler -p aura -- -D clippy::correctness`.
- Finished the April 19 architecture documentation pass: reviewed the full Aurora monorepo and added a new `architecture_docs/` documentation set covering the system architecture, AST/source model, lexer, parser, semantic analysis, MIR, MIR runtime, native backend/runtime, package system, CLI/build tooling, editor tooling, testing strategy, and an end-to-end walkthrough, including Mermaid diagrams plus standalone SVGs for the compiler pipeline, runtime layering, and tooling flow; linked the new docs from the root README; verified the markdown links in the new docs; and recorded the work in the dated work log.
- Finished the April 19 evented networking runtime pass: converted the maintained socket-backed runtime onto nonblocking descriptors plus poll-driven waits, fixed websocket accept/connect handshake resumption on nonblocking sockets, made timeout handling honor the caller’s full budget instead of a single poll slice, tightened TLS socket polling so handshake progress can wait on both read and write readiness, added direct runtime regressions for nonblocking descriptor invariants plus timeout-budget coverage, updated the maintained READMEs/tutorials to describe the new socket model accurately, and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, and `npm run check:extension`.
- Finished the April 18-19 networking expansion/stabilization pass: expanded the maintained `io`/`fs`/`net` surface from the initial blocking file/TCP subset to the richer blocking runtime that now covers byte-oriented file and socket I/O, timeout-aware TCP/Unix/TLS/HTTP/WebSocket operations, UDP, Unix sockets, and TLS; filled the compiler-backed builtin-module completion gap for the new resource members; made the maintained Unix/TLS example self-contained with embedded certificate material; stabilized network example timeouts plus the WebSocket accept/handshake path under full-suite load; removed the dead networking helper/runtime warning leftovers; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, and `npm run check:extension`.
- Finished the April 18 I/O and network surface pass: added maintained builtin `io`, `fs`, and `net` modules; introduced `io.Error`, `fs.File`, `net.TcpListener`, and `net.TcpStream`; wired blocking file and TCP I/O through the checker, MIR runtime, native direct backend, public compiler entrypoints, CLI product tests, and language-server analysis/completion surface; added maintained examples plus the `19-io-and-networking` tutorial chapter; aligned root/CLI/tutorial/example documentation with the implemented builtin I/O model; and reverified the final tree with `cargo fmt --all`, `cargo test -p aurora-compiler`, `cargo test -p aura`, `npm run test:lsp`, and `npm run check:extension`.
- Finished the April 18 concurrency surface removal pass: removed the remaining compatibility-era concurrency spellings from the checker and tooling so `Channel[T]`, `channel()`, `task_group()`, `Task.join()`, `Queue.send()/recv()/clone()`, `Task.clone()`, and `TaskGroup.spawn(...)` are no longer part of the maintained surface at all; converted stale positive fixtures, examples, tutorials, and LSP fallback tests to the queue/task-only model; renamed maintained example and fixture stems from `channel*`/`channels*` to `queue*`/`queues*` everywhere except explicit negative regressions for removed aliases; and reverified the compiler, CLI, LSP, and extension checks on the final queue/task-only tree.
- Finished the April 17 concurrency ergonomics redesign: added the maintained `Queue[T]` / `queue()` / `tasks()` / `Task.result()` / `TaskGroup.start(...)` surface as compatibility-first aliases over the existing concurrency runtime, made queue/task handles cheap copy-like values, added `Queue.get(timeout=...)`, updated MIR/direct backend/tooling support for `put` / `get` / `result` / `start`, refreshed the maintained concurrency examples and tutorials to present queues and structured tasks as the primary model, updated regression fixtures and compiler/LSP coverage for the new surface, and verified the full compiler suite, CLI suite, LSP suite, and extension build checks.
- Finished the April 17 review hardening follow-up pass 4: hardened git checkouts against hostile symlinked contents and interactive credential prompts, tightened cached git revision reads with `O_NOFOLLOW`, replaced the `IntegerValue` inherent `cmp` with canonical `Ord`/`PartialOrd` implementations, capped hostile embedded MIR complexity, diagnosed MIR/deadline `Instant::checked_add` overflow instead of firing immediately, added the empty field-path direct-backend guard, documented task-group/refcount runtime invariants, removed the redundant direct-runtime acquire fence, and verified the final tree with the compiler suite, CLI suite, LSP suite, extension checks, and a clippy correctness pass.
- Finished the April 17 review hardening follow-up pass 3: marked the remaining raw-pointer FFI entrypoints as `unsafe extern "C"` with safety docs, tightened git revision validation to reject overly-short hashes, added embedded-input length caps for `aurora_native_run`, replaced the remaining `is_some_and(...).unwrap()` MIR-lowering sites, revalidated that nested pattern payload arity is already rejected recursively by maintained checker tests, and verified the result with the compiler suite, CLI suite, LSP suite, extension build checks, and a clippy correctness pass.
- Finished the April 17 review hardening follow-up pass 2: extended parser recursion guards to statements, types, patterns, and f-string interpolation parsing; kept the recursion cap at `128` intentionally because higher values hit the host stack before Aurora can diagnose; cleaned up partially-written unique temp files on failure; made runtime task/channel locks poison-tolerant; hardened git revision validation plus temp-path generation and Windows replace semantics for atomic cache/lockfile writes; added opaque refcount overflow/underflow guards; fixed float division-by-zero diagnostics to match modulo; restored thread SIGPIPE masks on broken-pipe paths without regressing clean built-binary exits; removed the remaining production `unwrap` / `expect` sites in `sema.rs` / `integer.rs`; and added regression coverage plus maintained-fixture updates for the confirmed issues.
- Finished the April 17 review hardening follow-up: added parser recursion limits and f-string nesting limits, removed the negative-literal inference panic path, validated git branch/tag selectors, made lockfile and git revision-cache writes atomic, made MIR stdout handling poison-tolerant, diagnosed float modulo by zero, switched exact integer-to-float casts to reject silent precision loss, replaced the direct runtime's Arc-based opaque retain/release with explicit atomic reference counting, tightened several checker internal-error paths, added a defensive positional/named call-binding guard, hardened malformed builtin `MapEntry` field typing, and added regression coverage across compiler, package, MIR runtime, native runtime, CLI, and editor-facing tests.

- Finished the April 16 runtime/package hardening pass: replaced direct-runtime opaque allocations with explicit retain/release support, fixed spawned-argument ownership through native thunks, hardened direct-runtime stdout handling so built binaries exit cleanly on broken pipes without global `SIGPIPE` suppression, removed unsafe borrowed UTF-8 decoding in the direct runtime, tightened MIR runtime panic/error paths, hardened git dependency resolution (`--` separation, source validation, hashed cache keys, revision markers, lockfile/version/package validation, dependency-count caps), fixed canonical import-root checks, and added regression coverage across compiler, CLI, and runtime tests.
- Removed the redundant compiler-side `run_*_via_mir` aliases left behind after interpreter removal, collapsed internal coverage onto the canonical `run_*` entrypoints plus explicit `lower_*_to_mir + run_mir(...)` where MIR-level coverage is still intentional, renamed stale CLI tests that still implied a removed `run-mir` path, and hardened git dependency checkout caching to fall back to a temp cache root when a home-directory cache is unavailable.
- Removed the tree-walk interpreter from the maintained Aurora architecture: extracted shared runtime state into `runtime_value.rs`, switched the public `run` path onto MIR, removed the `run-mir` CLI command, deleted `interpreter.rs` / `interpreter_tests.rs`, added dedicated runtime-value coverage, and aligned READMEs/tutorials/tests/work logs with the reduced two-path model (`run` via MIR, `build` via native codegen).
- Finished the April 16 major language-surface pass across compiler/runtime/tooling/docs: richer enum `match` with expression-form and nested/multi-payload patterns, float literal match cases, default trait methods, ordering traits for `<`/`<=`/`>`/`>=`, explicit borrow labels such as `` for borrowed-return lifetimes, positional class constructors, keyword enum payload arguments, bare built-in enum constructors with expected type, explicit `channel[T]()` construction, expanded `spawn`/`TaskGroup.spawn(...)` targets, and an `auto` build fallback that preserves native build coverage for richer source programs.

- Added `aura deps update` and `aura deps update <package>` so branch/tag/default-main git dependencies can be refreshed without deleting `Aurora.lock`, with direct compiler coverage, CLI product tests, and maintained README/tutorial updates for the new workflow.
- Extended the Aurora package system from local path dependencies to git-backed dependencies, with manifest support for `git`, `rev`, `tag`, and `branch`, default `main` branch fallback, lockfile-pinned git revisions, compiler/CLI/LSP regression coverage, and README/tutorial updates for the maintained package surface.
- Implemented the first Aurora package-system milestone with `Aurora.toml` manifests, manifest-rooted `src/` packages, local path dependencies, workspace roots, manifest-aware CLI/compiler entrypoints, relative `Aurora.lock` generation, maintained package examples, tutorial/README coverage, compiler/CLI regression tests, and an LSP compiler-bridge regression for package-aware analysis/completion.
- Added another direct checker/interpreter sweep covering empty-`select` validation, direct index/member assignment helper branches, runtime `main` parameter rejection, extra inferred builtin member types, invalid runtime `select` arms, additional loop-control branches, float-to-int cast overflow edges, map render/equality edges, and current-module namespace fallback resolution; verified the new focused tests and restarted a fresh full `cargo llvm-cov` summary from the updated source tree.
- Extended compiler-backed `analyze` / `complete` and the LSP from local-module behavior to fully correct cross-file definitions for imported items, including fields, methods, variants, and trait methods that resolve back to their defining source files.
- Narrowed the JS fallback so hover and go-to-definition now stay compiler-owned whenever compiler analysis succeeds, using JS only when the compiler cannot analyze the buffer.
- Extended the maintained trait surface with specialized generic trait bounds and operator traits across the checker, interpreter, MIR/runtime, direct builds, examples, tutorials, CLI coverage, and compiler/LSP regression suites.
- Raised the enforced coverage gates after new compiler and LSP regression/unit coverage: compiler to lines `67%`, functions `74%`, regions `67%`; language server to statements `89%`, branches `78%`, functions `98%`, and lines `89%`.
- Raised the enforced coverage gates again after additional fallback-helper, bridge, lexer, call-surface, and AST coverage work: compiler to lines `68%`, functions `74%`, regions `68%`; language server to statements `91%`, branches `82%`, functions `98%`, and lines `91%`.
- Raised the enforced coverage gates again after the April 14 compiler/runtime/helper sweep: compiler to lines `77%`, functions `78%`, regions `78%`; language server to statements `91%`, branches `83%`, functions `100%`, and lines `91%`.
- Expanded direct compiler coverage across `native_codegen`, `native_runtime`, `mir_runtime`, `analysis`, `sema`, `interpreter`, and runnable maintained examples, moving compiler coverage to roughly `77.47%` lines / `78.15%` functions / `78.99%` regions and keeping the LSP at `91.17%` statements / `83.69%` branches / `100%` functions / `91.17%` lines.
- Added another focused compiler helper sweep over `interpreter`, `mir_runtime`, `sema`, and `native_runtime`, moving compiler coverage to `82.45%` lines / `81.35%` functions / `83.69%` regions while the language server sits at `91.49%` statements / `84.08%` branches / `100%` functions / `91.49%` lines.
- Added another April 14 helper sweep across `analysis`, `native_codegen`, `sema`, and `interpreter`, moving compiler coverage to `83.75%` lines / `82.71%` functions / `84.95%` regions and the language server to `93.34%` statements / `86.41%` branches / `100%` functions / `93.34%` lines.
- Completed a fresh full compiler coverage run and raised the measured compiler baseline for that pass to `84.10%` lines / `82.87%` functions / `85.23%` regions while the language server moved to `94.55%` statements / `87.64%` branches / `100%` functions / `94.55%` lines.
- Added another helper sweep across `diag`, `integer`, `ast`, `call`, `lexer`, `parser`, `sema`, and `native_runtime`, then reran compiler coverage to move the compiler to `84.69%` lines / `83.37%` functions / `85.81%` regions.
- Fixed the latest `mir_runtime` helper-test imports, verified the new targeted `mir_runtime` and `sema` tests, and resumed the compiler coverage push from a green baseline.
- Added another dense validation/helper sweep in `sema`, `mir_runtime`, and `native_codegen`, then reran the full compiler coverage pass to move the compiler to `85.02%` lines / `83.54%` functions / `86.07%` regions.
- Added another helper sweep in `lib`, `lexer`, and `interpreter`, then reran the full compiler coverage pass to move the compiler to `85.11%` lines / `83.58%` functions / `86.15%` regions.
- Added another helper sweep in `interpreter`, `native_runtime`, and `native_codegen`, then reran the full compiler coverage pass to move the compiler to `85.49%` lines / `83.84%` functions / `86.54%` regions while the remaining gap stayed concentrated in `sema`, `interpreter`, `native_codegen`, and `mir_runtime`.
- Added another helper sweep in `sema`, `mir_runtime`, `interpreter`, `native_runtime`, and `native_codegen`, then reran the full compiler coverage pass to move the compiler to `85.63%` lines / `83.85%` functions / `86.65%` regions while the remaining gap stayed concentrated in `sema`, `interpreter`, `native_codegen`, and `mir_runtime`.
- Added another helper sweep in `sema`, `interpreter`, `mir_runtime`, and `native_codegen`, then reran the full compiler suite and coverage pass to move the compiler to `86.30%` lines / `84.20%` functions / `87.15%` regions while the remaining gap stayed concentrated in `sema`, `interpreter`, `native_codegen`, `mir_runtime`, and `native_runtime`.
- Added another helper sweep in `interpreter`, `sema`, `mir_runtime`, and `native_runtime`, then reran the full compiler suite and coverage pass to move the compiler to `86.44%` lines / `84.41%` functions / `87.27%` regions while the remaining gap stayed concentrated in `interpreter`, `sema`, `native_codegen`, `mir_runtime`, and `analysis`.
- Added another helper sweep in `interpreter` and `native_runtime`, then reran the full compiler suite and coverage pass to move the compiler to `86.57%` lines / `84.59%` functions / `87.39%` regions while the remaining gap stayed concentrated in `interpreter`, `sema`, `native_codegen`, `mir_runtime`, and `analysis`.
- Added another helper sweep in `analysis`, `sema`, `interpreter`, `mir_runtime`, and `native_runtime`, verified the new targeted tests plus the full `aurora-compiler` lib suite, and resumed the next full compiler coverage run from that green baseline.
- Completed that full compiler coverage run and moved the compiler to `86.78%` lines / `84.74%` functions / `87.59%` regions while the remaining gap stayed concentrated in `interpreter`, `sema`, `native_codegen`, `mir_runtime`, and `native_runtime`.
- Added another checker/runtime helper sweep in `sema`, `interpreter`, `mir_runtime`, and `native_runtime`, verified the expanded `aurora-compiler` lib suite at 213 passing tests, and reran compiler coverage to move the compiler to `86.93%` lines / `84.81%` functions / `87.71%` regions.
- Added another helper sweep in `sema`, `interpreter`, `mir_runtime`, `native_runtime`, and `native_codegen`, verified the expanded `aurora-compiler` lib suite at 214 passing tests, and reran compiler coverage to move the compiler to `87.07%` lines / `84.82%` functions / `87.79%` regions.
- Added another dense runtime/member and helper sweep in `lib`, `native_codegen`, and `mir_runtime`, including a runtime member matrix across `String` / `Vec` / `Map` / `Set` / `Channel` / `Task` / `TaskGroup`, direct thunk helper coverage, and more MIR operator/task helper coverage; reran the expanded `aurora-compiler` lib suite at 216 passing tests and moved compiler coverage to `87.54%` lines / `84.83%` functions / `88.35%` regions.
- Added another helper sweep across `sema`, `interpreter`, `mir_runtime`, `native_codegen`, and `lib`, covering builtin enum-constructor hints, literal-pattern rendering, trait-bound lookup helpers, more MIR operator/task branches, direct thunk helpers, and a dense runtime member matrix compiled through both execution paths; reran the expanded `aurora-compiler` lib suite at 220 passing tests and moved compiler coverage to `88.06%` lines / `85.22%` functions / `88.98%` regions.
- Added another helper sweep across `interpreter`, `mir_runtime`, and `native_codegen` to cover callable-default evaluation, borrowed writeback and spawnability helpers, and more direct type-parameter / opaque-fallback lowering paths; reran the expanded `aurora-compiler` lib suite at 223 passing tests and moved compiler coverage to `88.16%` lines / `85.29%` functions / `89.04%` regions.
- Added a denser runtime/codegen matrix across `lib.rs` and `native_codegen.rs` to drive borrow-mut writebacks, named `range(...)`, `select` arms, cleanup resources, and spawn/task paths through interpreter, MIR runtime, and direct backend compilation; reran the expanded `aurora-compiler` lib suite at 225 passing tests and moved compiler coverage to `88.19%` lines / `85.30%` functions / `89.05%` regions.
- Added another direct-path helper sweep across `analysis.rs`, `interpreter.rs`, `mir_runtime.rs`, `native_codegen.rs`, and `lib.rs`, then reran the full compiler suite and coverage pass to move the compiler to `88.51%` lines / `85.55%` functions / `89.28%` regions while keeping the lib suite green at 228 passing tests.
- Reworked closure-heavy coverage hot spots in `native_runtime.rs`, `native_codegen.rs`, `interpreter.rs`, and `mir_runtime.rs`, then reran the full compiler suite and coverage pass to move the compiler to `88.51%` lines / `87.70%` functions / `89.34%` regions while keeping the lib suite green at 228 passing tests.
- Added another helper/refactor sweep across `native_codegen.rs`, `native_runtime.rs`, `interpreter.rs`, `mir_runtime.rs`, `sema.rs`, `lexer.rs`, and `call.rs`, then reran the full compiler coverage pass to move the compiler to `88.64%` lines / `88.29%` functions / `89.49%` regions while keeping the lib suite green at 230 passing tests.
- Added another helper/control-flow sweep across `native_codegen.rs`, `interpreter.rs`, and `mir_runtime.rs`, then reran the full compiler coverage pass to move the compiler to `88.62%` lines / `88.57%` functions / `89.52%` regions while keeping the lib suite green at 230 passing tests.
- Added another MIR/checker/direct-backend sweep across `mir.rs`, `sema.rs`, and `native_codegen.rs`, then reran the full compiler coverage pass to move the compiler to `89.08%` lines / `89.21%` functions / `89.90%` regions while keeping the compiler suite green at 237 passing lib tests plus the fixture and module suites.
- Added another interpreter/checker/direct-backend sweep across `interpreter.rs`, `sema.rs`, and `native_codegen.rs`, then reran the full compiler coverage pass to move the compiler to `89.34%` lines / `89.34%` functions / `90.17%` regions while keeping the compiler suite green at 241 passing lib tests plus the fixture and module suites.
- Tightened another `native_codegen.rs` error-mapping batch with a macro-based refactor that preserved function coverage while shaving more uncovered backend lines, then reran the full compiler coverage pass to move the compiler to `89.38%` lines / `89.34%` functions / `90.17%` regions.
- Added another helper sweep in `interpreter.rs`, `sema.rs`, and `native_codegen.rs`, then reran the full compiler coverage pass to move the compiler to `89.45%` lines / `89.35%` functions / `90.20%` regions while the remaining drag stayed concentrated in `interpreter`, `sema`, and `native_codegen`.
- Added another checker/runtime helper sweep in `sema.rs` and `interpreter.rs`, then reran the full compiler coverage pass to move the compiler to `90.26%` lines / `89.45%` functions / `90.79%` regions while the remaining drag stayed concentrated in `interpreter`, `sema`, and `native_codegen`.
- Completed the next full compiler coverage run after that helper sweep, moving the compiler to `90.85%` lines / `89.51%` functions / `91.09%` regions while the remaining drag stayed concentrated in `interpreter`, `native_codegen`, and `sema`.
- Added another interpreter-focused helper sweep over runtime equality/casting, `for`/`select`/`eval_expr` control-flow branches, specialized collection constructors, `try`, logical operators, enum members, and index errors, then reran the full compiler coverage pass to move the compiler to `91.29%` lines / `89.63%` functions / `91.35%` regions while the remaining drag stayed concentrated in `native_codegen`, `interpreter`, and `sema`.
- Added another direct-backend constructor/thunk sweep in `native_codegen.rs`, covering receiver/writeback metadata registration for lowered methods plus float/bool/plain-class thunk parameter lowering and unit-return `main` wrappers, then reran the full compiler coverage pass to move the compiler to `91.33%` lines / `89.65%` functions / `91.40%` regions while the remaining drag stayed concentrated in `native_codegen`, `interpreter`, and `sema`.
- Added another checker/direct-backend sweep in `sema.rs` and `native_codegen.rs`, covering default-argument and recursive-type helper paths, reserved built-in `Result`/`Option` name rejection, scalar `to_string` member typing, scalar direct-type rendering, and opaque thunk error handling, then reran the full compiler coverage pass to move the compiler to `91.46%` lines / `89.84%` functions / `91.50%` regions while the remaining drag stayed concentrated in `native_codegen`, `interpreter`, and `sema`.
- Added another direct-backend/interpreter sweep in `native_codegen.rs` and `interpreter.rs`, removing unreachable collection/task clone-specialization branches from the direct backend, adding direct cleanup/task-group smoke coverage, and covering unsigned cast plus unary operator fallback interpreter paths, then reran the full compiler coverage pass to move the compiler to `91.67%` lines / `89.84%` functions / `91.65%` regions while the remaining drag stayed concentrated in `native_codegen`, `interpreter`, and `sema`.
- Added another checker/interpreter sweep in `sema.rs` and `interpreter.rs`, covering specialized enum member diagnostics, member assignment mismatch diagnostics, qualified module class/enum checker paths, imported module runtime evaluation, constructor error handling, builtin propagation on `try`, and more numeric/string runtime paths, then reran the full compiler coverage pass to move the compiler through `91.98%` lines / `90.17%` functions / `92.08%` regions and then to `92.07%` lines / `90.26%` functions / `92.21%` regions while the remaining drag stayed concentrated in `native_codegen`, `interpreter`, and `sema`.
- Flattened several `native_codegen.rs` direct-backend test scaffolds to remove unexecuted panic closures in the coverage denominator, reran the touched direct-backend tests, and then reran the full compiler coverage pass to move the compiler to `92.12%` lines / `90.59%` functions / `92.25%` regions while `native_codegen.rs` rose to `91.47%` lines / `80.36%` functions / `92.71%` regions.
- Fixed compiler-backed `Vec.insert(...)` analysis/completion metadata so the compiler and LSP bridge now report the correct `-> bool` signature instead of the stale `-> None` detail.
- Added practical parsing/formatting and collection-finisher support across the checker, interpreter, MIR/runtime, direct backend, compiler/LSP tooling, fixtures, examples, and tutorials: `parse_int32`, `parse_int64`, `parse_float64`, scalar and boolean `.to_string()`, `String.join(...)`, `Vec.insert(...)` / `clear()` / `reverse()`, `Map.items()` / `entries()` / `clear()` / `extend(...)`, builtin `MapEntry[K, V]`, and owned `Set[T]` collections with `Set{...}` literals, iteration, and the maintained set method surface.
- Added literal `match` patterns over `bool`, integer, and `String` scrutinees across the parser, checker, interpreter, MIR lowering/runtime, maintained examples, CLI smoke tests, and tutorial track, while keeping wildcard exhaustiveness requirements for open-ended literal domains.
- Added builtin owned `Vec[T]` collections across the checker, interpreter, MIR runtime, direct backend, CLI, compiler-backed tooling, fixtures, maintained examples, and tutorials, including list literals, borrow-safe indexing, indexed assignment, by-value/shared/mutable iteration, `len() -> int32`, equality, and the maintained method surface `len`, `is_empty`, `clone`, `push`, `pop`, `get`, `set`, `remove`, `swap`, `contains`, and `extend`.
- Added builtin `String` utility methods, numeric helper builtins, and owned `Map[K, V]` collections across the checker, interpreter, MIR/runtime, direct backend, compiler-backed tooling, fallback LSP analysis, fixtures, maintained examples, tutorials, and CLI smoke coverage, including map literals, indexed map reads/writes, and the maintained `Map` method surface `len`, `is_empty`, `clone`, `get`, `set`, `remove`, `contains_key`, `keys`, and `values`.
- Expanded the maintained `String` utility surface with `split`, `replace`, `to_lower`, `to_upper`, `strip_prefix`, and `strip_suffix` across the checker, interpreter, MIR runtime, direct backend, compiler/LSP tooling, fixtures, examples, tutorials, and CLI smoke coverage.
- Fixed postfix parsing so indexed expressions can chain members and calls correctly again, and locked in compiler/LSP coverage for indexed expressions inside f-string interpolations such as `f"{counts["key"]}"`.
- Fixed `for value in mut vec:` so it now requires a mutable `Vec[T]` place during checking, instead of silently mutating immutable bindings.
- Fixed interpreter and MIR `Vec[T]` equality for mixed-construction vectors, so empty-annotated-plus-push vectors now compare by element contents just like literal-built vectors.
- Fixed the remaining Vec follow-up gaps by requiring mutable places for `mut ` vector iteration and teaching MIR/direct build inference that `Vec[T]()` constructor locals still carry `Vec[T]` types into later `for` lowering.
- Fixed overlapping borrowed call arguments so free functions and method receivers can no longer alias the same place across `mut ` / `mut ` or `` / `mut ` combinations.
- Fixed direct/default native builds for `float64` returns from enum `match` arms that destructure payloads, keeping build parity with `run` and `run-mir`.
- Removed the duplicate prefix spelling for ordinary borrowed parameters so free-function borrows are now written only as `name: Type` / `name: mut Type`, with parser regression coverage and aligned examples/tutorials.
- Fixed direct `check` / `analyze` package-root inference for nested package modules, so opening files like `examples/modules/pkg/user.au` no longer resolves imports through a duplicated path segment.
- Added normal `aura help` / `aura --help` / `aura version` / `aura --version` success paths and documented them in the maintained CLI/tutorial surface.
- Replaced machine-local absolute repo paths in the maintained READMEs/tutorials with portable relative links or `$(pwd)`-style command examples, and refreshed `examples/README.md` to include `examples/modules/trait_impl_imports.au`.
- Documented the current first-user limitations that are still real surface constraints, including no `String(...)` constructor, no bare `Ok(...)` / `Err(...)` constructors, required `Channel[T]` context for `channel()`, and named-function-only `spawn` targets.
- Fixed bare `None` parity across `run`, `run-mir`, and native builds, recovered compiler-backed analysis/completions for buffers with multiple dangling member accesses, and restored source-aware arithmetic runtime diagnostics for built binaries.
- Fixed f-string lexing/parsing so interpolations can contain inner string literals and nested braces, with maintained compiler fixture coverage for both checking and execution.
- Added maintained regression coverage for `Option.None` inference, namespace-qualified imports inside imported module bodies, and closed-channel `select` timers.
- Fixed field-level move tracking for owned member reads so Aurora now rejects reusing a moved field while still allowing access to untouched fields and explicit field reinitialization.
- Fixed `select` with `after(...)` over closed-and-empty channels so timer arms no longer starve behind immediate `recv()` closure results, and added maintained runtime regression coverage for that path.
- Fixed specialized generic trait impl dispatch across interpreter, MIR runtime, and direct native builds, and added maintained examples plus CLI coverage for specialized dispatch and trait-associated methods.
- Fixed direct-backend multi-implementor trait dispatch so bounded generic calls like `animal.describe()` now build natively across multiple concrete receiver types, with maintained example and CLI coverage.
- Reserved built-in type names such as `Task`, `Channel`, and `Result` for the language/runtime surface so user-defined classes, enums, and traits now fail early with a clear diagnostic instead of later type-arity confusion.
- Fixed module-crossing trait impl resolution across checking, interpreter/MIR execution, direct builds, compiler-backed completions, and the LSP bridge.
- Added generic trait declarations plus generic impl headers across the parser, checker, interpreter, MIR/runtime, direct builds, fixtures, examples, tutorials, and CLI smoke coverage.
- Fixed module-qualified `spawn` targets so `check`, `run`, `run-mir`, and `build` now report a user diagnostic instead of letting MIR lowering panic.
- Fixed compiler-backed definitions for namespace-imported symbols and enum variants used in `match` patterns, with matching LSP bridge coverage.
- Added module-qualified type annotations to the maintained module surface and updated the examples/tutorials to use them directly.
- Extended compiler-backed dangling-member recovery so `aura analyze` / `aura complete` still recover symbols and completions when `counter.` is the final buffer line.
- Fixed direct-backend native builds for recursive match payloads and `Task.join()` values that carry plain classes, including spawned functions that return plain-class values.
- Hardened compiler, MIR/runtime, and direct-backend parity around external regression cases, including stdin-backed local-module execution, generic dispatch/composition, borrowed field projections, large negative literals, float rendering, and maintained-example native builds.
- Added a Rust workspace root with `aurora-compiler` and `aura`.
- Added the first compiler modules: diagnostics, AST, lexer, parser, semantic checker, and evaluator.
- Added the first milestone sample program at `examples/point.au`.
- Added `examples/README.md` with instructions for running, checking, and inspecting example programs.
- Added `crates/aura/README.md` with release-build and direct binary usage instructions.
- Added in-repo work tracking under `work/`.
- Verified `cargo test` passes.
- Verified `cargo run -p aura -- run examples/point.au` prints `5.0`.
- Added support for `def name(...):` as shorthand for `-> None`.
- Added support for running top-level script statements without an explicit `main`.
- Renamed primitive language types to explicit spellings like `int32`, `uint64`, and `float64`.
- Renamed the line-printing builtin from `println` to `print`.
- Verified `examples/basic_addition.au` and `examples/top_level_addition.au` both run and print `16`.
- Added `tools/vscode-aurora` as an in-repo VS Code extension package.
- Added `tools/aurora-language-server` as an in-repo LSP package.
- Added a root npm workspace manifest for repo-managed tools.
- Verified the VS Code extension analysis/tests with `npm run check:extension` and `npm run test:extension`.
- Switched the VS Code package from local editor analysis to an LSP client.
- Added a bundled `dist/` build for the VS Code extension so VSIX packaging stays self-contained inside the monorepo.
- Verified `npm run package:extension` produces `tools/vscode-aurora/aurora-language.vsix`.
- Regenerated `docs/aurora_language_proposal.html` from the updated proposal Markdown.
- Added parser, semantic checker, and interpreter support for `if`, `elif`, `else`, `while`, `break`, `continue`, strings, booleans, comparison operators, and compound assignment.
- Added `examples/control_flow.au` and verified the control-flow bootstrap path.
- Improved CLI diagnostics so parser/type/runtime errors render with source context and a caret.
- Staged compiler MIR lowering with explicit basic blocks and a new `aura mir <file.au>` command.
- Added LSP hover, go-to-definition, and document diagnostics on top of the current Aurora-aware analysis layer.
- Added categorized examples covering most of the currently implemented language surface.
- Added a `tutorials/` directory with Markdown chapters for the implemented subset and documented the maintenance rule that examples and tutorials must evolve with the language.
- Fixed LSP false positives for top-level script bindings and added member resolution for parenthesized receiver expressions such as `(dx * dx + dy * dy).sqrt()`.
- Added a repo-level `AGENTS.md` and `docs/testing_strategy.md` to define the test-first workflow.
- Added fixture-based compiler tests for parse/check/run/diagnostic behavior under `crates/aurora-compiler/tests/fixtures/`.
- Added `crates/aurora-compiler/README.md` documenting compiler test layers and fixture categories.
- Added `npm run coverage:lsp` as the repeatable language-server coverage command and documented it in the repo.
- Added `npm run coverage:compiler` and measured the first Rust compiler-library coverage baseline with `cargo-llvm-cov`.
- Added parser, checker, interpreter, MIR, examples, and LSP support for non-generic enums with unit and single-payload variants plus exhaustive statement-form `match`.
- Added parser, checker, interpreter, MIR, examples, tutorials, and LSP support for `for` loops over `range(...)`.
- Added parser, checker, interpreter, examples, tutorials, and LSP support for user-defined instance methods with `self` plus associated methods.
- Added built-in generic `Result[T, E]` and `Option[T]` support across the checker, interpreter, examples, tutorials, and LSP analysis.
- Added fuller mutating receiver semantics with member-target assignment, `mut self`, mutating methods, and regression fixtures.
- Added `try expr` over built-in `Result[T, E]` with checker/runtime support, examples, tutorials, and diagnostics.
- Added `with` scoped cleanup using `close(mut self)` resources, plus examples, tutorials, and runtime cleanup on early return.
- Added bootstrap concurrency with `Channel[T]`, `channel()`, `spawn`, `Task[T]`, `send`, `recv`, `close`, and `join()`, plus examples, fixtures, and LSP support.
- Added bootstrap structured concurrency with `task_group()`, `with task_group() as group:`, `group.spawn(...)`, `group.cancel()`, cooperative `cancelled()`, `select`, and duration literals for `after(...)`, plus examples, fixtures, tutorials, MIR support, and LSP coverage.
- Added explicit detached tasks with `spawn detached`, proposal-level `Channel.send() -> Result[None, SendError[T]]`, and broader `select` send/recv/timer arm support across the compiler, runtime, examples, fixtures, tutorials, syntax highlighting, and LSP.
- Fixed LSP false diagnostics for `after(...)` select timers and duration literals like `5ms` in concurrency examples.
- Added machine-readable compiler analysis output plus `aura analyze` and `aura ast-json`.
- Switched the language server to prefer compiler-owned diagnostics, symbols, hover, and go-to-definition via `aura analyze`, with local JS analysis kept as fallback and for completions.
- Added machine-readable compiler completions via `aura complete`.
- Switched the language server to prefer compiler-owned completions, leaving the JS analysis layer as fallback for incomplete or currently-invalid buffers.
- Expanded the tutorial track so it covers the full currently implemented bootstrap language surface, not just the features already represented by the example walkthroughs.
- Fixed VS Code indentation so pressing Enter after Aurora block headers keeps the expected block indent instead of jumping back to column 0.
- Added an Aurora-specific VS Code Enter handler so indentation now deterministically follows Aurora block structure instead of relying only on editor heuristics.
- Added named arguments for ordinary functions, instance methods, associated methods, and spawned function targets, aligning callable syntax more closely with class construction.
- Added a shared compiler-side call binding layer for user-defined callables and builtins.
- Added named arguments for supported builtins, including `print(value=...)`, `range(stop=...)`, `range(start=..., stop=...)`, `after(duration=...)`, and `Channel.send(value=...)`.
- Added compiler and LSP regression coverage plus categorized examples and tutorial updates for builtin named arguments.
- Added integer-literal range enforcement for fixed-width integer annotations and default `int32` literals.
- Added support for `String.clone()` in the checker/runtime and removed unsupported `String.as_str()` from the documented current surface and completions.
- Improved the diagnostic for builtin method references like `ch.send` so they report a missing call instead of a misleading generic-type error.
- Clarified current limitations and `aura complete` semantics in the README and tutorial track so the documented bootstrap surface matches the implementation more closely.
- Made `aura complete --trigger .` tolerate the common incomplete-editor state where the current buffer contains a dangling member access like `counter.`.
- Made `aura analyze` recover symbols and occurrences for the common dangling-dot editor state while still surfacing the parse diagnostic.
- Added CLI product tests for broken-pipe stdout handling in `ast` and `mir`, and fixed those commands to exit cleanly when piped into consumers like `head`.
- Added `aura build -o <output>` as a bootstrap standalone-binary path by generating and compiling a Rust launcher linked against `aurora-compiler`.
- Added a MIR runtime for the current simpler subset plus `aura run-mir` for exercising that execution path directly.
- Expanded `aura run-mir` so it now covers the current implemented Aurora surface natively through MIR, including concurrency, `try`, and `with`.
- Switched `aura build` from embedding source execution to embedding checked MIR and running it directly through `run_mir(...)`.
- Added backend regression coverage for native MIR execution through both `run-mir` and built binaries.
- Added native MIR support for `try expr`, removing `try` from the backend fallback surface.
- Added native MIR support for `with` cleanup, removing `with` from the backend fallback surface.
- Added boolean operators `and`, `or`, and `not` across the parser, checker, interpreter, MIR lowering, and MIR runtime.
- Added unary minus support across the parser, checker, interpreter, MIR lowering, and MIR runtime.
- Added checker-level use-after-move diagnostics for straight-line moves through function arguments, value receivers, constructors, enum payloads, and channel sends.
- Added clean Aurora diagnostics for division by zero and integer overflow in both the interpreter and MIR runtime.
- Added runtime enforcement for annotated fixed-width integer bindings and assignments instead of silently widening values.
- Unified `main` parameter validation so both execution paths reject parameterized `main` functions during checking.
- Added contextual `float32` literal support so floating-point literals can be used in typed `float32` bindings, parameters, returns, and class fields.
- Added explicit numeric casts with `expr as Type` across the parser, checker, interpreter, MIR runtime, compiler analysis, fixtures, and maintained examples.
- Added user-defined generic `class`, `enum`, and `def` declarations with generic inference across the checker, runtimes, fixtures, examples, tutorials, and LSP fallback analysis.
- Added first-pass traits with `trait`, `impl Trait for Type`, bounded generic functions, trait method checking, interpreter/MIR trait dispatch, compiler-backed trait symbols/completions, and maintained examples/tutorial coverage.
- Added default parameter values on ordinary functions and class methods, including checker/runtime/MIR support, call-site omission handling, and proposal-aligned restrictions on ordering and parameter references.
- Promoted multiple trait bounds with `T: A + B` from an untracked capability to a maintained surface with fixtures, examples, and tutorial coverage.
- Fixed the compiler-backed LSP bridge to prefer the current source-tree compiler via `cargo run` inside the Aurora repo, avoiding stale `target/debug/aura` behavior during local development and tests.
- Added `pass` as a maintained no-op statement for intentionally empty blocks.
- Added the `sleep(duration)` builtin across checking, runtime, MIR, examples, tutorials, and editor tooling.
- Added local file module support with `import`, `from ... import ...`, and `public` module boundaries across checking, interpreter execution, MIR execution, CLI run/build, examples, tutorials, and compiler tests.
- Extended compiler-backed `aura analyze` / `aura complete` and the LSP bridge so stdin/file analysis now resolves local module imports for diagnostics, hover, and completions.
- Added CI-style repo gates plus enforced baseline coverage thresholds for the compiler and language server.
- Fixed generic method inference for method calls on generic class instances inside generic functions.
- Fixed user-defined generic enum unit variants so they retain instantiated type arguments.
- Fixed specialized generic trait impl dispatch for concrete generic instances such as `impl Trait for Box[String]`.
- Raised integer and duration literal parsing to `i128`, including minute duration literals with `m`.
- Added wildcard `case _:` support in statement-form `match`.
- Added trait bounds on generic class and enum type parameters.
- Added empty marker traits with `pass`.
- Rejected direct recursive class fields without `indirect` and added proposal-aligned `indirect` recursive fields to the maintained compiler surface.
- Fixed direct-expression narrow integer overflow checking so runtime arithmetic respects annotated widths even when values flow straight into calls.
- Fixed whole-number float rendering so values like `5.0` and `9.0` preserve their `.0` suffix in output.
- Added ordinary free-function `` and `mut ` parameters across the parser, checker, interpreter, MIR runtime, fixtures, examples, tutorials, and LSP fallback analysis.
- Fixed namespace-imported classes and enums so `import a.b` now supports `a.b.Type(...)`, `a.b.Enum.Variant`, and qualified `match` arms in both the interpreter and MIR execution paths.
- Finished the remaining numeric-runtime gap for true full-range `uint128` execution across the checker, interpreter, MIR runtime, direct backend, fixtures, CLI coverage, and maintained examples/tutorials.
- Clarified in the maintained tutorials/examples that `range(...)` is still limited to the current signed index space in the bootstrap compiler, without freezing that limitation into the proposal.
- Brought several proposal-defined syntax/features into the maintained compiler surface: `copy class`, `indirect Node?`, `str` parameters, `match `, unqualified match variants, `for` iteration over `Channel[T]`, contextual `copy` keyword handling, f-strings, and explicit generic constructor specialization like `Box[int32](...)`.
- Added maintained examples, fixture coverage, tutorial updates, and LSP fallback coverage for those proposal-alignment features.
- Replaced `aura build`'s generated Rust launcher with a native MIR artifact build path that embeds serialized MIR in a native launcher and links it against a compiled Aurora runtime library.
- Added product coverage for stdin-backed native builds with local modules and for binaries that still run after the original source file is removed.
- Added a true direct native backend for a supported scalar/control-flow MIR subset and exposed it through `aura build --backend direct`.
- Switched `aura build` to a three-way backend matrix with `--backend auto|direct|mir-runtime`, where `auto` now tries direct native codegen first and falls back when needed.
- Added compiler-side direct-backend coverage so the enforced Rust coverage gate remains green after introducing native codegen modules.
- Expanded the direct native backend to support floats, plain classes, field access, associated methods, and immutable instance methods, including clean broken-pipe handling for direct-built binaries.
- Expanded the direct native backend to cover the full currently implemented Aurora language surface, including mutable borrows, `range`/`for`, traits, generics, resource cleanup, and concurrency/task-group/select examples.
- Verified direct backend parity against every runnable maintained example by building with `--backend direct` and comparing output to `aura run`.
- Removed `--backend mir-runtime` from the CLI and docs now that the maintained Aurora surface has full native direct coverage.
- Fixed direct-backend parity bugs for float comparisons, float modulo, normal-scope `with` cleanup, scalar return values through `with`, boolean printing, narrow integer overflow checks, and trait method dispatch on builtin types.
- Fixed interpreter `float32` display so round-tripped `float32` values render without leaking binary noise like `3.140000104904175`.
- Fixed generic trait dispatch contamination in the tree-walk interpreter so repeated trait-bounded generic calls no longer reuse the first concrete type across later calls.
- Fixed `mir --stdin` so it now resolves local module imports using the provided path, matching `run-mir --stdin`.
- Fixed explicit built-in enum constructor specialization such as `Result[int32, String].Ok(...)` across checking, interpreter execution, MIR lowering/runtime, examples, and tutorials.
- Fixed imported functions that return module-local classes so the caller can use the returned value's fields and methods without importing the class separately.
- Fixed f-string interpolation diagnostics so inner expression errors point at the interpolation site instead of the start of the enclosing function.
- Rejected mutual recursive class fields without `indirect` and replaced raw recursion stack overflows with a friendly runtime call-depth diagnostic.

## Blocked

- None currently.
