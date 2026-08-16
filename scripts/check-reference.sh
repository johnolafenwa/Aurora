#!/usr/bin/env bash
set -euo pipefail
trap 'echo "reference check failed: $BASH_COMMAND" >&2' ERR

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

assert_wrapped_text() {
  local path="$1"
  local expected="$2"

  python3 - "$path" "$expected" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
expected = " ".join(sys.argv[2].split())
actual = " ".join(path.read_text(encoding="utf-8").split())
if expected not in actual:
    raise SystemExit(f"missing reference text in {path}: {expected}")
PY
}

python3 scripts/generate_llms.py --check

proposal_stem="auro""ra_language_proposal"
proposal_md="docs/${proposal_stem}.md"
proposal_html="docs/${proposal_stem}.html"
former_name="Auro""ra"

required_pages=(
  language-specification
  grammar
  names-and-scopes
  static-semantics
  execution-model
  assertions
  tuples
  bytes
  json
  numeric-arrays
  math
  randomness
  diagnostics
  conformance
)

for page in "${required_pages[@]}"; do
  path="docs/manual/${page}.md"
  if [[ ! -s "$path" ]]; then
    echo "missing normative reference page: $path" >&2
    exit 1
  fi
  if ! grep -Fq "/manual/${page}" docs/manual/index.md; then
    echo "manual index does not link normative page: $page" >&2
    exit 1
  fi
  if ! grep -Fq "/manual/${page}" docs/.vitepress/config.mts; then
    echo "VitePress sidebar does not link normative page: $page" >&2
    exit 1
  fi
done

test -s examples/numbers/scalar_math.au
grep -Fq '`scalar_math.au`' examples/README.md
grep -Fq 'math.pow(2.0, -3.0)' examples/numbers/scalar_math.au
grep -Fq 'math.pi' examples/numbers/scalar_math.au
grep -Fq '0x7ff8000000000000' docs/manual/math.md
grep -Fq 'compiler bridge exposes the complete math module function surface' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq '| `math.pow` | `pow(base: float64, exponent: float64) -> float64`' docs/manual/api-index.md
grep -Fq 'the executable [Math Module](/manual/math) block' docs/manual/conformance.md

grep -Fq 'module = { module-element }' docs/manual/grammar.md
grep -Fq 'postfix-expression' docs/manual/grammar.md
grep -Fq 'left-to-right' docs/manual/execution-model.md
grep -Fq 'compiler fixtures' docs/manual/conformance.md
grep -Fq 'MUST' docs/manual/language-specification.md
grep -Fq '`AU4006` means invalid runtime configuration; and `AU4007` means a numeric Array shape or reduction violation.' docs/manual/cli-and-tooling.md
grep -Fq '`int` is an alias for `int64`' docs/manual/types.md
grep -Fq 'contracts remain `int32`, including `main()` exit statuses' docs/manual/types.md
assert_wrapped_text docs/manual/lexical-structure.md 'otherwise the literal defaults to `int64`'
grep -Fq 'otherwise it defaults to `int64`' docs/manual/static-semantics.md
grep -Fq 'assert-statement' docs/manual/grammar.md
grep -Fq 'A failed assertion is `AU4001` at the `assert` keyword location.' docs/manual/diagnostics.md
grep -Fq 'An assertion evaluates its condition exactly once.' docs/manual/execution-model.md
grep -Fq 'An `assert` condition must have exactly type `bool`.' docs/manual/static-semantics.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0024-assertion-evaluation-and-diagnostic-policy.md
grep -Fq '0024-assertion-evaluation-and-diagnostic-policy.md' architecture_docs/decisions/README.md
test -s examples/basics/assertions.au
grep -Fq '`assertions.au`' examples/README.md
grep -Fq '[23-assertions-and-tests.md]' tutorials/README.md
test -s examples/agents/retrying_network_worker.au
grep -Fq 'random.Rng(42)' examples/agents/retrying_network_worker.au
grep -Fq 'if status != 503:' examples/agents/retrying_network_worker.au
grep -Fq 'if attempt == max_attempts:' examples/agents/retrying_network_worker.au
grep -Fq 'while total_requests < 7:' examples/agents/retrying_network_worker.au
grep -Fq 'request_with_retry(address, "/rate", "rate", 3, 4ms, rng)' examples/agents/retrying_network_worker.au
grep -Fq 'retrying_network_worker.au' README.md
grep -Fq '`retrying_network_worker.au`' examples/README.md
grep -Fq 'retrying_network_worker.au' tutorials/13-concurrency.md
grep -Fq 'retrying_network_worker.au' tutorials/19-io-and-networking.md
grep -Fq 'retrying_network_worker_runs_with_computed_backoff_on_both_backends' docs/manual/conformance.md
grep -Fq 'fn retrying_network_worker_runs_with_computed_backoff_on_both_backends()' crates/aura/tests/cli.rs
grep -Fq 'Inside an unmatched `(`, `[`, or `{`, an ordinary physical newline does not' docs/manual/lexical-structure.md
grep -Fq 'Backslash continuation is not implemented.' docs/manual/lexical-structure.md
grep -Fq 'Ordinary, raw, and f-strings remain' docs/manual/lexical-structure.md
grep -Fq 'it does not add a trailing comma to any list form' docs/manual/grammar.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0025-newline-continuation-and-delimited-layout.md
grep -Fq '0025-newline-continuation-and-delimited-layout.md' architecture_docs/decisions/README.md
test -s examples/basics/multiline_expressions.au
grep -Fq '`multiline_expressions.au`' examples/README.md
grep -Fq 'examples/basics/multiline_expressions.au' README.md
grep -Fq '[24-multiline-expressions.md]' tutorials/README.md
grep -Fq 'Delimiter continuation, ignored continuation indentation' docs/manual/conformance.md
grep -Fq 'compiler bridge analyzes and completes inside continued delimiters' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'Aura newline indentation handles source delimiters' tools/vscode-aura/test/package.test.js
grep -Fq 'tuple-expression' docs/manual/grammar.md
grep -Fq 'tuple-type' docs/manual/grammar.md
grep -Fq 'unpack-target' docs/manual/grammar.md
grep -Fq 'tuple-pattern' docs/manual/grammar.md
grep -Fq 'Unpacking a non-copy tuple consumes the whole source exactly once' docs/manual/tuples.md
grep -Fq 'Mutable-borrow iteration with a tuple target is rejected.' docs/manual/tuples.md
grep -Fq 'no empty tuple, multi-element trailing tuple comma, tuple' docs/manual/tuples.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0026-minimal-tuples.md
grep -Fq '## 2026-07-26 Amendment: Tuple Equality' architecture_docs/decisions/0026-minimal-tuples.md
grep -Fq '0026-minimal-tuples.md' architecture_docs/decisions/README.md
grep -Fq 'Tuple value `==` and `!=` require both operands to have the same static tuple' docs/manual/tuples.md
grep -Fq 'comparison reads the two resulting tuple values and consumes neither' docs/manual/tuples.md
grep -Fq 'Tuple ordering remains a static error.' docs/manual/tuples.md
test -s examples/basics/tuples.au
grep -Fq '`tuples.au`' examples/README.md
grep -Fq 'examples/basics/tuples.au' README.md
grep -Fq '[25-tuples.md]' tutorials/README.md
grep -Fq 'assert baseline == same' examples/basics/tuples.au
grep -Fq 'assert baseline != changed' examples/basics/tuples.au
grep -Fq 'assert same != changed' examples/basics/tuples.au
test -s crates/aura-compiler/tests/fixtures/run-pass/tuple_structural_equality.au
test -s crates/aura-compiler/tests/fixtures/run-pass/tuple_structural_equality.stdout
grep -Fq 'nested_with_score' crates/aura-compiler/tests/fixtures/run-pass/tuple_structural_equality.au
grep -Fq 'generic_equal' crates/aura-compiler/tests/fixtures/run-pass/tuple_structural_equality.au
grep -Fq 'trace_singleton' crates/aura-compiler/tests/fixtures/run-pass/tuple_structural_equality.au
grep -Fq 'trace_text' crates/aura-compiler/tests/fixtures/run-pass/tuple_structural_equality.au

# B3.0-e: `AU3005` guidance is classified the same way the rejection is, so the
# recommended recovery is never something `AU3007` rejects in turn.
test -s crates/aura-compiler/tests/fixtures/check-fail/random_list_index_requires_transfer.diag
test -s crates/aura-compiler/tests/fixtures/check-fail/random_transitive_dict_index_requires_transfer.diag
test -s crates/aura-compiler/tests/fixtures/check-fail/generic_list_index_clone_safety_guidance.diag
test -s crates/aura-compiler/tests/fixtures/check-fail/generic_dict_index_clone_safety_guidance.diag
test -s crates/aura-compiler/tests/fixtures/run-pass/random_index_remove_transfers_ownership.stdout
grep -Fq 'cannot clone it because' crates/aura-compiler/tests/fixtures/check-fail/random_list_index_requires_transfer.diag
grep -Fq 'cannot clone it because' crates/aura-compiler/tests/fixtures/check-fail/random_transitive_dict_index_requires_transfer.diag
grep -Fq 'requires a clone-safe' crates/aura-compiler/tests/fixtures/check-fail/generic_list_index_clone_safety_guidance.diag
grep -Fq 'requires a clone-safe' crates/aura-compiler/tests/fixtures/check-fail/generic_dict_index_clone_safety_guidance.diag
grep -Fq 'clone-safety' docs/manual/diagnostics.md
grep -Fq 'clone-safety' docs/manual/conformance.md
grep -Fq 'clone-safety classification' architecture_docs/decisions/0014-map-literals-and-indexing.md
grep -Fq 'pop(index)' tutorials/02-bindings-and-types.md
grep -Fq 'pop(index)' tutorials/14-current-language-surface.md

# B3.0-e: builtin function redefinition owns `AU2007` instead of the `AU2999`
# catch-all.
grep -Fq 'error[AU2007]' crates/aura-compiler/tests/fixtures/check-fail/builtin_function_names_cannot_be_redefined.diag
grep -Fq '`AU2007` builtin function redefinition' docs/manual/diagnostics.md
grep -Fq 'AU2007' tutorials/14-current-language-surface.md

