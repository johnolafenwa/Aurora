# Backend boundary: checked semantics and mechanical emission

Status: pre-Batch-1 item 2 delivered for the 0.3.4 update. This is an inventory
and proposed design, not a refactor or a second backend. Cranelift remains the
native backend through Batch 6; MIR execution remains maintained.

## Reading the inventory

Line references below refer to the source in the pre-Batch-1 implementation;
the named function is the durable navigation anchor. Paths are relative to the
repository root. “Enough” asks whether checked MIR already carries the semantic
inputs, not whether it has an explicit operation ready for mechanical emission.
Representation choices such as register allocation stay in an emitter. Language
choices such as copying versus consuming, failure classification, and evaluation
order belong in shared lowering or a specified runtime operation.

## Native codegen inventory

All references in this table are in
[`crates/aura-compiler/src/native_codegen.rs`](../crates/aura-compiler/src/native_codegen.rs).

| Line and function | Observable semantic currently decided here | Category | Does checked MIR carry enough? |
| --- | --- | --- | --- |
| 236 `DirectType::abi_types`, 275 `field_slice` | Flattens class values and locates mutable field writeback slots; opaque versus scalar handling determines the ownership ABI. | typing, ownership | Types and field declarations exist; an explicit layout/writeback contract is missing. Keep target register placement native. |
| 319 `bind_function_value_args`, 766 `ordered_named_args` | Binds positional/named arguments and supplies the ordering used by indirect and builtin calls. | evaluation order | `MirArg`, parameters, and default-function metadata exist; represent bound argument slots once, preserving already-lowered source evaluation order. |
| 3516 `define_function_thunk`, 3690 `define_function_default_binder` | Adapts code pointers, default evaluation, mutable argument returns, and boxed results. | typing, ownership, evaluation order | Most inputs exist; unify a callable ABI plan rather than reconstructing capabilities in each emitter. |
| 3788 `define_cleanup_thunks`, 3805 `define_cleanup_thunk` | Selects cleanup calls and reconstructs resource arguments for scope/trap exits. | cleanup, runtime-call selection | `PushCleanup`/`PopCleanup` and types exist; a resolved cleanup target and explicit ownership of captured cleanup arguments are still needed. |
| 4091 `reachable_direct_block_labels`, 4137 `direct_view_selector_tags`, 4246 `direct_closure_writeback_live_ins` | Reconstructs reachable loan alternatives and live closure writebacks across control flow. | ownership | Loan instructions and CFG exist; lowering should publish a validated place/loan dataflow plan. Backend block layout must not decide provenance. |
| 4810 `temporary_owns_opaque`, 4835 `release_all_temporary_owned`, 4878 `transfer_opaque_arg`, 4900 `export_return_value` | Decides retain/release/transfer for temporaries, arguments, roots, and returns. | ownership, cleanup | Passing modes and types exist, but value liveness/ownership operations remain implicit. Add explicit value effects before moving emission. |
| 5047 `compile_instruction` | Chooses scheduling fuel and translates loan acquisition, projected write-through, returned projection selection, and cleanup registration. | ownership, cleanup, runtime-call selection | MIR names the semantic operations. Fuel is a backend cost policy; selected-place and cleanup behavior must be a shared contract. |
| 5303 `compile_terminator`, 5478 `emit_return_value` | Performs exit releases and return writeback while choosing branches, assertion traps, and match paths. | cleanup, trap classification | Terminators, spans, and captures exist; ordered exit effects are not fully explicit. |
| 5656 `compile_closure` | Builds capture storage and passes consuming/repeatable metadata to the runtime. | ownership, typing | `Rvalue::Closure`, capture modes, and signature exist. Resolve environment layout and capture transfer centrally. |
| 5822 `compile_unary`, 5886 `compile_wide_integer_negation`, 5912 `compile_cast` | Chooses checked negation, cast bounds, narrowing, float rounding, and boxed conversion routes. | typing, trap classification | Operand/target types exist; a shared typed numeric operation must select the exact check and diagnostic. |
| 6029 `compile_binary`, 6187 `compile_int32_binary`, 6244 `compile_signed_floor_divmod`, 6271 `compile_wide_integer_binary`, 6429 `compile_float_binary` | Distinguishes checked overflow, divisor-sign floor/remainder, float width, Array/scalar arithmetic, and fallback boxed arithmetic. | typing, trap classification, runtime-call selection | Operators, types, and spans exist. Explicit typed operations and trap policy are still missing. |
| 6537 `emit_int_division_guard`, 6547 `emit_integer_overflow_failure_branch`, 6577 `emit_float_division_guard` | Chooses when to trap and how to classify/location-tag the diagnostic. | trap classification | Source spans exist; diagnostic identity and failure edges should be resolved before native emission. |
| 6610 `compile_call`, 6641 `compile_extern_call`, 6753 `compile_function_value_call` | Selects direct/indirect/FFI calls and adapts owned versus mutable arguments and returns. | ownership, runtime-call selection | MIR distinguishes targets and FFI contracts. The concrete argument effect and writeback plan is still backend-derived. |
| 6929 `compile_print`, 6976 `compile_format_string` | Chooses scalar versus boxed rendering and formatting helpers. | runtime-call selection, evaluation order | Operands and formatting data exist; canonical rendering operation IDs would remove string-based dispatch. |
| 7027 `compile_named_call`, 7738 `compile_host_builtin_named_call`, 7908 `compile_builtin_io_named_call` | Selects builtin behavior, optional arguments, resource helpers, and result representations. | typing, runtime-call selection | Shared builtin metadata exists; resolve a typed builtin opcode and argument slots in MIR. |
| 8231 `compile_for_range`, 8285 `compile_trait_member_call`, 8317 `compile_member_call` | Chooses iteration behavior, trait dispatch, scalar/Array special cases, and receiver writeback. | ownership, typing, runtime-call selection | Checked types and contracts exist, but resolved calls should replace repeated member-name inference. |
| 8548 `compile_construct`, 8611 `type_of_place`, 8650 `load_operand` | Selects field layout, operand coercion, projection access, and copy/opaque loads. | typing, ownership | Nominal declarations and local types exist; publish typed place IDs and explicit load modes. |