# B3.0-e: `AU3002` recovery help names the access that actually conflicts, so a
# pure read or consumption is never told to move "the mutation".
grep -Fq 'perform the consumption in a separate statement' crates/aura-compiler/tests/fixtures/check-fail/nested_consume_and_borrow_same_call.diag
grep -Fq 'consumed values must be exclusive' crates/aura-compiler/tests/fixtures/check-fail/call_own_then_projected_copy_read_overlaps.diag
grep -Fq 'perform the mutation in a separate statement' crates/aura-compiler/tests/fixtures/check-fail/binary_left_borrow_rejects_later_mutation.diag
test -s crates/aura-compiler/tests/fixtures/check-pass/tuple_equality_contextual_literals.au
test -s crates/aura-compiler/tests/fixtures/check-fail/tuple_ordering_rejected.au
test -s crates/aura-compiler/tests/fixtures/check-fail/tuple_ordering_rejected.diag
test -s crates/aura-compiler/tests/fixtures/check-fail/tuple_comparison_chain_left_borrow_rejects_later_mutation.au
test -s crates/aura-compiler/tests/fixtures/check-fail/tuple_comparison_chain_middle_borrow_rejects_later_mutation.au
grep -Fq 'tuple ordering is not supported; use `==` or `!=`, or compare tuple elements explicitly' crates/aura-compiler/tests/fixtures/check-fail/tuple_ordering_rejected.diag
grep -Fq 'fn tuple_equality_and_inequality_are_structural_and_non_consuming()' crates/aura-compiler/src/sema_tests.rs
grep -Fq 'fn tuple_equality_requires_the_same_static_tuple_type()' crates/aura-compiler/src/sema_tests.rs
grep -Fq 'fn tuple_ordering_rejects_all_four_operators_with_the_teaching_diagnostic()' crates/aura-compiler/src/sema_tests.rs
grep -Fq 'fn tuple_value_equality_uses_elements_not_runtime_type_metadata()' crates/aura-compiler/src/runtime_value_tests.rs
grep -Fq 'fn analysis_exposes_structural_tuple_equality_without_consuming_operands()' crates/aura-compiler/src/analysis_tests.rs
grep -Fq 'compiler bridge exposes structural tuple equality and ordering diagnostics' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'same-type recursive structural `==`/`!=`' docs/manual/conformance.md
grep -Fq 'Tuple `==` and `!=` compare same-typed values structurally and' docs/manual/status-and-compatibility.md
grep -Fq 'the executable `docs/manual/tuples.md` fence' docs/manual/conformance.md
if [[ -e crates/aura-compiler/tests/fixtures/check-fail/tuple_equality_rejected.au ||
      -e crates/aura-compiler/tests/fixtures/check-fail/tuple_equality_rejected.diag ]]; then
  echo "retired tuple-equality rejection fixture is still present" >&2
  exit 1
fi
grep -Fq 'conditional-expression' docs/manual/grammar.md
grep -Fq 'The condition is evaluated first, exactly once' docs/manual/expressions.md
grep -Fq 'The unselected arm performs no' docs/manual/execution-model.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0027-conditional-expressions.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0028-membership-and-comparison-chains.md
grep -Fq '0027-conditional-expressions.md' architecture_docs/decisions/README.md
test -s examples/control_flow/conditional_expressions.au
grep -Fq '`conditional_expressions.au`' examples/README.md
grep -Fq 'examples/control_flow/conditional_expressions.au' README.md
grep -Fq 'examples/control_flow/conditional_expressions.au' tutorials/04-control-flow.md
grep -Fq 'Conditional-expression precedence' docs/manual/conformance.md
grep -Fq '`value if condition else alternative`' tutorials/14-current-language-surface.md
grep -Fq 'conditional expressions' tutorials/README.md
grep -Fq 'conditional expressions' docs/manual/index.md
grep -Fq 'ADR-0027' docs/manual/status-and-compatibility.md
grep -Fq 'compiler bridge preserves conditional operands and bool diagnostics' tools/aura-language-server/test/compiler_bridge.test.js
test -s crates/aura-compiler/tests/fixtures/check-pass/conditional_expression_contexts.au
test -s crates/aura-compiler/tests/fixtures/run-pass/conditional_expressions.au
test -s crates/aura-compiler/tests/fixtures/check-fail/conditional_expression_condition_must_be_bool.au
test -s crates/aura-compiler/tests/fixtures/check-fail/conditional_expression_arm_type_mismatch.au
test -s crates/aura-compiler/tests/fixtures/check-fail/conditional_expression_conditional_move.au
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0029-enumerate-and-zip-loop-forms.md
grep -Fq '0029-enumerate-and-zip-loop-forms.md' architecture_docs/decisions/README.md
grep -Fq 'distinct typed binding identities' architecture_docs/decisions/0029-enumerate-and-zip-loop-forms.md
grep -Fq 'ADR-0028, and ADR-0029.' docs/manual/status-and-compatibility.md
grep -Fq 'function-wide per-loop binding-slot isolation' docs/manual/conformance.md
grep -Fq 'mut numbers = list[int64]()' crates/aura-compiler/tests/fixtures/run-pass/enumerate_and_zip.au
grep -Fq 'for number, word in zip(numbers, words):' crates/aura-compiler/tests/fixtures/run-pass/enumerate_and_zip.au
grep -Fq 'for number, word in zip(words, numbers):' crates/aura-compiler/tests/fixtures/run-pass/enumerate_and_zip.au
grep -Fxq 'one=1' crates/aura-compiler/tests/fixtures/run-pass/enumerate_and_zip.stdout
grep -Fxq 'two=2' crates/aura-compiler/tests/fixtures/run-pass/enumerate_and_zip.stdout
grep -Fq 'fn every_ordinary_for_form_uses_a_fresh_scoped_target_slot()' crates/aura-compiler/src/mir_tests.rs
grep -Fq 'for label, value in jobs:' crates/aura-compiler/tests/fixtures/run-pass/tuple_for_pattern_queue.au
grep -Fq 'def update_first(values: mut list[int64]) -> int64:' crates/aura-compiler/tests/fixtures/run-pass/list_mut_iteration.au
test "$(grep -Fxc '24' crates/aura-compiler/tests/fixtures/run-pass/list_mut_iteration.stdout)" -eq 3
grep -Fq '= "int" | "int8"' docs/manual/grammar.md
grep -Fq 'Integer literals default to `int64`' tutorials/02-bindings-and-types.md
grep -Fq '`int` is an alias for `int64`' "$proposal_md"
grep -Fq '<code>int</code> is an alias for <code>int64</code>' "$proposal_html"
assert_wrapped_text docs/manual/lexical-structure.md '+ += - -= * *= ** **= / /= // //= % %= & &= | |= ^ ^= ~ << <<= >> >>='
assert_wrapped_text docs/manual/grammar.md 'assignment-operator = "=" | "+=" | "-=" | "*=" | "**=" | "/=" | "//=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>=" ;'
grep -Fq '{ ("*" | "/" | "//" | "%"), prefix-expression } ;' docs/manual/grammar.md
grep -Fq 'integer `/` is not supported; use `//` for floor division, or call `.to_float()` on both operands for true division' docs/manual/static-semantics.md
grep -Fq 'CPython-compatible divmod correction' docs/manual/execution-model.md
grep -Fq 'integer `.to_float()` converts to `float64`' docs/manual/execution-model.md
grep -Fq '| `left // right` | `FloorDiv.floor_div` |' docs/manual/generics-and-traits.md
grep -Fq 'trait FloorDiv[Rhs, Out]:' docs/manual/generics-and-traits.md
grep -Fq '`Duration` stores a signed 128-bit count of nanoseconds.' docs/manual/types.md
grep -Fq '| `Duration.ms` | `Duration.ms(value: int64) -> Duration`' docs/manual/api-index.md
grep -Fq '`Duration // int64`' docs/manual/expressions.md
grep -Fq '`//=` uses the builtin numeric or' docs/manual/statements.md
grep -Fq 'attempt * 1ms' docs/manual/concurrency.md
grep -Fq 'using at most six fractional digits and trimming trailing fractional zeros' docs/manual/execution-model.md
grep -Fq 'exact low and high 64-bit' docs/manual/execution-model.md
grep -Fq 'Deadline overflow never' docs/manual/execution-model.md
grep -Fq 'Omitting `process.run(timeout=...)` uses an internal absence marker' architecture_docs/decisions/0019-duration-conversion-and-timer-policy.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0019-duration-conversion-and-timer-policy.md
grep -Fq '0019-duration-conversion-and-timer-policy.md' architecture_docs/decisions/README.md
grep -Fq '| `random.Rng.next_int` | `next_int(lo: int64, hi: int64) -> int64`' docs/manual/api-index.md
grep -Fq 'result = rotl(s1 * 5, 7) * 9' docs/manual/randomness.md
grep -Fq 'threshold = 2^64 mod span' docs/manual/randomness.md
grep -Fq 'secure_bytes(0)' docs/manual/randomness.md
grep -Fq 'stable throughout the Aura 0.3.x' docs/manual/randomness.md
grep -Fq '3321214725393783201' docs/manual/randomness.md
grep -Fq 'The no-clone rule is transitive.' docs/manual/randomness.md
grep -Fq '`AU3007` rejects an operation that would duplicate non-cloneable state.' docs/manual/diagnostics.md
grep -Fq 'Generic clone-safety obligations are inferred from clone-producing operations in callable bodies.' docs/manual/generics-and-traits.md
grep -Fq 'A generic-to-generic call propagates the obligation to the caller.' docs/manual/generics-and-traits.md
grep -Fq "An explicit implementation MUST NOT strengthen its trait method's clone-safety contract." docs/manual/generics-and-traits.md
grep -Fq 'Clone-safety obligations survive module imports as part of the callable contract.' docs/manual/packages.md
grep -Fq 'Task and Queue handles are clone barriers' docs/manual/randomness.md
grep -Fq 'unsafe concrete specialization' docs/manual/diagnostics.md
grep -Fq 'code: "AU3007"' crates/aura-compiler/src/diag.rs
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0020-randomness-algorithm-and-security-boundary.md
grep -Fq '0020-randomness-algorithm-and-security-boundary.md' architecture_docs/decisions/README.md
grep -Fq '| `json.parse` | `parse(text: str) -> Result[json.Value, json.Error]` |' docs/manual/api-index.md
grep -Fq '| `json.dumps` | `dumps(value: json.Value, indent: Option[int64] = None) -> str` |' docs/manual/api-index.md
grep -Fq '`json.Value` is a move type' docs/manual/types.md
grep -Fq 'JSON input-data failures are typed `json.Error` values' docs/manual/diagnostics.md
grep -Fq 'recursive JSON parse/dump semantics' docs/manual/conformance.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0021-json-value-model-and-codec-policy.md
grep -Fq '0021-json-value-model-and-codec-policy.md' architecture_docs/decisions/README.md
grep -Fq 'Derived class/enum schemas and generated codecs remain deferred beyond Phase 6.' docs/manual/json.md
grep -Fq 'Derived class/enum schemas and generated codecs remain deferred beyond Phase 6.' tutorials/21-json.md
grep -Fq 'Derived class/enum schemas and generated codecs remain deferred beyond Phase 6.' architecture_docs/decisions/0021-json-value-model-and-codec-policy.md
test -s examples/json/dynamic_values.au
grep -Fq '`dynamic_values.au`' examples/README.md
grep -Fq '[21-json.md]' tutorials/README.md
grep -Fq '| `str.to_bytes` | `to_bytes() -> list[uint8]`' docs/manual/api-index.md
grep -Fq '| `str.from_bytes` | `from_bytes(bytes: list[uint8]) -> Result[str, bytes.Error]`' docs/manual/api-index.md
grep -Fq '| `bytes.base64_decode` | `base64_decode(text: str) -> Result[list[uint8], bytes.Error]`' docs/manual/api-index.md
grep -Fq '| `bytes.sha256_string` | `sha256_string(text: str) -> list[uint8]`' docs/manual/api-index.md
grep -Fq 'ordinary shared-borrow default' docs/manual/bytes.md
grep -Fq 'standard alphabet and canonical padding' docs/manual/bytes.md
grep -Fq 'InvalidHexDigit(index: int32, byte: uint8)' architecture_docs/decisions/0023-byte-vector-codecs-and-hashing-policy.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0023-byte-vector-codecs-and-hashing-policy.md
grep -Fq '0023-byte-vector-codecs-and-hashing-policy.md' architecture_docs/decisions/README.md
test -s examples/bytes/codecs_and_hashing.au
grep -Fq '`codecs_and_hashing.au`' examples/README.md
grep -Fq '[22-bytes.md]' tutorials/README.md
test -s examples/randomness/deterministic_rng.au
grep -Fq 'shuffle_rng.shuffle(values)' examples/randomness/deterministic_rng.au
grep -Fq '`deterministic_rng.au`' examples/README.md
test -s examples/generics/clone_safety_obligations.au
grep -Fq '`clone_safety_obligations.au`' examples/README.md
test -s examples/traits/clone_safety_contract.au
grep -Fq '`clone_safety_contract.au`' examples/README.md
grep -Fq '[20-randomness.md]' tutorials/README.md
test -s examples/concurrency/duration_arithmetic.au
grep -Fq 'Duration.minutes(-1) < 0ms' examples/concurrency/duration_arithmetic.au
grep -Fq 'Duration.seconds(2).to_ms()' examples/concurrency/duration_arithmetic.au
grep -Fq '`duration_arithmetic.au`' examples/README.md
test -s examples/basics/numbers.au
grep -Fq '`numbers.au`' examples/README.md
grep -Fq '[examples/basics/numbers.au]' tutorials/07-strings-and-numbers.md
grep -Fq 'Ordinary string literals use matching single or double quote delimiters' docs/manual/lexical-structure.md
assert_wrapped_text docs/manual/lexical-structure.md 'F-strings support the same escapes as ordinary strings and remain double-quoted.'
grep -Fq 'Counts Unicode scalar values in O(n)' docs/manual/api-index.md
grep -Fq 'Returns the UTF-8 byte count in O(1)' docs/manual/api-index.md
grep -Fq 'normalize a negative position' docs/manual/collections.md
grep -Fq 'once as `len() + index`' docs/manual/collections.md
grep -Fq '`insert` applies Python clamping.' docs/manual/collections.md
grep -Fq "unicode = 'A🎉'" examples/strings/string_methods.au
grep -Fq 'unicode.len()' examples/strings/string_methods.au
grep -Fq 'unicode.byte_len()' examples/strings/string_methods.au
grep -Fq 'values.insert(index=-1, value=2)' examples/collections/list_polish.au
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0030-len-and-str-builtins.md
grep -Fq '## B3.0-d amendment and ratification' architecture_docs/decisions/0030-len-and-str-builtins.md
grep -Fq '`len(value)` and `value.len()` are the same operation with the same static' architecture_docs/decisions/0030-len-and-str-builtins.md
grep -Fq 'result type, value, and ownership behavior: both produce `int64`' architecture_docs/decisions/0030-len-and-str-builtins.md
grep -Fq '0030-len-and-str-builtins.md' architecture_docs/decisions/README.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0004-string-semantics.md
grep -Fq -- '- Amended: 2026-07-26 (B3.0-d codec output safety ceiling clarification)' architecture_docs/decisions/0023-byte-vector-codecs-and-hashing-policy.md
grep -Fq 'The 2026-07-26 B3.0-d amendment preserves both the exact codec destination' architecture_docs/decisions/0023-byte-vector-codecs-and-hashing-policy.md
grep -Fq '| `str.len` | `len() -> int64` | Counts Unicode scalar values in O(n). |' docs/manual/api-index.md
grep -Fq '| `str.byte_len` | `byte_len() -> int64` | Returns the UTF-8 byte count in O(1). |' docs/manual/api-index.md
grep -Fq '| `list.len` | `len() -> int64` | Element count. |' docs/manual/api-index.md
grep -Fq '| `dict.len` | `len() -> int64` | Entry count. |' docs/manual/api-index.md
grep -Fq '| `set.len` | `len() -> int64` | Unique value count. |' docs/manual/api-index.md
grep -Fq 'so `len(value)` and `value.len()` have the' docs/manual/expressions.md
grep -Fq 'same static type and value. `str.byte_len()` likewise produces `int64`' docs/manual/expressions.md
grep -Fq 'Self::StringLen => "len() -> int64"' crates/aura-compiler/src/call.rs
grep -Fq 'Self::StringByteLen => "byte_len() -> int64"' crates/aura-compiler/src/call.rs
grep -Fq 'Self::VecLen => "len() -> int64"' crates/aura-compiler/src/call.rs
grep -Fq 'Self::MapLen => "len() -> int64"' crates/aura-compiler/src/call.rs
grep -Fq 'Self::SetLen => "len() -> int64"' crates/aura-compiler/src/call.rs
grep -Fq 'if len(text) != text_length:' crates/aura-compiler/tests/fixtures/run-pass/len_and_str.au
grep -Fq 'if len(values) != values_length:' crates/aura-compiler/tests/fixtures/run-pass/len_and_str.au
grep -Fq 'if len(ages) != ages_length:' crates/aura-compiler/tests/fixtures/run-pass/len_and_str.au
grep -Fq 'if len(tags) != tags_length:' crates/aura-compiler/tests/fixtures/run-pass/len_and_str.au
grep -Fq 'unicode_length: int64 = unicode_text.len()' crates/aura-compiler/tests/fixtures/run-pass/len_and_str.au
grep -Fq 'unicode_byte_length: int64 = unicode_text.byte_len()' crates/aura-compiler/tests/fixtures/run-pass/len_and_str.au
grep -Fq 'fn len_delegates_to_the_value_and_str_renders_it()' crates/aura-compiler/src/sema_tests.rs
grep -Fq 'fn mir_types_public_length_members_as_int64()' crates/aura-compiler/src/mir_tests.rs
grep -Fq 'fn analysis_and_completion_report_public_length_members_as_int64()' crates/aura-compiler/src/analysis_tests.rs
grep -Fq 'test("compiler bridge exposes all public length members as int64"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq '"free len(...) and the corresponding member length must have the same int64 type"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq '"```aura\nlen(value: str|list[T]|dict[K, V]|set[T]) -> int64\n```"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'test("compiler bridge includes list collection members in completions"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'test("compiler bridge includes str and dict builtin members in completions"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'test("compiler bridge exposes canonical set members and tuple-shaped dict items"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'test("compiler bridge exposes collection with_capacity constructors"' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'assert.equal(details.get("len"), "len() -> int64");' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq '"byte_len() -> int64"' tools/aura-language-server/test/compiler_bridge.test.js
test "$(grep -Fc '"len() -> int64"' tools/aura-language-server/test/compiler_bridge.test.js)" -ge 4
for builtin in list dict set str; do
  grep -Fq "  \"${builtin}\"," tools/aura-language-server/src/recovery.js
done
grep -Fq 'float64|str|list|dict|set|Duration' tools/vscode-aura/syntaxes/aura.tmLanguage.json
grep -Fq 'List positions and written slice endpoints use the `int64` index domain.' docs/manual/collections.md
grep -Fq 'end_index: int64 = items.len()' tutorials/02-bindings-and-types.md
grep -Fq 'for index in range(values.len()):' examples/collections/list_polish.au
test -s crates/aura-compiler/tests/fixtures/run-pass/list_len_range.au
grep -Fq 'for index in range(values.len()):' crates/aura-compiler/tests/fixtures/run-pass/list_len_range.au
grep -Fq 'fn direct_member_length_explicit_int32_cast_keeps_checked_narrowing()' crates/aura-compiler/src/native_codegen_tests.rs
grep -Fq 'execute `int64` member lengths, `len(value) == value.len()`' README.md
grep -Fq 'cast-free length-driven indexing' README.md
grep -Fq 'the `int64` results of `str.len()`, `str.byte_len()`, `list.len()`' examples/README.md
grep -Fq '`dict.len()`, and `set.len()`; `len(value) == value.len()`' examples/README.md
grep -Fq '`str.byte_len()`, `list.len()`, `dict.len()`, and `set.len()` all return' tutorials/README.md
grep -Fq 'host_count: int64 = hosts.len()' examples/basics/len_and_str.au
grep -Fq 'assert len(hosts) == host_count' examples/basics/len_and_str.au
grep -Fq 'byte_count: int64 = text.byte_len()' examples/basics/len_and_str.au
grep -Fq '`list.len()`, `dict.len()`, and `set.len()` all return `int64`.' tutorials/02-bindings-and-types.md
grep -Fq '`len()` and therefore satisfies `len(value) == value.len()`' tutorials/14-current-language-surface.md
grep -Fq 'Positions use `int64`. Negative positions count from the end:' docs/learn/collections.md
grep -Fq 'values.append(40)' docs/learn/collections.md
grep -Fq 'pub(crate) const MAX_CODEC_OUTPUT_LEN: usize = i32::MAX as usize;' crates/aura-compiler/src/bytes_codec.rs
grep -Fq 'fn checked_codec_output_len(output_len: Option<usize>) -> Result<usize, BytesResourceError>' crates/aura-compiler/src/bytes_codec.rs
grep -Fq 'Some(output_len) if output_len <= MAX_CODEC_OUTPUT_LEN => Ok(output_len)' crates/aura-compiler/src/bytes_codec.rs
grep -Fq 'RequestExceedsCeiling { requested: usize, maximum: usize }' crates/aura-compiler/src/randomness.rs
grep -Fq 'SecureRandomError::RequestExceedsCeiling' crates/aura-compiler/src/mir_runtime.rs
grep -Fq 'SecureRandomError::RequestExceedsCeiling' crates/aura-compiler/src/native_runtime.rs
for fixture in \
  random_secure_bytes_request_ceiling \
  random_secure_bytes_request_ceiling_i64_max; do
  test -s "crates/aura-compiler/tests/fixtures/run-fail/${fixture}.au"
  test -s "crates/aura-compiler/tests/fixtures/run-fail/${fixture}.diag"
  grep -Fq "\`${fixture}\`" docs/manual/conformance.md
  grep -Fq 'exceeds the secure-random request ceiling `2147483647`' \
    "crates/aura-compiler/tests/fixtures/run-fail/${fixture}.diag"
done
grep -Fq 'fn bytes_error_index_retains_the_int32_bytes_error_payload_boundary()' crates/aura-compiler/src/runtime_value_tests.rs
grep -Fq 'byte-codec error metadata exceeds the `bytes.Error` int32 payload range' crates/aura-compiler/src/runtime_value.rs
grep -Fq 'byte-codec error metadata exceeds the `bytes.Error` int32 payload range' crates/aura-compiler/src/runtime_value_tests.rs
grep -Fq 'Required malformed-data metadata above the `int32` maximum traps with' docs/manual/bytes.md
grep -Fq 'whose exact reported offset or length exceeds `2147483647` also traps with' docs/manual/bytes.md
grep -Fq 'secure-random request and resource ceiling. This ceiling bounds allocation' architecture_docs/decisions/0020-randomness-algorithm-and-security-boundary.md
grep -Fq 'or narrow the public `list` length domain or the result of `list.len()`.' docs/manual/randomness.md
grep -Fq 'independent of the public str and `list` length domains.' docs/manual/bytes.md
grep -Fq 'Its offsets and lengths remain `int32` as the current error-payload' docs/manual/bytes.md
grep -Fq 'Crossing this codec output/resource cap' docs/manual/current-limits.md
grep -Fq 'the public str and `list` length domains.' docs/manual/current-limits.md
grep -Fq 'resource and safety ceiling, independently of the public `list` length' docs/manual/current-limits.md

if rg -n '(byte_)?len\(\) (->|-&gt;) int32' \
  architecture_docs/decisions \
  docs/manual \
  docs/learn \
  tutorials \
  examples \
  README.md \
  examples/README.md \
  tutorials/README.md; then
  echo "maintained length surface still exposes an int32 len or byte_len result" >&2
  exit 1
fi

if rg -n -i '\b(?:len|byte_len)(?:\([^)]*\))?\b[^.\n]{0,80}\b(?:returns?|produces?|result type is|has (?:the )?(?:static )?(?:result )?type)\b[^.\n]{0,40}\bint32\b' \
  architecture_docs/decisions \
  docs/manual \
  docs/learn \
  tutorials \
  examples \
  README.md \
  examples/README.md \
  tutorials/README.md; then
  echo "maintained prose still describes len or byte_len as returning int32" >&2
  exit 1
fi

# Historical work notes and the explicitly historical language proposal retain
# superseded wording. Maintained surfaces must use operation-specific names and
# describe these numeric ceilings as resource boundaries, never as the maximum
# representable list/collection size.
if rg -n 'MAX_(VEC|VECTOR|COLLECTION)(_OUTPUT)?_(LEN|LENGTH|SIZE)|checked_(vec|vector|collection)(_output)?_(len|length|size)|SecureRandomError::(LengthTooLarge|RequestTooLarge)|Self::(LengthTooLarge|RequestTooLarge)|BytesResourceError::((Vec|Vector|Collection)(Length|Output)?TooLarge)' \
  crates/aura-compiler/src \
  crates/aura-compiler/tests; then
  echo "retired collection-limit implementation names remain in maintained code" >&2
  exit 1
fi

if rg -U -n -i 'maximum (representable )?(list|collection) (length|size)|(?:maximum|largest)[^.\n]{0,80}(?:list|collection)[^.\n]{0,50}(?:length|size)|(?:list|collection) (?:length|size)[^\n]{0,100}(?:is |are )?(?:capped|limited|bounded) (?:at|to|by) (?:2,147,483,647|2147483647)|(?:list|collection)[^.\n]{0,50}(?:length|size)[^.\n]{0,80}(?:i32::MAX|int32 maximum)|(?:2,147,483,647|2147483647)[^\n]{0,80}(?:maximum|representable)[^\n]{0,50}(?:list|collection)' \
  architecture_docs/decisions \
  docs/manual \
  docs/learn \
  tutorials \
  examples \
  README.md \
  examples/README.md \
  tutorials/README.md; then
  echo "maintained reference still derives a list or collection-length limit from a resource ceiling" >&2
  exit 1
fi
grep -Fq 'class enum def trait impl import from mut own indirect public extern opaque' docs/manual/lexical-structure.md
grep -Fq '| "own", "self"' docs/manual/grammar.md
grep -Fq 'Bare `self` is the shared receiver, `mut self` is mutable, and `own self` is consuming' docs/manual/grammar.md
grep -Fq '| `own self` | Consuming receiver.' docs/manual/classes.md
grep -Fq '`self: Type` is not a method receiver' architecture_docs/decisions/0005-method-receivers.md
grep -Fq '`own self` for by-value consumption' "$proposal_md"
grep -Fq '<code>own self</code> for by-value consumption' "$proposal_html"
grep -Fq '`value: T` | Shared access. An implementation may pass copy bits directly without changing the source contract.' docs/manual/functions.md
grep -Fq '`value: own T` | Owned argument' docs/manual/functions.md
grep -Fq 'caller-invisible temporary' docs/manual/functions.md
grep -Fq 'declaration-stable' docs/manual/generics-and-traits.md
grep -Fq 'An `impl` targeting any builtin type MUST' docs/manual/generics-and-traits.md
grep -Fq 'a collision' docs/manual/generics-and-traits.md
grep -Fq 'does not collide still implements and dispatches normally on a builtin target' docs/manual/generics-and-traits.md
grep -Fq 'NOT explicitly define or inherit a trait method whose name is a builtin member' docs/manual/generics-and-traits.md
grep -Fq 'builtin target members always retain builtin dispatch' docs/manual/generics-and-traits.md
grep -Fq 'for value in own values' docs/manual/statements.md
grep -Fq 'Queue iteration receives values' docs/manual/concurrency.md
test -s examples/concurrency/yield_now.au
grep -Fq 'yield_now()' examples/concurrency/yield_now.au
grep -Fq '| `yield_now` | `yield_now() -> None` |' docs/manual/api-index.md
grep -Fq 'places the current lightweight task back in the scheduler ready set' docs/manual/concurrency.md
grep -Fq '[examples/concurrency/yield_now.au](../examples/concurrency/yield_now.au)' tutorials/13-concurrency.md
grep -Fq '`yield_now` cooperative scheduling' docs/manual/conformance.md
grep -Fq 'The compiler inserts a cooperative scheduling check on every semantic loop' docs/manual/execution-model.md
grep -Fq 'as does `continue`; `break`, `return`, and another exit that leaves the loop do' docs/manual/execution-model.md
grep -Fq 'MIR execution amortizes the cooperative yield with 8 units' docs/manual/execution-model.md
grep -Fq 'Direct native code uses 4,096 units' docs/manual/execution-model.md
grep -Fq 'automatic checks do not inspect cancellation' docs/manual/current-limits.md
grep -Fq '## Automatic Loop Safepoints' tutorials/13-concurrency.md
grep -Fq 'compiler-inserted scheduling checks on every ordinary and `continue` loop backedge' docs/manual/conformance.md
grep -Fq 'fn loop_backedge_safepoints_prevent_timer_and_queue_starvation()' crates/aura/tests/cli.rs
grep -Fq 'fn loop_backedge_safepoints_prevent_socket_readiness_starvation()' crates/aura/tests/cli.rs
test -s benchmarks/scalable_runtime/sleeper_vs_hot_loop.au
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0032-guarded-lightweight-task-stacks.md
grep -Fq '0032-guarded-lightweight-task-stacks.md' architecture_docs/decisions/README.md
grep -Fq '| `start_with_stack` | `start_with_stack(bytes: int64, function, own ...) -> Task[T]` |' docs/manual/concurrency.md
grep -Fq '| `TaskGroup.start_soon_with_stack` | `start_soon_with_stack(bytes: int64, function, own ...) -> None` |' docs/manual/api-index.md
grep -Fq 'Values outside that range are rejected,' docs/manual/concurrency.md
grep -Fq 'rounded upward to the host page size and guard-protected' docs/manual/execution-model.md
grep -Fq 'distinct bounded protocol-step service' docs/manual/execution-model.md
grep -Fq 'This protocol-step pool is lazily initialized and shared by every' docs/manual/execution-model.md
grep -Fq 'and file reads use the generic blocking-I/O pool.' docs/manual/execution-model.md
grep -Fq 'PEM parsing and rustls construction run on protocol workers' docs/manual/execution-model.md
grep -Fq 'Phase 5.10 measurement at `181204b`' docs/manual/current-limits.md
grep -Fq '207,798,272 bytes of worst whole-process RSS and 198,787,072' docs/manual/current-limits.md
grep -Fq 'The runtime accepts larger task counts; 10,000 sleepers is the maintained' docs/manual/current-limits.md
grep -Fq '1,170,735,104,' docs/manual/current-limits.md
grep -Fq '1,921,531,904, and 2,001,305,600 bytes. Two of three exceed the 1.5 GiB' docs/manual/current-limits.md
grep -Fq 'The Phase 5.9 passing observation depended on' docs/manual/current-limits.md
grep -Fq 'The contractual 10,000-sleeper bound plus the timer, idle, starvation, and' docs/manual/current-limits.md
grep -Fq '`1.039673x` paired median wall-time ratio with `396.73%`' docs/manual/current-limits.md
grep -Fq 'A dynamic value outside that range and a stack-allocation or' docs/manual/diagnostics.md
grep -Fq '## Choosing A Custom Task Stack' docs/learn/concurrency.md
grep -Fq '### Per-task Stack Overrides' tutorials/13-concurrency.md
grep -Fq 'compiler bridge exposes guarded TaskGroup stack override completion and hover' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'ordinary starts use the safe 512 KiB default' crates/aura-compiler/src/call.rs
grep -Fq 'Accepted ADR-0032 guarded 512 KiB default task stacks' docs/manual/conformance.md
grep -Fq 'consuming a bare shared parameter reports that parameter `x` is' docs/manual/diagnostics.md
grep -Fq 'the current compiler emits at most one' docs/manual/diagnostics.md
grep -Fq 'constant tuple indexing that selects a non-copy element' docs/manual/diagnostics.md
grep -Fq 'corresponding `list` or `dict` indexed compound assignment' docs/manual/diagnostics.md
grep -Fq 'code: "AU3005"' crates/aura-compiler/src/diag.rs
grep -Fq 'code: "AU3006"' crates/aura-compiler/src/diag.rs
grep -Fq 'or: aura build -o <output>' crates/aura/src/main.rs
if grep -Fq 'aura build [-o <output>]' crates/aura/src/main.rs; then
  echo 'aura help still presents required build output as optional' >&2
  exit 1
fi
if grep -Fq '<check|run|build' crates/aura/src/main.rs; then
  echo 'aura help still advertises build through a form without required output' >&2
  exit 1
fi
grep -Fq 'Class field defaults cannot call user-defined functions' docs/manual/current-limits.md
test -s crates/aura-compiler/tests/fixtures/check-fail/class_field_default_user_function_not_supported.au
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0031-cli-backend-defaults.md
grep -Fq '0031-cli-backend-defaults.md' architecture_docs/decisions/README.md
grep -Fq '`aura run --backend mir` executes the lowered MIR and is the default.' docs/manual/cli-and-tooling.md
grep -Fq 'run_backend_parsing_defaults_to_mir_and_accepts_every_selector' crates/aura/src/main.rs
grep -Fq 'MIR and native direct-backend traps carry the same typed Aura call frames' docs/manual/current-limits.md
grep -Fq 'Those generated strings are not stored in structured `notes`' docs/manual/diagnostics.md
grep -Fq '"call_frames": []' docs/manual/diagnostics.md
grep -Fq '"task_ancestry": []' docs/manual/diagnostics.md
grep -Fq 'MIR-specific frame-note masking.' docs/manual/cli-and-tooling.md
if rg -n \
  'Structured frame-list fields are deferred|Native-backend Aura backtraces are deferred|direct native traps may omit|native frame parity.*unavailable|three-note parity carve-out' \
  README.md crates/aura/README.md docs/manual docs/learn tutorials \
  architecture_docs/07-mir-runtime.md \
  architecture_docs/08-native-codegen-and-runtime.md \
  architecture_docs/10-cli-and-build-tools.md; then
  echo 'maintained documentation reintroduced a stale deferred native-frame or parity-mask claim' >&2
  exit 1
fi
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0014-map-literals-and-indexing.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0015-explicit-and-default-argument-order.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0016-retained-noncopy-expression-borrows.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0017-iteration-source-selection.md
grep -Fq 'Checkpoint disposition (historical): ADR-0014 through ADR-0017 are Accepted.' work/task-board.md
grep -Fq 'retained_receiver_nested_consumption_repro' docs/manual/conformance.md
grep -Fq 'retained_argument_nested_consumption_repro' docs/manual/conformance.md
grep -Fq 'method_receiver_rejects_nested_argument_consumption' docs/manual/conformance.md
grep -Fq 'retained_parameter_rejects_nested_argument_consumption' docs/manual/conformance.md
for fixture in \
  retained_receiver_nested_consumption_repro \
  retained_argument_nested_consumption_repro \
  method_receiver_rejects_nested_argument_consumption \
  retained_parameter_rejects_nested_argument_consumption; do
  test -s "crates/aura-compiler/tests/fixtures/check-fail/${fixture}.au"
  test -s "crates/aura-compiler/tests/fixtures/check-fail/${fixture}.diag"
done
for namespace in io fs net process bytes json sys path toml log trace metrics random; do
  grep -Fq -- "- \`${namespace}\`" tutorials/14-current-language-surface.md
done
grep -Fq 'default temporary lives until the call completes' docs/manual/execution-model.md
grep -Fq 'append(value: own T)' docs/manual/api-index.md
grep -Fq 'update(other: own dict[K, V])' docs/manual/api-index.md
grep -Fq 'put(value: own T' docs/manual/api-index.md
grep -Fq 'result_or(default: own T' docs/manual/api-index.md
grep -Fq 'start(function, own ...) -> Task[T]' docs/manual/api-index.md
grep -Fq 'restart: own process.RestartPolicy' docs/manual/api-index.md
grep -Fq 'bare `for value in queue:` form' architecture_docs/decisions/0006-parameter-and-loop-ownership-defaults.md
grep -Fq 'for name in own names' tutorials/06-ownership-and-borrowing.md
grep -Fq 'def handle(stream: own net.TcpStream)' docs/manual/network.md
grep -Fq 'def handle(stream: own net.TcpStream)' docs/learn/io-process-networking.md
grep -Fq 'def serve(addresses: Queue[str])' tutorials/19-io-and-networking.md
grep -Fq 'Listeners and other live network resources are not `Transfer`' tutorials/19-io-and-networking.md
grep -Fq 'def process_file(handle: own FileHandle)' tutorials/12-error-propagation.md
grep -Fq '`Queue[T]` is a copy handle to shared runtime state.' tutorials/06-ownership-and-borrowing.md
grep -Fq '`Task[T]` is always safe' tutorials/06-ownership-and-borrowing.md
grep -Fq 'admit only structurally' tutorials/06-ownership-and-borrowing.md
grep -Fq 'first result' tutorials/06-ownership-and-borrowing.md
grep -Fq 'declaration-stable' "$proposal_html"
grep -Fq 'Queue iteration receives each item already owned' "$proposal_html"
grep -Fq 'const MAX_FILESYSTEM_READ_BYTES: usize = 256 * 1024 * 1024;' crates/aura-compiler/src/runtime_value.rs
grep -Fq 'const MAX_STREAM_READ_BYTES: usize = 64 * 1024 * 1024;' crates/aura-compiler/src/runtime_value.rs
grep -Fq 'const MAX_HTTP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;' crates/aura-compiler/src/runtime_value.rs
grep -Fq 'capped at 256 MiB of remaining content' docs/manual/filesystem.md
grep -Fq 'Filesystem one-shot reads and `fs.File` whole-file reads are capped at 256 MiB' docs/manual/current-limits.md
grep -Fq 'Process-pipe and captured-output reads plus TCP, Unix, and TLS whole/bounded reads remain capped at 64 MiB.' docs/manual/current-limits.md
grep -Fq 'Incoming HTTP parsing accepts at most 64 headers and 16 MiB of wire data per message' docs/manual/current-limits.md
grep -Fq 'Each `process.run` captured stream and each whole-pipe read is capped at 64 MiB' docs/manual/process.md
grep -Fq 'Whole TCP text reads, TCP line reads, and individual byte-count reads are capped at 64 MiB' docs/manual/network.md
grep -Fq 'Incoming parsed HTTP messages are capped at 16 MiB of wire data and 64 headers.' docs/manual/network.md
grep -Fq 'This stream ceiling is independent of the larger filesystem whole-read limit.' docs/manual/process.md
grep -Fq 'one-shot and `fs.File` whole-file reads are capped at 256 MiB' tutorials/14-current-language-surface.md
grep -Fq 'capped at 64 MiB; TLS certificate, private-key, and CA-file loading uses the' tutorials/14-current-language-surface.md
grep -Fq 'incoming HTTP parsing is capped at 16 MiB of wire data per message' tutorials/14-current-language-surface.md
grep -Fq 'One-shot `fs.read_to_string` and `fs.read_bytes` are capped at 256 MiB.' docs/learn/io-process-networking.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0018-fixed-resource-read-limits.md
grep -Fq 'fixed 256 MiB whole-read policy is accepted under ADR-0018' docs/manual/filesystem.md
grep -Fq 'fixed resource-cap policy recorded by ADR-0018 is Accepted' docs/manual/network.md
grep -Fq 'cap is Accepted under ADR-0018' docs/manual/control-plane.md
grep -Fq 'fixed stream-cap policy recorded by ADR-0018 is Accepted' docs/manual/process.md

if rg -U -n -i '(filesystem|fs\.File|whole[- ]file reads?|file reads?)[^.\n]{0,120}(?:is |are )?(?:capped|limited|bounded) (?:at|to) 64 MiB|64 MiB (?:filesystem|fs\.File|whole[- ]file|whole[- ]read|file-read) (?:cap|ceiling|limit)' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "filesystem reference still describes the retired 64 MiB whole-read limit" >&2
  exit 1
fi

if rg -ni '(http|parser|message)[^\n]*1 MiB|1 MiB[^\n]*(http|parser|message)' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "reference still describes the retired 1 MiB HTTP parser limit" >&2
  exit 1
fi

if grep -Fq -- '- `Queue[T]`, `Task[T]`, `TaskGroup`' tutorials/06-ownership-and-borrowing.md; then
  echo "ownership tutorial still classifies Queue and Task copy handles as move types" >&2
  exit 1
fi

if rg -n 'no (integer )?floor division|integer division truncates toward zero|Result\.Ok\([^)]* / [^)]*\)' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "reference still describes retired integer division behavior" >&2
  exit 1
fi

if rg -n 'There is no `FloorDiv`|has no `FloorDiv`|no `FloorDiv` operator trait|Duration arithmetic[^.\n]*(not implemented|unavailable)|signed 128-bit milliseconds|normalized to milliseconds|DurationLiteral\(i128\)[^.\n]*milliseconds' \
  architecture_docs \
  docs/manual \
  tutorials \
  examples; then
  echo "reference still describes the retired Duration or FloorDiv surface" >&2
  exit 1
fi

if rg -U -n 'Newlines are not continuation|ordinary calls remain on one\s+physical line|Collection literals[^.]*remain on one\s+physical line|Because general delimiter continuation does not exist|only maintained multiline accommodation inside a surrounding delimiter|general multiline\s+continuation is unavailable|general multiline literals are\s+unavailable|general\s+delimiter-based line continuation is unavailable|general multiline delimiters' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "reference still describes delimiter continuation as unavailable" >&2
  exit 1
fi

if rg -n 'expressions do not include tuples|tuples, callable types|Callable, closure, tuple|tuples and destructuring|Destructuring assignment or loop targets|detached spawn, tuples, attributes|tuple punctuation' \
  docs/manual \
  tutorials; then
  echo "reference still describes the implemented tuple kernel as unavailable" >&2
  exit 1
fi

# Historical work notes intentionally retain the earlier provisional boundary.
if rg -U -n -i 'identity rule does not add tuple value|does not add tuple equality|tuple equality or ordering|tuple equality, ordering|methods, equality, ordering|equality/order rejection|Provisional\s+under ADR-0026|provisional\s+extent recorded by ADR-0026|ADR-0026[^.\n]*Provisional|These choices remain Provisional|^- Status: Provisional$' \
  architecture_docs/decisions/0026-minimal-tuples.md \
  docs/manual \
  tutorials \
  examples \
  README.md \
  examples/README.md \
  tutorials/README.md; then
  echo "maintained reference still describes tuple equality as rejected or ADR-0026 as provisional" >&2
  exit 1
fi

if rg -n 'secure_float' \
  architecture_docs \
  docs/manual \
  tutorials \
  examples; then
  echo "reference exposes the unapproved secure_float API" >&2
  exit 1
fi

if rg -n 'not a dynamic JSON tree|Dynamic JSON trees[^.\n]*unavailable|runtime integration[^.\n]*in progress|executable-reference integration[^.\n]*in progress|target contract rather than claiming' \
  architecture_docs/decisions/0021-json-value-model-and-codec-policy.md \
  docs/manual/json.md \
  docs/manual/control-plane.md \
  tutorials/21-json.md; then
  echo "reference still describes the implemented recursive JSON surface as unavailable or integration-only" >&2
  exit 1
fi

if rg -U -n '`//`[^\n]*has no\s+operator trait|`//` is deliberately absent' \
  docs/manual \
  tutorials; then
  echo "reference still rejects the FloorDiv extension point" >&2
  exit 1
fi

if rg -ni 'defaults? to (`|<code>)?int32|default for most integer work|use (`|<code>)?int32[^[:space:]]* and (`|<code>)?float64[^[:space:]]* by default|no (unsuffixed|bare) (`|<code>)?int' \
  docs/manual \
  tutorials \
  "$proposal_md" \
  "$proposal_html"; then
  echo "reference still describes the retired int32 default or rejects the int alias" >&2
  exit 1
fi

if rg -n 'Strings use double quotes|Strings are double-quoted|`STRING` is a double-quoted|Single-quoted, triple-quoted' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "reference still describes ordinary strings as double-quoted only" >&2
  exit 1
fi

# Batch S1 S4.1/S4.6: exact string forms and the closed static formatting
# grammar move with the compiler, both backends, teaching track, and editor.
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0046-string-literals-and-format-specifications.md
grep -Fq 'Three matching quotes create an exact multiline string' docs/manual/lexical-structure.md
grep -Fq 'The grammar is `[[fill]align][sign][width][,][.precision][type]`' docs/manual/lexical-structure.md
grep -Fq 'Accepted ADR-0046 exact triple-quoted and raw string forms' docs/manual/conformance.md
test -s examples/strings/literal_forms_and_formatting.au
test -s crates/aura-compiler/tests/fixtures/run-pass/string_literal_forms_and_format_specs.au
test -s crates/aura-compiler/tests/fixtures/check-fail/fstring_spec_type_mismatch.diag
grep -Fq 'compiler bridge analyzes exact string forms and typed format specifications' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'extension grammar and snippets cover Aura 0.3 string forms' tools/vscode-aura/test/package.test.js

if rg -n '`self` -- by-value|plain `self` receiver|`self` consumes|\| `self` \| Consume' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "reference still describes bare self as a consuming receiver" >&2
  exit 1
fi

if rg -n 'for x in expr:` consumes|for value in values:` \| Consumes|`for value in names` iterates by value|dict\.get[^\n]*(takes|consumes) (its )?key by value|Every task target parameter must be by value|target.s ordinary parameters must be by value|TaskGroup[^\n]*(do not|does not) yet support shared parameters' \
  docs/manual \
  tutorials \
  docs/learn \
  "$proposal_md" \
  "$proposal_html"; then
  echo "reference still describes retired parameter, loop, lookup, or task-capture ownership behavior" >&2
  exit 1
fi

if rg -U -n 'rejects an unconstrained clone-producing generic operation|A polymorphic\s+clone-producing operation[^.]*is rejected|`\.clone\(\)` produces an explicit independent copy of a move type|Use `get\([^`]*\)` for an explicit cloned optional read or `remove\([^`]*\)` to transfer' \
  docs/manual \
  tutorials \
  docs/learn; then
  echo "reference still describes the retired eager generic rejection or blanket clone/get behavior" >&2
  exit 1
fi

if rg -n 'maintained interpreter|tree-walk interpreter' docs/manual; then
  echo "manual still describes the removed interpreter as maintained" >&2
  exit 1
fi

# Batch 4 checkpoint ratification plus the Batch 5 select-payload closure:
# guarded stacks, structural Transfer, typed select, and native structured
# frames are Accepted everywhere they are named.
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0033-structural-transfer-and-task-results.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0036-native-structured-runtime-frames.md
grep -Fq 'Accepted ADR-0033 structural Transfer' docs/manual/conformance.md
grep -Fq 'Accepted ADR-0036 typed runtime call frames' docs/manual/conformance.md
for accepted_adr in \
  architecture_docs/decisions/0032-guarded-lightweight-task-stacks.md \
  architecture_docs/decisions/0033-structural-transfer-and-task-results.md \
  architecture_docs/decisions/0034-typed-heterogeneous-select.md \
  architecture_docs/decisions/0035-configurable-blocking-io-pool.md \
  architecture_docs/decisions/0036-native-structured-runtime-frames.md; do
  if grep -Fq '## Provisional decision' "$accepted_adr"; then
    echo "accepted ADR retains a provisional decision heading: $accepted_adr" >&2
    exit 1
  fi
done
if rg -U -n 'Provisional\s+ADR-003(2|3|4|5|6)' \
  architecture_docs \
  docs/manual \
  docs/learn \
  tutorials \
  README.md; then
  echo "maintained reference still describes an accepted ADR as provisional" >&2
  exit 1
fi

# Batch 6 opens by accepting the completed value-capture design. Phase 6.5 then
# implements the accepted place-loan design for Aura 0.3 across the maintained
# compiler, tooling, and reference surfaces.
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0037-expression-closures-and-value-capture.md
grep -Fq '0037-expression-closures-and-value-capture.md) — Accepted at the Batch 6 opening checkpoint' architecture_docs/decisions/README.md
grep -Fq 'Accepted ADR-0037 expression closures' docs/manual/conformance.md
grep -Fq 'implemented under Accepted ADR-0037' docs/manual/closures.md

grep -Fq -- '- Status: Implemented' architecture_docs/decisions/0038-place-based-loans-and-views.md
grep -Fq -- '- Implementation: Complete' architecture_docs/decisions/0038-place-based-loans-and-views.md
grep -Fq -- '- Version target: 0.3' architecture_docs/decisions/0038-place-based-loans-and-views.md
test "$(grep -Ec '^[0-9]+\. \*\*Answer: Yes\.\*\*' architecture_docs/decisions/0038-place-based-loans-and-views.md)" -eq 10
grep -Fq '0038-place-based-loans-and-views.md) — Implemented for Aura 0.3' architecture_docs/decisions/README.md
rg -U -q 'ADR-0038 implements explicit,\s*>?\s*exhaustive shared, mutable, and owned closure capture lists for Aura 0\.3\.' architecture_docs/decisions/0013-callable-sequencing-and-ownership.md
grep -Fq -- '- Implemented design amendment: ADR-0038 (explicit loan capture lists in Aura 0.3)' architecture_docs/decisions/0037-expression-closures-and-value-capture.md
grep -Fq 'Implemented ADR-0038 place-based loans and views' docs/manual/conformance.md

if rg -U -n 'Provisional\s+ADR-0037|under Provisional\s+ADR-0037|ADR-0037[^.\n]*Provisional' \
  architecture_docs/decisions/0037-expression-closures-and-value-capture.md \
  docs/manual \
  docs/learn \
  tutorials \
  README.md; then
  echo "maintained reference still describes accepted ADR-0037 as provisional" >&2
  exit 1
fi

if rg -n 'post-ratification loan/view design|future first-class loan or view design must specify|pending the separate loan/view design|waits for the separate loan/view design|Any future loan or view design will be' \
  architecture_docs/decisions/0013-callable-sequencing-and-ownership.md \
  architecture_docs/decisions/0037-expression-closures-and-value-capture.md \
  docs/manual \
  docs/learn \
  tutorials \
  README.md; then
  echo "maintained reference still describes accepted ADR-0038 as an undecided future design" >&2
  exit 1
fi

# Batch 6 B6.0-c keeps the callable documentation aligned with the implemented
# repeatable-closure contract at the two compiler-known callback families.
rg -U -q 'The worker may be a capture-free function value or a repeatable\s+value-capturing closure\.' docs/manual/control-plane.md
rg -U -q 'The helper can therefore reuse one repeatable capturing closure across all\s+attempts without consuming its environment\.' docs/manual/control-plane.md
grep -Fq 'The callback must be repeatable.' docs/manual/collections.md
rg -U -q 'An inner lambda without a list cannot capture a bare parameter of its enclosing\s+lambda\. An explicit bare entry creates a shared contained reborrow' docs/manual/closures.md

# Phase 5.8: Accepted ADR-0034 is an implemented builtin call, not the
# historical statement form. Keep the normative reference, teaching track,
# runnable example, and ADR ledger synchronized.
test -s architecture_docs/decisions/0034-typed-heterogeneous-select.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0034-typed-heterogeneous-select.md
grep -Fq '0034-typed-heterogeneous-select.md) — Accepted after the Batch 5 nested-payload closure' architecture_docs/decisions/README.md
test -s examples/concurrency/typed_select.au
grep -Fq 'execute typed heterogeneous Queue, Task, and relative-deadline selection' README.md
grep -Fq '`typed_select.au`' examples/README.md
grep -Fq '`select`' docs/manual/index.md
grep -Fq '`select(...)` provides a typed heterogeneous Queue/Task/deadline wait' docs/manual/status-and-compatibility.md
grep -Fq '`select(source, ...)` evaluates its Queue, Task, and relative-Duration sources' docs/manual/execution-model.md
grep -Fq '`select` | `select(source, ...) -> SelectOutcome[Q, T]`' docs/manual/api-index.md
grep -Fq 'Accepted ADR-0034 typed heterogeneous `select`' docs/manual/conformance.md
grep -Fq 'Use the builtin `select(...)` when one wait mixes queues, tasks, and a relative' tutorials/13-concurrency.md
grep -Fq 'explicit-stack start methods, typed `select(...)` over Queue, Task, and' tutorials/README.md
grep -Fq 'typed `select(queue_or_task_or_duration, ...)`' tutorials/14-current-language-surface.md
test -s crates/aura-compiler/tests/fixtures/parse-fail/select_statement_form_rejected.au
test -s crates/aura-compiler/tests/fixtures/parse-fail/select_statement_form_rejected.diag
grep -Fq 'select:' crates/aura-compiler/tests/fixtures/parse-fail/select_statement_form_rejected.au
grep -Fq 'error[AU1101]: expected Newline' crates/aura-compiler/tests/fixtures/parse-fail/select_statement_form_rejected.diag
grep -Fq 'compiler bridge exposes typed select inference, hover, and outcome completions' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'compiler bridge preserves typed select diagnostic codes and guidance' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'assert.ok(names.includes("select"));' tools/aura-language-server/test/recovery.test.js
grep -Fq 'assert.ok(names.includes("SelectOutcome"));' tools/aura-language-server/test/recovery.test.js
grep -Fq '"SelectOutcome",' tools/vscode-aura/test/package.test.js

if rg -n 'language-level `select`|instead of the removed `select` statement|typed heterogeneous selection[^.\n]*(unavailable|not implemented)' \
  README.md \
  docs/manual \
  docs/learn \
  tutorials \
  examples/README.md; then
  echo "maintained reference still describes typed select as unavailable" >&2
  exit 1
fi

# Phase 5.9: Accepted ADR-0035 configures the generic blocking-I/O pool
# without changing the independent protocol-step or JSON services. Pin the
# operational controls, pending-only capacity accounting, admission boundary,
# backend preflight, and the retained stuck-worker limitation.
test -s architecture_docs/decisions/0035-configurable-blocking-io-pool.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0035-configurable-blocking-io-pool.md
grep -Fq '0035-configurable-blocking-io-pool.md) — Accepted after the Batch 5 default-parallel watchdog closure' architecture_docs/decisions/README.md
grep -Fq '`AURA_BLOCKING_WORKERS=<positive integer>`' architecture_docs/07-mir-runtime.md
grep -Fq 'selects its exact worker count without clamping.' architecture_docs/07-mir-runtime.md
grep -Fq 'resulting configuration is immutable for the process lifetime.' architecture_docs/07-mir-runtime.md
grep -Fq 'Valid preflight creates no blocking-pool worker threads.' architecture_docs/07-mir-runtime.md
grep -Fq 'submission creates the complete worker set, which is reused until process exit;' architecture_docs/07-mir-runtime.md
grep -Fq 'production has no Aura shutdown or join surface for this pool.' architecture_docs/07-mir-runtime.md
grep -Fq '`AURA_BLOCKING_QUEUE_CAPACITY=<positive integer>` optionally bounds accepted' docs/manual/execution-model.md
grep -Fq 'The generic pool is also process-global.' docs/manual/execution-model.md
grep -Fq 'MIR execution, direct execution, and launched standalone native binaries' docs/manual/cli-and-tooling.md
grep -Fq 'a non-Unicode' docs/manual/cli-and-tooling.md
grep -Fq 'value is displayed lossily.' docs/manual/cli-and-tooling.md
grep -Fq 'timeout or cancellation prevents submission; after insertion' docs/manual/current-limits.md
grep -Fq 'Full-queue admission is FIFO and scheduler-aware' docs/manual/status-and-compatibility.md
grep -Fq 'Accepted ADR-0035 blocking-I/O worker configuration' docs/manual/conformance.md
grep -Fq '`AU4006` reports invalid process runtime configuration' docs/manual/diagnostics.md
grep -Fq 'limits accepted pending backlog, not' docs/learn/io-process-networking.md
grep -Fq 'cannot guarantee unrelated blocking-I/O progress while' docs/learn/io-process-networking.md
grep -Fq 'cannot interrupt accepted work or guarantee' tutorials/19-io-and-networking.md
grep -Fq 'unrelated blocking-I/O progress while every worker remains stuck' tutorials/19-io-and-networking.md
grep -Fq 'AURA_BLOCKING_WORKERS' README.md
grep -Fq 'AURA_BLOCKING_QUEUE_CAPACITY' CHANGELOG.md

if rg -n 'blocking pool uses 2 through 8 host threads|no 0\.1 configuration or queue backpressure|count and queue are not configurable|current queue has no admission bound|configurable blocking-pool sizing[^.\n]*unavailable|bounded blocking service' \
  README.md \
  architecture_docs/07-mir-runtime.md \
  docs/manual \
  docs/learn \
  tutorials; then
  echo "maintained reference still describes the pre-Phase-5.9 blocking-I/O pool contract" >&2
  exit 1
fi

# Phase 7.1: Accepted ADR-0039 keeps eager comprehensions synchronized across
# grammar, semantics, diagnostics, maintained examples, and teaching material.
test -s architecture_docs/decisions/0039-comprehensions.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0039-comprehensions.md
grep -Fq '0039-comprehensions.md) — Accepted for Aura 0.2 in Batch 6, Phase 7.1' architecture_docs/decisions/README.md
grep -Fq 'list-comprehension' docs/manual/grammar.md
grep -Fq 'comprehension-clauses' docs/manual/grammar.md
grep -Fq 'Nested clauses execute in outer-major order.' docs/manual/collections.md
grep -Fq 'Filters run from left to right.' docs/manual/collections.md
grep -Fq 'A dictionary comprehension evaluates its key before its value.' docs/manual/collections.md
grep -Fq 'No comprehension target is visible after the closing' docs/manual/names-and-scopes.md
grep -Fq 'A dictionary captures its key before evaluating its value.' docs/manual/execution-model.md
grep -Fq 'generator expressions are unavailable; use an eager owned list comprehension or an explicit loop' docs/manual/diagnostics.md
grep -Fq 'comprehensions use bare iteration; remove `mut` or `own` and write `for name in values`' docs/manual/diagnostics.md
grep -Fq 'Accepted ADR-0039 eager owned comprehensions' docs/manual/conformance.md
test -s examples/collections/comprehensions.au
grep -Fq 'for left in values if left < 3' examples/collections/comprehensions.au
grep -Fq '`comprehensions.au`' examples/README.md
grep -Fq 'examples/collections/comprehensions.au' README.md
grep -Fq 'examples/collections/comprehensions.au' tutorials/02-bindings-and-types.md
grep -Fq 'examples/collections/comprehensions.au' tutorials/04-control-flow.md
grep -Fq 'Comprehensions use the bare form' tutorials/06-ownership-and-borrowing.md
grep -Fq 'comprehensions with filters and nested clauses' tutorials/README.md

# Phase 7.2: Accepted ADR-0040 keeps the owned list/str slice contract
# synchronized across syntax, diagnostics, runtime fixtures, reference, and
# maintained teaching material.
test -s architecture_docs/decisions/0040-owned-vec-and-string-slices.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0040-owned-vec-and-string-slices.md
grep -Fq '0040-owned-vec-and-string-slices.md) — Accepted for Aura 0.2 in Batch 6, Phase 7.2' architecture_docs/decisions/README.md
grep -Fq '| "[", [ expression ], ":", [ expression ], "]" ;' docs/manual/grammar.md
grep -Fq 'both endpoints must be in the' architecture_docs/decisions/0040-owned-vec-and-string-slices.md
grep -Fq 'slice steps are unavailable; use an explicit loop to select a stride' docs/manual/diagnostics.md
grep -Fq 'slice assignment is unavailable because slices are owned copies; mutate the source by index or build a new value' docs/manual/diagnostics.md
grep -Fq 'Aura deliberately differs from Python here: slice endpoints are **not' docs/manual/expressions.md
grep -Fq 'One-colon slices return fresh owned lists.' docs/manual/collections.md
grep -Fq 'Accepted ADR-0040 owned list/str slices' docs/manual/conformance.md
test -s crates/aura-compiler/tests/fixtures/parse-pass/owned_slices.au
test -s crates/aura-compiler/tests/fixtures/check-pass/slice_static_semantics.au
test -s crates/aura-compiler/tests/fixtures/run-pass/owned_list_string_slices.au
test -s crates/aura-compiler/tests/fixtures/run-pass/owned_list_string_slices.stdout
for fixture in \
  slice_step_explicit \
  slice_step_fully_omitted \
  slice_step_omitted_bounds \
  slice_step_omitted_value; do
  test -s "crates/aura-compiler/tests/fixtures/parse-fail/${fixture}.au"
  test -s "crates/aura-compiler/tests/fixtures/parse-fail/${fixture}.diag"
  grep -Fq 'slice steps are unavailable; use an explicit loop to select a stride' \
    "crates/aura-compiler/tests/fixtures/parse-fail/${fixture}.diag"
done
for fixture in \
  slice_assignment_unavailable \
  slice_compound_assignment_unavailable; do
  test -s "crates/aura-compiler/tests/fixtures/parse-fail/${fixture}.au"
  test -s "crates/aura-compiler/tests/fixtures/parse-fail/${fixture}.diag"
  grep -Fq 'slice assignment is unavailable because slices are owned copies; mutate the source by index or build a new value' \
    "crates/aura-compiler/tests/fixtures/parse-fail/${fixture}.diag"
done
for fixture in \
  list_slice_start_out_of_bounds \
  list_slice_end_out_of_bounds \
  list_slice_reversed_bounds \
  string_slice_start_out_of_bounds \
  string_slice_end_out_of_bounds \
  string_slice_reversed_bounds; do
  test -s "crates/aura-compiler/tests/fixtures/run-fail/${fixture}.au"
  test -s "crates/aura-compiler/tests/fixtures/run-fail/${fixture}.diag"
  grep -Fq 'error[AU4003]' \
    "crates/aura-compiler/tests/fixtures/run-fail/${fixture}.diag"
done
test -s examples/collections/slices.au
grep -Fq 'celebration = text[1:2]' examples/collections/slices.au
grep -Fq '`slices.au`' examples/README.md
grep -Fq 'examples/collections/slices.au' README.md
grep -Fq 'examples/collections/slices.au' tutorials/02-bindings-and-types.md
grep -Fq 'owned list/str slices' tutorials/README.md

if rg -n 'slicing waits for Phase 7|slice surface is reserved for Phase 7|Collection slicing is reserved|integer indexing or slicing (is|are) not supported on `str`|integer indexing and slicing are not defined for `str`' \
  architecture_docs/decisions \
  docs/manual \
  docs/learn \
  tutorials; then
  echo "maintained reference still describes implemented owned slicing as future work" >&2
  exit 1
fi

# Phase 7.3: Accepted ADR-0041 keeps the exact contiguous numeric Array and
# explicit integer arithmetic-mode contract synchronized with the maintained
# reference, teaching track, editor regression surface, example, and benchmark
# evidence protocol.
test -s architecture_docs/decisions/0041-contiguous-numeric-arrays.md
grep -Fq -- '- Status: Accepted' architecture_docs/decisions/0041-contiguous-numeric-arrays.md
grep -Fq '0041-contiguous-numeric-arrays.md) — Accepted for Aura 0.2 in Batch 6, Phase 7.3' architecture_docs/decisions/README.md
grep -Fq 'array [ expression { , expression } ]' docs/manual/numeric-arrays.md
grep -Fq 'copies its scalar elements, and leaves the' docs/manual/numeric-arrays.md
grep -Fq 'shared source list usable' docs/manual/numeric-arrays.md
grep -Fq 'method `set`, direct indexed read, and direct indexed assignment' docs/manual/numeric-arrays.md
grep -Fq '`Some(old_value)` on success and traps on an invalid coordinate or rank.' docs/manual/expressions.md
grep -Fq 'Floating reductions visit elements' docs/manual/numeric-arrays.md
grep -Fq 'left to right with deterministic dtype rounding and propagate NaN.' docs/manual/numeric-arrays.md
grep -Fq '`Array[T]` is non-Copy and cloneable.' docs/manual/numeric-arrays.md
grep -Fq 'always structurally `Transfer`' docs/manual/numeric-arrays.md
grep -Fq '`AU4005` reports shape-product/element-count overflow and allocation failure.' docs/manual/numeric-arrays.md
grep -Fq '`AU4002` reports checked integer Array arithmetic overflow.' docs/manual/numeric-arrays.md
grep -Fq '`AU4004` reports floating Array division when any divisor is zero.' docs/manual/numeric-arrays.md
grep -Fq '`AU4007` (`numeric array shape or reduction violation`) reports:' docs/manual/numeric-arrays.md
grep -Fq 'Accepted ADR-0041 contiguous numeric Arrays and explicit integer modes' docs/manual/conformance.md
grep -Fq '`AU4002` checked Array overflow and `AU4004` floating Array zero-divisor failures' docs/manual/conformance.md
grep -Fq 'error[AU4002]: array addition overflowed at flat index 0' \
  crates/aura-compiler/tests/fixtures/run-fail/array_checked_overflow.diag
grep -Fq 'error[AU4004]: array division has a zero divisor at flat index 0' \
  crates/aura-compiler/tests/fixtures/run-fail/array_division_by_zero.diag
grep -Fq 'Phase 7.3 adds global contiguous `Array[T]` values under Accepted ADR-0041.' docs/manual/status-and-compatibility.md
test -s examples/numbers/numeric_arrays.au
grep -Fq 'Array[int32].from_list' examples/numbers/numeric_arrays.au
grep -Fq '`numeric_arrays.au`' examples/README.md
grep -Fq 'examples/numbers/numeric_arrays.au' README.md
grep -Fq 'examples/numbers/numeric_arrays.au' tutorials/02-bindings-and-types.md
grep -Fq 'owned contiguous numeric `Array[T]` values' tutorials/README.md
grep -Fq 'compiler bridge exposes the global numeric Array surface and result types' tools/aura-language-server/test/compiler_bridge.test.js
grep -Fq 'bundled language server preserves numeric Array hover completion and diagnostics' tools/vscode-aura/test/server_protocol.test.js
grep -Fq 'names.includes("Array")' tools/aura-language-server/test/recovery.test.js
grep -Fq '"Array",' tools/vscode-aura/test/package.test.js
test -s benchmarks/numeric_arrays/README.md
test -s benchmarks/numeric_arrays/numpy_reference.py
test -s benchmarks/numeric_arrays/float64_add.au
test -s benchmarks/numeric_arrays/float64_sum.au
test -s scripts/bench-numeric-arrays.py
test -s scripts/test_bench_numeric_arrays.py
grep -Fq '"bench:numeric-arrays"' package.json
grep -Fq '"test:bench-numeric-arrays"' package.json
grep -Fq 'single-thread environment and records release evidence.' benchmarks/numeric_arrays/README.md
grep -Fq '`scripts/benchmark_process.py`, which owns process-group launch,' benchmarks/numeric_arrays/README.md
grep -Fq 'process that remains at or above 50% CPU in two snapshots 0.25 seconds apart,' benchmarks/numeric_arrays/README.md
python3 -m unittest scripts/test_bench_numeric_arrays.py

if rg -n '\bSIMD\b' \
  architecture_docs/decisions/0041-contiguous-numeric-arrays.md \
  docs/manual/numeric-arrays.md \
  benchmarks/numeric_arrays/README.md; then
  echo "Phase 7.3 docs claim an unratified SIMD implementation detail" >&2
  exit 1
fi

# The Batch S1 checkpoint is closed: ADR-0045's implemented testing contract is
# binding, and ADR-0049 formally defers class patterns to a future dedicated
# design rather than leaving their disposition provisional.
grep -Fq -- '- Status: Accepted' \
  architecture_docs/decisions/0045-testing-framework-and-assertion-introspection.md
grep -Fq 'Accepted for Aura 0.3 at the Batch S1 checkpoint' \
  architecture_docs/decisions/README.md
grep -Fq -- '- Status: Accepted; class patterns deferred' \
  architecture_docs/decisions/0049-match-guards-and-or-patterns.md
grep -Fq 'class patterns formally deferred to a future dedicated ADR' \
  architecture_docs/decisions/README.md
grep -Fq 'Accepted ADR-0045 assertion introspection' docs/manual/conformance.md
grep -Fq 'Accepted ADR-0045 test runner' docs/manual/conformance.md
grep -Fq 'class patterns are formally deferred' docs/manual/conformance.md

if rg -n '[Pp]rovisional' \
  architecture_docs/decisions/0045-testing-framework-and-assertion-introspection.md \
  architecture_docs/decisions/0049-match-guards-and-or-patterns.md; then
  echo "ratified ADR-0045/ADR-0049 checkpoint text still contains a provisional disposition" >&2
  exit 1
fi

python3 scripts/test_reference_integrity.py
python3 scripts/reference_integrity.py
python3 -m unittest scripts/test_release_metadata.py
node --test docs/.vitepress/release-metadata.test.mjs
node --test docs/.vitepress/agent-docs.test.mjs
node --test docs/.vitepress/landing-examples.test.mjs