## Native runtime inventory

All references in this table are in
[`crates/aura-compiler/src/native_runtime.rs`](../crates/aura-compiler/src/native_runtime.rs).
A runtime must still implement dynamic checks and host I/O. The goal is to make
those operations specified and shared, not to move operating-system execution
into compile time.

| Line and function | Observable semantic currently decided here | Category | Does checked MIR carry enough? |
| --- | --- | --- | --- |
| 739 `capture_direct_runtime_frames_once` | Captures innermost-first Aura call frames and task ancestry once for the primary diagnostic. | trap classification | Spans and call identities exist; task ancestry is necessarily dynamic. Specify one cross-backend frame contract. |
| 769 `release_direct_task_runtime_state`, 805 `push_direct_cleanup_registration`, 819 `take_direct_cleanup_registration`, 859 `DirectPrimaryDiagnosticGuard::install` | Releases task-owned values, orders registered cleanup, and preserves the first failure. | cleanup, ownership, trap classification | Scope actions exist; runtime registration/draining and failure precedence need a shared explicit contract. |
| 1132 `extract_duration_nanoseconds`, 1157 `direct_timer_diagnostic` | Converts Duration to host timer bounds and maps failure to Aura diagnostics. | typing, trap classification | Duration type is known; actual value/host bounds are dynamic. Shared conversion policy should be named by the operation. |
| 1173 `boxed_value_with_type`, 1188 `retain_ref_count`, 1206 `release_ref_count` | Registers boxed ownership and determines last-release destruction. | ownership, cleanup | MIR lacks explicit retains/releases today. Runtime representation stays here after ownership effects become explicit. |
| 1292 `canonical_runtime_type_name`, 1349 `runtime_type_from_name`, 1458 `runtime_type_pattern_matches` | Reconstructs nominal/generic identities for dynamic dispatch and value tagging. | typing | MIR carries types; use canonical type IDs/descriptors instead of independently interpreting strings. |
| 1981 `evaluate_direct_json_host_builtin` | Chooses exact JSON variants, accessor outcomes, consuming payload extraction, and parse/dump error policy. | typing, ownership, trap classification | Builtin target/types are known; codec results are dynamic. Centralize shared builtin semantics while retaining runtime codec work. |
| 2335 `direct_value_to_ffi`, 2399 `direct_ffi_to_value`, 2448 `direct_ffi_write_back_mut_bytes`, 2463 `direct_ffi_error` | Marshals scalar/view/handle arguments, copies mutable scratch back, and maps native adapter failures. | ownership, typing, trap classification | MIR has FFI signature/capabilities. Dynamic native results still require runtime validation under the same ABI contract. |
| 3934 `aura_direct_function_bind_defaults`, 4002 `aura_direct_function_call` | Applies default binders, claims consuming closure captures, reconstructs environments, and writes mutable captures back. | ownership, evaluation order | Static capture/call-kind metadata exists. Dynamic invocation and consumption checks stay runtime operations with specified effects. |
| 5705–5984 Array entrypoints | Determines runtime shape/index bounds, fresh allocation versus mutation, integer mode, callback traversal, and sequential reduction. | typing, ownership, trap classification | Dtype and operation exist; shape and indexes are dynamic. Shared Array helpers should remain canonical. |
| 6643 `aura_direct_binary_value`, 6684 `aura_direct_binary_value_at`, 6734 `aura_direct_cast_value` | Maps numeric opcode/width to value semantics and consumes boxed operands; emits typed traps. | ownership, typing, trap classification | Operators, widths, and spans exist. Resolve these into a typed runtime call without duplicating arithmetic rules. |
| 3024 `runtime_diagnostic_error`, 3073 `task_runtime_boundary` | Converts runtime failures to typed diagnostics and drains cleanup on unwinding, retaining primary failure and Aura frames. | cleanup, trap classification | Static spans exist; dynamic failure state requires a shared boundary contract. |
| 8326 `aura_direct_task_join`, 8676 `aura_direct_wait_all`, 8807 `aura_direct_select` | Claims task results, chooses timeout/cancel/error carriers, and validates heterogeneous result metadata. | ownership, typing, trap classification | Static result types exist; readiness/consumption are dynamic. Preserve one-observation semantics in shared runtime operations. |
| 8871 `aura_direct_task_group_close`, 8888 `aura_direct_close_value` | Decides cancellation-before-join and selects resource-specific close behavior, including ignored WebSocket close errors. | cleanup, runtime-call selection | `cancel_before_cleanup` exists; resolved resource close operation and failure policy should be explicit. |

## Proposed mechanical builder

Shared lowering would consume a checked `Program` and produce validated executable
MIR with typed values/places, bound calls, ownership effects, ordered exits,
explicit trap descriptors, and stable runtime operation IDs. A target-neutral ABI
plan would describe argument/result slots without choosing machine registers.
The emitter would implement a small builder that creates blocks, values, loads,
stores, calls, and terminators. It would not infer a receiver capability from a
method name or decide whether a return consumes an object.

This sketch is illustrative, not a committed API or new Rust implementation:

```rust
trait NativeBuilder {
    type Value;
    type Block;
    type Error;

    fn block(&mut self, id: BlockId) -> Self::Block;
    fn switch_to(&mut self, block: Self::Block);
    fn scalar(&mut self, op: ResolvedScalarOp, args: &[Self::Value])
        -> Result<Self::Value, Self::Error>;
    fn load(&mut self, place: &ResolvedPlace) -> Result<Self::Value, Self::Error>;
    fn store(&mut self, place: &ResolvedPlace, value: Self::Value)
        -> Result<(), Self::Error>;
    fn call(&mut self, call: &ResolvedCall, args: &[Self::Value])
        -> Result<Vec<Self::Value>, Self::Error>;
    fn effect(&mut self, effect: &ResolvedEffect, args: &[Self::Value])
        -> Result<(), Self::Error>;
    fn terminate(&mut self, exit: &ResolvedTerminator, args: &[Self::Value])
        -> Result<(), Self::Error>;
}
```

`ResolvedScalarOp` selects width and a previously specified arithmetic mode;
`ResolvedCall` names the exact function/runtime operation and ABI slots;
`ResolvedEffect` covers retain/release, loan transitions, cleanup registration,
and safepoints. Trap descriptors retain diagnostic code, source span, and frame
identity. The builder may optimize instruction selection, but cannot omit an
observable check or reorder an effect. Runtime implementations remain responsible
for dynamic allocation, readiness, host errors, and actual cleanup execution.

## Ordered Batch 1 moves and gates

Every move starts with a failing behavioral or MIR regression. Every move must
pass the complete forced MIR/direct fixture matrix (no fallback, loopback enabled),
compiler coverage floors **96.30% lines / 97.21% functions / 94.71% regions**, and
all affected CLI/native acceptance tests. No floor reduction or fixture rewriting
is permitted to accommodate a semantic change.

1. Introduce stable typed operation and runtime-call descriptors alongside the
   existing serialized MIR contract. Pin builtin argument binding/default order,
   numeric width, and diagnostic spans with MIR and public-interface tests.
2. Move numeric-operation and trap selection into shared lowering. Exercise
   integer extremes, floor division/remainder, float widths, casts, and Array
   modes on both backends; retain exact diagnostic comparisons.
3. Publish canonical place/projection and reachable loan plans. Regress returned
   view alternatives, reborrow suspension, branch expiry, and closure writeback
   before replacing codegen's reconstruction. Keep public/serialized MIR checks.
4. Make ownership effects and ordered exit actions explicit. Test every normal,
   early-return, trap, and cancellation exit, with exact-once cleanup and primary
   failure preservation. Both direct runtime tests and CLI frame tests are gates.
5. Resolve callable/default/receiver ABI plans for Batch 1's new callable types.
   Preserve left-to-right evaluation, bound names/defaults, mutable sinks, and
   consuming captures. Run compiler analysis/LSP regressions and 100% LSP coverage
   when semantic metadata changes; bump its schema when representation requires it.
6. Route Cranelift through the small builder after each operation family has a
   shared contract. Retain standalone packaged execution and source diagnostics;
   do not add another native backend or change the default execution mode.

The runtime/compiler crate split remains Batch 5. A second native backend is a
Batch 7 evidence-based decision under [ADR-0064](decisions/0064-native-backend-strategy-and-codegen-boundary.md).
