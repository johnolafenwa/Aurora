use super::{
    bind_args, bind_builtin_args, bind_optional_builtin_args, build_range, bytes_vec_value,
    collect_queue_handles, collect_runtime_type_substitutions, collect_type_params_from_type,
    eval_ordering, evaluate_named_args, option_none, option_some, render_runtime_error, result_err,
    result_ok, run_serialized_mir, send_error_closed, task_result_ready, write_stream,
    CancellationContext, Env, EvaluatedMirArg, MirRuntime, TaskGroupValue, TaskValue,
};
use crate::diag::{Diagnostic, RuntimeCallFrame, RuntimeSourceSpan, RuntimeTaskFrame, Span};
use crate::integer::{IntegerKind, IntegerValue};
use crate::mir::{
    AssertionCapture, BasicBlock, CallTarget, Instruction, MirArg, MirClass, MirClosureCapture,
    MirExternCall, MirExternParam, MirFunction, MirLocalType, MirMatchArm, MirMethod, MirModule,
    MirParam, MirReceiverKind, MirTraitImpl, Operand, Rvalue, Terminator,
};
use crate::randomness::SecureRandomError;
use crate::runtime_value::{
    ArrayStorage, ArrayValue, ChannelValue, EnumVariantValue, FfiHandleValue, FileValue,
    HttpListenerValue, HttpResponseValue, InstanceValue, MapValue, ProcessChildValue,
    ProcessCompletedValue, ProcessStdioConfig, ProcessSupervisorValue, RangeValue, SetValue,
    TcpListenerValue, TcpStreamValue, TlsListenerValue, TlsStreamValue, TupleValue,
    UdpDatagramValue, UdpSocketValue, Value, VecValue, WebSocketListenerValue, WebSocketValue,
};
use crate::sema::Type;
use rcgen::generate_simple_self_signed;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_void;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration as StdDuration;

#[cfg(unix)]
use crate::runtime_value::{UnixListenerValue, UnixStreamValue};

fn test_function_operand(name: &str, params: Vec<Type>, return_type: Type) -> Operand {
    Operand::Function {
        name: name.to_string(),
        signature: Box::new(Type::Function {
            params: params
                .into_iter()
                .map(|ty| crate::sema::FunctionParamContract {
                    name: String::new(),
                    ty,
                    passing: crate::ast::ReceiverKind::Value,
                    has_default: false,
                    default_erased: false,
                })
                .collect(),
            return_type: Box::new(return_type),
        }),
    }
}

#[test]
fn adr0038_mutable_views_write_through_and_reborrows_share_one_place() {
    let output = crate::run_source(
        r#"
class Counter:
    value: int64

def main():
    mut counter = Counter(value=1)
    view mut value = counter.value
    view mut nested = value
    nested = nested + 4
    print(counter.value)
"#,
    )
    .expect("mutable views must execute through the MIR loan representation");
    assert_eq!(output.stdout, "5\n");

    let tuple_output = crate::run_source(
        r#"
def main():
    mut pair = (1, 2)
    view mut second = pair[1]
    second = 7
    print(pair)
"#,
    )
    .expect("fixed tuple-position views must write through their stable place");
    assert_eq!(tuple_output.stdout, "(1, 7)\n");

    let nested_tuple_class = crate::run_source(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut wrapper = (Pair(left=1, right=2), 3)
    view mut field = wrapper[0].right
    field = 9
    print(wrapper[0].right)
"#,
    )
    .expect("nested tuple-to-class views must lower to a canonical physical place");
    assert_eq!(nested_tuple_class.stdout, "9\n");
}

#[test]
fn borrow_mut_operator_writebacks_preserve_projected_places() {
    let output = crate::run_source(
        r#"
trait Add[Rhs, Out]:
    def add(mut self, rhs: mut Rhs) -> Out

copy class Counter:
    value: int32

class Holder:
    counter: Counter

impl Add[Counter, Counter] for Counter:
    def add(mut self, rhs: mut Counter) -> Counter:
        self.value += rhs.value
        rhs.value += 1
        return self

def main():
    mut holder = Holder(counter=Counter(value=10))
    mut member_rhs = Counter(value=3)
    holder.counter += member_rhs
    print(holder.counter.value)
    print(member_rhs.value)

    mut pair = (Counter(value=20),)
    mut tuple_rhs = Counter(value=4)
    view mut tuple_counter = pair[0]
    tuple_counter += tuple_rhs
    print(tuple_counter.value)
    print(tuple_rhs.value)

    mut values = [Counter(value=30)]
    mut list_rhs = Counter(value=5)
    values[0] += list_rhs
    print(values[0].value)
    print(list_rhs.value)
"#,
    )
    .expect("BorrowMut operators should write through supported projected targets");
    assert_eq!(output.stdout, "13\n4\n24\n5\n35\n6\n");
}

#[test]
fn adr0038_shared_reborrows_do_not_suspend_their_shared_parent() {
    let output = crate::run_source(
        r#"
def main():
    value = "Ada"
    view first = value
    view second = first
    print(first)
    print(second)
"#,
    )
    .expect("overlapping shared parent and child views remain readable");
    assert_eq!(output.stdout, "Ada\nAda\n");
}

#[test]
fn adr0038_condition_edges_and_branch_local_views_balance_the_runtime_ledger() {
    let branch = crate::run_source(
        r#"
def main():
    mut left = 1
    mut right = 2
    if true:
        view mut selected = left
        if false:
            pass
        selected = 10
    else:
        view mut selected = right
        selected = 20
    print(left)
    print(right)
"#,
    )
    .expect("branch-local view identities should join with an empty loan ledger");
    assert_eq!(branch.stdout, "10\n2\n");

    let condition = crate::run_source(
        r#"
def main():
    mut value = 1
    view alias = value
    if alias == 1:
        value = 2
    print(value)
"#,
    )
    .expect("a condition-only last use should end on both outgoing edges exactly once");
    assert_eq!(condition.stdout, "2\n");

    let generic_mutation = crate::run_source(
        r#"
trait Project:
    def get(mut self) -> view mut int64 from self

class Box:
    value: int64

impl Project for Box:
    def get(mut self) -> view mut int64 from self:
        return view mut self.value

def update[T: Project](item: mut T):
    view mut alias = item.get()
    alias = 9

def main():
    mut box = Box(value=7)
    update(box)
    print(box.value)
"#,
    )
    .expect("generic trait returned views should preserve mutable write-through");
    assert_eq!(generic_mutation.stdout, "9\n");
}

#[test]
fn adr0038_nested_last_uses_end_in_the_selected_control_flow_path() {
    for source in [
        r#"
def main():
    mut value = 1
    view alias = value
    if true:
        print(alias)
        value = 2
    print(value)
"#,
        r#"
def main():
    mut value = 1
    view alias = value
    match true:
        case true:
            print(alias)
            value = 2
        case false:
            pass
    print(value)
"#,
        r#"
def main():
    mut value = 1
    view alias = value
    with TaskGroup() as group:
        print(alias)
        value = 2
    print(value)
"#,
    ] {
        let output = crate::run_source(source)
            .expect("an inherited loan must end immediately after its nested last use");
        assert_eq!(output.stdout, "1\n2\n");
    }
}

#[test]
fn adr0038_return_cleanup_alias_forwarding_and_view_capture_remain_live() {
    let returned_child = crate::run_source(
        r#"
class Pair:
    left: int64

def left(pair: Pair) -> view int64 from pair:
    view parent = pair
    view child = parent.left
    return view child

def main():
    pair = Pair(left=7)
    view result = left(pair)
    print(result)
"#,
    )
    .expect("non-returned ancestors must clean up before the returned-loan handoff");
    assert_eq!(returned_child.stdout, "7\n");

    let captured_view = crate::run_source(
        r#"
def main():
    value = 1
    view alias = value
    get: def() -> int64 = lambda [alias]: alias
    print(get())
"#,
    )
    .expect("a closure capture of an existing view must keep a stable source alive");
    assert_eq!(captured_view.stdout, "1\n");

    let captured_mut_view = crate::run_source(
        r#"
def main():
    mut values = [1]
    view mut alias = values
    mut push: def(int64) -> None = lambda [mut alias] next: alias.append(next)
    push(4)
    print(values)
"#,
    )
    .expect("a mutable closure capture of an existing view must write through its stable source");
    assert_eq!(captured_mut_view.stdout, "[1, 4]\n");

    let forwarded_alias = crate::run_source(
        r#"
def inner(value: int64) -> view int64 from value:
    return view value

def outer(value: int64) -> view int64 from value:
    view alias = inner(value)
    return view alias

def main():
    value = 1
    view result = outer(value)
    print(result)
"#,
    )
    .expect("a local alias must preserve a forwarded returned-view projection");
    assert_eq!(forwarded_alias.stdout, "1\n");

    let cleanup_region = crate::run_source(
        r#"
def borrow(value: int64) -> view int64 from value:
    with TaskGroup() as group:
        return view value

def main():
    value = 4
    view result = borrow(value)
    print(result)
"#,
    )
    .expect("managed cleanup may run after handoff while preserving the returned projection");
    assert_eq!(cleanup_region.stdout, "4\n");
}

#[test]
fn adr0038_returned_view_handoffs_survive_reentrant_cleanup_calls() {
    let output = crate::run_source(
        r#"
class Pair:
    left: int64
    right: int64

def choose(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

class Resource:
    def close(mut self):
        mut local = Pair(left=101, right=102)
        view mut selected = choose(local, true)
        print(selected)

def clean_return(pair: mut Pair, left: bool) -> view mut int64 from pair:
    with Resource() as resource:
        return view mut choose(pair, left)

def main():
    mut pair = Pair(left=1, right=2)
    view mut selected = clean_return(pair, false)
    selected = 9
    print(pair)
"#,
    )
    .expect("cleanup calls must not replace the enclosing returned-view handoff");
    assert_eq!(output.stdout, "101\nPair(left=1, right=9)\n");
}

#[test]
fn adr0038_dynamic_returned_view_closure_captures_keep_the_selected_descriptor() {
    let output = crate::run_source(
        r#"
class Pair:
    left: int64
    right: int64

def choose(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def forward(pair: mut Pair, left: bool) -> view mut int64 from pair:
    return view mut choose(pair, left)

def choose_shared(pair: Pair, left: bool) -> view int64 from pair:
    if left:
        return view pair.left
    return view pair.right

def assign(value: mut int64, next: int64):
    value = next

def main():
    mut pair = Pair(left=1, right=2)
    view mut captured = forward(pair, false)
    mut update: def(int64) -> None = lambda [mut captured] next: assign(captured, next)
    update(41)
    print(pair)
    view chosen = choose_shared(pair, true)
    read: def() -> int64 = lambda [chosen]: chosen
    print(read())
"#,
    )
    .expect("a closure must capture the selected returned-view descriptor, not its broad origin");
    assert_eq!(output.stdout, "Pair(left=1, right=41)\n1\n");
}

#[test]
fn adr0038_returned_view_descendants_compose_through_captures_and_forwarding() {
    let output = crate::run_source(
        r#"
class ScalarPair:
    left: int64
    right: int64

def choose_mut(pair: mut ScalarPair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def choose_shared(pair: ScalarPair, left: bool) -> view int64 from pair:
    if left:
        return view pair.left
    return view pair.right

def assign(value: mut int64, next: int64):
    value = next

class Cell:
    value: int64

class CellPair:
    left: Cell
    right: Cell

class TuplePair:
    left: (int64, int64)
    right: (int64, int64)

def left_cell(pair: mut CellPair) -> view mut Cell from pair:
    return view mut pair.left

def choose_cell(pair: mut CellPair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def cell_value(cell: mut Cell) -> view mut int64 from cell:
    return view mut cell.value

def static_forward(pair: mut CellPair) -> view mut int64 from pair:
    return view mut left_cell(pair).value

def dynamic_forward(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    return view mut choose_cell(pair, left).value

def nested_forward(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    return view mut cell_value(choose_cell(pair, left))

def local_class_forward(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    view mut selected = choose_cell(pair, left)
    return view mut selected.value

def choose_tuple(pair: mut TuplePair, left: bool) -> view mut (int64, int64) from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def local_tuple_forward(pair: mut TuplePair, left: bool) -> view mut int64 from pair:
    view mut selected = choose_tuple(pair, left)
    return view mut selected[1]

def main():
    mut mutable_pair = ScalarPair(left=1, right=2)
    view mut selected_mut = choose_mut(mutable_pair, false)
    view mut child_mut = selected_mut
    mut update: def(int64) -> None = lambda [mut child_mut] next: assign(child_mut, next)
    update(9)
    print(mutable_pair)

    shared_pair = ScalarPair(left=3, right=4)
    view selected_shared = choose_shared(shared_pair, true)
    view child_shared = selected_shared
    read: def() -> int64 = lambda [child_shared]: child_shared
    print(read())

    mut cells = CellPair(left=Cell(value=10), right=Cell(value=20))
    view mut static_value = static_forward(cells)
    static_value = 11
    view mut dynamic_value = dynamic_forward(cells, false)
    dynamic_value = 21
    view mut nested_value = nested_forward(cells, true)
    nested_value = 12
    print(cells)

    view mut local_class_value = local_class_forward(cells, false)
    local_class_value = 22
    print(cells)

    mut tuples = TuplePair(left=(30, 40), right=(50, 60))
    view mut local_tuple_value = local_tuple_forward(tuples, true)
    local_tuple_value = 41
    print(tuples)
"#,
    )
    .expect("returned-view descendants must compose in the MIR backend");
    assert_eq!(
        output.stdout,
        "ScalarPair(left=1, right=9)\n3\nCellPair(left=Cell(value=12), right=Cell(value=21))\nCellPair(left=Cell(value=12), right=Cell(value=22))\nTuplePair(left=(30, 41), right=(50, 60))\n"
    );
}

#[test]
fn adr0038_generic_returned_view_forwarding_uses_declaration_context() {
    let output = crate::run_source(
        r#"
trait Project:
    def get(self) -> view int64 from self

class Box:
    value: int64

impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.value

def forward[T: Project](item: T) -> view int64 from item:
    return view item.get()

def main():
    box = Box(value=7)
    view alias = forward(box)
    print(alias)
"#,
    )
    .expect("generic trait returned-view forwarding must lower in the declaration context");
    assert_eq!(output.stdout, "7\n");

    let same_module = crate::run_source(
        r#"
class Box[T]:
    value: T

def inner[T](box: Box[T]) -> view T from box:
    return view box.value

def outer[T](box: Box[T]) -> view T from box:
    return view inner(box)

def main():
    box = Box(value=11)
    view alias = outer(box)
    print(alias)
"#,
    )
    .expect("same-module generic returned-view forwarding must not panic after checking");
    assert_eq!(same_module.stdout, "11\n");

    let distinct_impls = crate::run_source(
        r#"
trait Project:
    def get(self) -> view int64 from self

class LeftBox:
    left: int64

class RightBox:
    right: int64

impl Project for LeftBox:
    def get(self) -> view int64 from self:
        return view self.left

impl Project for RightBox:
    def get(self) -> view int64 from self:
        return view self.right

def forward[T: Project](item: T) -> view int64 from item:
    return view item.get()

def main():
    left = LeftBox(left=7)
    right = RightBox(right=8)
    view left_value = forward(left)
    view right_value = forward(right)
    print(left_value)
    print(right_value)
"#,
    )
    .expect("generic returned-view projections must use the concrete trait implementation");
    assert_eq!(distinct_impls.stdout, "7\n8\n");
}

#[test]
fn adr0038_nested_and_projected_returned_calls_compose_descriptors() {
    let output = crate::run_source(
        r#"
class Cell:
    value: int64

class Pair:
    left: Cell
    right: Cell

def choose(pair: mut Pair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def left_cell(pair: mut Pair) -> view mut Cell from pair:
    return view mut pair.left

def cell_value(cell: mut Cell) -> view mut int64 from cell:
    return view mut cell.value

def bump(value: mut int64):
    value += 1

def main():
    mut pair = Pair(left=Cell(value=1), right=Cell(value=2))
    view mut nested = cell_value(choose(pair, false))
    nested = 9
    view mut child = left_cell(pair).value
    child = 8
    bump(cell_value(choose(pair, false)))
    print(pair)
"#,
    )
    .expect("nested returned origins and call-rooted child reborrows should compose");
    assert_eq!(
        output.stdout,
        "Pair(left=Cell(value=8), right=Cell(value=10))\n"
    );
}

#[test]
fn adr0038_grouped_specialized_forwarding_retains_its_projection() {
    let output = crate::run_source(
        r#"
class Box[T]:
    value: T

def inner[T](box: Box[T]) -> view T from box:
    return view box.value

def outer[T](box: Box[T]) -> view T from box:
    return view (inner[T](box))

def main():
    box = Box(value=11)
    view alias = (outer[int64])(box)
    print(alias)
"#,
    )
    .expect("groups around specialized forwarded calls must be transparent");
    assert_eq!(output.stdout, "11\n");
}

#[test]
fn adr0038_parameterized_trait_forwarding_narrows_the_caller_descriptor() {
    let output = crate::run_source(
        r#"
trait Project[Item]:
    def get(self) -> view Item from self

class LeftBox:
    left: int64

class RightBox:
    right: int64

impl Project[int64] for LeftBox:
    def get(self) -> view int64 from self:
        return view self.left

impl Project[int64] for RightBox:
    def get(self) -> view int64 from self:
        return view self.right

def forward[Item, T: Project[Item]](item: T) -> view Item from item:
    return view item.get()

def main():
    left = LeftBox(left=7)
    right = RightBox(right=8)
    view left_value = forward[int64, LeftBox](left)
    view right_value = forward[int64, RightBox](right)
    print(left_value)
    print(right_value)
"#,
    )
    .expect("caller specialization may narrow a generic trait projection contract");
    assert_eq!(output.stdout, "7\n8\n");
}

#[test]
fn adr0038_returned_views_keep_the_callers_origin() {
    let output = crate::run_source(
        r#"
class Counter:
    value: int64

def value(counter: Counter) -> view int64 from counter:
    return view counter.value

def value_mut(counter: mut Counter) -> view mut int64 from counter:
    return view mut counter.value

def bump(value: mut int64):
    value += 1

def main():
    mut counter = Counter(value=2)
    view initial = value(counter)
    print(initial)
    view mut editable = value_mut(counter)
    editable = 9
    print(counter.value)
    bump(value_mut(counter))
    print(counter.value)
"#,
    )
    .expect("returned views must alias the declared caller origin");
    assert_eq!(output.stdout, "2\n9\n10\n");

    let selected = crate::run_source(
        r#"
class Pair:
    left: int64
    right: int64

def choose(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def bump(value: mut int64):
    value += 1

def main():
    mut pair = Pair(left=1, right=10)
    view mut left = choose(pair, true)
    left = 2
    view mut right = choose(pair, false)
    right = 20
    bump(choose(pair, false))
    print(pair.left)
    print(pair.right)
"#,
    )
    .expect("returned views must retain the exact control-flow-selected projection");
    assert_eq!(selected.stdout, "2\n21\n");
}

#[test]
fn adr0038_trapping_mutable_calls_publish_writebacks_before_cleanup() {
    let cases = [
        r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def mutate_then_trap(resource: mut Resource):
    resource.value = 9
    print(1 // 0)

def main():
    with resource = Resource(value=1):
        mut action: def() -> None = lambda [mut resource]: mutate_then_trap(resource)
        action()
"#,
        r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def mutate_then_trap(resource: mut Resource):
    resource.value = 9
    print(1 // 0)

def main():
    with resource = Resource(value=1):
        view mut alias = resource
        mutate_then_trap(alias)
"#,
        r#"
class Resource:
    value: int64

    def close(mut self):
        print(self.value)

def borrow_mut(resource: mut Resource) -> view mut Resource from resource:
    return view mut resource

def mutate_then_trap(resource: mut Resource):
    resource.value = 9
    print(1 // 0)

def main():
    with resource = Resource(value=1):
        mutate_then_trap(borrow_mut(resource))
"#,
    ];

    for source in cases {
        let module = crate::lower_source_to_mir(source)
            .expect("trap-time mutable write-through source should lower");
        let stdout = Arc::new(Mutex::new(String::new()));
        let mut runtime = MirRuntime::new(module, stdout.clone(), CancellationContext::default());
        let error = runtime
            .run_main()
            .expect_err("the mutation witness should trap after writing");
        assert_eq!(error.code, "AU4004");
        assert_eq!(error.message, "division by zero");
        assert_eq!(
            stdout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_str(),
            "9\n",
            "cleanup must observe the successful mutation before the trap"
        );
    }
}

#[test]
fn adr0038_forwarded_returned_views_preserve_the_transferred_projection() {
    let output = crate::run_source(
        r#"
class Pair:
    left: int64
    right: int64

def inner(pair: Pair) -> view int64 from pair:
    return view pair.left

def outer(pair: Pair) -> view int64 from pair:
    return view inner(pair)

def main():
    pair = Pair(left=7, right=8)
    view result = outer(pair)
    print(result)
"#,
    )
    .expect("returned views may be forwarded without a lowering panic");
    assert_eq!(output.stdout, "7\n");
}

#[test]
fn adr0038_public_mir_runtime_validates_loan_authority_before_execution() {
    let mut module = crate::lower_source_to_mir(
        r#"
def main():
    value = 1
    view parent = value
    view child = parent
    print(child)
"#,
    )
    .expect("valid shared reborrow source should lower");
    let reborrow = module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Reborrow { mutable, .. } => Some(mutable),
            _ => None,
        })
        .expect("the child view should lower as a reborrow");
    *reborrow = true;

    let error = crate::mir_runtime::run(&module)
        .expect_err("public MIR execution must reject forged mutable authority");
    assert_eq!(error.code, "AU4001");
    assert!(error.message.contains("escalates shared parent"));
}

#[test]
fn adr0038_mutable_closure_capture_writes_back_to_its_live_source() {
    let output = crate::run_source(
        r#"
def main():
    mut values = [1]
    mut update: def(int64) -> None = lambda [mut values] item: values.append(item)
    update(2)
    update(3)
    print(values)
"#,
    )
    .expect("a mutable-repeatable closure must reborrow and write through on every call");
    assert_eq!(output.stdout, "[1, 2, 3]\n");

    let mixed = crate::run_source(
        r#"
def consume(value: own str):
    pass

def main():
    mut values = [1]
    text = "owned"
    callback: def(int64) -> (None, None) = lambda [mut values, own text] item: (values.append(item), consume(text))
    callback(2)
    print(values)
"#,
    )
    .expect("a consuming closure may still write through its live mutable capture before teardown");
    assert_eq!(mixed.stdout, "[1, 2]\n");
}

#[test]
fn adr0038_mir_loan_environment_covers_alias_resolution_and_nested_place_errors() {
    let pair_value = || {
        Value::Instance(InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::from([(
                "nested".to_string(),
                Value::Tuple(TupleValue {
                    element_types: vec![Type::named("int64")],
                    elements: vec![Value::Int(IntegerValue::from_signed(1))],
                }),
            )]),
        })
    };
    let mut env = Env::default();
    env.define_typed("pair", Type::named("Pair"), pair_value());
    env.begin_loan("root", "pair", true)
        .expect("root loan should begin");
    env.begin_loan("nested", "root.nested", true)
        .expect("reborrowed loan should resolve through its parent");
    assert_eq!(
        env.resolve_loan_place("nested.0")
            .expect("loan suffix should compose"),
        "pair.nested.0"
    );
    assert!(env.loans.get("root").expect("loan should exist").mutable);
    assert!(env
        .begin_loan("shared", "pair", false)
        .expect_err("a shared loan must not overlap an active mutable loan")
        .message
        .contains("overlaps active mutable loan"));
    assert_eq!(
        env.returned_view_projection("root", "pair")
            .expect("root projection should be empty"),
        ""
    );
    assert_eq!(
        env.returned_view_projection("nested", "pair")
            .expect("nested projection should be relative to the origin"),
        "nested"
    );
    assert!(env
        .returned_view_projection("pair", "nested")
        .expect_err("a returned view may not escape its origin")
        .message
        .contains("outside declared origin"));
    assert_eq!(
        env.begin_loan("missing", "unknown", false)
            .expect_err("loans require a live root")
            .message,
        "cannot begin MIR loan `missing` from unknown place `unknown`"
    );
    assert_eq!(
        env.end_loan("missing")
            .expect_err("unknown loans cannot end")
            .message,
        "cannot end unknown MIR loan `missing`"
    );
    env.loans.insert(
        "cycle_a".to_string(),
        super::RuntimeLoan {
            source: "cycle_b".to_string(),
            mutable: false,
            parent: None,
        },
    );
    env.loans.insert(
        "cycle_b".to_string(),
        super::RuntimeLoan {
            source: "cycle_a".to_string(),
            mutable: false,
            parent: None,
        },
    );
    assert!(env
        .resolve_loan_place("cycle_a")
        .expect_err("loan cycles must be diagnosed")
        .message
        .contains("cyclic MIR loan descriptor"));

    assert!(env
        .place_ref("pair.nested.bad")
        .expect_err("tuple projections must be numeric")
        .message
        .contains("is not a fixed position"));
    assert!(env
        .place_ref("pair.nested.4")
        .expect_err("tuple projections must be in bounds")
        .message
        .contains("has no element at index 4"));

    let mut nested = pair_value();
    super::write_nested_place(
        &mut nested,
        &["nested", "0"],
        Value::Int(IntegerValue::from_signed(8)),
        "pair.nested.0",
    )
    .expect("nested tuple writes should succeed");
    let mut tuple_then_instance = Value::Tuple(TupleValue {
        element_types: vec![Type::named("Pair")],
        elements: vec![Value::Instance(InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(1)),
            )]),
        })],
    });
    super::write_nested_place(
        &mut tuple_then_instance,
        &["0", "value"],
        Value::Int(IntegerValue::from_signed(5)),
        "pair.0.value",
    )
    .expect("tuple-to-instance nested writes should recurse");
    assert!(
        super::write_nested_place(&mut nested, &[], Value::Unit, "pair")
            .expect_err("empty nested write paths must fail")
            .message
            .contains("cannot assign empty nested MIR place")
    );
    assert!(super::write_nested_place(
        &mut nested,
        &["nested", "bad"],
        Value::Unit,
        "pair.nested.bad",
    )
    .expect_err("tuple write projections must be numeric")
    .message
    .contains("is not a fixed position"));
    assert!(
        super::write_nested_place(&mut nested, &["nested", "3"], Value::Unit, "pair.nested.3",)
            .expect_err("tuple write projections must be in bounds")
            .message
            .contains("has no element at index 3")
    );

    let mut taken = pair_value();
    assert_eq!(
        super::take_nested_place(
            &mut taken,
            &["nested".to_string(), "0".to_string()],
            "pair.nested.0",
        )
        .expect("nested tuple moves should succeed"),
        Value::Int(IntegerValue::from_signed(1))
    );
    let mut tuple_then_instance = Value::Tuple(TupleValue {
        element_types: vec![Type::named("Pair")],
        elements: vec![Value::Instance(InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(6)),
            )]),
        })],
    });
    assert_eq!(
        super::take_nested_place(
            &mut tuple_then_instance,
            &["0".to_string(), "value".to_string()],
            "pair.0.value",
        )
        .expect("tuple-to-instance nested moves should recurse"),
        Value::Int(IntegerValue::from_signed(6))
    );
    assert!(super::take_nested_place(&mut taken, &[], "pair")
        .expect_err("empty nested move paths must fail")
        .message
        .contains("cannot move empty nested MIR place"));
    for (segment, expected) in [("bad", "not a fixed position"), ("3", "has no element")] {
        let mut tuple = Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64")],
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        });
        assert!(
            super::take_nested_place(&mut tuple, &[segment.to_string()], "pair")
                .expect_err("invalid tuple move projection must fail")
                .message
                .contains(expected)
        );
    }

    let mut mutable = pair_value();
    *super::nested_place_mut(
        &mut mutable,
        &["nested".to_string(), "0".to_string()],
        "pair.nested.0",
    )
    .expect("nested mutable lookup should succeed") = Value::Int(IntegerValue::from_signed(9));
    for (segment, expected) in [("bad", "not a fixed position"), ("3", "has no element")] {
        let mut tuple = Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64")],
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        });
        assert!(
            super::nested_place_mut(&mut tuple, &[segment.to_string()], "pair")
                .expect_err("invalid mutable tuple projection must fail")
                .message
                .contains(expected)
        );
    }
    let mut scalar = Value::Int(IntegerValue::from_signed(1));
    assert!(
        super::nested_place_mut(&mut scalar, &["field".to_string()], "value.field")
            .expect_err("scalars have no nested mutable place")
            .message
            .contains("on a non-instance value")
    );
}

#[test]
fn adr0038_mir_returned_loan_instruction_reports_handoff_errors_and_root_projection() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "pair",
        Type::Tuple(vec![Type::named("int64")]),
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("int64")],
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
    );
    let mut cleanup = Vec::new();
    let mut fuel = crate::mir::MIR_LOOP_SAFEPOINT_INTERVAL;
    let instruction = Instruction::BeginReturnedLoan {
        loan: "selected".to_string(),
        origin: "pair".to_string(),
        projections: vec![String::new(), "0".to_string()],
        mutable: true,
    };
    assert!(runtime
        .execute_instruction(&instruction, &mut env, &mut cleanup, &mut fuel)
        .expect_err("a returned loan requires a projection handoff")
        .message
        .contains("has no transferred projection"));
    runtime.pending_returned_view_projection = Some("missing".to_string());
    assert!(runtime
        .execute_instruction(&instruction, &mut env, &mut cleanup, &mut fuel)
        .expect_err("the selected projection must be declared")
        .message
        .contains("selected undeclared projection"));
    runtime.pending_returned_view_projection = Some(String::new());
    runtime
        .execute_instruction(&instruction, &mut env, &mut cleanup, &mut fuel)
        .expect("an empty projection aliases the origin itself");
    assert_eq!(env.resolve_loan_place("selected").unwrap(), "pair");

    runtime
        .execute_instruction(
            &Instruction::Reborrow {
                loan: "same".to_string(),
                parent: "selected".to_string(),
                projection: String::new(),
                mutable: true,
            },
            &mut env,
            &mut cleanup,
            &mut fuel,
        )
        .expect("an empty reborrow projection aliases its parent");
    assert_eq!(env.resolve_loan_place("same").unwrap(), "pair");
    env.end_loan("same")
        .expect("the first child must end before creating an overlapping sibling");
    runtime
        .execute_instruction(
            &Instruction::Reborrow {
                loan: "element".to_string(),
                parent: "selected".to_string(),
                projection: "0".to_string(),
                mutable: true,
            },
            &mut env,
            &mut cleanup,
            &mut fuel,
        )
        .expect("a projected reborrow should compose with its parent");
    assert_eq!(env.resolve_loan_place("element").unwrap(), "pair.0");
}

fn test_runtime() -> MirRuntime {
    MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    )
}

#[test]
fn mir_module_constant_reads_share_one_stored_non_copy_value() {
    let module =
        crate::lower_source_to_mir("values = [1, 2, 3]\n\ndef main():\n    print(values.len())\n")
            .expect("module constant source should lower");
    let constant = module
        .constants
        .first()
        .cloned()
        .expect("lowering should record the module constant initializer");
    let mut runtime = MirRuntime::new(
        module,
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );

    let first = runtime
        .read_module_constant(&constant.key, &constant.initializer)
        .expect("first read should initialize the constant");
    let clones_after_initialization = super::mir_value_clone_count();
    let second = runtime
        .read_module_constant(&constant.key, &constant.initializer)
        .expect("later reads should reuse the initialized constant");

    assert!(Arc::ptr_eq(&first, &second));
    assert!(matches!(first.as_ref(), Value::Vec(_)));
    assert_eq!(
        super::mir_value_clone_count(),
        clones_after_initialization,
        "reading an initialized module constant must not snapshot its aggregate value"
    );
}

#[test]
fn builtin_math_constant_uses_once_initialized_shared_module_storage() {
    let module = crate::lower_source_to_mir(
        "import math\n\ndef main():\n    print(math.pi)\n    print(math.pi)\n",
    )
    .expect("math constant source should lower");
    let pi = module
        .constants
        .iter()
        .find(|constant| constant.key == "math::pi")
        .cloned()
        .expect("generic MIR constants should include math.pi exactly once");
    assert_eq!(
        module
            .constants
            .iter()
            .filter(|constant| constant.key == "math::pi")
            .count(),
        1
    );
    let mut runtime = MirRuntime::new(
        module,
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );

    let first = runtime
        .read_module_constant(&pi.key, &pi.initializer)
        .expect("first math.pi read should initialize storage");
    let second = runtime
        .read_module_constant(&pi.key, &pi.initializer)
        .expect("second math.pi read should reuse storage");

    assert!(Arc::ptr_eq(&first, &second));
    let Value::Float(value) = first.as_ref() else {
        panic!("math.pi storage should contain float64");
    };
    assert_eq!(value.to_bits(), 0x4009_21fb_5444_2d18);
}

#[test]
fn failed_module_constant_reads_replay_the_original_diagnostic() {
    let module = crate::lower_source_to_mir(
        r#"
def initialize() -> int64:
    print("initializing")
    return 1 // 0

value = initialize()

def main():
    print("unreachable")
"#,
    )
    .expect("failing module constant source should lower");
    let constant = module
        .constants
        .first()
        .cloned()
        .expect("lowering should record the failing constant initializer");
    let stdout = Arc::new(Mutex::new(String::new()));
    let mut runtime = MirRuntime::new(module, stdout.clone(), CancellationContext::default());

    let first = runtime
        .read_module_constant(&constant.key, &constant.initializer)
        .expect_err("the constant initializer must fail");
    let second = runtime
        .read_module_constant(&constant.key, &constant.initializer)
        .expect_err("later reads must replay the initializer failure");

    assert_eq!(first.code, "AU4004");
    assert_eq!(second, first);
    assert_eq!(
        *stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        "initializing\n",
        "a failed initializer must run once even when the constant is read again"
    );
}

#[test]
fn canonical_collection_capacity_failures_have_stable_runtime_diagnostics() {
    let cases = [
        (
            "list constructor negative capacity",
            "def main():\n    values = list[int64].with_capacity(-1)\n    print(values)\n",
            "AU4003",
            "collection capacity cannot be negative",
        ),
        (
            "dict constructor negative capacity",
            "def main():\n    values = dict[str, int64].with_capacity(-1)\n    print(values)\n",
            "AU4003",
            "collection capacity cannot be negative",
        ),
        (
            "set constructor negative capacity",
            "def main():\n    values = set[str].with_capacity(-1)\n    print(values)\n",
            "AU4003",
            "collection capacity cannot be negative",
        ),
        (
            "list constructor allocation failure",
            "def main():\n    values = list[int64].with_capacity(9223372036854775807)\n    print(values)\n",
            "AU4005",
            "list capacity allocation failed",
        ),
        (
            "dict constructor allocation failure",
            "def main():\n    values = dict[str, int64].with_capacity(9223372036854775807)\n    print(values)\n",
            "AU4005",
            "dictionary capacity allocation failed",
        ),
        (
            "set constructor allocation failure",
            "def main():\n    values = set[str].with_capacity(9223372036854775807)\n    print(values)\n",
            "AU4005",
            "set capacity allocation failed",
        ),
        (
            "list reserve negative capacity",
            "def main():\n    mut values: list[int64] = []\n    values.reserve(-1)\n",
            "AU4003",
            "collection capacity cannot be negative",
        ),
        (
            "dict reserve negative capacity",
            "def main():\n    mut values: dict[str, int64] = {}\n    values.reserve(-1)\n",
            "AU4003",
            "collection capacity cannot be negative",
        ),
        (
            "set reserve negative capacity",
            "def main():\n    mut values: set[str] = set[str]()\n    values.reserve(-1)\n",
            "AU4003",
            "collection capacity cannot be negative",
        ),
        (
            "list reserve allocation failure",
            "def main():\n    mut values: list[int64] = []\n    values.reserve(9223372036854775807)\n",
            "AU4005",
            "list capacity allocation failed",
        ),
        (
            "dict reserve allocation failure",
            "def main():\n    mut values: dict[str, int64] = {}\n    values.reserve(9223372036854775807)\n",
            "AU4005",
            "dictionary capacity allocation failed",
        ),
        (
            "set reserve allocation failure",
            "def main():\n    mut values: set[str] = set[str]()\n    values.reserve(9223372036854775807)\n",
            "AU4005",
            "set capacity allocation failed",
        ),
    ];

    for (label, source, code, message) in cases {
        let error = crate::run_source(source).expect_err(label);
        assert_eq!(error.code, code, "{label}");
        assert_eq!(error.message, message, "{label}");
    }
}

#[test]
fn canonical_list_index_absence_is_au4008_with_actionable_help() {
    let error = crate::run_source(
        "def main():\n    values: list[int64] = [10, 20]\n    print(values.index(30))\n",
    )
    .expect_err("list.index must reject an absent value");

    assert_eq!(error.code, "AU4008");
    assert_eq!(error.message, "collection value was not found");
    assert_eq!(
        error.help,
        ["check `value in values` before searching when absence is expected"]
    );
}

#[test]
fn public_mir_collection_absence_and_clear_contracts_are_exact() {
    let list_error = crate::run_source(
        "def main():\n    mut values: list[int64] = [10, 20]\n    values.remove(30)\n",
    )
    .expect_err("removing an absent list value must fail");
    assert_eq!(list_error.code, "AU4008");
    assert_eq!(list_error.message, "collection value was not found");
    assert_eq!(
        list_error.help,
        ["check `value in values` before removing when absence is expected"]
    );

    let dict_error = crate::run_source(
        "def main():\n    values: dict[str, int64] = {\"present\": 1}\n    print(values[\"missing\"])\n",
    )
    .expect_err("indexing a missing dictionary key must fail");
    assert_eq!(dict_error.code, "AU4003");
    assert_eq!(dict_error.message, "dict key `missing` was not present");
    assert_eq!(dict_error.span, Some(Span::new(3, 18)));

    let output = crate::run_source(
        r#"
def main():
    mut values: set[str] = {"Aura", "systems"}
    values.clear()
    print(values.len())
    print(values.is_empty())
"#,
    )
    .expect("clearing a mutable set should execute through MIR");
    assert_eq!(output.stdout, "0\ntrue\n");
}

#[test]
fn public_mir_flush_and_constant_failure_cleanup_are_observable() {
    let output = crate::run_source(include_str!(
        "../tests/fixtures/run-pass/io_write_builtin.au"
    ))
    .expect("the canonical io.write/io.flush fixture should execute through MIR");
    assert_eq!(output.stdout, "hello");

    let error = crate::run_source(
        r#"
def initialize() -> int64:
    print("initializing")
    return 1 // 0

value = initialize()

def main():
    print(value)
"#,
    )
    .expect_err("a failing module constant must prevent main from running");
    assert_eq!(error.code, "AU4004");
    assert_eq!(error.message, "division by zero");
    assert_eq!(error.partial_stdout(), Some("initializing\n"));
}

#[test]
fn guarded_or_pattern_expressions_preserve_owned_commit_and_mutable_writeback() {
    let output = crate::run_source(
        r#"
enum Payload:
    Text(str)
    Bytes(str)

enum Reading:
    Exact(int32)
    Approx(int32)

enum Code:
    Value(int32)

def take(value: own Payload) -> str:
    return match own value:
        case Payload.Text(text) | Payload.Bytes(text) if len(text) > 0: text
        case Payload.Text(text) | Payload.Bytes(text): text

def mutate_and_reject(value: mut int32) -> bool:
    value += 5
    return false

def choose(reading: mut Reading) -> int32:
    return match mut reading:
        case Reading.Exact(value) | Reading.Approx(value) if mutate_and_reject(value): 0
        case Reading.Approx(value): value
        case Reading.Exact(value): 0 - value

def take_pair(value: own (int32, str)) -> str:
    return match own value:
        case (1 | 2, text): text
        case (_, text): text

def code_label(code: own Code) -> str:
    return match own code:
        case Code.Value(1 | 2): "small"
        case Code.Value(_): "other"

def main():
    print(take(Payload.Bytes("owned")))
    print(f"<{take(Payload.Text(""))}>")
    print(take_pair((2, "pair")))
    print(code_label(Code.Value(1)))
    print(code_label(Code.Value(7)))

    mut reading = Reading.Approx(1)
    print(choose(reading))
    match reading:
        case Reading.Approx(value):
            print(value)
        case Reading.Exact(value):
            print(0 - value)
"#,
    )
    .expect("guarded or-pattern expressions should execute through MIR");

    assert_eq!(output.stdout, "owned\n<>\npair\nsmall\nother\n6\n6\n");
}

#[test]
fn mir_public_numeric_parse_failure_and_bare_function_value_are_exact() {
    let output = crate::run_source(
        r#"
def double(value: int64) -> int64:
    return value * 2

def main():
    callback = double
    print(callback(21))
    match parse_int64("not-an-integer"):
        case Result.Ok(value):
            print(value)
        case Result.Err(message):
            print(message)
"#,
    )
    .expect("public function values and parse failure results should execute through MIR");

    assert_eq!(output.stdout, "42\ninvalid digit found in string\n");
}

#[test]
fn mir_f_string_format_variants_match_the_runtime_contract() {
    let output = crate::run_source(
        r#"
def main():
    positive: int64 = 12345
    value32: float32 = 1.5
    pair = (1, "one")
    print(f"|{'Aura':<8s}|")
    print(f"{positive:-d} {positive:,.2f} {0:.0e} {999:.1e}")
    print(f"{12345.5:,.2f} {0.0012:.2e}")
    print(f"{value32} {pair}")
"#,
    )
    .expect("accepted format variants should execute through MIR");

    assert_eq!(
        output.stdout,
        "|Aura    |\n12345 12,345.00 0e+00 1.0e+03\n12,345.50 1.20e-03\n1.5 (1, one)\n"
    );
}

#[test]
fn mir_math_edges_preserve_values_signed_zero_and_au_classes() {
    let output = crate::run_source(
        r#"
import math

def main():
    print(math.floor(-1.25))
    print(math.ceil(-1.25))
    print(math.trunc(-1.75))
    print(math.pow(math.nan, 0.0))
    print(math.pow(1.0, math.nan))
    print(math.pow(-0.0, 3.0))
    print(math.exp(0.0 - math.inf))
    print(math.log(math.inf))
    print(math.log2(math.inf))
    print(math.log10(math.inf))
    print(math.sin(-0.0))
    print(math.cos(-0.0))
    print(math.tan(-0.0))
    print(math.exp(math.nan))
    print(math.log(math.nan))
    print(math.sin(math.nan))
    print(round(-9223372036854775808.0))
    print(divmod(0.0, -3.0))
"#,
    )
    .expect("accepted math edge values should execute through MIR");

    assert_eq!(
        output.stdout,
        "-2\n-1\n-1\n1.0\n1.0\n-0.0\n0.0\ninf\ninf\ninf\n-0.0\n1.0\n-0.0\nNaN\nNaN\nNaN\n-9223372036854775808\n(-0.0, -0.0)\n"
    );

    let failures = [
        ("math.floor(math.inf)", "AU4002"),
        ("math.ceil(math.nan)", "AU4002"),
        ("math.trunc(0.0 - math.inf)", "AU4002"),
        ("math.pow(0.0, -1.0)", "AU4001"),
        ("math.pow(-2.0, 0.5)", "AU4001"),
        ("math.pow(1.7976931348623157e308, 2.0)", "AU4002"),
        ("math.exp(1000.0)", "AU4002"),
        ("math.log(0.0)", "AU4001"),
        ("math.log2(-1.0)", "AU4001"),
        ("math.log10(-1.0)", "AU4001"),
        ("math.sin(math.inf)", "AU4001"),
        ("math.cos(0.0 - math.inf)", "AU4001"),
        ("math.tan(math.inf)", "AU4001"),
        ("round(9223372036854775808.0)", "AU4002"),
        ("divmod(1, 0)", "AU4004"),
    ];
    for (expression, code) in failures {
        let source = format!("import math\n\ndef main():\n    print({expression})\n");
        let error = crate::run_source(&source).expect_err(expression);
        assert_eq!(error.code, code, "{expression}");
    }
}

#[test]
fn mir_numeric_overflow_closures_preserve_public_diagnostics_and_spans() {
    let cases = [
        (
            "integer unary negation",
            r#"
def main():
    minimum: int64 = -9223372036854775808
    print(-minimum)
"#,
            "AU4002",
            "integer value `9223372036854775808` does not fit in `int64`",
            true,
        ),
        (
            "integer abs",
            r#"
def main():
    minimum: int64 = -9223372036854775808
    print(abs(minimum))
"#,
            "AU4002",
            "integer value `9223372036854775808` does not fit in `int64`",
            false,
        ),
        (
            "floating power",
            r#"
def main():
    base: float64 = 1.7976931348623157e308
    exponent: float64 = 2.0
    print(base ** exponent)
"#,
            "AU4002",
            "floating power overflow",
            true,
        ),
        (
            "integer power",
            r#"
def main():
    base: int8 = 2
    exponent: int8 = 7
    print(base ** exponent)
"#,
            "AU4002",
            "integer power overflow",
            true,
        ),
        (
            "integer shift",
            r#"
def main():
    value: int8 = 1
    count: int8 = 8
    print(value << count)
"#,
            "AU4002",
            "integer shift count `8` is outside the required range `0..8`",
            true,
        ),
        (
            "integer shift method",
            r#"
def main():
    value: uint8 = 1
    count: uint8 = 8
    print(value.wrapping_shl(count))
"#,
            "AU4002",
            "integer shift count `8` is outside the required range `0..8`",
            false,
        ),
    ];

    for (label, source, code, message, has_span) in cases {
        let error = crate::run_source(source).expect_err(label);
        assert_eq!(error.code, code, "{label}");
        assert_eq!(error.message, message, "{label}");
        assert_eq!(error.span.is_some(), has_span, "{label} source span");
    }
}

#[test]
fn mir_trait_method_arguments_preserve_mutable_writeback_through_dispatch() {
    let output = crate::run_source(include_str!(
        "../tests/fixtures/run-pass/trait_impl_queue_borrow_mut_writeback.au"
    ))
    .expect("trait method arguments should execute through MIR");

    assert_eq!(output.stdout, "7\n7\n");
}

fn lower_ffi_runtime_source(source: &str) -> MirModule {
    let module = crate::parse_source(source).expect("FFI runtime source should parse");
    let program = crate::check_module_with_builtin_imports(module)
        .expect("FFI runtime source should type check");
    crate::mir::lower(&program)
}

#[test]
fn mir_arrays_execute_dense_members_kernels_and_row_major_callbacks() {
    let source = r#"
def double(value: int32) -> float64:
    print(value)
    return value.to_float() * 2.0

def main():
    source: list[int32] = [1, 2, 3, 4]
    scalar: int32 = 10
    mut values = Array[int32].from_list(values=source, shape=[2, 2])
    copied = values.clone()
    print(values.shape())
    print(values.len())
    print(values.get(index=[-3, 0]))
    print(values.get(index=[0]))
    print(values.get(index=[-1, -1]))
    print(values.set(index=[0, 1], value=7))
    values[0, 1] = 6
    print(values[0, 1])
    print(values[-1, -1])
    print(values[:1])
    print(values + copied)
    print(values + scalar)
    print(scalar - values)
    print(values.wrapping_add(rhs=2147483647))
    print(values.saturating_mul(rhs=2147483647))
    print(values.map(f=double))
    print(values.sum())
    print(values.min())
    print(values.max())
    print(values.mean())
"#;
    let output = crate::run_source(source)
        .expect("the complete Array surface should execute through the MIR interpreter");
    assert_eq!(
        output.stdout,
        "\
[2, 2]\n\
4\n\
Option.None\n\
Option.None\n\
Option.Some(4)\n\
Option.Some(2)\n\
6\n\
4\n\
Array[int32](shape=[1, 2], values=[1, 6])\n\
Array[int32](shape=[2, 2], values=[2, 8, 6, 8])\n\
Array[int32](shape=[2, 2], values=[11, 16, 13, 14])\n\
Array[int32](shape=[2, 2], values=[9, 4, 7, 6])\n\
Array[int32](shape=[2, 2], values=[-2147483648, -2147483643, -2147483646, -2147483645])\n\
Array[int32](shape=[2, 2], values=[2147483647, 2147483647, 2147483647, 2147483647])\n\
1\n\
6\n\
3\n\
4\n\
Array[float64](shape=[2, 2], values=[2.0, 12.0, 6.0, 8.0])\n\
14\n\
1\n\
6\n\
3.5\n"
    );
    assert_eq!(output.value, Value::Unit);
}

#[test]
fn mir_arrays_preserve_named_argument_order_and_capturing_map_results() {
    let source = r#"
def shape() -> list[int64]:
    print("shape")
    return [2]

def seed() -> int32:
    print("seed")
    return 4

def coordinate() -> list[int64]:
    print("index")
    return [1]

def replacement() -> int32:
    print("value")
    return 7

def main():
    mut values = Array[int32].full(value=seed(), shape=shape())
    offset: int32 = 3
    mapped = values.map(f=lambda item: item + offset)
    print(mapped)
    print(values.set(value=replacement(), index=coordinate()))
    print(values)
"#;
    let output = crate::run_source(source)
        .expect("named Array arguments and a capturing map callback should execute in MIR");
    assert_eq!(
        output.stdout,
        "\
seed\n\
shape\n\
Array[int32](shape=[2], values=[7, 7])\n\
value\n\
index\n\
Option.Some(4)\n\
Array[int32](shape=[2], values=[4, 7])\n"
    );
}

#[test]
fn mir_array_place_operations_borrow_storage_and_mutate_in_place() {
    let module = crate::lower_source_to_mir(
        r#"
def keep(value: int32) -> int32:
    return value

def main():
    pass
"#,
    )
    .expect("Array borrow regression callback should lower");
    let mut runtime = MirRuntime::new(
        module,
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    env.define_typed(
        "values",
        Type::Named("Array".to_string(), vec![Type::named("int32")]),
        Value::Array(
            ArrayValue::new(
                vec![4].into_boxed_slice(),
                ArrayStorage::Int32(vec![1, 2, 3, 4].into_boxed_slice()),
            )
            .unwrap(),
        ),
    );
    let member = |field: &str| CallTarget::Member {
        object: Operand::Place("values".to_string()),
        field: field.to_string(),
        receiver_place: Some("values".to_string()),
    };

    for (field, args) in [
        ("shape", Vec::new()),
        ("get", vec![mir_arg(Some("index"), Operand::Int(0))]),
        (
            "__index",
            vec![
                mir_arg(None, Operand::Int(0)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ],
        ),
        ("sum", Vec::new()),
        ("min", Vec::new()),
        ("max", Vec::new()),
        ("mean", Vec::new()),
        ("wrapping_add", vec![mir_arg(Some("rhs"), Operand::Int(1))]),
        (
            "wrapping_add",
            vec![mir_arg(Some("rhs"), Operand::Place("values".to_string()))],
        ),
        (
            "saturating_mul",
            vec![mir_arg(Some("rhs"), Operand::Int(2))],
        ),
    ] {
        let clone_count = super::mir_array_place_clone_count();
        runtime
            .evaluate_call(&member(field), &args, &mut env)
            .unwrap_or_else(|error| panic!("Array.{field} should execute: {error}"));
        assert_eq!(
            super::mir_array_place_clone_count(),
            clone_count,
            "shared Array.{field} must not deep-clone its receiver"
        );
    }

    let clone_count = super::mir_array_place_clone_count();
    let mapped = runtime
        .evaluate_call(
            &member("map"),
            &[mir_arg(
                Some("f"),
                test_function_operand("keep", vec![Type::named("int32")], Type::named("int32")),
            )],
            &mut env,
        )
        .expect("Array.map should execute while borrowing its source storage");
    assert_eq!(
        mapped,
        Value::Array(
            ArrayValue::new(
                vec![4].into_boxed_slice(),
                ArrayStorage::Int32(vec![1, 2, 3, 4].into_boxed_slice()),
            )
            .unwrap()
        )
    );
    assert_eq!(
        super::mir_array_place_clone_count(),
        clone_count,
        "Array.map must borrow the source and read one scalar at a time"
    );

    for field in ["clone", "shape"] {
        let clone_count = super::mir_array_place_clone_count();
        let result = crate::runtime_value::with_array_allocation_budget(0, || {
            runtime.evaluate_call(&member(field), &[], &mut env)
        });
        let error = match result {
            Err(error) => error,
            Ok(value) => {
                panic!(
                    "Array.{field} should report an injected allocation failure, found {value:?}"
                )
            }
        };
        assert_eq!(error.code, "AU4005");
        assert_eq!(
            super::mir_array_place_clone_count(),
            clone_count,
            "Array.{field} must borrow its source before fallible result allocation"
        );
    }

    for (label, left, right) in [
        (
            "same-place array addition",
            Operand::Place("values".to_string()),
            Operand::Place("values".to_string()),
        ),
        (
            "array-scalar addition",
            Operand::Place("values".to_string()),
            Operand::Int(1),
        ),
        (
            "scalar-array addition",
            Operand::Int(1),
            Operand::Place("values".to_string()),
        ),
    ] {
        let clone_count = super::mir_array_place_clone_count();
        runtime
            .evaluate_rvalue(
                &Rvalue::Binary {
                    op: crate::ast::BinaryOp::Add,
                    left,
                    right,
                    span: Span::new(1, 1),
                },
                &mut env,
            )
            .unwrap_or_else(|error| panic!("{label} should execute: {error}"));
        assert_eq!(
            super::mir_array_place_clone_count(),
            clone_count,
            "{label} must borrow retained Array operands"
        );
    }

    let source_allocation = match env.place_ref("values").unwrap() {
        Value::Array(ArrayValue {
            storage: ArrayStorage::Int32(values),
            ..
        }) => values.as_ptr(),
        other => panic!("expected int32 Array, found {other:?}"),
    };
    for (field, args) in [
        (
            "set",
            vec![
                mir_arg(Some("index"), Operand::Int(0)),
                mir_arg(Some("value"), Operand::Int(9)),
            ],
        ),
        ("fill", vec![mir_arg(Some("value"), Operand::Int(6))]),
        (
            "__set_index",
            vec![
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(8)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ],
        ),
    ] {
        let clone_count = super::mir_array_place_clone_count();
        runtime
            .evaluate_call(&member(field), &args, &mut env)
            .unwrap_or_else(|error| panic!("Array.{field} should execute: {error}"));
        assert_eq!(
            super::mir_array_place_clone_count(),
            clone_count,
            "mutable Array.{field} must not clone its receiver before writeback"
        );
        match env.place_ref("values").unwrap() {
            Value::Array(ArrayValue {
                storage: ArrayStorage::Int32(values),
                ..
            }) => assert_eq!(
                values.as_ptr(),
                source_allocation,
                "Array.{field} must mutate the existing storage allocation"
            ),
            other => panic!("expected int32 Array, found {other:?}"),
        }
    }
}

#[test]
fn mir_array_containing_vec_and_map_copies_are_independent() {
    let output = crate::run_source(
        r#"
def first(values: list[Array[int32]]) -> Array[int32]:
    match own values.get(0):
        case Option.Some(value):
            return value
        case Option.None:
            return Array[int32].zeros([1])

def named(values: dict[str, Array[int32]]) -> Array[int32]:
    match own values.get("values"):
        case Option.Some(value):
            return value
        case Option.None:
            return Array[int32].zeros([1])

def main():
    source_values: list[int32] = [1, 2]
    source = Array[int32].from_list(source_values, [2])

    nested: list[Array[int32]] = [source.clone()]
    copied = nested.copy()
    mut copied_array = first(copied)
    copied_array[0] = 9
    print(first(nested))
    print(copied_array)

    sliced = nested[:]
    mut sliced_array = first(sliced)
    sliced_array[0] = 8
    print(first(nested))
    print(sliced_array)

    lookup: dict[str, Array[int32]] = {"values": source.clone()}
    mut mapped_array = named(lookup)
    mapped_array[0] = 7
    print(named(lookup))
    print(mapped_array)
"#,
    )
    .expect("Array-containing Vec and Map copies should execute");
    assert_eq!(
        output.stdout,
        "\
Array[int32](shape=[2], values=[1, 2])\n\
Array[int32](shape=[2], values=[9, 2])\n\
Array[int32](shape=[2], values=[1, 2])\n\
Array[int32](shape=[2], values=[8, 2])\n\
Array[int32](shape=[2], values=[1, 2])\n\
Array[int32](shape=[2], values=[7, 2])\n"
    );
}

#[test]
fn mir_nested_array_clone_allocation_failure_is_au4005_and_preserves_source() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "nested",
        Type::Named(
            "list".to_string(),
            vec![Type::Named("Array".to_string(), vec![Type::named("int32")])],
        ),
        Value::Vec(VecValue {
            element_type: Type::Named("Array".to_string(), vec![Type::named("int32")]),
            elements: vec![Value::Array(
                ArrayValue::new(
                    vec![2].into_boxed_slice(),
                    ArrayStorage::Int32(vec![1, 2].into_boxed_slice()),
                )
                .unwrap(),
            )],
        }),
    );
    let source_storage = match env.place_ref("nested").unwrap() {
        Value::Vec(vector) => match &vector.elements[0] {
            Value::Array(ArrayValue {
                storage: ArrayStorage::Int32(values),
                ..
            }) => values.as_ptr(),
            other => panic!("expected nested Array, found {other:?}"),
        },
        other => panic!("expected list[Array[int32]], found {other:?}"),
    };

    let error = crate::runtime_value::with_array_allocation_budget(1, || {
        runtime.evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("nested".to_string()),
                field: "copy".to_string(),
                receiver_place: Some("nested".to_string()),
            },
            &[],
            &mut env,
        )
    })
    .expect_err("nested Array allocation failure should trap");
    assert_eq!(error.code, "AU4005");
    match env.place_ref("nested").unwrap() {
        Value::Vec(vector) => match &vector.elements[0] {
            Value::Array(ArrayValue {
                storage: ArrayStorage::Int32(values),
                ..
            }) => assert_eq!(values.as_ptr(), source_storage),
            other => panic!("expected nested Array, found {other:?}"),
        },
        other => panic!("expected list[Array[int32]], found {other:?}"),
    }
}

#[test]
fn mir_runtime_ffi_calls_process_symbols_and_reports_missing_lookups() {
    let getpid = lower_ffi_runtime_source(
        r#"
public extern "C" def getpid() -> int32

def main() -> int32:
    return getpid()
"#,
    );
    let output =
        super::run_trusted(&getpid).expect("trusted compiler-lowered FFI MIR should execute");
    let Value::Int(process_id) = output.value else {
        panic!("getpid should return int32");
    };
    assert!(process_id.as_i128().is_some_and(|value| value > 0));
    assert_eq!(process_id.runtime_kind(), Some(IntegerKind::Int32));

    let serialized_getpid = serde_json::to_vec(&getpid).expect("trusted FFI MIR should serialize");
    let embedded = super::run_serialized_mir_trusted(
        &serialized_getpid,
        "/tmp/trusted-getpid.au",
        "def main():\n    pass\n",
    )
    .expect("compiler-embedded FFI MIR should retain its trusted runtime route");
    let Value::Int(embedded_process_id) = embedded.value else {
        panic!("embedded getpid should return int32");
    };
    assert!(embedded_process_id.as_i128().is_some_and(|value| value > 0));
    assert_eq!(embedded_process_id.runtime_kind(), Some(IntegerKind::Int32));

    let missing = lower_ffi_runtime_source(
        r#"
public extern "C" def __aura_missing_mir_ffi_symbol__() -> int32

def main():
    __aura_missing_mir_ffi_symbol__()
"#,
    );
    let error = super::run_trusted(&missing).expect_err("a missing FFI symbol must trap");
    assert_eq!(error.code, "AU4005");
    assert!(error
        .message
        .starts_with("FFI call to `__aura_missing_mir_ffi_symbol__` failed: FFI symbol"));
}

#[test]
fn public_mir_execution_rejects_caller_supplied_ffi_metadata() {
    let forged = MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<forged>".to_string(),
            source_path: Some("/tmp/forged.au".to_string()),
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![MirLocalType {
                name: "process_id".to_string(),
                ty: Type::named("int32"),
            }],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::Assign {
                    target: "process_id".to_string(),
                    value: Rvalue::Call {
                        callee: CallTarget::Extern(MirExternCall {
                            symbol: "getpid".to_string(),
                            abi: "C".to_string(),
                            params: Vec::new(),
                            return_type: Type::named("int32"),
                        }),
                        args: Vec::new(),
                    },
                }],
                terminator: Terminator::Return(Operand::Place("process_id".to_string())),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let public_results = [
        ("run_mir", crate::run_mir(&forged)),
        (
            "run_mir_with_stdout_sink",
            crate::run_mir_with_stdout_sink(&forged, None),
        ),
        (
            "run_mir_with_stdout_sink_and_program_args",
            crate::run_mir_with_stdout_sink_and_program_args(&forged, None, Vec::new()),
        ),
        (
            "run_mir_entry",
            crate::run_mir_entry(&forged, Some("main"), None, Vec::new()),
        ),
    ];
    let mut rejection_message = None;
    for (api, result) in public_results {
        let diagnostic =
            result.expect_err("safe public MIR execution must reject caller-supplied FFI metadata");
        assert_eq!(diagnostic.code, "AU4001", "{api}: {diagnostic:?}");
        assert!(
            diagnostic.message.contains("getpid"),
            "{api}: {}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("manifest-rooted package")
                && diagnostic.message.contains("allow_ffi = true")
                && diagnostic.message.contains("path-based"),
            "{api}: {}",
            diagnostic.message
        );
        match &rejection_message {
            Some(expected) => assert_eq!(&diagnostic.message, expected, "{api}"),
            None => rejection_message = Some(diagnostic.message),
        }
    }

    let serialized = serde_json::to_vec(&forged).expect("forged MIR should serialize");
    let deserialized =
        crate::run_serialized_mir(&serialized, "/tmp/forged.au", "def main():\n    pass\n")
            .expect_err("safe serialized-MIR execution must reject forged FFI metadata");
    assert_eq!(deserialized.code, "AU4001");
    assert_eq!(Some(deserialized.message), rejection_message);

    let mut write_forged = forged.clone();
    let Instruction::Assign { value, .. } = &write_forged.functions[0].blocks[0].instructions[0]
    else {
        panic!("fixture must begin with the forged extern assignment");
    };
    write_forged.functions[0].blocks[0].instructions[0] = Instruction::WriteLoan {
        loan: "forged_alias".to_string(),
        value: value.clone(),
    };
    let nested = crate::run_mir(&write_forged)
        .expect_err("safe public MIR execution must inspect WriteLoan rvalues for extern calls");
    assert_eq!(nested.code, "AU4001");
    assert!(nested.message.contains("getpid"));

    let serialized = serde_json::to_vec(&write_forged).expect("forged MIR should serialize");
    let nested_serialized =
        crate::run_serialized_mir(&serialized, "/tmp/forged.au", "def main():\n    pass\n")
            .expect_err("serialized MIR must inspect WriteLoan rvalues for extern calls");
    assert_eq!(nested_serialized.code, "AU4001");
    assert!(nested_serialized.message.contains("getpid"));
}

#[test]
fn mir_runtime_ffi_marshalling_preserves_boundaries_mutable_writeback_and_opaque_handles() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "minimum",
        Type::named("int8"),
        Value::Int(
            IntegerValue::from_typed_signed(i8::MIN.into(), IntegerKind::Int8)
                .expect("i8 minimum is representable"),
        ),
    );
    env.define_typed(
        "maximum",
        Type::named("uint64"),
        Value::Int(IntegerValue::from_u64(u64::MAX)),
    );
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        super::bytes_runtime_value(&[0, 254]),
    );
    let source_pointer = 0x1234usize as *mut c_void;
    env.define_typed(
        "handle",
        Type::named("Token"),
        Value::FfiHandle(
            FfiHandleValue::new("Token".to_string(), source_pointer)
                .expect("test handle is non-null"),
        ),
    );

    let call = MirExternCall {
        symbol: "aura_test_exchange".to_string(),
        abi: "C".to_string(),
        params: vec![
            MirExternParam {
                name: "minimum".to_string(),
                passing: MirReceiverKind::Borrow,
                ty: Type::named("int8"),
            },
            MirExternParam {
                name: "maximum".to_string(),
                passing: MirReceiverKind::Borrow,
                ty: Type::named("uint64"),
            },
            MirExternParam {
                name: "bytes".to_string(),
                passing: MirReceiverKind::BorrowMut,
                ty: Type::Named("list".to_string(), vec![Type::named("uint8")]),
            },
            MirExternParam {
                name: "handle".to_string(),
                passing: MirReceiverKind::Value,
                ty: Type::named("Token"),
            },
        ],
        return_type: Type::named("Token"),
    };
    let args = vec![
        MirArg {
            name: None,
            value: Operand::Place("minimum".to_string()),
            writeback_place: None,
        },
        MirArg {
            name: None,
            value: Operand::Place("maximum".to_string()),
            writeback_place: None,
        },
        MirArg {
            name: None,
            value: Operand::Place("bytes".to_string()),
            writeback_place: Some("bytes".to_string()),
        },
        MirArg {
            name: None,
            value: Operand::MovePlace("handle".to_string()),
            writeback_place: None,
        },
    ];
    let returned_pointer = 0x5678usize as *mut c_void;
    let result = runtime
        .evaluate_extern_call_with(&call, &args, &mut env, |symbol, signature, arguments| {
            assert_eq!(symbol, "aura_test_exchange");
            assert_eq!(
                signature.parameters(),
                &[
                    crate::ffi::FfiType::I8,
                    crate::ffi::FfiType::U64,
                    crate::ffi::FfiType::BytesViewMut,
                    crate::ffi::FfiType::OpaqueHandle,
                ]
            );
            assert_eq!(signature.result(), crate::ffi::FfiType::OpaqueHandle);
            assert!(matches!(arguments[0], crate::ffi::FfiValue::I8(i8::MIN)));
            assert!(matches!(arguments[1], crate::ffi::FfiValue::U64(u64::MAX)));
            assert!(matches!(
                &arguments[2],
                crate::ffi::FfiValue::Bytes(bytes) if bytes == &[0, 254]
            ));
            assert!(matches!(
                &arguments[3],
                crate::ffi::FfiValue::OpaqueHandle(handle)
                    if handle.as_ptr() == source_pointer
            ));
            let crate::ffi::FfiValue::Bytes(bytes) = &mut arguments[2] else {
                unreachable!("third argument is the mutable byte view");
            };
            bytes.copy_from_slice(&[1, 255]);
            Ok(crate::ffi::FfiValue::OpaqueHandle(
                crate::ffi::OpaqueHandle::new(returned_pointer)
                    .expect("returned test handle is non-null"),
            ))
        })
        .expect("the injected FFI call should succeed");

    assert!(
        env.read_place("handle").is_err(),
        "own handles are consumed"
    );
    assert_eq!(
        env.read_place("bytes").expect("bytes remain live").render(),
        "[1, 255]"
    );
    let Value::FfiHandle(handle) = result else {
        panic!("opaque results should use the dedicated runtime representation");
    };
    assert_eq!(handle.type_name(), "Token");
    assert_eq!(handle.as_ptr(), returned_pointer);
    assert_eq!(Value::FfiHandle(handle).render(), "<opaque Token>");
}

#[test]
fn mir_runtime_ffi_maps_noncanonical_bool_returns_to_au4001() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let call = MirExternCall {
        symbol: "bad_bool".to_string(),
        abi: "C".to_string(),
        params: Vec::new(),
        return_type: Type::named("bool"),
    };
    let error = runtime
        .evaluate_extern_call_with(&call, &[], &mut env, |_, _, _| {
            Err(crate::ffi::FfiError::NonCanonicalBoolReturn(2))
        })
        .expect_err("a noncanonical C bool must trap");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "FFI call to `bad_bool` failed: FFI bool return must be encoded as 0 or 1, but received 2"
    );
}

#[test]
fn mir_runtime_ffi_marshals_every_scalar_and_shared_view_source_shape() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let values = [
        ("boolean", Type::named("bool"), Value::Bool(true)),
        (
            "i8_value",
            Type::named("int8"),
            Value::Int(
                IntegerValue::from_typed_signed((-8).into(), IntegerKind::Int8)
                    .expect("-8 is representable as int8"),
            ),
        ),
        (
            "i16_value",
            Type::named("int16"),
            Value::Int(
                IntegerValue::from_typed_signed((-1_600).into(), IntegerKind::Int16)
                    .expect("-1600 is representable as int16"),
            ),
        ),
        (
            "i32_value",
            Type::named("int32"),
            Value::Int(IntegerValue::from_i32(-32_000)),
        ),
        (
            "int_value",
            Type::named("int"),
            Value::Int(IntegerValue::from_i64(-64_000)),
        ),
        (
            "i64_value",
            Type::named("int64"),
            Value::Int(IntegerValue::from_i64(i64::MIN)),
        ),
        (
            "u8_value",
            Type::named("uint8"),
            Value::Int(
                IntegerValue::from_typed_unsigned(8, IntegerKind::Uint8)
                    .expect("8 is representable as uint8"),
            ),
        ),
        (
            "u16_value",
            Type::named("uint16"),
            Value::Int(
                IntegerValue::from_typed_unsigned(1_600, IntegerKind::Uint16)
                    .expect("1600 is representable as uint16"),
            ),
        ),
        (
            "u32_value",
            Type::named("uint32"),
            Value::Int(
                IntegerValue::from_typed_unsigned(32_000, IntegerKind::Uint32)
                    .expect("32000 is representable as uint32"),
            ),
        ),
        (
            "u64_value",
            Type::named("uint64"),
            Value::Int(IntegerValue::from_u64(u64::MAX)),
        ),
        ("f32_value", Type::named("float32"), Value::Float(3.5)),
        ("f64_value", Type::named("float64"), Value::Float(7.25)),
        ("text", Type::named("str"), Value::String(String::new())),
        (
            "bytes",
            Type::Named("list".to_string(), vec![Type::named("uint8")]),
            super::bytes_runtime_value(&[]),
        ),
    ];
    for (name, ty, value) in values {
        env.define_typed(name, ty, value);
    }

    let parameter_specs = [
        ("boolean", Type::named("bool")),
        ("i8_value", Type::named("int8")),
        ("i16_value", Type::named("int16")),
        ("i32_value", Type::named("int32")),
        ("int_value", Type::named("int")),
        ("i64_value", Type::named("int64")),
        ("u8_value", Type::named("uint8")),
        ("u16_value", Type::named("uint16")),
        ("u32_value", Type::named("uint32")),
        ("u64_value", Type::named("uint64")),
        ("f32_value", Type::named("float32")),
        ("f64_value", Type::named("float64")),
        ("text", Type::named("str")),
        (
            "bytes",
            Type::Named("list".to_string(), vec![Type::named("uint8")]),
        ),
    ];
    let call = MirExternCall {
        symbol: "aura_test_observe_all".to_string(),
        abi: "C".to_string(),
        params: parameter_specs
            .iter()
            .map(|(name, ty)| MirExternParam {
                name: (*name).to_string(),
                passing: MirReceiverKind::Borrow,
                ty: ty.clone(),
            })
            .collect(),
        return_type: Type::Unit,
    };
    let args = parameter_specs
        .iter()
        .map(|(name, _)| MirArg {
            name: None,
            value: Operand::Place((*name).to_string()),
            writeback_place: None,
        })
        .collect::<Vec<_>>();

    let result = runtime
        .evaluate_extern_call_with(&call, &args, &mut env, |symbol, signature, arguments| {
            use crate::ffi::{FfiType, FfiValue};

            assert_eq!(symbol, "aura_test_observe_all");
            assert_eq!(
                signature.parameters(),
                &[
                    FfiType::Bool,
                    FfiType::I8,
                    FfiType::I16,
                    FfiType::I32,
                    FfiType::I64,
                    FfiType::I64,
                    FfiType::U8,
                    FfiType::U16,
                    FfiType::U32,
                    FfiType::U64,
                    FfiType::F32,
                    FfiType::F64,
                    FfiType::StringView,
                    FfiType::BytesView,
                ]
            );
            assert_eq!(signature.result(), FfiType::Unit);
            assert_eq!(
                arguments,
                &[
                    FfiValue::Bool(true),
                    FfiValue::I8(-8),
                    FfiValue::I16(-1_600),
                    FfiValue::I32(-32_000),
                    FfiValue::I64(-64_000),
                    FfiValue::I64(i64::MIN),
                    FfiValue::U8(8),
                    FfiValue::U16(1_600),
                    FfiValue::U32(32_000),
                    FfiValue::U64(u64::MAX),
                    FfiValue::F32(3.5),
                    FfiValue::F64(7.25),
                    FfiValue::String(String::new()),
                    FfiValue::Bytes(Vec::new()),
                ]
            );
            Ok(FfiValue::Unit)
        })
        .expect("all FFI v0 scalar and shared-view shapes should marshal");
    assert_eq!(result, Value::Unit);
}

#[test]
fn mir_runtime_ffi_unmarshals_every_fixed_width_scalar_result() {
    fn evaluate(
        runtime: &mut MirRuntime,
        ty: Type,
        expected_ffi_type: crate::ffi::FfiType,
        returned: crate::ffi::FfiValue,
    ) -> Value {
        let call = MirExternCall {
            symbol: "aura_test_scalar_result".to_string(),
            abi: "C".to_string(),
            params: Vec::new(),
            return_type: ty,
        };
        runtime
            .evaluate_extern_call_with(
                &call,
                &[],
                &mut Env::default(),
                move |symbol, signature, arguments| {
                    assert_eq!(symbol, "aura_test_scalar_result");
                    assert!(arguments.is_empty());
                    assert_eq!(signature.parameters(), &[]);
                    assert_eq!(signature.result(), expected_ffi_type);
                    Ok(returned)
                },
            )
            .expect("matching FFI scalar result should unmarshal")
    }

    fn assert_integer(value: Value, expected: i128, kind: IntegerKind) {
        let Value::Int(value) = value else {
            panic!("expected integer result");
        };
        assert_eq!(value.as_i128(), Some(expected));
        assert_eq!(value.runtime_kind(), Some(kind));
    }

    use crate::ffi::{FfiType, FfiValue};
    let mut runtime = test_runtime();
    assert_eq!(
        evaluate(&mut runtime, Type::Unit, FfiType::Unit, FfiValue::Unit),
        Value::Unit
    );
    assert_eq!(
        evaluate(
            &mut runtime,
            Type::named("bool"),
            FfiType::Bool,
            FfiValue::Bool(true),
        ),
        Value::Bool(true)
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("int8"),
            FfiType::I8,
            FfiValue::I8(i8::MIN),
        ),
        i128::from(i8::MIN),
        IntegerKind::Int8,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("int16"),
            FfiType::I16,
            FfiValue::I16(i16::MIN),
        ),
        i128::from(i16::MIN),
        IntegerKind::Int16,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("int32"),
            FfiType::I32,
            FfiValue::I32(i32::MIN),
        ),
        i128::from(i32::MIN),
        IntegerKind::Int32,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("int"),
            FfiType::I64,
            FfiValue::I64(i64::MIN),
        ),
        i128::from(i64::MIN),
        IntegerKind::Int64,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("int64"),
            FfiType::I64,
            FfiValue::I64(i64::MAX),
        ),
        i128::from(i64::MAX),
        IntegerKind::Int64,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("uint8"),
            FfiType::U8,
            FfiValue::U8(u8::MAX),
        ),
        i128::from(u8::MAX),
        IntegerKind::Uint8,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("uint16"),
            FfiType::U16,
            FfiValue::U16(u16::MAX),
        ),
        i128::from(u16::MAX),
        IntegerKind::Uint16,
    );
    assert_integer(
        evaluate(
            &mut runtime,
            Type::named("uint32"),
            FfiType::U32,
            FfiValue::U32(u32::MAX),
        ),
        i128::from(u32::MAX),
        IntegerKind::Uint32,
    );
    let Value::Int(u64_result) = evaluate(
        &mut runtime,
        Type::named("uint64"),
        FfiType::U64,
        FfiValue::U64(u64::MAX),
    ) else {
        panic!("expected uint64 result");
    };
    assert_eq!(u64_result.to_string(), u64::MAX.to_string());
    assert_eq!(u64_result.runtime_kind(), Some(IntegerKind::Uint64));
    assert_eq!(
        evaluate(
            &mut runtime,
            Type::named("float32"),
            FfiType::F32,
            FfiValue::F32(3.5),
        ),
        Value::Float(3.5)
    );
    assert_eq!(
        evaluate(
            &mut runtime,
            Type::named("float64"),
            FfiType::F64,
            FfiValue::F64(7.25),
        ),
        Value::Float(7.25)
    );
}

#[test]
fn mir_runtime_ffi_writes_mutable_views_back_before_foreign_and_result_errors() {
    use crate::ffi::{FfiError, FfiType, FfiValue};

    fn call() -> MirExternCall {
        MirExternCall {
            symbol: "aura_test_writeback_order".to_string(),
            abi: "C".to_string(),
            params: vec![MirExternParam {
                name: "bytes".to_string(),
                passing: MirReceiverKind::BorrowMut,
                ty: Type::Named("list".to_string(), vec![Type::named("uint8")]),
            }],
            return_type: Type::named("int32"),
        }
    }

    fn args(writeback_place: Option<&str>) -> Vec<MirArg> {
        vec![MirArg {
            name: None,
            value: Operand::Place("bytes".to_string()),
            writeback_place: writeback_place.map(str::to_string),
        }]
    }

    fn env() -> Env {
        let mut env = Env::default();
        env.define_typed(
            "bytes",
            Type::Named("list".to_string(), vec![Type::named("uint8")]),
            super::bytes_runtime_value(&[1, 2]),
        );
        env
    }

    let mut runtime = test_runtime();
    let mut foreign_error_env = env();
    let foreign_error = runtime
        .evaluate_extern_call_with(
            &call(),
            &args(Some("bytes")),
            &mut foreign_error_env,
            |_, signature, arguments| {
                assert_eq!(signature.parameters(), &[FfiType::BytesViewMut]);
                let FfiValue::Bytes(bytes) = &mut arguments[0] else {
                    panic!("mutable list[uint8] should marshal as bytes");
                };
                bytes.copy_from_slice(&[3, 4]);
                Err(FfiError::NullOpaqueHandleReturn)
            },
        )
        .expect_err("foreign boundary errors should trap");
    assert_eq!(foreign_error.code, "AU4005");
    assert_eq!(
        foreign_error.message,
        "FFI call to `aura_test_writeback_order` failed: FFI function returned a null opaque handle"
    );
    assert_eq!(
        foreign_error_env
            .read_place("bytes")
            .expect("bytes remain live")
            .render(),
        "[3, 4]"
    );

    let mut result_error_env = env();
    let result_error = runtime
        .evaluate_extern_call_with(
            &call(),
            &args(Some("bytes")),
            &mut result_error_env,
            |_, _, arguments| {
                let FfiValue::Bytes(bytes) = &mut arguments[0] else {
                    panic!("mutable list[uint8] should marshal as bytes");
                };
                bytes.copy_from_slice(&[5, 6]);
                Ok(FfiValue::Bool(true))
            },
        )
        .expect_err("a C result with the wrong runtime shape should trap");
    assert_eq!(result_error.code, "AU4005");
    assert_eq!(
        result_error.message,
        "FFI call to `aura_test_writeback_order` failed: FFI result `bool` does not match source return type `int32`"
    );
    assert_eq!(
        result_error_env
            .read_place("bytes")
            .expect("bytes remain live")
            .render(),
        "[5, 6]"
    );

    let mut missing_place_env = env();
    let missing_place = runtime
        .evaluate_extern_call_with(
            &call(),
            &args(None),
            &mut missing_place_env,
            |_, _, arguments| {
                let FfiValue::Bytes(bytes) = &mut arguments[0] else {
                    panic!("mutable list[uint8] should marshal as bytes");
                };
                bytes.copy_from_slice(&[7, 8]);
                Ok(FfiValue::I32(0))
            },
        )
        .expect_err("mutable FFI views require a source writeback place");
    assert_eq!(missing_place.code, "AU4005");
    assert_eq!(
        missing_place.message,
        "FFI call to `aura_test_writeback_order` failed: mutable parameter `bytes` requires a writeback place"
    );
    assert_eq!(
        missing_place_env
            .read_place("bytes")
            .expect("bytes remain live")
            .render(),
        "[1, 2]"
    );
}

#[test]
fn mir_runtime_ffi_reports_runtime_abi_and_argument_binding_errors_before_dispatch() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "value",
        Type::named("int32"),
        Value::Int(IntegerValue::from_i32(1)),
    );
    let params = vec![MirExternParam {
        name: "value".to_string(),
        passing: MirReceiverKind::Borrow,
        ty: Type::named("int32"),
    }];

    let unsupported_abi = runtime
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_bad_abi".to_string(),
                abi: "stdcall".to_string(),
                params: params.clone(),
                return_type: Type::Unit,
            },
            &[MirArg {
                name: None,
                value: Operand::Place("value".to_string()),
                writeback_place: None,
            }],
            &mut env,
            |_, _, _| panic!("unsupported ABI must be rejected before dispatch"),
        )
        .expect_err("only the C ABI is executable");
    assert_eq!(unsupported_abi.code, "AU4005");
    assert_eq!(
        unsupported_abi.message,
        "FFI call to `aura_test_bad_abi` failed: unsupported runtime ABI `stdcall`"
    );

    let unknown_argument = runtime
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_bad_binding".to_string(),
                abi: "C".to_string(),
                params,
                return_type: Type::Unit,
            },
            &[MirArg {
                name: Some("other".to_string()),
                value: Operand::Place("value".to_string()),
                writeback_place: None,
            }],
            &mut env,
            |_, _, _| panic!("invalid argument binding must be rejected before dispatch"),
        )
        .expect_err("unknown named FFI arguments must be diagnosed");
    assert_eq!(unknown_argument.code, "AU4005");
    assert_eq!(
        unknown_argument.message,
        "FFI call to `aura_test_bad_binding` failed: unknown MIR argument `other`"
    );
}

#[test]
fn mir_runtime_ffi_rejects_forged_runtime_shapes_at_the_boundary() {
    use crate::ffi::{FfiError, FfiSignature, FfiValue};

    fn boundary_dispatch(
        symbol: &str,
        _: &FfiSignature,
        arguments: &mut [FfiValue],
    ) -> std::result::Result<FfiValue, FfiError> {
        match symbol {
            "aura_test_mutable_shape_guard" => {
                arguments[0] = FfiValue::Bool(true);
                Ok(FfiValue::Unit)
            }
            "aura_test_foreign_failure" => Err(FfiError::NullOpaqueHandleReturn),
            "aura_test_result_shape_guard" => Ok(FfiValue::Bool(true)),
            _ => Ok(FfiValue::Unit),
        }
    }

    fn shape_error(param_ty: Type, value: Value) -> Diagnostic {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        env.define_typed("value", param_ty.clone(), value);
        runtime
            .evaluate_extern_call_with(
                &MirExternCall {
                    symbol: "aura_test_shape_guard".to_string(),
                    abi: "C".to_string(),
                    params: vec![MirExternParam {
                        name: "value".to_string(),
                        passing: MirReceiverKind::Borrow,
                        ty: param_ty,
                    }],
                    return_type: Type::Unit,
                },
                &[MirArg {
                    name: None,
                    value: Operand::Place("value".to_string()),
                    writeback_place: None,
                }],
                &mut env,
                boundary_dispatch,
            )
            .expect_err("forged MIR runtime shapes must not cross the FFI boundary")
    }

    let malformed_shapes = [
        (Type::Unit, Value::Bool(true)),
        (Type::named("bool"), Value::Int(IntegerValue::from_i32(1))),
        (Type::named("int8"), Value::Bool(true)),
        (Type::named("uint8"), Value::Bool(true)),
        (
            Type::named("uint8"),
            Value::Int(IntegerValue::from_signed(-1)),
        ),
        (Type::named("float32"), Value::Bool(true)),
        (Type::named("float64"), Value::Bool(true)),
        (Type::named("str"), Value::Bool(true)),
        (Type::named("Token"), Value::Bool(true)),
        (
            Type::Named("list".to_string(), vec![Type::named("uint8")]),
            Value::Bool(true),
        ),
        (
            Type::Named("list".to_string(), vec![Type::named("uint8")]),
            Value::Vec(VecValue {
                element_type: Type::named("uint8"),
                elements: vec![Value::Bool(true)],
            }),
        ),
    ];
    for (ty, value) in malformed_shapes {
        let rendered_type = ty.to_string();
        let error = shape_error(ty, value);
        assert_eq!(error.code, "AU4005", "{rendered_type}: {error:?}");
        assert!(
            error.message.contains("incompatible runtime shape"),
            "{rendered_type}: {}",
            error.message
        );
    }

    let unsupported = shape_error(
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(VecValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_i32(1))],
        }),
    );
    assert_eq!(unsupported.code, "AU4005");
    assert_eq!(
        unsupported.message,
        "unsupported FFI source type `list[int32]` reached MIR execution"
    );

    let bytes_type = Type::Named("list".to_string(), vec![Type::named("uint8")]);
    let mut env = Env::default();
    env.define_typed(
        "bytes",
        bytes_type.clone(),
        super::bytes_runtime_value(&[1]),
    );
    let mutated_shape = test_runtime()
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_mutable_shape_guard".to_string(),
                abi: "C".to_string(),
                params: vec![MirExternParam {
                    name: "bytes".to_string(),
                    passing: MirReceiverKind::BorrowMut,
                    ty: bytes_type,
                }],
                return_type: Type::Unit,
            },
            &[MirArg {
                name: None,
                value: Operand::Place("bytes".to_string()),
                writeback_place: Some("bytes".to_string()),
            }],
            &mut env,
            boundary_dispatch,
        )
        .expect_err("foreign dispatch must not replace a mutable byte view's runtime shape");
    assert_eq!(mutated_shape.code, "AU4005");
    assert_eq!(
        mutated_shape.message,
        "FFI call to `aura_test_mutable_shape_guard` failed: mutable parameter `bytes` did not marshal as bytes"
    );
    assert_eq!(
        env.read_place("bytes")
            .expect("rejected writeback keeps the original place live")
            .render(),
        "[1]"
    );

    let binding_error = test_runtime()
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_binding_guard".to_string(),
                abi: "C".to_string(),
                params: vec![MirExternParam {
                    name: "expected".to_string(),
                    passing: MirReceiverKind::Borrow,
                    ty: Type::named("int32"),
                }],
                return_type: Type::Unit,
            },
            &[MirArg {
                name: Some("other".to_string()),
                value: Operand::Place("integer".to_string()),
                writeback_place: None,
            }],
            &mut {
                let mut env = Env::default();
                env.define_typed(
                    "integer",
                    Type::named("int32"),
                    Value::Int(IntegerValue::from_i32(1)),
                );
                env
            },
            boundary_dispatch,
        )
        .expect_err("unknown FFI argument names must fail before dispatch");
    assert_eq!(binding_error.code, "AU4005");
    assert!(binding_error
        .message
        .contains("unknown MIR argument `other`"));

    let missing_writeback = test_runtime()
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_missing_writeback".to_string(),
                abi: "C".to_string(),
                params: vec![MirExternParam {
                    name: "bytes".to_string(),
                    passing: MirReceiverKind::BorrowMut,
                    ty: Type::Named("list".to_string(), vec![Type::named("uint8")]),
                }],
                return_type: Type::Unit,
            },
            &[MirArg {
                name: None,
                value: Operand::Place("bytes".to_string()),
                writeback_place: None,
            }],
            &mut {
                let mut env = Env::default();
                env.define_typed(
                    "bytes",
                    Type::Named("list".to_string(), vec![Type::named("uint8")]),
                    super::bytes_runtime_value(&[1]),
                );
                env
            },
            boundary_dispatch,
        )
        .expect_err("mutable FFI views require an explicit source writeback place");
    assert_eq!(missing_writeback.code, "AU4005");
    assert!(missing_writeback
        .message
        .contains("requires a writeback place"));

    let foreign_failure = test_runtime()
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_foreign_failure".to_string(),
                abi: "C".to_string(),
                params: Vec::new(),
                return_type: Type::Unit,
            },
            &[],
            &mut Env::default(),
            boundary_dispatch,
        )
        .expect_err("foreign failures must be mapped to a source runtime diagnostic");
    assert_eq!(foreign_failure.code, "AU4005");
    assert!(foreign_failure
        .message
        .contains("FFI function returned a null opaque handle"));

    let result_shape = test_runtime()
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_result_shape_guard".to_string(),
                abi: "C".to_string(),
                params: Vec::new(),
                return_type: Type::named("int32"),
            },
            &[],
            &mut Env::default(),
            boundary_dispatch,
        )
        .expect_err("foreign result shapes must match the declared source return type");
    assert_eq!(result_shape.code, "AU4005");
    assert!(result_shape
        .message
        .contains("FFI result `bool` does not match source return type `int32`"));

    let unit = test_runtime()
        .evaluate_extern_call_with(
            &MirExternCall {
                symbol: "aura_test_success".to_string(),
                abi: "C".to_string(),
                params: Vec::new(),
                return_type: Type::Unit,
            },
            &[],
            &mut Env::default(),
            boundary_dispatch,
        )
        .expect("a matching Unit result should cross the boundary");
    assert_eq!(unit, Value::Unit);
}

#[test]
fn mir_runtime_captures_typed_frames_once_in_contract_order() {
    let mut runtime = test_runtime();
    runtime.call_stack = vec![
        RuntimeCallFrame {
            function: "main".to_string(),
            span: RuntimeSourceSpan::point(Some("/workspace/main.au".to_string()), Span::new(8, 1)),
        },
        RuntimeCallFrame {
            function: "child".to_string(),
            span: RuntimeSourceSpan::point(
                Some("/workspace/worker.au".to_string()),
                Span::new(1, 1),
            ),
        },
    ];
    runtime.task_ancestry = vec![
        RuntimeTaskFrame {
            task_function: "parent".to_string(),
            task_entry_span: RuntimeSourceSpan::point(None, Span::new(4, 1)),
            parent_function: "main".to_string(),
            spawn_span: RuntimeSourceSpan::point(None, Span::new(10, 9)),
        },
        RuntimeTaskFrame {
            task_function: "child".to_string(),
            task_entry_span: RuntimeSourceSpan::point(None, Span::new(1, 1)),
            parent_function: "parent".to_string(),
            spawn_span: RuntimeSourceSpan::point(None, Span::new(6, 13)),
        },
    ];
    let captured = runtime.annotate_runtime_trap_once(
        Diagnostic::coded_at("AU4003", Span::new(3, 18), "out of bounds")
            .with_note("semantic note"),
    );
    assert_eq!(
        captured
            .call_frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        vec!["child", "main"]
    );
    assert_eq!(
        captured
            .task_ancestry
            .iter()
            .map(|frame| frame.task_function.as_str())
            .collect::<Vec<_>>(),
        vec!["child", "parent"]
    );
    assert_eq!(captured.notes, ["semantic note"]);

    runtime.call_stack = vec![RuntimeCallFrame {
        function: "observer".to_string(),
        span: RuntimeSourceSpan::point(None, Span::new(99, 1)),
    }];
    runtime.task_ancestry.clear();
    let propagated = runtime.annotate_runtime_trap_once(captured.clone());
    assert_eq!(
        propagated, captured,
        "an observer must not append its frames"
    );
}

#[test]
fn mir_task_stack_override_accepts_contract_boundaries_and_reports_dynamic_violations() {
    let runtime = test_runtime();
    let mut env = Env::default();

    assert_eq!(
        runtime
            .evaluate_task_stack_size(&Operand::Int(262_144), &env)
            .expect("the minimum task stack override should be accepted"),
        262_144
    );
    assert_eq!(
        runtime
            .evaluate_task_stack_size(&Operand::Int(67_108_864), &env)
            .expect("the maximum task stack override should be accepted"),
        67_108_864
    );

    for (name, bytes) in [("below", 262_143), ("above", 67_108_865)] {
        env.define_typed(
            name,
            Type::named("int64"),
            Value::Int(IntegerValue::from_signed(bytes)),
        );
        let error = runtime
            .evaluate_task_stack_size(&Operand::Place(name.to_string()), &env)
            .expect_err("an out-of-contract dynamic stack override should fail");
        assert_eq!(error.code, "AU4005");
        assert_eq!(
            error.message,
            format!("task stack size must be between 262144 and 67108864 bytes, found {bytes}")
        );
    }

    env.define_typed("not_bytes", Type::named("bool"), Value::Bool(true));
    let error = runtime
        .evaluate_task_stack_size(&Operand::Place("not_bytes".to_string()), &env)
        .expect_err("malformed MIR must not pass a non-int64 task stack size to the scheduler");
    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "task stack size must evaluate to an int64 value"
    );

    env.define_typed(
        "unsigned_bytes",
        Type::named("uint128"),
        Value::Int(
            IntegerValue::from_typed_unsigned(u128::MAX, IntegerKind::Uint128)
                .expect("u128::MAX is representable as uint128"),
        ),
    );
    let error = runtime
        .evaluate_task_stack_size(&Operand::Place("unsigned_bytes".to_string()), &env)
        .expect_err("task stack sizes whose runtime integer metadata is not int64 must fail");
    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "task stack size must evaluate to an int64 value"
    );
}

#[test]
fn mir_task_waits_report_unrepresentable_deadlines_and_completed_cancellation() {
    let blocker = ChannelValue::new();
    let unblocker = blocker.clone();
    let pending_task = TaskValue::from_handle(std::thread::spawn(move || {
        let _ = unblocker.recv_with_cancellation(None, None);
        Ok(Value::Unit)
    }));
    let mut runtime = test_runtime();

    let join_error = runtime
        .join_task(pending_task.clone(), Some(StdDuration::MAX))
        .expect_err("an unrepresentable task-result deadline should be diagnosed");
    assert_eq!(join_error.code, "AU4001");
    assert!(
        join_error
            .message
            .contains("task result timeout exceeds the host deadline range"),
        "unexpected task-result deadline diagnostic: {join_error:?}"
    );
    let wait_any_error = runtime
        .wait_any(vec![pending_task.clone()], Some(StdDuration::MAX))
        .expect_err("an unrepresentable wait-any deadline should be diagnosed");
    assert_eq!(wait_any_error.code, "AU4001");
    assert_eq!(
        wait_any_error.message,
        "timeout overflows the MIR runtime deadline range"
    );
    let wait_all_error = runtime
        .wait_all(Vec::new(), Some(StdDuration::MAX))
        .expect_err("an unrepresentable wait-all deadline should be diagnosed");
    assert_eq!(wait_all_error.code, "AU4001");
    assert_eq!(
        wait_all_error.message,
        "timeout overflows the MIR runtime deadline range"
    );
    blocker.close();
    let _ =
        pending_task.wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None);

    let result = crate::runtime_value::run_lightweight_root_task(|| {
        let cancelled_task =
            crate::runtime_value::spawn_lightweight_task(|| -> crate::diag::Result<Value> {
                crate::runtime_value::cancel_current_lightweight_task_boundary()
            })?;
        match cancelled_task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .expect("cancelled task completion should be observable")
        {
            crate::runtime_value::TaskWaitStatus::Cancelled => {}
            other => panic!("expected a cancelled lightweight task, found {other:?}"),
        }

        let mut runtime = test_runtime();
        assert_eq!(
            runtime.wait_any(vec![cancelled_task], Some(StdDuration::from_secs(1)))?,
            super::wait_any_cancelled()
        );
        Ok(Value::Unit)
    })
    .expect("wait_any should report cancellation already completed by a child task");
    assert_eq!(result, Value::Unit);
}

fn mir_arg(name: Option<&str>, value: Operand) -> MirArg {
    MirArg {
        name: name.map(str::to_string),
        value,
        writeback_place: None,
    }
}

fn assert_mir_length_call_borrows_receiver(
    runtime: &mut MirRuntime,
    env: &mut Env,
    callee: &CallTarget,
    args: &[MirArg],
    receiver_place: &str,
    expected_length: i128,
    api: &str,
) {
    let clone_count = super::mir_value_clone_count();
    let result = runtime
        .evaluate_call(callee, args, env)
        .unwrap_or_else(|error| panic!("{api} should succeed: {error:?}"));
    let Value::Int(length) = result else {
        panic!("{api} should return an integer length, found {result:?}");
    };

    assert_eq!(
        length.as_i128(),
        Some(expected_length),
        "{api} should report the expected length"
    );
    assert!(
        env.place_ref(receiver_place).is_ok(),
        "{api} should preserve its borrowed receiver"
    );
    assert_eq!(
        super::mir_value_clone_count() - clone_count,
        0,
        "{api} must read its receiver by reference instead of cloning the full value"
    );
}

fn assert_mir_member_length_borrows_receiver(
    receiver_type: Type,
    receiver: Value,
    field: &str,
    expected_length: i128,
    api: &str,
) {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed("receiver", receiver_type, receiver);
    let callee = CallTarget::Member {
        object: Operand::Place("receiver".to_string()),
        field: field.to_string(),
        receiver_place: Some("receiver".to_string()),
    };

    assert_mir_length_call_borrows_receiver(
        &mut runtime,
        &mut env,
        &callee,
        &[],
        "receiver",
        expected_length,
        api,
    );
}

#[test]
fn mir_length_string_len_borrows_receiver_without_snapshot_clone() {
    assert_mir_member_length_borrows_receiver(
        Type::named("str"),
        Value::String("é🎉e\u{301}".to_string()),
        "len",
        4,
        "str.len",
    );
}

#[test]
fn mir_length_string_byte_len_borrows_receiver_without_snapshot_clone() {
    assert_mir_member_length_borrows_receiver(
        Type::named("str"),
        Value::String("é🎉e\u{301}".to_string()),
        "byte_len",
        9,
        "str.byte_len",
    );
}

#[test]
fn mir_length_vec_len_borrows_receiver_without_snapshot_clone() {
    assert_mir_member_length_borrows_receiver(
        Type::Named("list".to_string(), vec![Type::named("str")]),
        Value::Vec(VecValue {
            element_type: Type::named("str"),
            elements: vec![
                Value::String("first-vector-payload".repeat(64)),
                Value::String("second-vector-payload".repeat(64)),
            ],
        }),
        "len",
        2,
        "Vec.len",
    );
}

#[test]
fn mir_length_map_len_borrows_receiver_without_snapshot_clone() {
    assert_mir_member_length_borrows_receiver(
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("str")],
        ),
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("str"),
            entries: vec![
                (
                    Value::String("first-map-key".repeat(64)),
                    Value::String("first-map-value".repeat(64)),
                ),
                (
                    Value::String("second-map-key".repeat(64)),
                    Value::String("second-map-value".repeat(64)),
                ),
            ],
        }),
        "len",
        2,
        "Map.len",
    );
}

#[test]
fn mir_length_set_len_borrows_receiver_without_snapshot_clone() {
    assert_mir_member_length_borrows_receiver(
        Type::Named("set".to_string(), vec![Type::named("str")]),
        Value::Set(SetValue {
            element_type: Type::named("str"),
            elements: vec![
                Value::String("first-set-value".repeat(64)),
                Value::String("second-set-value".repeat(64)),
            ],
        }),
        "len",
        2,
        "Set.len",
    );
}

#[test]
fn mir_length_free_len_delegation_borrows_receiver_without_snapshot_clone() {
    let module = crate::lower_source_to_mir(
        r#"
def measure(values: list[str]) -> int64:
    return len(values)
"#,
    )
    .expect("free len source should lower to MIR");
    let (callee, args) = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| {
            let Instruction::Assign {
                value: Rvalue::Call { callee, args },
                ..
            } = instruction
            else {
                return None;
            };
            let CallTarget::Member {
                object: Operand::Place(place),
                field,
                receiver_place,
            } = callee
            else {
                return None;
            };
            (field == "len" && place == "values" && receiver_place.as_deref() == Some("values"))
                .then(|| (callee.clone(), args.clone()))
        })
        .expect("free len should lower to a borrowed member len call");

    let mut runtime = MirRuntime::new(
        module,
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    env.define_typed(
        "values",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        Value::Vec(VecValue {
            element_type: Type::named("str"),
            elements: vec![
                Value::String("first-free-len-payload".repeat(64)),
                Value::String("second-free-len-payload".repeat(64)),
            ],
        }),
    );

    assert_mir_length_call_borrows_receiver(
        &mut runtime,
        &mut env,
        &callee,
        &args,
        "values",
        2,
        "free len(list[str])",
    );
}

#[test]
fn mir_tuple_construct_project_and_take_preserve_ownership_boundaries() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let text = "owned tuple element".repeat(32);
    let text_ptr = text.as_ptr();
    env.define_typed("text", Type::named("str"), Value::String(text));
    env.define_typed(
        "number",
        Type::named("int64"),
        Value::Int(IntegerValue::from_signed(7)),
    );

    let outcome = runtime
        .evaluate_rvalue(
            &Rvalue::TupleLiteral {
                elements: vec![
                    Operand::MovePlace("text".to_string()),
                    Operand::Place("number".to_string()),
                ],
                element_types: vec![Type::named("str"), Type::named("int64")],
            },
            &mut env,
        )
        .expect("tuple construction should evaluate left-to-right");
    let super::RvalueOutcome::Value(tuple) = outcome else {
        panic!("expected tuple value");
    };
    assert!(env.place_ref("text").is_err(), "owned element must move");
    assert_eq!(
        env.read_place("number").expect("copy element must remain"),
        Value::Int(IntegerValue::from_signed(7))
    );
    env.define_typed(
        "tuple",
        Type::Tuple(vec![Type::named("str"), Type::named("int64")]),
        tuple,
    );

    let projected = runtime
        .evaluate_rvalue(
            &Rvalue::TupleElement {
                tuple: Operand::Place("tuple".to_string()),
                index: 1,
                element_type: Type::named("int64"),
            },
            &mut env,
        )
        .expect("copy tuple projection should succeed");
    let super::RvalueOutcome::Value(projected) = projected else {
        panic!("expected projected value");
    };
    assert_eq!(projected, Value::Int(IntegerValue::from_signed(7)));

    let taken = runtime
        .evaluate_rvalue(
            &Rvalue::TupleTakeElement {
                place: "tuple".to_string(),
                index: 0,
                element_type: Type::named("str"),
            },
            &mut env,
        )
        .expect("non-Copy tuple element should move");
    let super::RvalueOutcome::Value(Value::String(taken)) = taken else {
        panic!("expected moved str");
    };
    assert_eq!(taken.as_ptr(), text_ptr);
    let Value::Tuple(remaining) = env.read_place("tuple").expect("tuple owner remains") else {
        panic!("expected tuple owner");
    };
    assert_eq!(
        remaining.elements,
        vec![Value::Unit, Value::Int(IntegerValue::from_signed(7))]
    );

    let repeated = env
        .take_tuple_element("tuple", 0)
        .expect_err("a tuple slot cannot be moved twice");
    assert!(repeated.message.contains("has already been moved"));
}

#[test]
fn mir_tuple_operations_reject_invalid_shape_and_places() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let malformed = match runtime.evaluate_rvalue(
        &Rvalue::TupleLiteral {
            elements: vec![Operand::Bool(true)],
            element_types: Vec::new(),
        },
        &mut env,
    ) {
        Err(error) => error,
        Ok(_) => panic!("tuple metadata arity must match value arity"),
    };
    assert!(malformed.message.contains("1 values but 0 element types"));

    env.define_typed("flag", Type::named("bool"), Value::Bool(true));
    let non_tuple_projection = env
        .tuple_element("flag", 0)
        .expect_err("tuple projection must reject non-tuples");
    assert!(non_tuple_projection.message.contains("non-tuple MIR place"));
    let non_tuple = env
        .take_tuple_element("flag", 0)
        .expect_err("tuple take must reject non-tuples");
    assert!(non_tuple.message.contains("non-tuple MIR place"));
    let missing = env
        .take_tuple_element("missing", 0)
        .expect_err("tuple take must diagnose unknown MIR places");
    assert!(missing.message.contains("unknown MIR place `missing`"));

    env.define_typed(
        "empty",
        Type::Tuple(Vec::new()),
        Value::Tuple(TupleValue {
            element_types: Vec::new(),
            elements: Vec::new(),
        }),
    );
    let out_of_bounds = env
        .take_tuple_element("empty", 0)
        .expect_err("tuple take must enforce bounds");
    assert!(out_of_bounds.message.contains("no element at index 0"));
    let projection_out_of_bounds = env
        .tuple_element("empty", 0)
        .expect_err("tuple projection must enforce bounds");
    assert!(projection_out_of_bounds
        .message
        .contains("no element at index 0"));

    let temporary_projection = match runtime.evaluate_rvalue(
        &Rvalue::TupleElement {
            tuple: Operand::Bool(true),
            index: 0,
            element_type: Type::named("bool"),
        },
        &mut env,
    ) {
        Err(error) => error,
        Ok(_) => panic!("invalid MIR cannot project a tuple element from a scalar operand"),
    };
    assert!(temporary_projection
        .message
        .contains("tuple projection expected a tuple, found `true`"));

    let arity_mismatch = runtime
        .coerce_value_to_type(
            Value::Tuple(TupleValue {
                element_types: vec![Type::named("int64")],
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            }),
            &Type::Tuple(Vec::new()),
            None,
        )
        .expect_err("tuple coercion must preserve structural arity");
    assert!(arity_mismatch
        .message
        .contains("tuple value has 1 elements but target type expects 0"));

    let queue = ChannelValue::new();
    queue.set_runtime_type_name("Queue[int64]".to_string());
    let nested = Value::Tuple(TupleValue {
        element_types: vec![Type::Named("Queue".to_string(), vec![Type::named("int64")])],
        elements: vec![Value::Channel(queue)],
    });
    let mut queues = Vec::new();
    collect_queue_handles(&nested, &mut queues);
    assert_eq!(
        queues
            .first()
            .and_then(ChannelValue::runtime_type_name)
            .as_deref(),
        Some("Queue[int64]"),
        "task producer tracking must find queues nested inside tuples"
    );
}

#[test]
fn mir_tuple_coercion_preserves_owned_element_allocations_and_metadata() {
    let runtime = test_runtime();
    let text = "tuple coercion".repeat(32);
    let text_ptr = text.as_ptr();
    let coerced = runtime
        .coerce_value_to_type(
            Value::Tuple(TupleValue {
                element_types: vec![Type::named("str"), Type::named("int64")],
                elements: vec![
                    Value::String(text),
                    Value::Int(IntegerValue::from_signed(7)),
                ],
            }),
            &Type::Tuple(vec![Type::named("str"), Type::named("int8")]),
            None,
        )
        .expect("tuple elements should coerce structurally");
    let Value::Tuple(coerced) = coerced else {
        panic!("expected tuple");
    };
    assert_eq!(
        coerced.element_types,
        vec![Type::named("str"), Type::named("int8")]
    );
    let Value::String(text) = &coerced.elements[0] else {
        panic!("expected str element");
    };
    assert_eq!(text.as_ptr(), text_ptr);
    let Value::Int(number) = &coerced.elements[1] else {
        panic!("expected integer element");
    };
    assert_eq!(number.runtime_kind(), Some(IntegerKind::Int8));
    assert_eq!(
        MirRuntime::infer_value_type(&Value::Tuple(coerced)),
        Some(Type::Tuple(vec![Type::named("str"), Type::named("int8")])),
        "runtime inference must preserve the tuple's coerced element metadata"
    );
}

#[test]
fn mir_rng_dispatches_nonbuiltin_traits_and_opaque_user_clone_methods() {
    for (name, source, expected) in [
        (
            "Rng trait dispatch",
            include_str!("../tests/fixtures/run-pass/random_rng_trait_dispatch.au"),
            include_str!("../tests/fixtures/run-pass/random_rng_trait_dispatch.stdout"),
        ),
        (
            "opaque Holder clone dispatch",
            include_str!("../tests/fixtures/run-pass/random_opaque_user_clone_dispatch.au"),
            include_str!("../tests/fixtures/run-pass/random_opaque_user_clone_dispatch.stdout"),
        ),
    ] {
        let output = crate::run_source(source)
            .unwrap_or_else(|error| panic!("{name} should execute through MIR: {error}"));
        assert_eq!(output.stdout, expected, "{name}");
    }
}

#[test]
fn mir_own_user_and_trait_receivers_transfer_the_original_allocation() {
    let module = crate::lower_source_to_mir(
        r#"
class DirectBox:
    value: str

    def take(own self) -> str:
        return self.value

trait Take:
    def take(own self) -> str

class TraitBox:
    value: str

impl Take for TraitBox:
    def take(own self) -> str:
        return self.value

trait TakeString:
    def take_string(own self) -> str

impl TakeString for str:
    def take_string(own self) -> str:
        return self

def main():
    pass
"#,
    )
    .expect("own receiver fixtures should lower");
    let mut runtime = MirRuntime::new(
        module,
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );

    for (place, class_name, field) in [
        ("direct", "DirectBox", "take"),
        ("trait_value", "TraitBox", "take"),
    ] {
        let text = format!("{place}-").repeat(64);
        let text_ptr = text.as_ptr();
        let mut env = Env::default();
        env.define_typed(
            place,
            Type::named(class_name),
            Value::Instance(InstanceValue {
                class_name: class_name.to_string(),
                fields: BTreeMap::from([("value".to_string(), Value::String(text))]),
            }),
        );

        let returned = runtime
            .evaluate_call(
                &CallTarget::Member {
                    object: Operand::MovePlace(place.to_string()),
                    field: field.to_string(),
                    receiver_place: Some(place.to_string()),
                },
                &[],
                &mut env,
            )
            .unwrap_or_else(|error| panic!("{class_name}.{field} should run: {error}"));
        match returned {
            Value::String(value) => assert_eq!(
                value.as_ptr(),
                text_ptr,
                "an own {class_name} receiver must enter its method without a snapshot clone"
            ),
            other => panic!("expected str from {class_name}.{field}, found {other:?}"),
        }
        assert!(
            env.place_ref(place).is_err(),
            "the source of an own {class_name} receiver must stay consumed"
        );
    }

    let text = "builtin-trait-receiver-".repeat(64);
    let text_ptr = text.as_ptr();
    let mut env = Env::default();
    env.define_typed("text", Type::named("str"), Value::String(text));
    let returned = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::MovePlace("text".to_string()),
                field: "take_string".to_string(),
                receiver_place: Some("text".to_string()),
            },
            &[],
            &mut env,
        )
        .expect("an own trait receiver on a builtin value should run");
    let Value::String(returned) = returned else {
        panic!("expected str from str.take_string");
    };
    assert_eq!(
        returned.as_ptr(),
        text_ptr,
        "an own trait receiver on a builtin value must not snapshot-clone"
    );
    assert!(env.place_ref("text").is_err());
}

fn enum_payloads(value: Value, enum_name: &str, variant_name: &str) -> Vec<Value> {
    match value {
        Value::EnumVariant(variant) => {
            assert_eq!(variant.enum_name, enum_name);
            assert_eq!(variant.variant_name, variant_name);
            variant.payloads
        }
        other => panic!("expected {enum_name}.{variant_name}, found {other:?}"),
    }
}

fn result_ok_payload(value: Value) -> Value {
    let mut payloads = enum_payloads(value, "Result", "Ok");
    assert_eq!(payloads.len(), 1);
    payloads.remove(0)
}

fn assert_result_err(value: Value) {
    let payloads = enum_payloads(value, "Result", "Err");
    assert_eq!(payloads.len(), 1);
}

fn assert_process_invalid_input(value: Value) {
    let mut io_payloads = enum_payloads(value, "Error", "Io");
    assert_eq!(io_payloads.len(), 1);
    assert!(enum_payloads(io_payloads.remove(0), "io.Error", "InvalidInput").is_empty());
}

fn assert_process_invalid_input_result(value: Value) {
    let mut payloads = enum_payloads(value, "Result", "Err");
    assert_eq!(payloads.len(), 1);
    assert_process_invalid_input(payloads.remove(0));
}

fn call_name(
    runtime: &mut MirRuntime,
    name: &str,
    args: &[MirArg],
    env: &mut Env,
) -> crate::diag::Result<Value> {
    runtime.evaluate_call(&crate::mir::CallTarget::Name(name.to_string()), args, env)
}

fn json_value(variant_name: &str, payloads: Vec<Value>) -> Value {
    Value::EnumVariant(EnumVariantValue {
        enum_name: "json.Value".to_string(),
        variant_name: variant_name.to_string(),
        payloads,
    })
}

#[test]
fn mir_json_borrowed_host_args_reference_the_existing_runtime_value() {
    let mut env = Env::default();
    env.define_typed(
        "text",
        Type::named("str"),
        Value::String("{\"answer\":42}".repeat(32)),
    );
    env.define_typed(
        "value",
        Type::named("json.Value"),
        json_value("String", vec![Value::String("payload".repeat(32))]),
    );

    let text_ptr = match env.place_ref("text").expect("text should exist") {
        Value::String(text) => text.as_ptr(),
        other => panic!("expected str, found {other:?}"),
    };
    let value_ptr = env.place_ref("value").expect("value should exist") as *const Value;

    let text_operand = Operand::Place("text".to_string());
    let value_operand = Operand::Place("value".to_string());
    let text_arg =
        super::borrow_mir_operand(&text_operand, &env).expect("str place should be borrowed");
    let value_arg = super::borrow_mir_operand(&value_operand, &env)
        .expect("json.Value place should be borrowed");

    match text_arg.as_value() {
        Value::String(text) => assert_eq!(text.as_ptr(), text_ptr),
        other => panic!("expected str, found {other:?}"),
    }
    assert_eq!(value_arg.as_value() as *const Value, value_ptr);
    assert!(text_arg.is_borrowed_place());
    assert!(value_arg.is_borrowed_place());
}

#[test]
fn mir_json_into_accessors_move_payload_allocations_and_consume_places() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let text = "owned-text".repeat(64);
    let text_ptr = text.as_ptr();
    env.define_typed(
        "string_value",
        Type::named("json.Value"),
        json_value("String", vec![Value::String(text)]),
    );
    let string_result = call_name(
        &mut runtime,
        "json::into_string",
        &[mir_arg(
            None,
            Operand::MovePlace("string_value".to_string()),
        )],
        &mut env,
    )
    .expect("json.into_string should succeed");
    let mut string_payloads = enum_payloads(string_result, "Option", "Some");
    match string_payloads.remove(0) {
        Value::String(value) => assert_eq!(
            value.as_ptr(),
            text_ptr,
            "owned str payload must be transferred, not cloned"
        ),
        other => panic!("expected str payload, found {other:?}"),
    }
    assert!(
        env.place_ref("string_value").is_err(),
        "the owned source place must be consumed"
    );

    let values = vec![json_value("Null", Vec::new())];
    let values_ptr = values.as_ptr();
    env.define_typed(
        "array_value",
        Type::named("json.Value"),
        json_value(
            "Array",
            vec![Value::Vec(VecValue {
                element_type: Type::named("json.Value"),
                elements: values,
            })],
        ),
    );
    let array_result = call_name(
        &mut runtime,
        "json::into_array",
        &[mir_arg(None, Operand::MovePlace("array_value".to_string()))],
        &mut env,
    )
    .expect("json.into_array should succeed");
    let mut array_payloads = enum_payloads(array_result, "Option", "Some");
    match array_payloads.remove(0) {
        Value::Vec(value) => assert_eq!(
            value.elements.as_ptr(),
            values_ptr,
            "owned Vec payload must be transferred, not cloned"
        ),
        other => panic!("expected Vec payload, found {other:?}"),
    }
    assert!(env.place_ref("array_value").is_err());

    let entries = vec![(
        Value::String("k".to_string()),
        json_value("Null", Vec::new()),
    )];
    let entries_ptr = entries.as_ptr();
    env.define_typed(
        "object_value",
        Type::named("json.Value"),
        json_value(
            "Object",
            vec![Value::Map(MapValue {
                key_type: Type::named("str"),
                value_type: Type::named("json.Value"),
                entries,
            })],
        ),
    );
    let object_result = call_name(
        &mut runtime,
        "json::into_object",
        &[mir_arg(
            None,
            Operand::MovePlace("object_value".to_string()),
        )],
        &mut env,
    )
    .expect("json.into_object should succeed");
    let mut object_payloads = enum_payloads(object_result, "Option", "Some");
    match object_payloads.remove(0) {
        Value::Map(value) => assert_eq!(
            value.entries.as_ptr(),
            entries_ptr,
            "owned Map payload must be transferred, not cloned"
        ),
        other => panic!("expected Map payload, found {other:?}"),
    }
    assert!(env.place_ref("object_value").is_err());
}

#[test]
fn mir_json_variant_construction_moves_owned_payload_allocations() {
    let mut runtime = test_runtime();

    let text = "owned-text".repeat(64);
    let text_ptr = text.as_ptr();
    let values = vec![json_value("Null", Vec::new())];
    let values_ptr = values.as_ptr();
    let entries = vec![(
        Value::String("key".to_string()),
        json_value("Bool", vec![Value::Bool(true)]),
    )];
    let entries_ptr = entries.as_ptr();

    let cases = [
        ("text", "String", Type::named("str"), Value::String(text)),
        (
            "values",
            "Array",
            Type::Named("list".to_string(), vec![Type::named("json.Value")]),
            Value::Vec(VecValue {
                element_type: Type::named("json.Value"),
                elements: values,
            }),
        ),
        (
            "entries",
            "Object",
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("json.Value")],
            ),
            Value::Map(MapValue {
                key_type: Type::named("str"),
                value_type: Type::named("json.Value"),
                entries,
            }),
        ),
    ];

    for (place, variant_name, ty, payload) in cases {
        let mut env = Env::default();
        env.define_typed(place, ty, payload);
        let outcome = runtime
            .evaluate_rvalue(
                &Rvalue::EnumVariant {
                    enum_name: "json.Value".to_string(),
                    variant_name: variant_name.to_string(),
                    payloads: vec![Operand::MovePlace(place.to_string())],
                },
                &mut env,
            )
            .expect("json.Value construction should succeed");
        let super::RvalueOutcome::Value(Value::EnumVariant(variant)) = outcome else {
            panic!("expected json.Value.{variant_name}");
        };
        match variant.payloads.as_slice() {
            [Value::String(value)] => assert_eq!(value.as_ptr(), text_ptr),
            [Value::Vec(value)] => assert_eq!(value.elements.as_ptr(), values_ptr),
            [Value::Map(value)] => assert_eq!(value.entries.as_ptr(), entries_ptr),
            other => panic!("unexpected json.Value.{variant_name} payload: {other:?}"),
        }
        assert!(
            env.place_ref(place).is_err(),
            "owned json.Value.{variant_name} payload must be moved from its source place"
        );
    }
}

#[test]
fn mir_json_wrong_variant_owned_accessor_still_consumes_the_source() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "value",
        Type::named("json.Value"),
        json_value("Int", vec![Value::Int(IntegerValue::from_i64(7))]),
    );

    assert_eq!(
        call_name(
            &mut runtime,
            "json::into_string",
            &[mir_arg(None, Operand::MovePlace("value".to_string()))],
            &mut env,
        )
        .expect("wrong-variant extraction should return Option.None"),
        option_none()
    );
    assert!(
        env.place_ref("value").is_err(),
        "an own argument is consumed even when its variant does not match"
    );
}

#[test]
fn mir_json_owned_accessor_moves_a_nested_place_without_cloning_its_payload() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let text = "nested-owned".repeat(64);
    let text_ptr = text.as_ptr();
    env.define_typed(
        "holder",
        Type::named("Holder"),
        Value::Instance(InstanceValue {
            class_name: "Holder".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                json_value("String", vec![Value::String(text)]),
            )]),
        }),
    );

    let result = call_name(
        &mut runtime,
        "json::into_string",
        &[mir_arg(
            None,
            Operand::MovePlace("holder.value".to_string()),
        )],
        &mut env,
    )
    .expect("nested owned json.Value should be extracted");
    let mut payloads = enum_payloads(result, "Option", "Some");
    match payloads.remove(0) {
        Value::String(value) => assert_eq!(value.as_ptr(), text_ptr),
        other => panic!("expected str payload, found {other:?}"),
    }
    assert!(
        env.place_ref("holder.value").is_err(),
        "the moved nested field must no longer contain a runtime value"
    );
    assert!(
        env.place_ref("holder").is_ok(),
        "moving a field must preserve the containing instance"
    );
}

#[test]
fn mir_place_reads_clone_and_preserve_copy_enum_payload_sources() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "timeout",
        Type::named("Duration"),
        Value::Duration(2_000_000_000),
    );

    let outcome = runtime
        .evaluate_rvalue(
            &Rvalue::EnumVariant {
                enum_name: "Option".to_string(),
                variant_name: "Some".to_string(),
                payloads: vec![Operand::Place("timeout".to_string())],
            },
            &mut env,
        )
        .expect("copy enum payload construction should succeed");
    let super::RvalueOutcome::Value(Value::EnumVariant(variant)) = outcome else {
        panic!("expected Option.Some(Duration)");
    };
    assert_eq!(variant.payloads, vec![Value::Duration(2_000_000_000)]);
    assert_eq!(
        env.read_place("timeout")
            .expect("copy source should remain"),
        Value::Duration(2_000_000_000)
    );
}

#[test]
fn mir_move_variant_payload_preserves_allocation_and_marks_slot_consumed() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let text = "owned match payload".repeat(32);
    let text_ptr = text.as_ptr();
    env.define_typed(
        "packet",
        Type::named("Packet"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Packet".to_string(),
            variant_name: "Text".to_string(),
            payloads: vec![Value::String(text)],
        }),
    );

    let outcome = runtime
        .evaluate_rvalue(
            &Rvalue::VariantPayload {
                scrutinee: Operand::MovePlace("packet".to_string()),
                variant_name: "Text".to_string(),
                index: 0,
            },
            &mut env,
        )
        .expect("owned variant payload extraction should succeed");
    let super::RvalueOutcome::Value(Value::String(text)) = outcome else {
        panic!("expected moved str payload");
    };
    assert_eq!(text.as_ptr(), text_ptr);
    match env
        .read_place("packet")
        .expect("private match owner remains")
    {
        Value::EnumVariant(variant) => assert_eq!(variant.payloads, vec![Value::Unit]),
        other => panic!("expected Packet enum, found {other:?}"),
    }
}

#[test]
fn mir_json_borrowed_calls_leave_source_allocations_in_place_on_success_and_error() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "valid_text",
        Type::named("str"),
        Value::String("{\"answer\":42}".repeat(32)),
    );
    env.define_typed(
        "invalid_text",
        Type::named("str"),
        Value::String("{".repeat(32)),
    );
    let valid_text_ptr = match env.place_ref("valid_text").unwrap() {
        Value::String(value) => value.as_ptr(),
        other => panic!("expected str, found {other:?}"),
    };
    let invalid_text_ptr = match env.place_ref("invalid_text").unwrap() {
        Value::String(value) => value.as_ptr(),
        other => panic!("expected str, found {other:?}"),
    };

    for place in ["valid_text", "invalid_text"] {
        let parsed = call_name(
            &mut runtime,
            "json::parse",
            &[mir_arg(None, Operand::Place(place.to_string()))],
            &mut env,
        )
        .expect("json.parse returns parse failures as Result.Err");
        assert!(matches!(parsed, Value::EnumVariant(_)));
    }
    match env.place_ref("valid_text").unwrap() {
        Value::String(value) => assert_eq!(value.as_ptr(), valid_text_ptr),
        other => panic!("expected str, found {other:?}"),
    }
    match env.place_ref("invalid_text").unwrap() {
        Value::String(value) => assert_eq!(value.as_ptr(), invalid_text_ptr),
        other => panic!("expected str, found {other:?}"),
    }

    let payload = "borrowed-payload".repeat(32);
    let payload_ptr = payload.as_ptr();
    env.define_typed(
        "string_value",
        Type::named("json.Value"),
        json_value("String", vec![Value::String(payload)]),
    );
    env.define_typed(
        "indent",
        Type::named("Option"),
        option_some(Value::Int(IntegerValue::from_i64(2))),
    );
    let value_ptr = env.place_ref("string_value").unwrap() as *const Value;

    call_name(
        &mut runtime,
        "json::dumps",
        &[
            mir_arg(None, Operand::Place("string_value".to_string())),
            mir_arg(None, Operand::Place("indent".to_string())),
        ],
        &mut env,
    )
    .expect("json.dumps should borrow its value and copy indent");
    for name in [
        "json::is_null",
        "json::as_bool",
        "json::as_int",
        "json::as_float",
    ] {
        call_name(
            &mut runtime,
            name,
            &[mir_arg(None, Operand::Place("string_value".to_string()))],
            &mut env,
        )
        .unwrap_or_else(|error| panic!("{name} should accept a borrowed json.Value: {error}"));
    }

    let retained = env.place_ref("string_value").unwrap();
    assert_eq!(retained as *const Value, value_ptr);
    match retained {
        Value::EnumVariant(variant) => match variant.payloads.as_slice() {
            [Value::String(value)] => assert_eq!(value.as_ptr(), payload_ptr),
            other => panic!("expected one str payload, found {other:?}"),
        },
        other => panic!("expected json.Value.String, found {other:?}"),
    }
    assert_eq!(
        env.read_place("indent"),
        Ok(option_some(Value::Int(IntegerValue::from_i64(2)))),
        "copy-valued indent must remain available after json.dumps"
    );

    env.define_typed(
        "malformed",
        Type::named("json.Value"),
        json_value("String", Vec::new()),
    );
    let malformed_ptr = env.place_ref("malformed").unwrap() as *const Value;
    let error = call_name(
        &mut runtime,
        "json::dumps",
        &[
            mir_arg(None, Operand::Place("malformed".to_string())),
            mir_arg(None, Operand::Place("indent".to_string())),
        ],
        &mut env,
    )
    .expect_err("malformed json.Value should fail validation");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        env.place_ref("malformed").unwrap() as *const Value,
        malformed_ptr,
        "borrowed validation failure must not replace or consume the source"
    );
}

#[test]
fn mir_json_parse_materialization_allocation_failure_is_au4005_and_preserves_source() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "source",
        Type::named("str"),
        Value::String("[null]".to_string()),
    );
    let source_ptr = match env.place_ref("source").expect("source should exist") {
        Value::String(value) => value.as_ptr(),
        other => panic!("expected str, found {other:?}"),
    };

    let error = crate::runtime_value::with_json_runtime_allocation_budget(0, || {
        call_name(
            &mut runtime,
            "json::parse",
            &[mir_arg(None, Operand::Place("source".to_string()))],
            &mut env,
        )
    })
    .expect_err("MIR parse materialization allocation failure should trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing parsed JSON"
    );
    match env
        .place_ref("source")
        .expect("borrowed parse source should survive the trap")
    {
        Value::String(value) => assert_eq!(value.as_ptr(), source_ptr),
        other => panic!("expected str, found {other:?}"),
    }
}

#[test]
fn mir_bytes_adapter_propagates_materialization_allocation_failure_as_au4005() {
    let mut env = Env::default();
    env.define_typed(
        "source",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(vec![0xab]),
    );
    let source_elements = match env.place_ref("source").expect("source should exist") {
        Value::Vec(value) => value.elements.as_ptr(),
        other => panic!("expected list[uint8], found {other:?}"),
    };
    let args = [mir_arg(None, Operand::Place("source".to_string()))];

    let error = crate::runtime_value::with_bytes_runtime_allocation_budget(0, || {
        super::evaluate_bytes_mir_host_call("bytes::hex_encode", &args, &env)
            .expect("the MIR Bytes adapter should recognize bytes::hex_encode")
    })
    .expect_err("MIR byte materialization allocation failure should trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing byte data"
    );
    match env
        .place_ref("source")
        .expect("borrowed byte source should survive the trap")
    {
        Value::Vec(value) => assert_eq!(value.elements.as_ptr(), source_elements),
        other => panic!("expected list[uint8], found {other:?}"),
    }
}

#[test]
fn mir_bytes_adapter_reports_binding_and_operand_diagnostics() {
    let env = Env::default();
    let missing_argument = super::evaluate_bytes_mir_host_call("bytes::hex_encode", &[], &env)
        .expect("the MIR Bytes adapter should recognize bytes::hex_encode")
        .expect_err("a missing argument must remain a diagnostic");
    assert_eq!(missing_argument.code, "AU2004");
    assert_eq!(missing_argument.message, "missing MIR argument");

    let args = [mir_arg(None, Operand::Place("missing_bytes".to_string()))];
    let missing_operand = super::evaluate_bytes_mir_host_call("bytes::hex_encode", &args, &env)
        .expect("the MIR Bytes adapter should recognize bytes::hex_encode")
        .expect_err("an unresolved borrowed operand must remain a diagnostic");
    assert_eq!(missing_operand.message, "unknown MIR place `missing_bytes`");
    assert_eq!(missing_operand.code, "AU2001");
}

#[test]
fn mir_string_to_bytes_member_diagnostics_preserve_the_receiver() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let source = "still available".to_string();
    let source_ptr = source.as_ptr();
    env.define_typed("source", Type::named("Unknown"), Value::String(source));
    let args = [mir_arg(None, Operand::Int(1))];

    let error = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("source".to_string()),
                field: "to_bytes".to_string(),
                receiver_place: Some("source".to_string()),
            },
            &args,
            &mut env,
        )
        .expect_err("str.to_bytes must reject arguments before moving its receiver");
    assert_eq!(error.message, "`to_bytes` does not take arguments");
    match env
        .place_ref("source")
        .expect("the rejected call must preserve its receiver")
    {
        Value::String(value) => assert_eq!(value.as_ptr(), source_ptr),
        other => panic!("expected str, found {other:?}"),
    }
}

#[test]
fn mir_dynamic_string_to_bytes_borrows_receiver_before_allocation_failure() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let source = "dynamic-string-receiver-".repeat(64);
    let source_ptr = source.as_ptr();
    env.define_typed("source", Type::named("Unknown"), Value::String(source));
    let clone_count = super::mir_value_clone_count();

    let error = crate::runtime_value::with_bytes_runtime_allocation_budget(0, || {
        runtime.evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("source".to_string()),
                field: "to_bytes".to_string(),
                receiver_place: Some("source".to_string()),
            },
            &[],
            &mut env,
        )
    })
    .expect_err("dynamic str.to_bytes allocation failure should trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing byte data"
    );
    assert_eq!(
        super::mir_value_clone_count(),
        clone_count,
        "dynamic str.to_bytes must borrow its receiver before entering the byte adapter"
    );
    match env
        .place_ref("source")
        .expect("borrowed str receiver should survive the trap")
    {
        Value::String(value) => assert_eq!(value.as_ptr(), source_ptr),
        other => panic!("expected str, found {other:?}"),
    }
}

#[test]
fn mir_literal_string_to_bytes_avoids_snapshot_before_allocation_failure() {
    let module = crate::lower_source_to_mir(
        r#"
def make_text() -> str:
    return "temporary"

def literal_bytes() -> list[uint8]:
    return "literal-string-receiver".to_bytes()

def formatted_bytes() -> list[uint8]:
    return f"formatted-temporary".to_bytes()

def returned_bytes() -> list[uint8]:
    return make_text().to_bytes()

def main():
    literal_bytes()
"#,
    )
    .expect("literal and temporary str receivers should lower");

    fn to_bytes_operand<'a>(module: &'a crate::mir::MirModule, function_name: &str) -> &'a Operand {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .unwrap_or_else(|| panic!("{function_name} should lower"));
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                Instruction::Assign {
                    value:
                        Rvalue::Call {
                            callee: CallTarget::Name(name),
                            args,
                        },
                    ..
                } if name == "str.to_bytes" => args.first().map(|argument| &argument.value),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{function_name} should call str.to_bytes"))
    }

    let Operand::String(literal) = to_bytes_operand(&module, "literal_bytes") else {
        panic!("a literal str receiver should remain a borrowed MIR literal");
    };
    assert_eq!(literal, "literal-string-receiver");
    assert!(
        matches!(
            to_bytes_operand(&module, "formatted_bytes"),
            Operand::Place(_)
        ),
        "a formatted str receiver should lower to a borrowed temporary place"
    );
    assert!(
        matches!(
            to_bytes_operand(&module, "returned_bytes"),
            Operand::Place(_)
        ),
        "a returned str receiver should lower to a borrowed temporary place"
    );
    let mut runtime = MirRuntime::new(
        module,
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let clone_count = super::mir_value_clone_count();

    let error =
        crate::runtime_value::with_bytes_runtime_allocation_budget(0, || runtime.run_main())
            .expect_err("literal str.to_bytes allocation failure should trap");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "memory allocation failed while materializing byte data"
    );
    assert_eq!(
        super::mir_value_clone_count(),
        clone_count,
        "literal str.to_bytes must borrow the MIR literal before entering the byte adapter"
    );
}

#[test]
fn mir_json_adapters_reject_inexact_runtime_metadata() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    env.define_typed(
        "indent",
        Type::Named("Option".to_string(), vec![Type::named("int64")]),
        option_some(Value::Int(IntegerValue::from_i32(2))),
    );
    let indent_error = call_name(
        &mut runtime,
        "json::dumps",
        &[
            mir_arg(
                None,
                Operand::String("unused-immediate-is-not-json".to_string()),
            ),
            mir_arg(None, Operand::Place("indent".to_string())),
        ],
        &mut env,
    )
    .expect_err("an int32-backed indent must not be accepted as Option[int64]");
    assert_eq!(indent_error.code, "AU4001");
    assert!(indent_error.message.contains("contain an `int64`"));

    env.define_typed(
        "int_value",
        Type::named("json.Value"),
        json_value("Int", vec![Value::Int(IntegerValue::from_i32(7))]),
    );
    let int_error = call_name(
        &mut runtime,
        "json::as_int",
        &[mir_arg(None, Operand::Place("int_value".to_string()))],
        &mut env,
    )
    .expect_err("json.Value.Int must carry exact int64 metadata");
    assert_eq!(int_error.code, "AU4001");
    assert!(int_error.message.contains("json.Value.Int"));
    assert!(
        env.place_ref("int_value").is_ok(),
        "a rejected borrowed accessor must preserve its source"
    );

    for (place, variant, payload) in [
        (
            "array_value",
            "Array",
            Value::Vec(VecValue {
                element_type: Type::named("str"),
                elements: Vec::new(),
            }),
        ),
        (
            "object_value",
            "Object",
            Value::Map(MapValue {
                key_type: Type::named("str"),
                value_type: Type::named("bool"),
                entries: Vec::new(),
            }),
        ),
    ] {
        env.define_typed(
            place,
            Type::named("json.Value"),
            json_value(variant, vec![payload]),
        );
        let adapter = if variant == "Array" {
            "json::into_array"
        } else {
            "json::into_object"
        };
        let error = call_name(
            &mut runtime,
            adapter,
            &[mir_arg(None, Operand::MovePlace(place.to_string()))],
            &mut env,
        )
        .expect_err("owned JSON collection adapters must validate exact metadata");
        assert_eq!(error.code, "AU4001");
        assert!(error.message.contains(&format!("json.Value.{variant}")));
        assert!(
            env.place_ref(place).is_err(),
            "an own argument remains consumed when metadata validation fails"
        );
    }

    assert_eq!(
        IntegerValue::from_i64(1).runtime_kind(),
        Some(IntegerKind::Int64),
        "the canonical JSON integer constructor remains the accepted control"
    );
}

#[test]
fn mir_json_host_boundary_reports_malformed_values_without_hiding_consumption() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    env.define_typed("not_json", Type::named("bool"), Value::Bool(true));
    let error = call_name(
        &mut runtime,
        "json::as_bool",
        &[mir_arg(None, Operand::Place("not_json".to_string()))],
        &mut env,
    )
    .expect_err("borrowed JSON accessors must reject non-json runtime values");
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "`json::as_bool` expected a runtime `json.Value`, found `true`"
    );
    assert_eq!(
        env.read_place("not_json"),
        Ok(Value::Bool(true)),
        "a rejected borrowed accessor must preserve its source"
    );

    for (place, value, call, expected_message) in [
        (
            "malformed_null",
            json_value("Null", vec![Value::Unit]),
            "json::is_null",
            "malformed runtime `json.Value.Null` payload in `json::is_null`",
        ),
        (
            "missing_bool_payload",
            json_value("Bool", Vec::new()),
            "json::as_bool",
            "malformed runtime `json.Value.Bool` payload in `json::as_bool`",
        ),
        (
            "wrong_bool_payload",
            json_value("Bool", vec![Value::Int(IntegerValue::from_i64(1))]),
            "json::as_bool",
            "malformed runtime `json.Value.Bool` payload in `json::as_bool`",
        ),
    ] {
        env.define_typed(place, Type::named("json.Value"), value);
        let source_ptr =
            env.place_ref(place)
                .expect("borrowed malformed source should exist") as *const Value;
        let error = call_name(
            &mut runtime,
            call,
            &[mir_arg(None, Operand::Place(place.to_string()))],
            &mut env,
        )
        .unwrap_err();
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, expected_message);
        assert_eq!(
            env.place_ref(place).unwrap() as *const Value,
            source_ptr,
            "{call} must not replace a malformed borrowed source"
        );
    }

    env.define_typed(
        "valid_value",
        Type::named("json.Value"),
        json_value("Null", Vec::new()),
    );
    for (place, indent, expected_message) in [
        (
            "plain_indent",
            Value::Bool(false),
            "`json::dumps` expects `indent` to be `Option[int64]`",
        ),
        (
            "malformed_indent",
            Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "Some".to_string(),
                payloads: Vec::new(),
            }),
            "`json::dumps` expects `indent` to be `Option[int64]`",
        ),
    ] {
        env.define_typed(place, Type::named("Option"), indent);
        let error = call_name(
            &mut runtime,
            "json::dumps",
            &[
                mir_arg(None, Operand::Place("valid_value".to_string())),
                mir_arg(None, Operand::Place(place.to_string())),
            ],
            &mut env,
        )
        .expect_err("json.dumps must validate the runtime Option[int64] shape");
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, expected_message);
        assert!(env.place_ref(place).is_ok());
        assert!(env.place_ref("valid_value").is_ok());
    }

    env.define_typed(
        "valid_indent",
        Type::Named("Option".to_string(), vec![Type::named("int64")]),
        option_none(),
    );
    for (args, expected_message) in [
        (
            vec![mir_arg(None, Operand::Place("valid_value".to_string()))],
            "missing MIR argument",
        ),
        (
            vec![
                mir_arg(None, Operand::Place("valid_value".to_string())),
                mir_arg(None, Operand::Place("missing_indent".to_string())),
            ],
            "unknown MIR place `missing_indent`",
        ),
        (
            vec![
                mir_arg(None, Operand::Place("missing_value".to_string())),
                mir_arg(None, Operand::Place("valid_indent".to_string())),
            ],
            "unknown MIR place `missing_value`",
        ),
    ] {
        let error = call_name(&mut runtime, "json::dumps", &args, &mut env)
            .expect_err("json.dumps must report argument and place failures");
        assert_eq!(error.message, expected_message);
    }

    for (call, operand, expected_message) in [
        ("json::is_null", None, "missing MIR argument"),
        (
            "json::as_bool",
            Some(Operand::Place("missing_borrowed_json".to_string())),
            "unknown MIR place `missing_borrowed_json`",
        ),
        ("json::into_string", None, "missing MIR argument"),
        (
            "json::into_string",
            Some(Operand::MovePlace("missing_owned_json".to_string())),
            "unknown MIR place `missing_owned_json`",
        ),
    ] {
        let args = operand
            .map(|operand| vec![mir_arg(None, operand)])
            .unwrap_or_default();
        let error = call_name(&mut runtime, call, &args, &mut env)
            .expect_err("JSON host calls must report missing arguments and places");
        assert_eq!(error.message, expected_message);
    }

    for (place, value, expected_message) in [
        (
            "owned_non_json",
            Value::String("text".to_string()),
            "`json::into_string` expected a runtime `json.Value`",
        ),
        (
            "owned_wrong_enum",
            Value::EnumVariant(EnumVariantValue {
                enum_name: "Option".to_string(),
                variant_name: "Some".to_string(),
                payloads: vec![Value::String("text".to_string())],
            }),
            "`json::into_string` expected enum `json.Value`, found `Option`",
        ),
        (
            "owned_missing_payload",
            json_value("String", Vec::new()),
            "malformed runtime `json.Value.String` payload in `json::into_string`",
        ),
        (
            "owned_wrong_payload",
            json_value("String", vec![Value::Bool(true)]),
            "malformed runtime `json.Value.String` payload in `json::into_string`",
        ),
    ] {
        env.define_typed(place, Type::named("json.Value"), value);
        let error = call_name(
            &mut runtime,
            "json::into_string",
            &[mir_arg(None, Operand::MovePlace(place.to_string()))],
            &mut env,
        )
        .expect_err("owned JSON accessors must reject malformed runtime values");
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, expected_message);
        assert!(
            env.place_ref(place).is_err(),
            "an owned argument is consumed once runtime argument binding succeeds"
        );
    }
}

#[test]
fn mir_json_host_argument_errors_preserve_borrowed_sources() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let missing = call_name(&mut runtime, "json::parse", &[], &mut env)
        .expect_err("json.parse should reject a missing text argument");
    assert_eq!(missing.message, "missing MIR argument");

    let unknown = call_name(
        &mut runtime,
        "json::parse",
        &[mir_arg(Some("source"), Operand::String("null".to_string()))],
        &mut env,
    )
    .expect_err("json.parse should reject unknown named MIR arguments");
    assert_eq!(unknown.message, "unknown MIR argument `source`");

    let too_many = call_name(
        &mut runtime,
        "json::parse",
        &[
            mir_arg(Some("text"), Operand::String("null".to_string())),
            mir_arg(None, Operand::String("null".to_string())),
        ],
        &mut env,
    )
    .expect_err("json.parse should reject a positional argument after text is already bound");
    assert_eq!(too_many.message, "too many MIR arguments");

    let text = "{\"answer\":42}".repeat(32);
    let text_ptr = text.as_ptr();
    env.define_typed("text", Type::named("str"), Value::String(text));
    let consuming_borrow = call_name(
        &mut runtime,
        "json::parse",
        &[mir_arg(None, Operand::MovePlace("text".to_string()))],
        &mut env,
    )
    .expect_err("a borrowed JSON host call should reject a consuming MIR operand");
    assert_eq!(
        consuming_borrow.message,
        "cannot borrow consuming MIR operand `text` in `json::parse`"
    );
    match env
        .place_ref("text")
        .expect("the rejected consuming borrow must preserve its source")
    {
        Value::String(text) => assert_eq!(text.as_ptr(), text_ptr),
        other => panic!("expected preserved str, found {other:?}"),
    }

    let wrong_place_type = call_name(
        &mut runtime,
        "json::parse",
        &[mir_arg(None, Operand::Bool(true))],
        &mut env,
    )
    .expect_err("json.parse should diagnose non-str immediate operands");
    assert_eq!(wrong_place_type.code, "AU4001");
    assert_eq!(
        wrong_place_type.message,
        "`json::parse` expects `str`, found `true`"
    );

    env.define_typed(
        "value",
        Type::named("json.Value"),
        json_value("Null", Vec::new()),
    );
    let consuming_accessor = call_name(
        &mut runtime,
        "json::is_null",
        &[mir_arg(None, Operand::MovePlace("value".to_string()))],
        &mut env,
    )
    .expect_err("borrowed JSON accessors should reject consuming MIR operands");
    assert_eq!(
        consuming_accessor.message,
        "cannot borrow consuming MIR operand `value`"
    );
    assert!(
        env.place_ref("value").is_ok(),
        "the rejected consuming accessor must not consume its source"
    );
}

#[test]
fn mir_failed_owned_place_moves_preserve_unmoved_state() {
    let mut env = Env::default();
    env.define_typed(
        "scalar",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(7)),
    );
    let scalar_nested = env
        .take_place("scalar.value")
        .expect_err("moving through a scalar should fail");
    assert_eq!(
        scalar_nested.message,
        "cannot move nested MIR place `scalar.value` from a non-instance value"
    );
    assert_eq!(
        env.read_place("scalar"),
        Ok(Value::Int(IntegerValue::from_signed(7))),
        "a failed nested move must not consume its scalar root"
    );

    env.define_typed(
        "outer",
        Type::named("Outer"),
        Value::Instance(InstanceValue {
            class_name: "Outer".to_string(),
            fields: BTreeMap::from([
                (
                    "inner".to_string(),
                    Value::Instance(InstanceValue {
                        class_name: "Inner".to_string(),
                        fields: BTreeMap::from([(
                            "payload".to_string(),
                            Value::EnumVariant(EnumVariantValue {
                                enum_name: "Packet".to_string(),
                                variant_name: "Text".to_string(),
                                payloads: vec![Value::String("owned".repeat(32))],
                            }),
                        )]),
                    }),
                ),
                ("sibling".to_string(), Value::Bool(true)),
            ]),
        }),
    );

    let missing_root = env
        .take_place("missing")
        .expect_err("root moves should reject unknown places");
    assert_eq!(missing_root.message, "unknown MIR place `missing`");

    let missing_nested_root = env
        .take_place("missing.value")
        .expect_err("nested moves should reject unknown roots");
    assert_eq!(
        missing_nested_root.message,
        "unknown MIR place `missing.value`"
    );

    let missing_leaf = env
        .take_place("outer.missing")
        .expect_err("nested moves should reject unknown leaf fields");
    assert!(missing_leaf
        .message
        .contains("class `Outer` has no field `missing`"));

    let missing_intermediate = env
        .take_place("outer.missing.value")
        .expect_err("nested moves should reject unknown intermediate fields");
    assert!(missing_intermediate
        .message
        .contains("class `Outer` has no field `missing`"));

    let missing_variant_root = env
        .take_variant_payload("missing", 0)
        .expect_err("variant extraction should reject unknown roots");
    assert_eq!(missing_variant_root.message, "unknown MIR place `missing`");

    let missing_variant_field = env
        .take_variant_payload("outer.missing", 0)
        .expect_err("variant extraction should reject unknown nested fields");
    assert!(missing_variant_field
        .message
        .contains("class `Outer` has no field `missing`"));

    let scalar_variant_path = env
        .take_variant_payload("outer.sibling.value", 0)
        .expect_err("variant extraction should reject traversal through scalar fields");
    assert_eq!(
        scalar_variant_path.message,
        "cannot access nested MIR place `outer.sibling.value` on a non-instance value"
    );

    let non_enum = env
        .take_variant_payload("outer.sibling", 0)
        .expect_err("variant extraction should reject non-enum nested values");
    assert_eq!(
        non_enum.message,
        "cannot take enum payload from non-enum MIR place `outer.sibling`"
    );

    let missing_payload = env
        .take_variant_payload("outer.inner.payload", 1)
        .expect_err("variant extraction should reject absent payload indexes");
    assert!(missing_payload
        .message
        .contains("enum variant `Packet.Text` does not carry a payload at index 1"));

    let moved = env
        .take_variant_payload("outer.inner.payload", 0)
        .expect("the existing nested payload should move out");
    assert_eq!(moved, Value::String("owned".repeat(32)));
    assert_eq!(
        env.read_place("outer.sibling"),
        Ok(Value::Bool(true)),
        "moving one nested payload must preserve sibling state"
    );
    let Value::EnumVariant(remaining) = env
        .read_place("outer.inner.payload")
        .expect("the private match owner should remain after payload extraction")
    else {
        panic!("expected the Packet enum to remain");
    };
    assert_eq!(
        remaining.payloads,
        vec![Value::Unit],
        "the moved payload slot must be marked consumed"
    );
}

#[test]
fn mir_owned_collection_mutators_do_not_clone_inserted_values() {
    fn string_ptr(value: &Value) -> *const u8 {
        match value {
            Value::String(value) => value.as_ptr(),
            other => panic!("expected str, found {other:?}"),
        }
    }

    let string_type = Type::named("str");
    let vector_type = Type::Named("list".to_string(), vec![string_type.clone()]);
    for (method, index) in [("set", 0_u128), ("insert", 0), ("__set_index", 0)] {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        let payload = format!("{method}-payload-").repeat(64);
        let payload_ptr = payload.as_ptr();
        let vector = VecValue {
            element_type: string_type.clone(),
            elements: if method == "set" || method == "__set_index" {
                vec![Value::String("old".to_string())]
            } else {
                Vec::new()
            },
        };
        env.define_typed("vector", vector_type.clone(), Value::Vec(vector.clone()));
        env.define_typed("payload", string_type.clone(), Value::String(payload));
        let mut args = vec![
            mir_arg(None, Operand::Int(index)),
            mir_arg(None, Operand::MovePlace("payload".to_string())),
        ];
        if method == "__set_index" {
            args.extend([
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ]);
        }
        runtime
            .evaluate_vec_method(vector, method, Some("vector"), &args, &mut env)
            .unwrap_or_else(|error| panic!("Vec.{method} should succeed: {error}"));
        let Value::Vec(updated) = env
            .place_ref("vector")
            .expect("mutated vector should be written back")
        else {
            panic!("expected vector writeback");
        };
        assert_eq!(
            string_ptr(&updated.elements[0]),
            payload_ptr,
            "Vec.{method} must transfer its own value argument"
        );
        assert!(env.place_ref("payload").is_err());
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let extended = "extended-vector-value".repeat(64);
    let extended_ptr = extended.as_ptr();
    let vector = VecValue {
        element_type: string_type.clone(),
        elements: Vec::new(),
    };
    env.define_typed("vector", vector_type.clone(), Value::Vec(vector.clone()));
    env.define_typed(
        "other",
        vector_type.clone(),
        Value::Vec(VecValue {
            element_type: string_type.clone(),
            elements: vec![Value::String(extended)],
        }),
    );
    runtime
        .evaluate_vec_method(
            vector,
            "extend",
            Some("vector"),
            &[mir_arg(None, Operand::MovePlace("other".to_string()))],
            &mut env,
        )
        .expect("Vec.extend should succeed");
    let Value::Vec(updated) = env
        .place_ref("vector")
        .expect("vector should be written back")
    else {
        panic!("expected vector writeback");
    };
    assert_eq!(string_ptr(&updated.elements[0]), extended_ptr);
    assert!(env.place_ref("other").is_err());

    let map_type = Type::Named(
        "dict".to_string(),
        vec![string_type.clone(), string_type.clone()],
    );
    for method in ["set", "__set_index"] {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        let key = format!("{method}-key-").repeat(64);
        let key_ptr = key.as_ptr();
        let value = format!("{method}-value-").repeat(64);
        let value_ptr = value.as_ptr();
        let map = MapValue {
            key_type: string_type.clone(),
            value_type: string_type.clone(),
            entries: Vec::new(),
        };
        env.define_typed("map", map_type.clone(), Value::Map(map.clone()));
        env.define_typed("key", string_type.clone(), Value::String(key));
        env.define_typed("value", string_type.clone(), Value::String(value));
        let mut args = vec![
            mir_arg(None, Operand::MovePlace("key".to_string())),
            mir_arg(None, Operand::MovePlace("value".to_string())),
        ];
        if method == "__set_index" {
            args.extend([
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ]);
        }
        runtime
            .evaluate_map_method(map, method, Some("map"), &args, &mut env)
            .unwrap_or_else(|error| panic!("Map.{method} should succeed: {error}"));
        let Value::Map(updated) = env.place_ref("map").expect("map should be written back") else {
            panic!("expected map writeback");
        };
        assert_eq!(string_ptr(&updated.entries[0].0), key_ptr);
        assert_eq!(string_ptr(&updated.entries[0].1), value_ptr);
        assert!(env.place_ref("key").is_err());
        assert!(env.place_ref("value").is_err());
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let extended_key = "extended-map-key".repeat(64);
    let extended_key_ptr = extended_key.as_ptr();
    let extended_value = "extended-map-value".repeat(64);
    let extended_value_ptr = extended_value.as_ptr();
    let map = MapValue {
        key_type: string_type.clone(),
        value_type: string_type.clone(),
        entries: Vec::new(),
    };
    env.define_typed("map", map_type.clone(), Value::Map(map.clone()));
    env.define_typed(
        "other",
        map_type,
        Value::Map(MapValue {
            key_type: string_type.clone(),
            value_type: string_type.clone(),
            entries: vec![(Value::String(extended_key), Value::String(extended_value))],
        }),
    );
    runtime
        .evaluate_map_method(
            map,
            "update",
            Some("map"),
            &[mir_arg(None, Operand::MovePlace("other".to_string()))],
            &mut env,
        )
        .expect("dict.update should succeed");
    let Value::Map(updated) = env.place_ref("map").expect("map should be written back") else {
        panic!("expected map writeback");
    };
    assert_eq!(string_ptr(&updated.entries[0].0), extended_key_ptr);
    assert_eq!(string_ptr(&updated.entries[0].1), extended_value_ptr);
    assert!(env.place_ref("other").is_err());

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let added = "set-added-value".repeat(64);
    let added_ptr = added.as_ptr();
    let set_type = Type::Named("set".to_string(), vec![string_type.clone()]);
    let set = SetValue {
        element_type: string_type.clone(),
        elements: Vec::new(),
    };
    env.define_typed("set", set_type, Value::Set(set.clone()));
    env.define_typed("added", string_type, Value::String(added));
    runtime
        .evaluate_set_method(
            set,
            "add",
            Some("set"),
            &[mir_arg(None, Operand::MovePlace("added".to_string()))],
            &mut env,
        )
        .expect("set.add should succeed");
    let Value::Set(updated) = env.place_ref("set").expect("set should be written back") else {
        panic!("expected set writeback");
    };
    assert_eq!(string_ptr(&updated.elements[0]), added_ptr);
    assert!(env.place_ref("added").is_err());
}

#[test]
fn mir_owned_process_and_http_decoders_transfer_string_allocations() {
    let name = "service-name".repeat(64);
    let name_ptr = name.as_ptr();
    let decoded_name = super::expect_owned_string_value(Value::String(name), "start(name=...)")
        .expect("owned service name should decode");
    assert_eq!(decoded_name.as_ptr(), name_ptr);

    let command = vec![
        Value::String("/bin/echo".repeat(32)),
        Value::String("payload".repeat(32)),
    ];
    let command_ptrs = command
        .iter()
        .map(|value| match value {
            Value::String(value) => value.as_ptr(),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    let decoded_command = super::expect_owned_command_vec(
        Value::Vec(VecValue {
            element_type: Type::named("str"),
            elements: command,
        }),
        "start(command=...)",
    )
    .expect("owned command should decode");
    assert_eq!(
        decoded_command
            .iter()
            .map(|value| value.as_ptr())
            .collect::<Vec<_>>(),
        command_ptrs
    );

    let cwd = "/tmp/owned-cwd".repeat(32);
    let cwd_ptr = cwd.as_ptr();
    let decoded_cwd = super::expect_owned_optional_string_value(
        option_some(Value::String(cwd)),
        "start(cwd=...)",
    )
    .expect("owned cwd should decode")
    .expect("cwd should be Some");
    assert_eq!(decoded_cwd.as_ptr(), cwd_ptr);

    let header_name = "X-Owned-Header".repeat(32);
    let header_name_ptr = header_name.as_ptr();
    let header_value = "header-value".repeat(32);
    let header_value_ptr = header_value.as_ptr();
    let headers = super::expect_owned_headers_map(
        Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("str"),
            entries: vec![(Value::String(header_name), Value::String(header_value))],
        }),
        "respond_text(headers=...)",
    )
    .expect("owned HTTP headers should decode");
    assert_eq!(headers[0].0.as_ptr(), header_name_ptr);
    assert_eq!(headers[0].1.as_ptr(), header_value_ptr);

    assert_eq!(
        super::expect_owned_bytes_value(
            Value::Vec(VecValue {
                element_type: Type::named("uint8"),
                elements: vec![
                    Value::Int(
                        IntegerValue::from_typed_unsigned(1, IntegerKind::Uint8)
                            .expect("1 fits uint8"),
                    ),
                    Value::Int(
                        IntegerValue::from_typed_unsigned(255, IntegerKind::Uint8)
                            .expect("255 fits uint8"),
                    ),
                ],
            }),
            "respond_bytes(bytes=...)",
        )
        .expect("owned HTTP bytes should decode"),
        vec![1, 255]
    );
}

#[test]
fn mir_owned_queue_and_task_fallback_adapters_preserve_allocations() {
    fn expect_string_ptr(value: Value, expected: *const u8, label: &str) {
        match value {
            Value::String(value) => assert_eq!(
                value.as_ptr(),
                expected,
                "{label} must return the original owned allocation"
            ),
            other => panic!("{label} returned {other:?} instead of str"),
        }
    }

    for method in ["put", "try_put"] {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        let channel = ChannelValue::new();
        let payload = format!("{method}-queue-payload-").repeat(64);
        let payload_ptr = payload.as_ptr();
        env.define_typed("payload", Type::named("str"), Value::String(payload));
        runtime
            .evaluate_channel_method(
                channel.clone(),
                method,
                &[mir_arg(None, Operand::MovePlace("payload".to_string()))],
                &mut env,
            )
            .unwrap_or_else(|error| panic!("Queue.{method} should succeed: {error}"));
        let crate::runtime_value::TryRecvResult::Value(received) = channel.try_recv() else {
            panic!("Queue.{method} should enqueue one value");
        };
        expect_string_ptr(received, payload_ptr, &format!("Queue.{method}"));
        assert!(env.place_ref("payload").is_err());
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let queue_fallback = "queue-fallback".repeat(64);
    let queue_fallback_ptr = queue_fallback.as_ptr();
    env.define_typed(
        "queue_fallback",
        Type::named("str"),
        Value::String(queue_fallback),
    );
    let fallback = runtime
        .evaluate_channel_method(
            ChannelValue::new(),
            "get_or",
            &[mir_arg(
                None,
                Operand::MovePlace("queue_fallback".to_string()),
            )],
            &mut env,
        )
        .expect("an empty Queue.get_or should return its fallback");
    expect_string_ptr(fallback, queue_fallback_ptr, "Queue.get_or");
    assert!(env.place_ref("queue_fallback").is_err());

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let task_fallback = "task-fallback".repeat(64);
    let task_fallback_ptr = task_fallback.as_ptr();
    env.define_typed(
        "task_fallback",
        Type::named("str"),
        Value::String(task_fallback),
    );
    let pending = TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(50));
        Ok(Value::Unit)
    }));
    let fallback = runtime
        .evaluate_task_method(
            pending,
            "result_or",
            &[mir_arg(
                None,
                Operand::MovePlace("task_fallback".to_string()),
            )],
            &mut env,
        )
        .expect("a pending Task.result_or should return its fallback immediately");
    expect_string_ptr(fallback, task_fallback_ptr, "Task.result_or");
    assert!(env.place_ref("task_fallback").is_err());
}

#[test]
fn mir_owned_vec_and_set_iteration_take_elements_from_the_private_source() {
    fn expect_option_string_ptr(value: Value, expected: *const u8) {
        let mut payloads = enum_payloads(value, "Option", "Some");
        match payloads.remove(0) {
            Value::String(value) => assert_eq!(value.as_ptr(), expected),
            other => panic!("expected Option.Some(str), found {other:?}"),
        }
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let vector_text = "owned-vector-iteration".repeat(64);
    let vector_text_ptr = vector_text.as_ptr();
    let vector_type = Type::Named("list".to_string(), vec![Type::named("str")]);
    env.define_typed(
        "vector",
        vector_type,
        Value::Vec(VecValue {
            element_type: Type::named("str"),
            elements: vec![Value::String(vector_text)],
        }),
    );
    let value = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::MovePlace("vector".to_string()),
                field: "__take_index_option".to_string(),
                receiver_place: Some("vector".to_string()),
            },
            &[mir_arg(None, Operand::Int(0))],
            &mut env,
        )
        .expect("owned Vec iteration should take its next element");
    expect_option_string_ptr(value, vector_text_ptr);
    let Value::Vec(remaining) = env.read_place("vector").expect("vector should be restored") else {
        panic!("expected vector writeback");
    };
    assert!(remaining.elements.is_empty());

    let set_text = "owned-set-iteration".repeat(64);
    let set_text_ptr = set_text.as_ptr();
    let set_type = Type::Named("set".to_string(), vec![Type::named("str")]);
    env.define_typed(
        "set",
        set_type,
        Value::Set(SetValue {
            element_type: Type::named("str"),
            elements: vec![Value::String(set_text)],
        }),
    );
    let value = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::MovePlace("set".to_string()),
                field: "__take_index_option".to_string(),
                receiver_place: Some("set".to_string()),
            },
            &[mir_arg(None, Operand::Int(0))],
            &mut env,
        )
        .expect("owned Set iteration should take its next element");
    expect_option_string_ptr(value, set_text_ptr);
    let Value::Set(remaining) = env.read_place("set").expect("set should be restored") else {
        panic!("expected set writeback");
    };
    assert!(remaining.elements.is_empty());
}

#[test]
fn mir_secure_bytes_rejects_requests_above_the_resource_ceiling_with_au4005() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let requested = i32::MAX as u128 + 1;

    let error = call_name(
        &mut runtime,
        "random::secure_bytes",
        &[mir_arg(Some("n"), Operand::Int(requested))],
        &mut env,
    )
    .expect_err("secure byte counts above the request ceiling must fail before allocation");

    assert_eq!(error.code, "AU4005");
    assert_eq!(
        error.message,
        "`random.secure_bytes(n)` count `2147483648` exceeds the secure-random request ceiling `2147483647`"
    );
}

#[test]
fn mir_secure_random_diagnostics_preserve_validation_and_host_failures() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let invalid_bounds = call_name(
        &mut runtime,
        "random::secure_int",
        &[
            mir_arg(Some("lo"), Operand::Int(8)),
            mir_arg(Some("hi"), Operand::Int(8)),
        ],
        &mut env,
    )
    .expect_err("secure random bounds must be validated before requesting entropy");
    assert_eq!(invalid_bounds.code, "AU4003");
    assert_eq!(
        invalid_bounds.message,
        "random bounds require `lo < hi`, found `8 >= 8`"
    );

    env.define_typed(
        "negative_count",
        Type::named("int64"),
        Value::Int(IntegerValue::from_signed(-1)),
    );
    let negative_count = call_name(
        &mut runtime,
        "random::secure_bytes",
        &[mir_arg(
            Some("n"),
            Operand::Place("negative_count".to_string()),
        )],
        &mut env,
    )
    .expect_err("negative secure byte counts must fail before allocation or entropy");
    assert_eq!(negative_count.code, "AU4003");
    assert_eq!(
        negative_count.message,
        "`random.secure_bytes(n)` requires a non-negative byte count, found `-1`"
    );

    let allocation = Vec::<u8>::new()
        .try_reserve_exact(usize::MAX)
        .expect_err("an impossible byte allocation should fail");
    let allocation =
        super::random_resource_error_to_diagnostic(SecureRandomError::Allocation(allocation), None);
    assert_eq!(allocation.code, "AU4005");
    assert!(
        allocation
            .message
            .starts_with("secure random allocation failed: "),
        "unexpected allocation diagnostic: {}",
        allocation.message
    );

    let entropy = super::random_resource_error_to_diagnostic(
        SecureRandomError::Entropy(getrandom::Error::UNSUPPORTED),
        None,
    );
    assert_eq!(entropy.code, "AU4005");
    assert_eq!(
        entropy.message,
        format!(
            "operating-system random source failed: {}",
            getrandom::Error::UNSUPPORTED
        )
    );
}

fn string_vec_value(items: &[&str]) -> Value {
    Value::Vec(VecValue {
        element_type: Type::named("str"),
        elements: items
            .iter()
            .map(|item| Value::String((*item).to_string()))
            .collect(),
    })
}

fn string_map_value(items: &[(&str, &str)]) -> Value {
    Value::Map(crate::runtime_value::MapValue {
        key_type: Type::named("str"),
        value_type: Type::named("str"),
        entries: items
            .iter()
            .map(|(key, value)| {
                (
                    Value::String((*key).to_string()),
                    Value::String((*value).to_string()),
                )
            })
            .collect(),
    })
}

fn run_native_entry(
    mir_ptr: *const u8,
    mir_len: usize,
    source_path_ptr: *const u8,
    source_path_len: usize,
    source_ptr: *const u8,
    source_len: usize,
) -> i32 {
    unsafe {
        crate::mir_runtime::aura_native_run(
            mir_ptr,
            mir_len,
            source_path_ptr,
            source_path_len,
            source_ptr,
            source_len,
        )
    }
}

#[test]
fn contextual_none_rhs_equality_preserves_the_left_option_snapshot() {
    let source = r#"
def main() -> int32:
    value: Option[int32] = Option.None
    print(value == None)
    print(value != None)
    return 0
"#;
    let module = crate::lower_source_to_mir(source).expect("contextual None source should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should be lowered");
    let comparison_lefts = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Binary {
                        op: crate::ast::BinaryOp::Eq | crate::ast::BinaryOp::NotEq,
                        left: Operand::Place(left),
                        ..
                    },
                ..
            } => Some(left),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(comparison_lefts.len(), 2);
    for left in comparison_lefts {
        assert_eq!(
            main.local_types
                .iter()
                .find(|local| local.name == *left)
                .map(|local| &local.ty),
            Some(&Type::Named(
                "Option".to_string(),
                vec![Type::named("int32")]
            )),
            "the sequenced left operand `{left}` should retain its inferred Option type"
        );
    }
    let stdout = Arc::new(Mutex::new(String::new()));
    let mut runtime = MirRuntime::new(module, stdout.clone(), CancellationContext::default());

    assert_eq!(
        runtime
            .run_main()
            .expect("contextual None equality should execute"),
        Value::Int(IntegerValue::zero())
    );
    assert_eq!(stdout.lock().unwrap().as_str(), "true\nfalse\n");
}

#[test]
fn env_place_helpers_cover_nested_reads_and_writes() {
    let mut env = Env::default();
    env.define_typed(
        "counter",
        Type::named("Counter"),
        Value::Instance(InstanceValue {
            class_name: "Counter".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(1)),
            )]),
        }),
    );

    assert_eq!(
        env.read_place("counter.value")
            .expect("nested place should read"),
        Value::Int(IntegerValue::from_signed(1))
    );
    env.write_place("counter.value", Value::Int(IntegerValue::from_signed(4)))
        .expect("nested place should write");
    assert_eq!(
        env.read_place("counter.value")
            .expect("updated nested place should read"),
        Value::Int(IntegerValue::from_signed(4))
    );
    assert_eq!(env.place_type("counter"), Some(&Type::named("Counter")));
    env.set_place_type("counter.value", Type::named("int32"));
    assert_eq!(env.place_type("counter.value"), Some(&Type::named("int32")));

    env.define_typed(
        "outer",
        Type::named("Outer"),
        Value::Instance(InstanceValue {
            class_name: "Outer".to_string(),
            fields: BTreeMap::from([(
                "inner".to_string(),
                Value::Instance(InstanceValue {
                    class_name: "Inner".to_string(),
                    fields: BTreeMap::from([(
                        "value".to_string(),
                        Value::Int(IntegerValue::from_signed(8)),
                    )]),
                }),
            )]),
        }),
    );
    assert_eq!(
        env.read_member("outer.inner", "value")
            .expect("nested member reads should work"),
        Value::Int(IntegerValue::from_signed(8))
    );
    let nested_non_instance = env
        .read_member("outer.inner.value", "missing")
        .expect_err("nested member reads should reject non-instance leaves");
    assert!(nested_non_instance
        .message
        .contains("cannot access field `missing` on non-instance MIR place"));

    let missing_nested_member = env
        .read_member("counter.missing", "value")
        .expect_err("nested member reads should reject missing child fields");
    assert!(missing_nested_member
        .message
        .contains("has no field `missing` in MIR place `counter.missing`"));

    let missing_member_root = env
        .read_member("missing", "value")
        .expect_err("member reads should reject missing roots");
    assert!(missing_member_root
        .message
        .contains("unknown MIR place `missing`"));

    let error = env
        .read_place("counter.missing")
        .expect_err("unknown field should fail");
    assert!(error.message.contains("has no field `missing`"));

    env.define_typed(
        "count",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(9)),
    );
    let non_instance_read = env
        .read_place("count.value")
        .expect_err("non-instance nested reads should fail");
    assert!(non_instance_read
        .message
        .contains("cannot access field `value` on non-instance MIR place `count.value`"));
    let non_instance_member_root = env
        .read_member("count", "value")
        .expect_err("member reads should reject scalar roots");
    assert!(non_instance_member_root
        .message
        .contains("cannot access field `value` on non-instance MIR place `count`"));
    let non_instance_member_leaf = env
        .read_member("count.value", "leaf")
        .expect_err("member reads should reject scalar nested segments");
    assert!(non_instance_member_leaf
        .message
        .contains("cannot access field `value` on non-instance MIR place `count.value`"));
    let non_instance_write = env
        .write_place("count.value", Value::Int(IntegerValue::from_signed(2)))
        .expect_err("non-instance nested writes should fail");
    assert!(non_instance_write
        .message
        .contains("cannot assign nested MIR place `count.value` on non-instance value"));

    let missing_root = env
        .read_place("missing")
        .expect_err("unknown MIR places should fail");
    assert!(missing_root.message.contains("unknown MIR place `missing`"));

    let missing_root_write = env
        .write_place("missing.value", Value::Int(IntegerValue::from_signed(2)))
        .expect_err("nested writes should reject missing roots");
    assert!(missing_root_write
        .message
        .contains("unknown MIR place `missing.value`"));

    let missing_child_write = env
        .write_place(
            "counter.missing.value",
            Value::Int(IntegerValue::from_signed(2)),
        )
        .expect_err("nested writes should reject missing child fields");
    assert!(missing_child_write
        .message
        .contains("has no field `missing` in MIR place"));

    let trailing_dot = env
        .read_place("counter.")
        .expect_err("trailing dots should be invalid MIR places");
    assert!(trailing_dot
        .message
        .contains("invalid MIR place `counter.`"));
    let doubled_dot = env
        .read_place("counter..value")
        .expect_err("empty place segments should be invalid MIR places");
    assert!(doubled_dot
        .message
        .contains("invalid MIR place `counter..value`"));

    env.write_place("count", Value::Int(IntegerValue::from_signed(11)))
        .expect("root writes should succeed");
    assert_eq!(
        env.read_place("count").expect("root place should read"),
        Value::Int(IntegerValue::from_signed(11))
    );
}

#[test]
fn mir_runtime_helper_values_and_streams_cover_option_result_and_diagnostics() {
    assert_eq!(option_some(Value::Bool(true)).render(), "Option.Some(true)");
    assert_eq!(option_none().render(), "Option.None");
    assert_eq!(result_ok(Value::Bool(false)).render(), "Result.Ok(false)");
    assert_eq!(
        result_err(Value::String("oops".to_string())).render(),
        "Result.Err(oops)"
    );
    assert_eq!(
        send_error_closed(Value::Int(IntegerValue::from_signed(5))).render(),
        "SendError.Closed(5)"
    );

    let diagnostic = Diagnostic::at(Span::new(2, 3), "division by zero");
    let rendered = render_runtime_error("/tmp/test.au", "def main():\n    1 // 0\n", &diagnostic);
    assert!(rendered.contains("/tmp/test.au"));
    assert!(rendered.contains("division by zero"));

    let mut buffer = Vec::new();
    write_stream(&mut buffer, "aura").expect("write_stream should flush successfully");
    assert_eq!(String::from_utf8(buffer).unwrap(), "aura");

    assert_eq!(
        MirRuntime::infer_value_type(&Value::Duration(5)),
        Some(Type::named("Duration"))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::Range(RangeValue { start: 1, end: 3 })),
        Some(Type::named("Range"))
    );

    let string_type_error = super::expect_string_value(&Value::Bool(true), "path")
        .expect_err("string helper should reject booleans");
    assert!(string_type_error
        .message
        .contains("`path` expects `str`, found `true`"));

    let command_type_error = super::expect_command_vec(
        &Value::Vec(VecValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
        "command",
    )
    .expect_err("command helper should reject non-string vectors");
    assert!(command_type_error
        .message
        .contains("`command` expects `list[str]`"));
    assert_eq!(
        super::expect_command_vec(
            &Value::Vec(VecValue {
                element_type: Type::named("str"),
                elements: vec![Value::String("echo".to_string())],
            }),
            "command",
        )
        .expect("string vectors should decode as commands"),
        vec!["echo".to_string()]
    );
    let malformed_command_error = super::expect_command_vec(
        &Value::Vec(VecValue {
            element_type: Type::named("str"),
            elements: vec![Value::Bool(true)],
        }),
        "command",
    )
    .expect_err("command helper should validate string vector elements");
    assert!(malformed_command_error
        .message
        .contains("`command` expects `str`"));

    assert_eq!(
        super::expect_bytes_value(
            &Value::Vec(VecValue {
                element_type: Type::named("Unknown"),
                elements: vec![
                    Value::Int(IntegerValue::from_signed(65)),
                    Value::Int(IntegerValue::from_signed(66)),
                ],
            }),
            "payload",
        )
        .expect("Unknown integer vectors should decode as bytes"),
        b"AB".to_vec()
    );
    let bytes_type_error = super::expect_bytes_value(&Value::String("bad".to_string()), "payload")
        .expect_err("byte helper should reject non-vector payloads");
    assert!(bytes_type_error
        .message
        .contains("`payload` expects `list[uint8]`"));
    let bytes_range_error = super::expect_bytes_value(
        &Value::Vec(VecValue {
            element_type: Type::named("uint8"),
            elements: vec![Value::Int(IntegerValue::from_signed(300))],
        }),
        "payload",
    )
    .expect_err("byte helper should reject out-of-range integers");
    assert!(bytes_range_error
        .message
        .contains("`payload` expects `list[uint8]`"));

    let bool_type_error = super::expect_bool_value(&Value::String("yes".to_string()), "flag")
        .expect_err("bool helper should reject strings");
    assert!(bool_type_error
        .message
        .contains("`flag` expects `bool`, found `yes`"));

    assert_eq!(
        super::expect_optional_string_value(&Value::Unit, "event")
            .expect("unit should decode as absent optional string"),
        None
    );
    assert_eq!(
        super::expect_optional_string_value(&option_none(), "event")
            .expect("Option.None should decode as absent optional string"),
        None
    );
    assert_eq!(
        super::expect_optional_string_value(
            &option_some(Value::String("ready".to_string())),
            "event"
        )
        .expect("Option.Some(str) should decode"),
        Some("ready".to_string())
    );
    let malformed_option_error = super::expect_optional_string_value(
        &Value::EnumVariant(EnumVariantValue {
            enum_name: "Option".to_string(),
            variant_name: "Some".to_string(),
            payloads: Vec::new(),
        }),
        "event",
    )
    .expect_err("malformed Option.Some should be rejected");
    assert!(malformed_option_error
        .message
        .contains("malformed option payload"));
    let optional_string_type_error =
        super::expect_optional_string_value(&Value::Bool(false), "event")
            .expect_err("optional string helper should reject booleans");
    assert!(optional_string_type_error
        .message
        .contains("`event` expects `Option[str]`"));

    assert_eq!(
        super::expect_i32_value(&Value::Int(IntegerValue::from_signed(7)), "count")
            .expect("small integers should decode as i32"),
        7
    );
    let i32_range_error = super::expect_i32_value(
        &Value::Int(IntegerValue::from_signed(i128::from(i32::MAX) + 1)),
        "count",
    )
    .expect_err("oversized integers should be rejected as i32");
    assert!(i32_range_error.message.contains("`count` expects `int32`"));
    let i32_type_error = super::expect_i32_value(&Value::String("7".to_string()), "count")
        .expect_err("i32 helper should reject strings");
    assert!(i32_type_error
        .message
        .contains("`count` expects `int32`, found `7`"));

    assert_eq!(
        super::expect_process_optional_timeout(&Value::Unit, "timeout")
            .expect("unit process timeout should decode as absent"),
        None
    );
    let negative_process_timeout =
        super::expect_process_optional_timeout(&Value::Duration(-1), "timeout")
            .expect_err("an explicit negative process timeout must not mean omitted");
    assert_eq!(
        negative_process_timeout.render(),
        "Error.Io(io.Error.InvalidInput)"
    );
    assert_eq!(
        super::expect_process_optional_timeout(&Value::Duration(10_000_000), "timeout")
            .expect("positive process timeout should decode"),
        Some(StdDuration::from_millis(10))
    );
    let process_timeout_range_error =
        super::expect_process_optional_timeout(&Value::Duration(i128::MAX), "timeout")
            .expect_err("process timeout helper should reject oversized durations");
    assert_eq!(
        process_timeout_range_error.render(),
        "Error.Io(io.Error.InvalidInput)"
    );
    let process_timeout_type_error =
        super::expect_process_optional_timeout(&Value::String("soon".to_string()), "timeout")
            .expect_err("process timeout helper should reject strings");
    assert_eq!(
        process_timeout_type_error.render(),
        "Error.Io(io.Error.InvalidInput)"
    );

    assert_eq!(
        super::expect_duration_value(&Value::Duration(4_000_000), "timeout")
            .expect("positive duration should decode"),
        StdDuration::from_millis(4)
    );
    let negative_duration_error = super::expect_duration_value(&Value::Duration(-4), "timeout")
        .expect_err("negative durations should be rejected");
    assert_eq!(
        negative_duration_error.render(),
        "Error.Io(io.Error.InvalidInput)"
    );
    let duration_type_error =
        super::expect_duration_value(&Value::String("soon".to_string()), "timeout")
            .expect_err("duration helper should reject strings");
    assert_eq!(
        duration_type_error.render(),
        "Error.Io(io.Error.InvalidInput)"
    );

    assert_eq!(
        super::expect_supervisor_max_restarts(&Value::Int(IntegerValue::from_signed(-1)), "max")
            .expect("-1 should decode as unbounded max_restarts"),
        None
    );
    assert_eq!(
        super::expect_supervisor_max_restarts(&Value::Int(IntegerValue::from_signed(2)), "max")
            .expect("positive max_restarts should decode"),
        Some(2)
    );
    let restart_error =
        super::expect_supervisor_max_restarts(&Value::Int(IntegerValue::from_signed(-2)), "max")
            .expect_err("max_restarts below -1 should be rejected");
    assert!(restart_error.message.contains("to be -1 or greater"));

    assert_eq!(
        super::expect_optional_timeout(None, "timeout")
            .expect("missing optional timeout should decode as absent"),
        None
    );
    assert_eq!(
        super::expect_optional_timeout(Some(&Value::Unit), "timeout")
            .expect("unit optional timeout should decode as absent"),
        None
    );
    assert_eq!(
        super::expect_optional_timeout(Some(&Value::Duration(8_000_000)), "timeout")
            .expect("duration optional timeout should decode"),
        Some(StdDuration::from_millis(8))
    );
    let optional_timeout_negative =
        super::expect_optional_timeout(Some(&Value::Duration(-8)), "timeout")
            .expect_err("negative optional timeout should be rejected");
    assert!(optional_timeout_negative
        .message
        .contains("must be non-negative"));
    let optional_timeout_type_error =
        super::expect_optional_timeout(Some(&Value::String("soon".to_string())), "timeout")
            .expect_err("optional timeout should reject strings");
    assert!(optional_timeout_type_error
        .message
        .contains("`timeout` expects `Duration`"));

    assert_eq!(
        super::expect_headers_map(
            &Value::Map(MapValue {
                key_type: Type::named("str"),
                value_type: Type::named("str"),
                entries: vec![(
                    Value::String("Accept".to_string()),
                    Value::String("*/*".to_string())
                )],
            }),
            "headers",
        )
        .expect("string header maps should decode"),
        vec![("Accept".to_string(), "*/*".to_string())]
    );
    assert_eq!(
        super::headers_map_value(vec![("X-Test".to_string(), "1".to_string())]).render(),
        "{X-Test: 1}"
    );
    let malformed_headers_error = super::expect_headers_map(
        &Value::Map(MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("str"),
            entries: vec![(Value::Bool(true), Value::String("bad".to_string()))],
        }),
        "headers",
    )
    .expect_err("headers helper should validate map entries");
    assert!(malformed_headers_error
        .message
        .contains("`headers` expects `str`"));
    let headers_type_error = super::expect_headers_map(&Value::Bool(true), "headers")
        .expect_err("headers helper should reject non-maps");
    assert!(headers_type_error
        .message
        .contains("`headers` expects `dict[str, str]`"));
}

#[test]
fn mir_runtime_process_capture_helpers_cover_success_and_malformed_results() {
    fn assert_process_error_variant(value: Value, variant_name: &str) {
        let Value::EnumVariant(variant) = value else {
            panic!("expected process error enum variant");
        };
        assert_eq!(variant.variant_name, variant_name);
    }

    assert_process_error_variant(
        super::process_error_from_io(io::Error::new(io::ErrorKind::TimedOut, "slow")),
        "TimedOut",
    );
    assert_process_error_variant(
        super::process_error_from_io(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
        "Cancelled",
    );
    assert_process_error_variant(
        super::process_error_from_io(io::Error::new(io::ErrorKind::Other, "io failed")),
        "Io",
    );

    let runtime = test_runtime();
    assert_eq!(
        runtime
            .await_process_capture_task(None, "stdout")
            .expect("missing capture task should produce empty bytes"),
        Vec::<u8>::new()
    );

    let bytes_task = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: Type::named("uint8"),
            elements: vec![
                Value::Int(IntegerValue::from_signed(65)),
                Value::Int(IntegerValue::from_literal(66)),
            ],
        }))
    }));
    assert_eq!(
        runtime
            .await_process_capture_task(Some(bytes_task), "stdout")
            .expect("byte capture task should decode bytes"),
        b"AB".to_vec()
    );

    let non_byte_integer = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: Type::named("uint8"),
            elements: vec![Value::Int(IntegerValue::from_signed(300))],
        }))
    }));
    let error = runtime
        .await_process_capture_task(Some(non_byte_integer), "stdout")
        .expect_err("non-byte integers should fail capture decoding");
    assert!(error
        .message
        .contains("process stdout capture returned a non-byte integer"));

    let wrong_payload = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: Type::named("uint8"),
            elements: vec![Value::String("bad".to_string())],
        }))
    }));
    let error = runtime
        .await_process_capture_task(Some(wrong_payload), "stderr")
        .expect_err("non-integer byte payloads should fail capture decoding");
    assert!(error
        .message
        .contains("process stderr capture returned `bad` inside `list[uint8]"));

    let wrong_result_type = TaskValue::from_handle(thread::spawn(|| {
        Ok(Value::Vec(VecValue {
            element_type: Type::named("str"),
            elements: vec![Value::String("bad".to_string())],
        }))
    }));
    let error = runtime
        .await_process_capture_task(Some(wrong_result_type), "stderr")
        .expect_err("wrong capture result types should fail");
    assert!(error
        .message
        .contains("process stderr capture returned `[bad]` instead of `list[uint8]"));

    let capture_error =
        TaskValue::from_handle(thread::spawn(|| Err(Diagnostic::new("pipe failed"))));
    let error = runtime
        .await_process_capture_task(Some(capture_error), "stdout")
        .expect_err("capture task diagnostics should propagate");
    assert_eq!(error.message, "pipe failed");

    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        group.child_cancellation(),
    );
    let slow_capture = TaskValue::from_handle(thread::spawn(|| {
        thread::sleep(StdDuration::from_millis(50));
        Ok(Value::Vec(VecValue {
            element_type: Type::named("uint8"),
            elements: Vec::new(),
        }))
    }));
    group.cancel();
    let error = cancelled_runtime
        .await_process_capture_task(Some(slow_capture), "stderr")
        .expect_err("cancelled capture waits should fail");
    assert!(error
        .message
        .contains("process stderr capture was cancelled unexpectedly"));
}

#[test]
fn mir_runtime_infers_resource_value_types_for_runtime_backed_surfaces() {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let file_path = std::env::temp_dir().join(format!(
        "aura-mir-runtime-resource-inference-{timestamp}.txt"
    ));
    std::fs::write(&file_path, "resource").expect("test file should be written");
    let file = FileValue::open(file_path.to_str().expect("temp path should be utf-8"))
        .expect("file should open");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::File(file.clone())),
        Some(Type::named("fs.File"))
    );
    file.close();
    let _ = std::fs::remove_file(&file_path);

    let tcp_listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp listener should bind");
    let tcp_address = tcp_listener
        .local_addr()
        .expect("tcp listener address should be available");
    let tcp_server = {
        let listener = tcp_listener.clone();
        thread::spawn(move || {
            listener
                .accept(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("tcp accept should succeed")
        })
    };
    let tcp_client = TcpStreamValue::connect(
        &tcp_address,
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("tcp client should connect");
    let tcp_server_stream = tcp_server.join().expect("tcp server should join");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::TcpListener(tcp_listener.clone())),
        Some(Type::named("net.TcpListener"))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::TcpStream(tcp_client.clone())),
        Some(Type::named("net.TcpStream"))
    );
    tcp_client.close();
    tcp_server_stream.close();
    tcp_listener.close();

    let udp_socket = UdpSocketValue::bind("127.0.0.1:0").expect("udp socket should bind");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::UdpSocket(udp_socket.clone())),
        Some(Type::named("net.UdpSocket"))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::UdpDatagram(UdpDatagramValue {
            address: "127.0.0.1:7".to_string(),
            data: vec![1, 2, 3],
        })),
        Some(Type::named("net.UdpDatagram"))
    );
    udp_socket.close();

    let http_listener = HttpListenerValue::bind("127.0.0.1:0").expect("http listener should bind");
    let http_address = http_listener
        .local_addr()
        .expect("http listener address should be available");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::HttpListener(http_listener.clone())),
        Some(Type::named("net.HttpListener"))
    );
    let http_server = {
        let listener = http_listener.clone();
        thread::spawn(move || {
            let exchange = listener
                .accept(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("http accept should succeed");
            assert_eq!(
                MirRuntime::infer_value_type(&Value::HttpExchange(exchange.clone())),
                Some(Type::named("net.HttpExchange"))
            );
            exchange
                .respond_text(200, "ok", Vec::new())
                .expect("http response should write");
        })
    };
    let http_response = HttpResponseValue::request_text(
        "GET",
        &format!("http://{http_address}/types"),
        "",
        Vec::new(),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("http request should succeed");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::HttpResponse(http_response)),
        Some(Type::named("net.HttpResponse"))
    );
    http_server.join().expect("http server should join");

    let supervisor = ProcessSupervisorValue::new();
    assert_eq!(
        MirRuntime::infer_value_type(&Value::ProcessSupervisor(supervisor.clone())),
        Some(Type::named("process.Supervisor"))
    );
    supervisor.close();

    let child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf pipe".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("process child should spawn");
    let stdout = child.stdout().expect("child stdout should be piped");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::ProcessChild(child.clone())),
        Some(Type::named("process.Child"))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::ProcessPipe(stdout.clone())),
        Some(Type::named("process.Pipe"))
    );
    child.wait(
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    );
    stdout.close();

    let completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(0))],
        }),
        b"out".to_vec(),
        b"err".to_vec(),
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::ProcessCompleted(completed)),
        Some(Type::named("process.Completed"))
    );

    #[cfg(unix)]
    {
        let socket_path = std::path::PathBuf::from(format!(
            "/tmp/aura-mir-{}-{}.sock",
            std::process::id(),
            timestamp % 1_000_000
        ));
        let _ = std::fs::remove_file(&socket_path);
        let unix_listener =
            UnixListenerValue::bind(socket_path.to_str().expect("utf-8 socket path"))
                .expect("unix listener should bind");
        let unix_server = {
            let listener = unix_listener.clone();
            thread::spawn(move || {
                listener
                    .accept(
                        Some(StdDuration::from_secs(2)),
                        Some(&CancellationContext::default()),
                    )
                    .expect("unix accept should succeed")
            })
        };
        let unix_client = UnixStreamValue::connect(
            socket_path.to_str().expect("utf-8 socket path"),
            Some(StdDuration::from_secs(2)),
            Some(&CancellationContext::default()),
        )
        .expect("unix client should connect");
        let unix_server_stream = unix_server.join().expect("unix server should join");
        assert_eq!(
            MirRuntime::infer_value_type(&Value::UnixListener(unix_listener.clone())),
            Some(Type::named("net.UnixListener"))
        );
        assert_eq!(
            MirRuntime::infer_value_type(&Value::UnixStream(unix_client.clone())),
            Some(Type::named("net.UnixStream"))
        );
        unix_client.close();
        unix_server_stream.close();
        unix_listener.close();
        let _ = std::fs::remove_file(&socket_path);
    }

    let certificate =
        generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert generation");
    let cert_path = std::env::temp_dir().join(format!(
        "aura-mir-runtime-resource-inference-{timestamp}-cert.pem"
    ));
    let key_path = std::env::temp_dir().join(format!(
        "aura-mir-runtime-resource-inference-{timestamp}-key.pem"
    ));
    std::fs::write(&cert_path, certificate.cert.pem()).expect("cert should be written");
    std::fs::write(&key_path, certificate.key_pair.serialize_pem()).expect("key should be written");
    let tls_listener = TlsListenerValue::bind(
        "127.0.0.1:0",
        cert_path.to_str().expect("cert path should be utf-8"),
        key_path.to_str().expect("key path should be utf-8"),
    )
    .expect("tls listener should bind");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::TlsListener(tls_listener.clone())),
        Some(Type::named("net.TlsListener"))
    );
    let mut tls_runtime = test_runtime();
    let mut tls_env = Env::default();
    let tls_address = result_ok_payload(
        tls_runtime
            .evaluate_tls_listener_method(tls_listener.clone(), "local_addr", &[], &mut tls_env)
            .expect("tls listener local_addr should succeed"),
    );
    let Value::String(tls_address) = tls_address else {
        panic!("tls listener local_addr should return a string");
    };
    assert!(tls_runtime
        .evaluate_tls_listener_method(tls_listener.clone(), "unsupported", &[], &mut tls_env)
        .expect_err("unsupported tls listener methods should fail")
        .message
        .contains("unsupported MIR tls listener method"));
    let tls_server = {
        let listener = tls_listener.clone();
        thread::spawn(move || {
            let mut server_runtime = test_runtime();
            let mut server_env = Env::default();
            let stream = result_ok_payload(
                server_runtime
                    .evaluate_tls_listener_method(
                        listener,
                        "accept",
                        &[mir_arg(Some("timeout"), Operand::Duration(2_000_000_000))],
                        &mut server_env,
                    )
                    .expect("tls accept should succeed"),
            );
            let Value::TlsStream(stream) = stream else {
                panic!("tls accept should return a tls stream");
            };
            assert_eq!(
                MirRuntime::infer_value_type(&Value::TlsStream(stream.clone())),
                Some(Type::named("net.TlsStream"))
            );
            let line = result_ok_payload(
                server_runtime
                    .evaluate_tls_stream_method(
                        stream.clone(),
                        "read_line",
                        &[mir_arg(Some("timeout"), Operand::Duration(2_000_000_000))],
                        &mut server_env,
                    )
                    .expect("tls server read_line should succeed"),
            );
            assert_eq!(
                enum_payloads(line, "Option", "Some"),
                vec![Value::String("secure".to_string())]
            );
            assert_eq!(
                result_ok_payload(
                    server_runtime
                        .evaluate_tls_stream_method(
                            stream.clone(),
                            "write_all",
                            &[
                                mir_arg(Some("text"), Operand::String("ok".to_string())),
                                mir_arg(Some("timeout"), Operand::Duration(2_000_000_000),),
                            ],
                            &mut server_env,
                        )
                        .expect("tls server write_all should succeed")
                ),
                Value::Unit
            );
            assert_eq!(
                server_runtime
                    .evaluate_tls_stream_method(stream, "close", &[], &mut server_env)
                    .expect("tls server close should succeed"),
                Value::Unit
            );
        })
    };
    let tls_client = TlsStreamValue::connect(
        &tls_address,
        "localhost",
        Some(cert_path.to_str().expect("cert path should be utf-8")),
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    )
    .expect("tls client should connect");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::TlsStream(tls_client.clone())),
        Some(Type::named("net.TlsStream"))
    );
    assert_eq!(
        result_ok_payload(
            tls_runtime
                .evaluate_tls_stream_method(
                    tls_client.clone(),
                    "write_all",
                    &[
                        mir_arg(Some("text"), Operand::String("secure\n".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(2_000_000_000)),
                    ],
                    &mut tls_env,
                )
                .expect("tls client write_all should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            tls_runtime
                .evaluate_tls_stream_method(
                    tls_client.clone(),
                    "read_exact",
                    &[
                        mir_arg(Some("count"), Operand::Int(2)),
                        mir_arg(Some("timeout"), Operand::Duration(2_000_000_000)),
                    ],
                    &mut tls_env,
                )
                .expect("tls client read_exact should succeed")
        ),
        bytes_vec_value(b"ok".to_vec())
    );
    assert!(tls_runtime
        .evaluate_tls_stream_method(tls_client.clone(), "unsupported", &[], &mut tls_env)
        .expect_err("unsupported tls stream methods should fail")
        .message
        .contains("unsupported MIR tls stream method"));
    assert_eq!(
        tls_runtime
            .evaluate_tls_stream_method(tls_client, "close", &[], &mut tls_env)
            .expect("tls client close should succeed"),
        Value::Unit
    );
    tls_server.join().expect("tls server should join");
    tls_listener.close();
    let _ = std::fs::remove_file(&cert_path);
    let _ = std::fs::remove_file(&key_path);
}

#[test]
fn mir_runtime_resource_member_helpers_cover_io_process_and_network_paths() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(b"-bytes".to_vec()),
    );

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let read_path = std::env::temp_dir().join(format!(
        "aura-mir-runtime-read-{timestamp}-{}.txt",
        std::process::id()
    ));
    let write_path = std::env::temp_dir().join(format!(
        "aura-mir-runtime-write-{timestamp}-{}.txt",
        std::process::id()
    ));
    std::fs::write(&read_path, "hello").expect("read fixture should be written");

    let read_file = FileValue::open(read_path.to_str().expect("temp path should be utf-8"))
        .expect("read fixture should open");
    let file_text = runtime
        .evaluate_file_method(read_file.clone(), "read_all", &[], &mut env)
        .expect("file read_all should succeed");
    assert_eq!(
        result_ok_payload(file_text),
        Value::String("hello".to_string())
    );
    read_file.close();

    let read_file = FileValue::open(read_path.to_str().expect("temp path should be utf-8"))
        .expect("read fixture should reopen");
    let file_bytes = runtime
        .evaluate_file_method(read_file.clone(), "read_bytes", &[], &mut env)
        .expect("file read_bytes should succeed");
    assert_eq!(
        result_ok_payload(file_bytes),
        bytes_vec_value(b"hello".to_vec())
    );
    read_file.close();

    let write_file = FileValue::create(write_path.to_str().expect("temp path should be utf-8"))
        .expect("write fixture should open");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_file_method(
                    write_file.clone(),
                    "write_all",
                    &[mir_arg(
                        Some("text"),
                        Operand::String("written".to_string())
                    )],
                    &mut env,
                )
                .expect("file write_all should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_file_method(
                    write_file.clone(),
                    "write_bytes",
                    &[mir_arg(Some("bytes"), Operand::Place("bytes".to_string()))],
                    &mut env,
                )
                .expect("file write_bytes should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_file_method(write_file.clone(), "flush", &[], &mut env)
                .expect("file flush should succeed")
        ),
        Value::Unit
    );
    let bad_file_write = runtime
        .evaluate_file_method(
            write_file.clone(),
            "write_all",
            &[mir_arg(Some("text"), Operand::Bool(true))],
            &mut env,
        )
        .expect_err("file write_all should reject non-string text");
    assert!(bad_file_write.message.contains("expects `str`"));
    assert_eq!(
        runtime
            .evaluate_file_method(write_file.clone(), "close", &[], &mut env)
            .expect("file close should succeed"),
        Value::Unit
    );
    let missing_file_method = runtime
        .evaluate_file_method(write_file, "missing", &[], &mut env)
        .expect_err("unknown file method should fail");
    assert!(missing_file_method
        .message
        .contains("unsupported MIR file method"));
    assert_eq!(
        std::fs::read_to_string(&write_path).expect("write fixture should be readable"),
        "written-bytes"
    );
    let _ = std::fs::remove_file(&read_path);
    let _ = std::fs::remove_file(&write_path);

    let completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(0))],
        }),
        b"out".to_vec(),
        b"err".to_vec(),
    );
    enum_payloads(
        runtime
            .evaluate_process_completed_method(completed.clone(), "status", &[], &mut env)
            .expect("completed status should succeed"),
        "ExitStatus",
        "Exited",
    );
    assert_eq!(
        runtime
            .evaluate_process_completed_method(completed.clone(), "success", &[], &mut env)
            .expect("completed success should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        runtime
            .evaluate_process_completed_method(completed.clone(), "stdout", &[], &mut env)
            .expect("completed stdout should succeed"),
        Value::String("out".to_string())
    );
    assert_eq!(
        runtime
            .evaluate_process_completed_method(completed.clone(), "stderr_bytes", &[], &mut env)
            .expect("completed stderr bytes should succeed"),
        bytes_vec_value(b"err".to_vec())
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_completed_method(completed.clone(), "check", &[], &mut env)
                .expect("completed check should succeed")
        ),
        Value::Unit
    );
    let bad_completed_method = runtime
        .evaluate_process_completed_method(completed, "missing", &[], &mut env)
        .expect_err("unknown completed method should fail");
    assert!(bad_completed_method
        .message
        .contains("unsupported MIR process completed method"));

    let child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf out".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        false,
    )
    .expect("process child should spawn");
    enum_payloads(
        runtime
            .evaluate_process_child_method(child.clone(), "stdin", &[], &mut env)
            .expect("child stdin method should succeed"),
        "Option",
        "None",
    );
    enum_payloads(
        runtime
            .evaluate_process_child_method(child.clone(), "stdout", &[], &mut env)
            .expect("child stdout method should succeed"),
        "Option",
        "Some",
    );
    enum_payloads(
        runtime
            .evaluate_process_child_method(child.clone(), "stderr", &[], &mut env)
            .expect("child stderr method should succeed"),
        "Option",
        "Some",
    );
    let stdout_pipe = child.stdout().expect("child stdout should be piped");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(stdout_pipe, "read_all", &[], &mut env)
                .expect("pipe read_all should succeed")
        ),
        Value::String("out".to_string())
    );
    enum_payloads(
        runtime
            .evaluate_process_child_method(child.clone(), "wait", &[], &mut env)
            .expect("child wait should succeed"),
        "Wait",
        "Exited",
    );

    let ok_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 0".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("process child should spawn");
    enum_payloads(
        result_ok_payload(
            runtime
                .evaluate_process_child_method(ok_child, "wait_ok", &[], &mut env)
                .expect("wait_ok should succeed"),
        ),
        "ExitStatus",
        "Exited",
    );

    let cat = ProcessChildValue::spawn(
        vec!["/bin/cat".to_string()],
        None,
        Vec::new(),
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("cat should spawn");
    let stdin_pipe = cat.stdin().expect("cat stdin should be piped");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(
                    stdin_pipe.clone(),
                    "write_all",
                    &[mir_arg(Some("text"), Operand::String("cat".to_string()))],
                    &mut env,
                )
                .expect("pipe write_all should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(stdin_pipe.clone(), "flush", &[], &mut env)
                .expect("pipe flush should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_process_pipe_method(stdin_pipe, "close", &[], &mut env)
            .expect("pipe close should succeed"),
        Value::Unit
    );
    let cat_stdout = cat.stdout().expect("cat stdout should be piped");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(cat_stdout, "read_all", &[], &mut env)
                .expect("cat stdout should be readable")
        ),
        Value::String("cat".to_string())
    );
    enum_payloads(
        runtime
            .evaluate_process_child_method(cat, "wait", &[], &mut env)
            .expect("cat wait should succeed"),
        "Wait",
        "Exited",
    );

    let cat_bytes = ProcessChildValue::spawn(
        vec!["/bin/cat".to_string()],
        None,
        Vec::new(),
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("byte cat should spawn");
    let bytes_stdin = cat_bytes.stdin().expect("byte cat stdin should be piped");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(
                    bytes_stdin.clone(),
                    "write_bytes",
                    &[
                        mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("pipe write_bytes should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_process_pipe_method(bytes_stdin, "close", &[], &mut env)
            .expect("byte cat stdin close should succeed"),
        Value::Unit
    );
    let bytes_stdout = cat_bytes.stdout().expect("byte cat stdout should be piped");
    let byte_read = result_ok_payload(
        runtime
            .evaluate_process_pipe_method(
                bytes_stdout,
                "read_bytes",
                &[
                    mir_arg(Some("max_bytes"), Operand::Int(6)),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("pipe read_bytes should succeed"),
    );
    let byte_payload = enum_payloads(byte_read, "Option", "Some");
    assert_eq!(byte_payload, vec![bytes_vec_value(b"-bytes".to_vec())]);
    enum_payloads(
        runtime
            .evaluate_process_child_method(cat_bytes, "wait", &[], &mut env)
            .expect("byte cat wait should succeed"),
        "Wait",
        "Exited",
    );

    let supervisor = ProcessSupervisorValue::new();
    assert_eq!(
        runtime
            .evaluate_process_supervisor_method(supervisor.clone(), "is_empty", &[], &mut env)
            .expect("supervisor is_empty should succeed"),
        Value::Bool(true)
    );
    enum_payloads(
        runtime
            .evaluate_process_supervisor_method(supervisor.clone(), "wait", &[], &mut env)
            .expect("empty supervisor wait should time out immediately"),
        "SupervisorWait",
        "TimedOut",
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(supervisor.clone(), "stop", &[], &mut env)
                .expect("empty supervisor stop should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_process_supervisor_method(supervisor.clone(), "close", &[], &mut env)
            .expect("supervisor close should succeed"),
        Value::Unit
    );
    let supervisor_error = runtime
        .evaluate_process_supervisor_method(supervisor, "missing", &[], &mut env)
        .expect_err("unknown supervisor method should fail");
    assert!(supervisor_error
        .message
        .contains("unsupported MIR process supervisor method"));

    let tcp_listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp listener should bind");
    match result_ok_payload(
        runtime
            .evaluate_tcp_listener_method(tcp_listener.clone(), "local_addr", &[], &mut env)
            .expect("tcp listener local_addr should succeed"),
    ) {
        Value::String(address) => assert!(address.starts_with("127.0.0.1:")),
        other => panic!("expected tcp local address string, found {other:?}"),
    }
    enum_payloads(
        runtime
            .evaluate_tcp_listener_method(
                tcp_listener.clone(),
                "accept",
                &[mir_arg(Some("timeout"), Operand::Duration(1_000_000))],
                &mut env,
            )
            .expect("tcp listener accept should return a Result"),
        "Result",
        "Err",
    );
    assert_eq!(
        runtime
            .evaluate_tcp_listener_method(tcp_listener.clone(), "close", &[], &mut env)
            .expect("tcp listener close should succeed"),
        Value::Unit
    );
    let tcp_listener_error = runtime
        .evaluate_tcp_listener_method(tcp_listener, "missing", &[], &mut env)
        .expect_err("unknown tcp listener method should fail");
    assert!(tcp_listener_error
        .message
        .contains("unsupported MIR tcp listener method"));

    let udp_receiver = UdpSocketValue::bind("127.0.0.1:0").expect("udp receiver should bind");
    let udp_address = udp_receiver
        .local_addr()
        .expect("udp receiver address should be available");
    let udp_sender = UdpSocketValue::bind("127.0.0.1:0").expect("udp sender should bind");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_udp_socket_method(
                    udp_sender.clone(),
                    "send_text",
                    &[
                        mir_arg(Some("address"), Operand::String(udp_address.clone())),
                        mir_arg(Some("text"), Operand::String("ping".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("udp send_text should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_udp_socket_method(
                    udp_sender.clone(),
                    "send_bytes",
                    &[
                        mir_arg(Some("address"), Operand::String(udp_address.clone())),
                        mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("udp send_bytes should succeed")
        ),
        Value::Unit
    );
    let udp_recv = result_ok_payload(
        runtime
            .evaluate_udp_socket_method(
                udp_receiver.clone(),
                "recv",
                &[
                    mir_arg(Some("max_bytes"), Operand::Int(16)),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("udp recv should succeed"),
    );
    enum_payloads(udp_recv, "Option", "Some");
    let udp_recv_from = result_ok_payload(
        runtime
            .evaluate_udp_socket_method(
                udp_receiver.clone(),
                "recv_from",
                &[
                    mir_arg(Some("max_bytes"), Operand::Int(16)),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("udp recv_from should succeed"),
    );
    enum_payloads(udp_recv_from, "Option", "Some");
    match result_ok_payload(
        runtime
            .evaluate_udp_socket_method(udp_receiver.clone(), "local_addr", &[], &mut env)
            .expect("udp local_addr should succeed"),
    ) {
        Value::String(address) => assert!(address.starts_with("127.0.0.1:")),
        other => panic!("expected udp local address string, found {other:?}"),
    }
    enum_payloads(
        runtime
            .evaluate_udp_socket_method(udp_sender.clone(), "peer_addr", &[], &mut env)
            .expect("udp peer_addr should return a Result"),
        "Result",
        "Err",
    );
    assert_eq!(
        runtime
            .evaluate_udp_socket_method(udp_sender.clone(), "close", &[], &mut env)
            .expect("udp close should succeed"),
        Value::Unit
    );
    let udp_error = runtime
        .evaluate_udp_socket_method(udp_sender, "missing", &[], &mut env)
        .expect_err("unknown udp socket method should fail");
    assert!(udp_error
        .message
        .contains("unsupported MIR udp socket method"));
    udp_receiver.close();

    let datagram = UdpDatagramValue {
        address: "127.0.0.1:9".to_string(),
        data: b"text".to_vec(),
    };
    assert_eq!(
        runtime
            .evaluate_udp_datagram_method(datagram.clone(), "address", &[], &mut env)
            .expect("udp datagram address should succeed"),
        Value::String("127.0.0.1:9".to_string())
    );
    assert_eq!(
        runtime
            .evaluate_udp_datagram_method(datagram.clone(), "bytes", &[], &mut env)
            .expect("udp datagram bytes should succeed"),
        bytes_vec_value(b"text".to_vec())
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_udp_datagram_method(datagram.clone(), "text", &[], &mut env)
                .expect("udp datagram text should succeed")
        ),
        Value::String("text".to_string())
    );
    let datagram_error = runtime
        .evaluate_udp_datagram_method(datagram, "missing", &[], &mut env)
        .expect_err("unknown datagram method should fail");
    assert!(datagram_error
        .message
        .contains("unsupported MIR udp datagram method"));

    let http_listener = HttpListenerValue::bind("127.0.0.1:0").expect("http listener should bind");
    match result_ok_payload(
        runtime
            .evaluate_http_listener_method(http_listener.clone(), "local_addr", &[], &mut env)
            .expect("http listener local_addr should succeed"),
    ) {
        Value::String(address) => assert!(address.starts_with("127.0.0.1:")),
        other => panic!("expected http local address string, found {other:?}"),
    }
    enum_payloads(
        runtime
            .evaluate_http_listener_method(
                http_listener.clone(),
                "accept",
                &[mir_arg(Some("timeout"), Operand::Duration(1_000_000))],
                &mut env,
            )
            .expect("http listener accept should return a Result"),
        "Result",
        "Err",
    );
    assert_eq!(
        runtime
            .evaluate_http_listener_method(http_listener.clone(), "close", &[], &mut env)
            .expect("http listener close should succeed"),
        Value::Unit
    );
    let http_listener_error = runtime
        .evaluate_http_listener_method(http_listener, "missing", &[], &mut env)
        .expect_err("unknown http listener method should fail");
    assert!(http_listener_error
        .message
        .contains("unsupported MIR http listener method"));
}

#[test]
fn mir_runtime_network_member_helpers_cover_closed_and_validation_edges() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(b"payload".to_vec()),
    );
    env.define_typed(
        "negative",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(-1)),
    );

    let tcp_listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp listener should bind");
    let tcp_address = tcp_listener
        .local_addr()
        .expect("tcp listener address should be available");
    let tcp_server = {
        let listener = tcp_listener.clone();
        thread::spawn(move || {
            listener
                .accept(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("tcp server should accept")
        })
    };
    let tcp_client = TcpStreamValue::connect(
        &tcp_address,
        Some(StdDuration::from_secs(5)),
        Some(&CancellationContext::default()),
    )
    .expect("tcp client should connect");
    let tcp_server_stream = tcp_server.join().expect("tcp server should join");
    tcp_client.close();
    tcp_server_stream.close();
    for method in [
        "read_all",
        "read_line",
        "flush",
        "local_addr",
        "peer_addr",
        "shutdown_read",
        "shutdown_write",
        "shutdown_both",
    ] {
        assert_result_err(
            runtime
                .evaluate_tcp_stream_method(tcp_client.clone(), method, &[], &mut env)
                .expect("closed tcp stream methods should return Result.Err"),
        );
    }
    for (method, args) in [
        (
            "read_bytes",
            vec![
                mir_arg(Some("max_bytes"), Operand::Int(4)),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
        ),
        (
            "read_exact",
            vec![
                mir_arg(Some("count"), Operand::Int(4)),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
        ),
        (
            "write_bytes",
            vec![
                mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
        ),
    ] {
        assert_result_err(
            runtime
                .evaluate_tcp_stream_method(tcp_client.clone(), method, &args, &mut env)
                .expect("closed tcp stream method should return Result.Err"),
        );
    }
    let negative_tcp_read = runtime
        .evaluate_tcp_stream_method(
            tcp_client.clone(),
            "read_bytes",
            &[
                mir_arg(Some("max_bytes"), Operand::Place("negative".to_string())),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
            &mut env,
        )
        .expect_err("negative tcp read size should fail before IO");
    assert!(negative_tcp_read
        .message
        .contains("requires a non-negative max_bytes"));
    let bad_tcp_write = runtime
        .evaluate_tcp_stream_method(
            tcp_client.clone(),
            "write_all",
            &[
                mir_arg(Some("text"), Operand::Bool(true)),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
            &mut env,
        )
        .expect_err("tcp write_all should reject non-string input");
    assert!(bad_tcp_write.message.contains("expects `str`"));

    tcp_listener.close();
    assert_result_err(
        runtime
            .evaluate_tcp_listener_method(tcp_listener.clone(), "local_addr", &[], &mut env)
            .expect("closed tcp listener local_addr should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_tcp_listener_method(
                tcp_listener,
                "accept",
                &[mir_arg(Some("timeout"), Operand::Unit)],
                &mut env,
            )
            .expect("closed tcp listener accept should return Result.Err"),
    );

    let udp_socket = UdpSocketValue::bind("127.0.0.1:0").expect("udp socket should bind");
    let negative_udp_recv = runtime
        .evaluate_udp_socket_method(
            udp_socket.clone(),
            "recv",
            &[
                mir_arg(Some("max_bytes"), Operand::Place("negative".to_string())),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
            &mut env,
        )
        .expect_err("negative udp recv size should fail before IO");
    assert!(negative_udp_recv
        .message
        .contains("requires a non-negative max_bytes"));
    let negative_udp_recv_from = runtime
        .evaluate_udp_socket_method(
            udp_socket.clone(),
            "recv_from",
            &[
                mir_arg(Some("max_bytes"), Operand::Place("negative".to_string())),
                mir_arg(Some("timeout"), Operand::Unit),
            ],
            &mut env,
        )
        .expect_err("negative udp recv_from size should fail before IO");
    assert!(negative_udp_recv_from
        .message
        .contains("requires a non-negative max_bytes"));
    udp_socket.close();
    assert_result_err(
        runtime
            .evaluate_udp_socket_method(udp_socket.clone(), "local_addr", &[], &mut env)
            .expect("closed udp local_addr should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_udp_socket_method(udp_socket.clone(), "peer_addr", &[], &mut env)
            .expect("closed udp peer_addr should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_udp_socket_method(
                udp_socket,
                "send_bytes",
                &[
                    mir_arg(Some("address"), Operand::String("127.0.0.1:9".to_string())),
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                    mir_arg(Some("timeout"), Operand::Unit),
                ],
                &mut env,
            )
            .expect("closed udp send_bytes should return Result.Err"),
    );

    let invalid_datagram = UdpDatagramValue {
        address: "127.0.0.1:9".to_string(),
        data: vec![0xff, 0xfe],
    };
    assert_result_err(
        runtime
            .evaluate_udp_datagram_method(invalid_datagram, "text", &[], &mut env)
            .expect("invalid utf-8 datagram text should return Result.Err"),
    );

    let http_listener = HttpListenerValue::bind("127.0.0.1:0").expect("http listener should bind");
    http_listener.close();
    assert_result_err(
        runtime
            .evaluate_http_listener_method(http_listener.clone(), "local_addr", &[], &mut env)
            .expect("closed http listener local_addr should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_http_listener_method(
                http_listener,
                "accept",
                &[mir_arg(Some("timeout"), Operand::Unit)],
                &mut env,
            )
            .expect("closed http listener accept should return Result.Err"),
    );

    #[cfg(unix)]
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let socket_path =
            std::path::PathBuf::from(format!("/tmp/aum{}-{timestamp}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);
        let socket_text = socket_path.to_string_lossy().into_owned();
        let unix_listener =
            UnixListenerValue::bind(&socket_text).expect("unix listener should bind");
        let unix_server = {
            let listener = unix_listener.clone();
            thread::spawn(move || {
                listener
                    .accept(
                        Some(StdDuration::from_secs(5)),
                        Some(&CancellationContext::default()),
                    )
                    .expect("unix server should accept")
            })
        };
        let unix_client = UnixStreamValue::connect(
            &socket_text,
            Some(StdDuration::from_secs(5)),
            Some(&CancellationContext::default()),
        )
        .expect("unix client should connect");
        let unix_server_stream = unix_server.join().expect("unix server should join");
        unix_client.close();
        unix_server_stream.close();
        assert_result_err(
            runtime
                .evaluate_unix_stream_method(
                    unix_client.clone(),
                    "read_line",
                    &[mir_arg(Some("timeout"), Operand::Unit)],
                    &mut env,
                )
                .expect("closed unix read_line should return Result.Err"),
        );
        assert_result_err(
            runtime
                .evaluate_unix_stream_method(
                    unix_client.clone(),
                    "read_exact",
                    &[
                        mir_arg(Some("count"), Operand::Int(4)),
                        mir_arg(Some("timeout"), Operand::Unit),
                    ],
                    &mut env,
                )
                .expect("closed unix read_exact should return Result.Err"),
        );
        assert_result_err(
            runtime
                .evaluate_unix_stream_method(
                    unix_client.clone(),
                    "write_all",
                    &[
                        mir_arg(Some("text"), Operand::String("closed".to_string())),
                        mir_arg(Some("timeout"), Operand::Unit),
                    ],
                    &mut env,
                )
                .expect("closed unix write_all should return Result.Err"),
        );
        let negative_unix_read = runtime
            .evaluate_unix_stream_method(
                unix_client,
                "read_exact",
                &[
                    mir_arg(Some("count"), Operand::Place("negative".to_string())),
                    mir_arg(Some("timeout"), Operand::Unit),
                ],
                &mut env,
            )
            .expect_err("negative unix read_exact size should fail before IO");
        assert!(negative_unix_read
            .message
            .contains("requires a non-negative count"));
        unix_listener.close();
        assert_result_err(
            runtime
                .evaluate_unix_listener_method(
                    unix_listener,
                    "accept",
                    &[mir_arg(Some("timeout"), Operand::Unit)],
                    &mut env,
                )
                .expect("closed unix listener accept should return Result.Err"),
        );
        let _ = std::fs::remove_file(&socket_path);
    }
}

#[test]
fn mir_runtime_stream_and_http_member_helpers_cover_resource_branches() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(b"client-bytes".to_vec()),
    );

    let tcp_listener = TcpListenerValue::bind("127.0.0.1:0").expect("tcp listener should bind");
    let tcp_address = tcp_listener
        .local_addr()
        .expect("tcp listener address should be available");
    let tcp_server = {
        let listener = tcp_listener.clone();
        thread::spawn(move || {
            let stream = listener
                .accept(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("tcp server should accept");
            let request = stream
                .read_line(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("tcp server should read a line");
            assert_eq!(request.as_deref(), Some("ping"));
            stream
                .write_all(
                    "pong\nextra",
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("tcp server should write response");
            stream.flush().expect("tcp server flush should succeed");
            stream.close();
        })
    };
    let tcp_client = TcpStreamValue::connect(
        &tcp_address,
        Some(StdDuration::from_secs(5)),
        Some(&CancellationContext::default()),
    )
    .expect("tcp client should connect");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_tcp_stream_method(
                    tcp_client.clone(),
                    "write_all",
                    &[
                        mir_arg(Some("text"), Operand::String("ping\n".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(5_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("tcp write_all should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_tcp_stream_method(tcp_client.clone(), "flush", &[], &mut env)
                .expect("tcp flush should succeed")
        ),
        Value::Unit
    );
    // Inspect both endpoints while the peer is guaranteed to remain connected. Once the client
    // shuts down its write side, the server may finish and close before another socket query.
    match result_ok_payload(
        runtime
            .evaluate_tcp_stream_method(tcp_client.clone(), "local_addr", &[], &mut env)
            .expect("tcp local_addr should succeed"),
    ) {
        Value::String(address) => assert!(address.starts_with("127.0.0.1:")),
        other => panic!("expected tcp local address string, found {other:?}"),
    }
    match result_ok_payload(
        runtime
            .evaluate_tcp_stream_method(tcp_client.clone(), "peer_addr", &[], &mut env)
            .expect("tcp peer_addr should succeed"),
    ) {
        Value::String(address) => assert_eq!(address, tcp_address),
        other => panic!("expected tcp peer address string, found {other:?}"),
    }
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_tcp_stream_method(tcp_client.clone(), "shutdown_write", &[], &mut env)
                .expect("tcp shutdown_write should succeed")
        ),
        Value::Unit
    );
    let tcp_line = result_ok_payload(
        runtime
            .evaluate_tcp_stream_method(
                tcp_client.clone(),
                "read_line",
                &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                &mut env,
            )
            .expect("tcp read_line should succeed"),
    );
    let line_payloads = enum_payloads(tcp_line, "Option", "Some");
    assert_eq!(line_payloads, vec![Value::String("pong".to_string())]);
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_tcp_stream_method(
                    tcp_client.clone(),
                    "read_exact",
                    &[
                        mir_arg(Some("count"), Operand::Int(5)),
                        mir_arg(Some("timeout"), Operand::Duration(5_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("tcp read_exact should succeed")
        ),
        bytes_vec_value(b"extra".to_vec())
    );
    let no_more_tcp = result_ok_payload(
        runtime
            .evaluate_tcp_stream_method(
                tcp_client.clone(),
                "read_bytes",
                &[
                    mir_arg(Some("max_bytes"), Operand::Int(4)),
                    mir_arg(Some("timeout"), Operand::Duration(50_000_000)),
                ],
                &mut env,
            )
            .expect("tcp read_bytes should return a Result"),
    );
    enum_payloads(no_more_tcp, "Option", "None");
    assert_eq!(
        runtime
            .evaluate_tcp_stream_method(tcp_client.clone(), "close", &[], &mut env)
            .expect("tcp close should succeed"),
        Value::Unit
    );
    let tcp_error = runtime
        .evaluate_tcp_stream_method(tcp_client, "missing", &[], &mut env)
        .expect_err("unknown tcp stream method should fail");
    assert!(tcp_error
        .message
        .contains("unsupported MIR tcp stream method"));
    tcp_server.join().expect("tcp server should join");
    tcp_listener.close();

    #[cfg(unix)]
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let socket_path = std::env::temp_dir().join(format!(
            "aura-mir-runtime-unix-{}-{timestamp}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket_path);
        let socket_text = socket_path.to_string_lossy().into_owned();
        let unix_listener =
            UnixListenerValue::bind(&socket_text).expect("unix listener should bind");
        let unix_server = {
            let listener = unix_listener.clone();
            thread::spawn(move || {
                let stream = listener
                    .accept(
                        Some(StdDuration::from_secs(5)),
                        Some(&CancellationContext::default()),
                    )
                    .expect("unix server should accept");
                stream
                    .write_all(
                        "exact",
                        Some(StdDuration::from_secs(5)),
                        Some(&CancellationContext::default()),
                    )
                    .expect("unix server should write");
                stream.close();
            })
        };
        let unix_client = UnixStreamValue::connect(
            &socket_text,
            Some(StdDuration::from_secs(5)),
            Some(&CancellationContext::default()),
        )
        .expect("unix client should connect");
        assert_eq!(
            result_ok_payload(
                runtime
                    .evaluate_unix_stream_method(
                        unix_client.clone(),
                        "read_exact",
                        &[
                            mir_arg(Some("count"), Operand::Int(5)),
                            mir_arg(Some("timeout"), Operand::Duration(5_000_000_000)),
                        ],
                        &mut env,
                    )
                    .expect("unix read_exact should succeed")
            ),
            bytes_vec_value(b"exact".to_vec())
        );
        assert_eq!(
            runtime
                .evaluate_unix_stream_method(unix_client.clone(), "close", &[], &mut env)
                .expect("unix stream close should succeed"),
            Value::Unit
        );
        unix_server.join().expect("unix server should join");
        unix_listener.close();
        let _ = std::fs::remove_file(&socket_path);
    }

    let websocket_listener =
        WebSocketListenerValue::bind("127.0.0.1:0").expect("websocket listener should bind");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::WebSocketListener(websocket_listener.clone())),
        Some(Type::named("net.WebSocketListener"))
    );
    let websocket_address = websocket_listener
        .local_addr()
        .expect("websocket listener address should be available");
    let websocket_server = {
        let listener = websocket_listener.clone();
        thread::spawn(move || {
            let socket = listener
                .accept(Some(StdDuration::from_secs(5)))
                .expect("websocket server should accept");
            let mut server_runtime = test_runtime();
            let mut server_env = Env::default();
            server_env.define_typed(
                "bytes",
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
                bytes_vec_value(b"server-bytes".to_vec()),
            );
            let client_text = result_ok_payload(
                server_runtime
                    .evaluate_websocket_method(
                        socket.clone(),
                        "recv_text",
                        &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                        &mut server_env,
                    )
                    .expect("websocket recv_text should succeed"),
            );
            assert_eq!(
                enum_payloads(client_text, "Option", "Some"),
                vec![Value::String("hello websocket".to_string())]
            );
            assert_eq!(
                result_ok_payload(
                    server_runtime
                        .evaluate_websocket_method(
                            socket.clone(),
                            "send_bytes",
                            &[
                                mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                                mir_arg(Some("timeout"), Operand::Duration(5_000_000_000),),
                            ],
                            &mut server_env,
                        )
                        .expect("websocket send_bytes should succeed")
                ),
                Value::Unit
            );
            let client_bytes = result_ok_payload(
                server_runtime
                    .evaluate_websocket_method(
                        socket.clone(),
                        "recv_bytes",
                        &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                        &mut server_env,
                    )
                    .expect("websocket recv_bytes should succeed"),
            );
            assert_eq!(
                enum_payloads(client_bytes, "Option", "Some"),
                vec![bytes_vec_value(b"client-bytes".to_vec())]
            );
            assert_eq!(
                result_ok_payload(
                    server_runtime
                        .evaluate_websocket_method(
                            socket.clone(),
                            "send_text",
                            &[
                                mir_arg(Some("text"), Operand::String("server-done".to_string())),
                                mir_arg(Some("timeout"), Operand::Duration(5_000_000_000),),
                            ],
                            &mut server_env,
                        )
                        .expect("websocket send_text should succeed")
                ),
                Value::Unit
            );
            assert_eq!(
                server_runtime
                    .evaluate_websocket_method(socket, "close", &[], &mut server_env)
                    .expect("websocket close should succeed"),
                Value::Unit
            );
        })
    };
    let websocket_client = WebSocketValue::connect(
        &format!("ws://{websocket_address}"),
        Some(StdDuration::from_secs(5)),
    )
    .expect("websocket client should connect");
    assert_eq!(
        MirRuntime::infer_value_type(&Value::WebSocket(websocket_client.clone())),
        Some(Type::named("net.WebSocket"))
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_websocket_method(
                    websocket_client.clone(),
                    "send_text",
                    &[
                        mir_arg(Some("text"), Operand::String("hello websocket".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(5_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("websocket client send_text should succeed")
        ),
        Value::Unit
    );
    let server_bytes = result_ok_payload(
        runtime
            .evaluate_websocket_method(
                websocket_client.clone(),
                "recv_bytes",
                &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                &mut env,
            )
            .expect("websocket client recv_bytes should succeed"),
    );
    assert_eq!(
        enum_payloads(server_bytes, "Option", "Some"),
        vec![bytes_vec_value(b"server-bytes".to_vec())]
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_websocket_method(
                    websocket_client.clone(),
                    "send_bytes",
                    &[
                        mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                        mir_arg(Some("timeout"), Operand::Duration(5_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("websocket client send_bytes should succeed")
        ),
        Value::Unit
    );
    let done = result_ok_payload(
        runtime
            .evaluate_websocket_method(
                websocket_client.clone(),
                "recv_text",
                &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                &mut env,
            )
            .expect("websocket client recv_text should succeed"),
    );
    assert_eq!(
        enum_payloads(done, "Option", "Some"),
        vec![Value::String("server-done".to_string())]
    );
    assert_eq!(
        runtime
            .evaluate_websocket_method(websocket_client, "close", &[], &mut env)
            .expect("websocket client close should succeed"),
        Value::Unit
    );
    websocket_server
        .join()
        .expect("websocket server should join");

    let http_listener = HttpListenerValue::bind("127.0.0.1:0").expect("http listener should bind");
    let http_address = http_listener
        .local_addr()
        .expect("http listener address should be available");
    let http_server = {
        let listener = http_listener.clone();
        thread::spawn(move || {
            let mut server_runtime = test_runtime();
            let mut server_env = Env::default();
            server_env.define_typed(
                "headers",
                Type::Named(
                    "dict".to_string(),
                    vec![Type::named("str"), Type::named("str")],
                ),
                Value::Map(crate::runtime_value::MapValue {
                    key_type: Type::named("str"),
                    value_type: Type::named("str"),
                    entries: vec![(
                        Value::String("Content-Type".to_string()),
                        Value::String("text/plain".to_string()),
                    )],
                }),
            );
            server_env.define_typed(
                "bytes",
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
                bytes_vec_value(b"bytes-reply".to_vec()),
            );
            let exchange = listener
                .accept(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("http server should accept");
            assert_eq!(
                server_runtime
                    .evaluate_http_exchange_method(exchange.clone(), "method", &[], &mut server_env)
                    .expect("http method should succeed"),
                Value::String("POST".to_string())
            );
            assert_eq!(
                server_runtime
                    .evaluate_http_exchange_method(exchange.clone(), "path", &[], &mut server_env)
                    .expect("http path should succeed"),
                Value::String("/demo".to_string())
            );
            match server_runtime
                .evaluate_http_exchange_method(exchange.clone(), "headers", &[], &mut server_env)
                .expect("http headers should succeed")
            {
                Value::Map(headers) => assert!(!headers.entries.is_empty()),
                other => panic!("expected header map, found {other:?}"),
            }
            assert_eq!(
                result_ok_payload(
                    server_runtime
                        .evaluate_http_exchange_method(
                            exchange.clone(),
                            "body_text",
                            &[],
                            &mut server_env,
                        )
                        .expect("http body_text should succeed")
                ),
                Value::String("body".to_string())
            );
            assert_eq!(
                server_runtime
                    .evaluate_http_exchange_method(
                        exchange.clone(),
                        "body_bytes",
                        &[],
                        &mut server_env
                    )
                    .expect("http body_bytes should succeed"),
                bytes_vec_value(b"body".to_vec())
            );
            assert_eq!(
                result_ok_payload(
                    server_runtime
                        .evaluate_http_exchange_method(
                            exchange.clone(),
                            "respond_text",
                            &[
                                mir_arg(Some("status"), Operand::Int(200)),
                                mir_arg(Some("text"), Operand::String("reply".to_string())),
                                mir_arg(Some("headers"), Operand::Place("headers".to_string())),
                            ],
                            &mut server_env,
                        )
                        .expect("http respond_text should succeed")
                ),
                Value::Unit
            );
            let exchange_error = server_runtime
                .evaluate_http_exchange_method(exchange, "missing", &[], &mut server_env)
                .expect_err("unknown exchange method should fail");
            assert!(exchange_error
                .message
                .contains("unsupported MIR http exchange method"));

            let exchange = listener
                .accept(
                    Some(StdDuration::from_secs(5)),
                    Some(&CancellationContext::default()),
                )
                .expect("http server should accept a bytes request");
            assert_eq!(
                server_runtime
                    .evaluate_http_exchange_method(
                        exchange.clone(),
                        "body_bytes",
                        &[],
                        &mut server_env
                    )
                    .expect("http body_bytes should succeed"),
                bytes_vec_value(b"bytes".to_vec())
            );
            assert_eq!(
                result_ok_payload(
                    server_runtime
                        .evaluate_http_exchange_method(
                            exchange,
                            "respond_bytes",
                            &[
                                mir_arg(Some("status"), Operand::Int(200)),
                                mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                                mir_arg(Some("headers"), Operand::Place("headers".to_string())),
                            ],
                            &mut server_env,
                        )
                        .expect("respond_bytes should succeed")
                ),
                Value::Unit
            );
        })
    };
    let response = HttpResponseValue::request_text(
        "POST",
        &format!("http://{http_address}/demo"),
        "body",
        vec![("Content-Type".to_string(), "text/plain".to_string())],
        Some(StdDuration::from_secs(5)),
        Some(&CancellationContext::default()),
    )
    .expect("http request should succeed");
    assert_eq!(
        runtime
            .evaluate_http_response_method(response.clone(), "status", &[], &mut env)
            .expect("http response status should succeed"),
        Value::Int(IntegerValue::from_signed(200))
    );
    assert_eq!(
        runtime
            .evaluate_http_response_method(response.clone(), "reason", &[], &mut env)
            .expect("http response reason should succeed"),
        Value::String("OK".to_string())
    );
    match runtime
        .evaluate_http_response_method(response.clone(), "headers", &[], &mut env)
        .expect("http response headers should succeed")
    {
        Value::Map(headers) => assert!(!headers.entries.is_empty()),
        other => panic!("expected response header map, found {other:?}"),
    }
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_http_response_method(response.clone(), "text", &[], &mut env)
                .expect("http response text should succeed")
        ),
        Value::String("reply".to_string())
    );
    assert_eq!(
        runtime
            .evaluate_http_response_method(response.clone(), "bytes", &[], &mut env)
            .expect("http response bytes should succeed"),
        bytes_vec_value(b"reply".to_vec())
    );
    let bytes_response = HttpResponseValue::request_bytes(
        "POST",
        &format!("http://{http_address}/demo-bytes"),
        b"bytes",
        vec![(
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        )],
        Some(StdDuration::from_secs(5)),
        Some(&CancellationContext::default()),
    )
    .expect("http bytes request should succeed");
    assert_eq!(
        runtime
            .evaluate_http_response_method(bytes_response, "bytes", &[], &mut env)
            .expect("http bytes response should succeed"),
        bytes_vec_value(b"bytes-reply".to_vec())
    );
    let response_error = runtime
        .evaluate_http_response_method(response, "missing", &[], &mut env)
        .expect_err("unknown response method should fail");
    assert!(response_error
        .message
        .contains("unsupported MIR http response method"));
    http_server.join().expect("http server should join");
    http_listener.close();
}

struct FlushFailWriter;

impl Write for FlushFailWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"))
    }
}

struct WriteFailWriter {
    kind: io::ErrorKind,
}

impl Write for WriteFailWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(self.kind, "write failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn mir_runtime_stream_and_entrypoint_helpers_cover_success_and_error_paths() {
    let flush_error =
        write_stream(&mut FlushFailWriter, "aura").expect_err("flush failures should be surfaced");
    assert_eq!(flush_error.kind(), io::ErrorKind::BrokenPipe);

    let source = "def main() -> int32:\n    return 3\n";
    let mir = crate::lower_source_to_mir(source).expect("source should lower to MIR");
    let mir_json = serde_json::to_vec(&mir).expect("MIR should serialize");
    let source_path = b"/tmp/runtime_entry.au";
    let code = run_native_entry(
        mir_json.as_ptr(),
        mir_json.len(),
        source_path.as_ptr(),
        source_path.len(),
        source.as_ptr(),
        source.len(),
    );
    assert_eq!(code, 3);

    let invalid_json = b"not-json";
    let invalid_code = run_native_entry(
        invalid_json.as_ptr(),
        invalid_json.len(),
        source_path.as_ptr(),
        source_path.len(),
        source.as_ptr(),
        source.len(),
    );
    assert_eq!(invalid_code, 1);

    let tiny = [b'x'];
    let oversized_code = run_native_entry(
        tiny.as_ptr(),
        (1 << 30) + 1,
        source_path.as_ptr(),
        source_path.len(),
        source.as_ptr(),
        source.len(),
    );
    assert_eq!(oversized_code, 1);

    let stdout_source = "def main() -> int32:\n    print(\"before\")\n    return 5\n";
    let stdout_mir =
        crate::lower_source_to_mir(stdout_source).expect("stdout source should lower to MIR");
    let stdout_mir_json = serde_json::to_vec(&stdout_mir).expect("stdout MIR should serialize");
    let mut captured_stdout = Vec::new();
    let mut captured_stderr = Vec::new();
    let stdout_code = super::run_serialized_mir_entrypoint_with_streams(
        &stdout_mir_json,
        "/tmp/stdout.au",
        stdout_source,
        &mut captured_stdout,
        &mut captured_stderr,
    );
    assert_eq!(stdout_code, 5);
    assert_eq!(String::from_utf8(captured_stdout).unwrap(), "before\n");
    assert!(captured_stderr.is_empty());

    let unit_source = "def main():\n    print(\"unit\")\n";
    let unit_mir =
        crate::lower_source_to_mir(unit_source).expect("unit source should lower to MIR");
    let unit_mir_json = serde_json::to_vec(&unit_mir).expect("unit MIR should serialize");
    let mut unit_stdout = Vec::new();
    let mut unit_stderr = Vec::new();
    let unit_code = super::run_serialized_mir_entrypoint_with_streams(
        &unit_mir_json,
        "/tmp/unit.au",
        unit_source,
        &mut unit_stdout,
        &mut unit_stderr,
    );
    assert_eq!(unit_code, 0);
    assert_eq!(String::from_utf8(unit_stdout).unwrap(), "unit\n");
    assert!(unit_stderr.is_empty());

    let mut broken_stdout = WriteFailWriter {
        kind: io::ErrorKind::BrokenPipe,
    };
    let mut ignored_stderr = Vec::new();
    let broken_stdout_code = super::run_serialized_mir_entrypoint_with_streams(
        &stdout_mir_json,
        "/tmp/stdout.au",
        stdout_source,
        &mut broken_stdout,
        &mut ignored_stderr,
    );
    assert_eq!(broken_stdout_code, 0);
    assert!(ignored_stderr.is_empty());

    let mut failing_stdout = WriteFailWriter {
        kind: io::ErrorKind::PermissionDenied,
    };
    let mut write_error_stderr = Vec::new();
    let write_error_code = super::run_serialized_mir_entrypoint_with_streams(
        &stdout_mir_json,
        "/tmp/stdout.au",
        stdout_source,
        &mut failing_stdout,
        &mut write_error_stderr,
    );
    assert_eq!(write_error_code, 1);
    assert!(String::from_utf8(write_error_stderr)
        .unwrap()
        .contains("failed to write to stdout"));

    let error_source = "def main() -> int32:\n    print(\"before\")\n    return 1 // 0\n";
    let error_mir =
        crate::lower_source_to_mir(error_source).expect("error source should lower to MIR");
    let error_mir_json = serde_json::to_vec(&error_mir).expect("error MIR should serialize");

    let mut broken_error_stdout = WriteFailWriter {
        kind: io::ErrorKind::BrokenPipe,
    };
    let mut ignored_error_stderr = Vec::new();
    let broken_error_code = super::run_serialized_mir_entrypoint_with_streams(
        &error_mir_json,
        "/tmp/error.au",
        error_source,
        &mut broken_error_stdout,
        &mut ignored_error_stderr,
    );
    assert_eq!(broken_error_code, 0);
    assert!(ignored_error_stderr.is_empty());

    let mut failing_error_stdout = WriteFailWriter {
        kind: io::ErrorKind::PermissionDenied,
    };
    let mut error_write_stderr = Vec::new();
    let error_write_code = super::run_serialized_mir_entrypoint_with_streams(
        &error_mir_json,
        "/tmp/error.au",
        error_source,
        &mut failing_error_stdout,
        &mut error_write_stderr,
    );
    assert_eq!(error_write_code, 1);
    assert!(String::from_utf8(error_write_stderr)
        .unwrap()
        .contains("failed to write to stdout"));

    let mut partial_error_stdout = Vec::new();
    let mut rendered_partial_error_stderr = Vec::new();
    let partial_error_code = super::run_serialized_mir_entrypoint_with_streams(
        &error_mir_json,
        "/tmp/error.au",
        error_source,
        &mut partial_error_stdout,
        &mut rendered_partial_error_stderr,
    );
    assert_eq!(partial_error_code, 1);
    assert_eq!(String::from_utf8(partial_error_stdout).unwrap(), "before\n");
    assert!(String::from_utf8(rendered_partial_error_stderr)
        .unwrap()
        .contains("division by zero"));

    let mut rendered_error_stdout = Vec::new();
    let mut rendered_error_stderr = Vec::new();
    let rendered_error_code = super::run_serialized_mir_entrypoint_with_streams(
        invalid_json,
        "/tmp/error.au",
        error_source,
        &mut rendered_error_stdout,
        &mut rendered_error_stderr,
    );
    assert_eq!(rendered_error_code, 1);
    assert!(rendered_error_stdout.is_empty());
    assert!(String::from_utf8(rendered_error_stderr)
        .unwrap()
        .contains("failed to deserialize embedded MIR"));
}

#[test]
fn mir_runtime_public_run_wrappers_cover_serialized_success_and_error_paths() {
    let source = "def main() -> int32:\n    return 7\n";
    let mir = crate::lower_source_to_mir(source).expect("source should lower to MIR");

    let output = crate::mir_runtime::run(&mir).expect("run wrapper should succeed");
    assert_eq!(output.value, Value::Int(IntegerValue::from_signed(7)));
    assert_eq!(output.stdout, "");

    let serialized = serde_json::to_vec(&mir).expect("MIR should serialize");
    let from_json = run_serialized_mir(&serialized, "/tmp/demo.au", source)
        .expect("serialized MIR should execute");
    assert_eq!(from_json.value, Value::Int(IntegerValue::from_signed(7)));

    let error = run_serialized_mir(b"{", "/tmp/demo.au", source)
        .expect_err("invalid serialized MIR should fail");
    assert!(error.message.contains("failed to deserialize embedded MIR"));
}

#[test]
fn mir_runtime_argument_binding_helpers_cover_named_and_positional_cases() {
    let mut env = Env::default();
    env.define_typed(
        "count",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(7)),
    );
    let evaluated = evaluate_named_args(
        &[
            MirArg {
                name: Some("value".to_string()),
                value: Operand::Place("count".to_string()),
                writeback_place: None,
            },
            MirArg {
                name: None,
                value: Operand::Bool(true),
                writeback_place: Some("flag".to_string()),
            },
        ],
        &mut env,
    )
    .expect("args should evaluate");
    assert_eq!(evaluated[0].value, Value::Int(IntegerValue::from_signed(7)));
    assert_eq!(evaluated[1].writeback_place.as_deref(), Some("flag"));

    let bound = bind_builtin_args(
        &["left", "right"],
        vec![
            EvaluatedMirArg {
                ty: None,
                name: Some("right".to_string()),
                value: Value::Int(IntegerValue::from_signed(2)),
                writeback_place: None,
            },
            EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Int(IntegerValue::from_signed(1)),
                writeback_place: None,
            },
        ],
    )
    .expect("args should bind");
    assert_eq!(bound[0].value, Value::Int(IntegerValue::from_signed(1)));
    assert_eq!(bound[1].name.as_deref(), Some("right"));

    let named_first_then_positional = bind_builtin_args(
        &["value", "timeout"],
        vec![
            EvaluatedMirArg {
                ty: None,
                name: Some("value".to_string()),
                value: Value::Int(IntegerValue::from_signed(9)),
                writeback_place: None,
            },
            EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Duration(25),
                writeback_place: None,
            },
        ],
    )
    .expect("positional MIR args should skip named slots");
    assert_eq!(
        named_first_then_positional[0].value,
        Value::Int(IntegerValue::from_signed(9))
    );
    assert_eq!(named_first_then_positional[1].value, Value::Duration(25));

    let params = vec![
        MirParam {
            name: "left".to_string(),
            passing: crate::mir::MirReceiverKind::Value,
            ty: Type::named("int32"),
            default_function: None,
        },
        MirParam {
            name: "right".to_string(),
            passing: crate::mir::MirReceiverKind::Value,
            ty: Type::named("int32"),
            default_function: None,
        },
    ];
    let rebound = bind_args(&params, bound.clone()).expect("mir params should bind");
    assert_eq!(rebound.len(), 2);

    let missing = bind_builtin_args(
        &["value"],
        vec![EvaluatedMirArg {
            ty: None,
            name: Some("other".to_string()),
            value: Value::Bool(true),
            writeback_place: None,
        }],
    )
    .err()
    .expect("unknown MIR argument should fail");
    assert!(missing.message.contains("unknown MIR argument"));

    let duplicate = bind_builtin_args(
        &["value"],
        vec![
            EvaluatedMirArg {
                ty: None,
                name: Some("value".to_string()),
                value: Value::Int(IntegerValue::from_signed(1)),
                writeback_place: None,
            },
            EvaluatedMirArg {
                ty: None,
                name: Some("value".to_string()),
                value: Value::Int(IntegerValue::from_signed(2)),
                writeback_place: None,
            },
        ],
    )
    .expect("duplicate named MIR arguments should keep the last value");
    assert_eq!(duplicate[0].value, Value::Int(IntegerValue::from_signed(2)));

    let too_many = bind_builtin_args(
        &["value"],
        vec![
            EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Bool(true),
                writeback_place: None,
            },
            EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Bool(false),
                writeback_place: None,
            },
        ],
    )
    .err()
    .expect("extra positional MIR arguments should fail");
    assert!(too_many.message.contains("too many MIR arguments"));

    let optional_named_then_positional = bind_optional_builtin_args(
        &["left", "right"],
        vec![
            EvaluatedMirArg {
                ty: None,
                name: Some("left".to_string()),
                value: Value::Int(IntegerValue::from_signed(1)),
                writeback_place: None,
            },
            EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Int(IntegerValue::from_signed(2)),
                writeback_place: None,
            },
        ],
    )
    .expect("optional MIR args should skip pre-filled named slots");
    assert_eq!(
        optional_named_then_positional[0]
            .as_ref()
            .expect("left should be bound")
            .value,
        Value::Int(IntegerValue::from_signed(1))
    );
    assert_eq!(
        optional_named_then_positional[1]
            .as_ref()
            .expect("right should be bound")
            .value,
        Value::Int(IntegerValue::from_signed(2))
    );

    let optional_unknown = bind_optional_builtin_args(
        &["value"],
        vec![EvaluatedMirArg {
            ty: None,
            name: Some("other".to_string()),
            value: Value::Int(IntegerValue::from_signed(1)),
            writeback_place: None,
        }],
    )
    .err()
    .expect("unknown optional MIR arguments should fail");
    assert!(optional_unknown.message.contains("unknown MIR argument"));

    let optional_too_many = match bind_optional_builtin_args(
        &["value"],
        vec![
            EvaluatedMirArg {
                ty: None,
                name: Some("value".to_string()),
                value: Value::Int(IntegerValue::from_signed(1)),
                writeback_place: None,
            },
            EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Int(IntegerValue::from_signed(2)),
                writeback_place: None,
            },
        ],
    ) {
        Ok(_) => panic!("extra optional MIR args should fail"),
        Err(error) => error,
    };
    assert!(optional_too_many.message.contains("too many MIR arguments"));

    let missing_required = bind_builtin_args(
        &["left", "right"],
        vec![EvaluatedMirArg {
            ty: None,
            name: None,
            value: Value::Int(IntegerValue::from_signed(1)),
            writeback_place: None,
        }],
    )
    .err()
    .expect("missing MIR arguments should fail");
    assert!(missing_required.message.contains("missing MIR argument"));

    let eval_error = evaluate_named_args(
        &[MirArg {
            name: Some("value".to_string()),
            value: Operand::Place("missing".to_string()),
            writeback_place: None,
        }],
        &mut env,
    )
    .err()
    .expect("reading a missing MIR place should fail");
    assert!(eval_error.message.contains("unknown MIR place `missing`"));

    let unit_value = evaluate_named_args(
        &[MirArg {
            name: Some("unit".to_string()),
            value: Operand::Unit,
            writeback_place: None,
        }],
        &mut env,
    )
    .expect("unit operands should evaluate");
    assert_eq!(unit_value[0].value, Value::Unit);
}

#[test]
fn mir_runtime_deadline_helper_rejects_overflowing_instants() {
    let error = super::runtime_deadline_after_timeout(Some(StdDuration::MAX))
        .expect_err("overflowing instant deadlines should be rejected");
    assert!(error
        .message
        .contains("overflows the MIR runtime deadline range"));
}

#[test]
fn mir_runtime_complexity_guard_rejects_excessive_instruction_counts() {
    super::validate_embedded_runtime_length("MIR payload", super::MAX_EMBEDDED_RUNTIME_BYTES)
        .expect("embedded runtime payloads at the limit should pass");
    let length_error = super::validate_embedded_runtime_length(
        "MIR payload",
        super::MAX_EMBEDDED_RUNTIME_BYTES + 1,
    )
    .expect_err("embedded runtime payloads above the limit should fail");
    assert!(length_error.contains("exceeds the supported runtime limit"));

    let block = |label: &str, terminator: Terminator| BasicBlock {
        label: label.to_string(),
        instructions: Vec::new(),
        terminator,
    };
    let module_with_blocks = |blocks: Vec<BasicBlock>| MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks,
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let module = MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Eval {
                        value: Operand::Unit
                    };
                    super::MAX_RUNTIME_INSTRUCTIONS + 1
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let error = super::validate_runtime_module_complexity(&module)
        .expect_err("oversized MIR modules should be rejected");
    assert!(error.message.contains("instruction limit"));

    let block_error = super::validate_runtime_module_complexity_with_limits(
        &module_with_blocks(vec![
            block("entry", Terminator::Goto("overflow".to_string())),
            block("overflow", Terminator::Return(Operand::Int(0))),
        ]),
        super::RuntimeModuleLimits {
            max_blocks: 1,
            max_instructions: 10,
            max_terminator_arms: 10,
        },
    )
    .expect_err("block-heavy MIR modules should be rejected");
    assert!(block_error.message.contains("block limit"));

    let arm_limit_module = module_with_blocks(vec![block(
        "entry",
        Terminator::Match {
            scrutinee: Operand::Bool(true),
            arms: vec![
                MirMatchArm {
                    enum_name: None,
                    variant_name: None,
                    wildcard: true,
                    label: "done".to_string(),
                },
                MirMatchArm {
                    enum_name: None,
                    variant_name: None,
                    wildcard: true,
                    label: "done".to_string(),
                },
            ],
            otherwise: "done".to_string(),
        },
    )]);
    let arm_error = super::validate_runtime_module_complexity_with_limits(
        &arm_limit_module,
        super::RuntimeModuleLimits {
            max_blocks: 10,
            max_instructions: 10,
            max_terminator_arms: 1,
        },
    )
    .expect_err("branch-heavy MIR modules should be rejected");
    assert!(arm_error.message.contains("branching-arm limit"));

    let match_module = module_with_blocks(vec![block(
        "entry",
        Terminator::Match {
            scrutinee: Operand::Bool(true),
            arms: vec![MirMatchArm {
                enum_name: None,
                variant_name: None,
                wildcard: true,
                label: "done".to_string(),
            }],
            otherwise: "done".to_string(),
        },
    )]);
    super::validate_runtime_module_complexity(&match_module)
        .expect("small match terminator modules should be accepted");

    let amplified_origin = "o".repeat(300_000);
    let amplified = module_with_blocks(vec![BasicBlock {
        label: "entry".to_string(),
        instructions: vec![Instruction::BeginReturnedLoan {
            loan: "returned".to_string(),
            origin: amplified_origin,
            projections: (0..16).map(|index| index.to_string()).collect(),
            mutable: false,
        }],
        terminator: Terminator::Return(Operand::Int(0)),
    }]);
    let expansion_error = super::validate_runtime_module_complexity(&amplified)
        .expect_err("small serialized inputs must not amplify into multi-megabyte loan paths");
    assert!(
        expansion_error
            .message
            .contains("expanded loan-path byte limit"),
        "unexpected diagnostic: {}",
        expansion_error.message
    );

    let reborrow_amplification = module_with_blocks(vec![BasicBlock {
        label: "entry".to_string(),
        instructions: vec![
            Instruction::BeginReturnedLoan {
                loan: "parent".to_string(),
                origin: "origin".to_string(),
                projections: (0..4_096).map(|index| index.to_string()).collect(),
                mutable: false,
            },
            Instruction::Reborrow {
                loan: "first".to_string(),
                parent: "parent".to_string(),
                projection: "field".repeat(104),
                mutable: false,
            },
            Instruction::Reborrow {
                loan: "second".to_string(),
                parent: "parent".to_string(),
                projection: "field".repeat(104),
                mutable: false,
            },
        ],
        terminator: Terminator::Return(Operand::Int(0)),
    }]);
    let reborrow_error = super::validate_runtime_module_complexity(&reborrow_amplification)
        .expect_err("serialized reborrows must be budgeted before loan paths are expanded");
    assert!(
        reborrow_error
            .message
            .contains("expanded loan-path byte limit"),
        "unexpected diagnostic: {}",
        reborrow_error.message
    );
    let serialized = serde_json::to_vec(&reborrow_amplification)
        .expect("the amplification probe should serialize");
    assert!(
        serialized.len() < 64 * 1024,
        "the regression should remain a low-resource amplification probe"
    );
    let serialized_error = super::run_serialized_mir(&serialized, "<amplified>", "")
        .expect_err("the serialized runtime boundary must reject before loan expansion");
    assert!(
        serialized_error
            .message
            .contains("expanded loan-path byte limit"),
        "unexpected serialized diagnostic: {}",
        serialized_error.message
    );

    let control = module_with_blocks(vec![block("entry", Terminator::Return(Operand::Int(0)))]);
    let serialized_control =
        serde_json::to_vec(&control).expect("the ordinary control should serialize");
    super::run_serialized_mir(&serialized_control, "<control>", "")
        .expect("ordinary serialized MIR must remain executable");
}

#[test]
fn mir_runtime_task_detection_helpers_cover_task_and_process_shapes() {
    let make_function = |name: &str, instructions: Vec<Instruction>| MirFunction {
        name: name.to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions,
            terminator: Terminator::Return(Operand::Unit),
        }],
    };

    let ordinary = make_function(
        "ordinary",
        vec![
            Instruction::Eval {
                value: Operand::Unit,
            },
            Instruction::PushCleanup {
                place: "resource".to_string(),
            },
            Instruction::PopCleanup {
                place: "resource".to_string(),
                cancel_before_cleanup: false,
            },
            Instruction::Assign {
                target: "ignored".to_string(),
                value: Rvalue::Call {
                    callee: CallTarget::Member {
                        object: Operand::Place("service".to_string()),
                        field: "run".to_string(),
                        receiver_place: Some("service".to_string()),
                    },
                    args: Vec::new(),
                },
            },
            Instruction::Assign {
                target: "also_ignored".to_string(),
                value: Rvalue::Call {
                    callee: CallTarget::Name("print".to_string()),
                    args: Vec::new(),
                },
            },
        ],
    );
    assert!(!super::function_uses_lightweight_tasks(&ordinary));

    let started_task = make_function(
        "started_task",
        vec![Instruction::Assign {
            target: "task".to_string(),
            value: Rvalue::StartTask {
                returns_handle: true,
                result_is_copy: true,
                stack_size: None,
                task_group: Operand::Unit,
                function: test_function_operand("worker", Vec::new(), Type::Unit),
                args: Vec::new(),
                span: crate::diag::Span::new(1, 1),
            },
        }],
    );
    assert!(super::function_uses_lightweight_tasks(&started_task));

    let process_run = make_function(
        "process_run",
        vec![Instruction::Assign {
            target: "completed".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Name("process::run".to_string()),
                args: Vec::new(),
            },
        }],
    );
    assert!(super::function_uses_lightweight_tasks(&process_run));

    let without_tasks = MirModule {
        constants: Vec::new(),
        functions: vec![ordinary.clone()],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };
    assert!(!super::module_uses_lightweight_tasks(&without_tasks));

    let with_top_level_process_run = MirModule {
        constants: Vec::new(),
        functions: vec![ordinary],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: Some(process_run),
    };
    assert!(super::module_uses_lightweight_tasks(
        &with_top_level_process_run
    ));
}

#[test]
fn mir_function_value_process_run_selects_scheduler_only_when_source_materializes_it() {
    let synchronous = crate::lower_source_to_mir(
        r#"
def main():
    print("synchronous")
"#,
    )
    .expect("an ordinary program should lower");
    assert!(
        synchronous
            .functions
            .iter()
            .any(|function| function.name == "process::run"),
        "runtime-provided function wrappers should be present for first-class lookup"
    );
    assert!(
        !super::module_uses_lightweight_tasks(&synchronous),
        "an unused runtime wrapper must not force synchronous source onto the task scheduler"
    );

    let materialized = crate::lower_source_to_mir(
        r#"
import process

def main():
    runner = process.run
"#,
    )
    .expect("a process.run function value should lower");
    let main = materialized
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    assert!(
        main.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value:
                            Rvalue::Use(Operand::Function {
                                name,
                                ..
                            }),
                        ..
                    } if name == "process::run"
                )
            })
        }),
        "the source assignment should explicitly materialize process.run"
    );
    assert!(
        super::module_uses_lightweight_tasks(&materialized),
        "materializing process.run must select the scheduler needed by a later dynamic call"
    );
}

#[test]
fn mir_runtime_writeback_and_spawn_helpers_cover_borrow_mut_edges() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "target",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(1)),
    );

    let params = vec![
        MirParam {
            name: "source".to_string(),
            passing: crate::mir::MirReceiverKind::Borrow,
            ty: Type::named("int32"),
            default_function: None,
        },
        MirParam {
            name: "target".to_string(),
            passing: crate::mir::MirReceiverKind::BorrowMut,
            ty: Type::named("int32"),
            default_function: None,
        },
    ];
    let writeback_places = vec![None, Some("target".to_string())];
    runtime
        .apply_borrowed_param_writebacks(
            &params,
            &writeback_places,
            vec![
                (0, Value::Int(IntegerValue::from_signed(4))),
                (1, Value::Int(IntegerValue::from_signed(7))),
                (9, Value::Int(IntegerValue::from_signed(11))),
            ],
            &mut env,
        )
        .expect("borrow-mut writebacks should update explicit writeback places");
    assert_eq!(
        env.read_place("target"),
        Ok(Value::Int(IntegerValue::from_signed(7)))
    );

    let missing_writeback = runtime
        .apply_borrowed_param_writebacks(
            &params,
            &[None, None],
            vec![(1, Value::Int(IntegerValue::from_signed(9)))],
            &mut env,
        )
        .expect_err("borrow-mut writebacks require an explicit writeback place");
    assert!(missing_writeback
        .message
        .contains("requires a writeback place"));

    let text = "borrow-mut-writeback".repeat(64);
    let text_ptr = text.as_ptr();
    env.define_typed(
        "text_target",
        Type::named("str"),
        Value::String("old".to_string()),
    );
    runtime
        .apply_borrowed_param_writebacks(
            &[MirParam {
                name: "text".to_string(),
                passing: crate::mir::MirReceiverKind::BorrowMut,
                ty: Type::named("str"),
                default_function: None,
            }],
            &[Some("text_target".to_string())],
            vec![(0, Value::String(text))],
            &mut env,
        )
        .expect("borrow-mut writeback should transfer its returned allocation");
    let Value::String(text_target) = env
        .place_ref("text_target")
        .expect("borrow-mut target should be updated")
    else {
        panic!("expected str borrow-mut target");
    };
    assert_eq!(
        text_target.as_ptr(),
        text_ptr,
        "the final writeback handoff must not deep-clone the updated value"
    );

    let by_value = MirFunction {
        name: "work".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "value".to_string(),
            passing: crate::mir::MirReceiverKind::Value,
            ty: Type::named("int32"),
            default_function: None,
        }],
        local_types: Vec::new(),
        return_type: Type::named("int32"),
        entry: "entry".to_string(),
        blocks: Vec::new(),
    };
    runtime
        .require_task_startable_function(&by_value)
        .expect("by-value MIR functions should be task-startable");
    runtime
        .require_task_startable_function(&MirFunction {
            params: vec![MirParam {
                name: "value".to_string(),
                passing: crate::mir::MirReceiverKind::Borrow,
                ty: Type::named("str"),
                default_function: None,
            }],
            ..by_value.clone()
        })
        .expect("shared borrowed MIR parameters should be task-startable");
    let task_start_error = runtime
        .require_task_startable_function(&MirFunction {
            params: vec![MirParam {
                name: "value".to_string(),
                passing: crate::mir::MirReceiverKind::BorrowMut,
                ty: Type::named("int32"),
                default_function: None,
            }],
            ..by_value
        })
        .expect_err("mutable borrowed params should not be task-startable in MIR");
    assert_eq!(task_start_error.code, "AU3002");
    assert_eq!(
        task_start_error.message,
        "task starting does not support mutable MIR parameter `value` on function `work`; child tasks cannot write back through the starting call frame"
    );
}

#[test]
fn mir_runtime_builtin_call_surface_covers_named_and_error_paths() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed("delay", Type::named("Duration"), Value::Duration(0));
    env.define_typed(
        "negative_delay",
        Type::named("Duration"),
        Value::Duration(-1),
    );
    env.define_typed(
        "neg",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(-4)),
    );
    env.define_typed(
        "uint",
        Type::named("uint64"),
        Value::Int(IntegerValue::from_literal(7)),
    );
    env.define_typed("ratio", Type::named("float64"), Value::Float(-2.5));
    env.define_typed("text", Type::named("str"), Value::String("12".to_string()));
    env.define_typed(
        "word",
        Type::named("str"),
        Value::String("Aura".to_string()),
    );
    env.define_typed(
        "float_text",
        Type::named("str"),
        Value::String("1.5e2".to_string()),
    );
    env.define_typed(
        "infinite_text",
        Type::named("str"),
        Value::String("inf".to_string()),
    );

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("range".to_string()),
                &[MirArg {
                    name: Some("stop".to_string()),
                    value: Operand::Int(3),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("named range call should succeed"),
        Value::Range(RangeValue { start: 0, end: 3 })
    );
    assert!(matches!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("list".to_string()),
                &[],
                &mut env
            )
            .expect("Vec() should succeed"),
        Value::Vec(_)
    ));
    assert!(matches!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("set".to_string()),
                &[],
                &mut env
            )
            .expect("Set() should succeed"),
        Value::Set(_)
    ));
    assert!(matches!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("dict".to_string()),
                &[],
                &mut env
            )
            .expect("Map() should succeed"),
        Value::Map(_)
    ));
    assert!(matches!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("TaskGroup".to_string()),
                &[],
                &mut env
            )
            .expect("TaskGroup() should succeed"),
        Value::TaskGroup(_)
    ));
    assert!(matches!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("Queue".to_string()),
                &[MirArg {
                    name: Some("capacity".to_string()),
                    value: Operand::Int(2),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("Queue(capacity=...) should create a bounded queue"),
        Value::Channel(_)
    ));
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("cancelled".to_string()),
                &[],
                &mut env,
            )
            .expect("cancelled() should succeed"),
        Value::Bool(false)
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("yield_now".to_string()),
                &[],
                &mut env,
            )
            .expect("yield_now() should succeed"),
        Value::Unit
    );
    let yield_arg_error = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("yield_now".to_string()),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("yield_now() should reject arguments in malformed MIR");
    assert_eq!(yield_arg_error.message, "too many MIR arguments");
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("sleep".to_string()),
                &[MirArg {
                    name: Some("duration".to_string()),
                    value: Operand::Place("delay".to_string()),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("sleep() should accept zero duration"),
        Value::Unit
    );
    let sleep_error = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("sleep".to_string()),
            &[MirArg {
                name: Some("duration".to_string()),
                value: Operand::Place("negative_delay".to_string()),
                writeback_place: None,
            }],
            &mut env,
        )
        .expect_err("negative sleep durations should fail");
    assert!(sleep_error
        .message
        .contains("sleep(...) must be non-negative"));

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("abs".to_string()),
                &[mir_arg(None, Operand::Place("neg".to_string()))],
                &mut env,
            )
            .expect("abs(int) should succeed"),
        Value::Int(IntegerValue::from_signed(4))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("abs".to_string()),
                &[mir_arg(None, Operand::Place("uint".to_string()))],
                &mut env,
            )
            .expect("abs(uint) should succeed"),
        Value::Int(IntegerValue::from_literal(7))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("abs".to_string()),
                &[mir_arg(None, Operand::Place("ratio".to_string()))],
                &mut env,
            )
            .expect("abs(float) should succeed"),
        Value::Float(2.5)
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("min".to_string()),
                &[
                    mir_arg(None, Operand::Int(8)),
                    mir_arg(None, Operand::Int(3))
                ],
                &mut env,
            )
            .expect("min(int, int) should succeed"),
        Value::Int(IntegerValue::from_signed(3))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("min".to_string()),
                &[
                    mir_arg(None, Operand::Int(3)),
                    mir_arg(None, Operand::Int(8))
                ],
                &mut env,
            )
            .expect("min(int, int) should keep the left value when smaller"),
        Value::Int(IntegerValue::from_signed(3))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("max".to_string()),
                &[
                    mir_arg(None, Operand::Float(1.5)),
                    mir_arg(None, Operand::Float(2.5)),
                ],
                &mut env,
            )
            .expect("max(float, float) should succeed"),
        Value::Float(2.5)
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("max".to_string()),
                &[
                    mir_arg(None, Operand::Float(3.5)),
                    mir_arg(None, Operand::Float(2.5)),
                ],
                &mut env,
            )
            .expect("max(float, float) should keep the left value when larger"),
        Value::Float(3.5)
    );
    let min_error = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("min".to_string()),
            &[
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Bool(true)),
            ],
            &mut env,
        )
        .expect_err("min() should reject mismatched types");
    assert!(min_error
        .message
        .contains("expects matching numeric arguments"));
    let sqrt_error = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("sqrt".to_string()),
            &[mir_arg(None, Operand::Int(9))],
            &mut env,
        )
        .expect_err("sqrt() should reject integer operands");
    assert!(sqrt_error
        .message
        .contains("expects `float32` or `float64`"));

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("parse_int32".to_string()),
                &[MirArg {
                    name: Some("text".to_string()),
                    value: Operand::Place("text".to_string()),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("parse_int32() should succeed"),
        result_ok(Value::Int(IntegerValue::from_signed(12)))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("parse_int64".to_string()),
                &[MirArg {
                    name: Some("text".to_string()),
                    value: Operand::Place("text".to_string()),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("parse_int64() should succeed"),
        result_ok(Value::Int(IntegerValue::from_signed(12)))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("parse_float64".to_string()),
                &[MirArg {
                    name: Some("text".to_string()),
                    value: Operand::Place("float_text".to_string()),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("parse_float64() should parse finite floats"),
        result_ok(Value::Float(150.0))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("parse_float64".to_string()),
                &[MirArg {
                    name: Some("text".to_string()),
                    value: Operand::Place("word".to_string()),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("parse_float64() should return Result.Err for bad strings"),
        result_err(Value::String("invalid float literal".to_string()))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("parse_float64".to_string()),
                &[MirArg {
                    name: Some("text".to_string()),
                    value: Operand::Place("infinite_text".to_string()),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("parse_float64() should reject non-finite floats as Result.Err"),
        result_err(Value::String("float must be finite".to_string()))
    );
    let queue_error = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("Queue".to_string()),
            &[
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(2)),
            ],
            &mut env,
        )
        .expect_err("Queue() should reject extra arguments");
    assert!(queue_error
        .message
        .contains("expects at most one optional `capacity` argument"));
}

#[test]
fn mir_runtime_round_and_divmod_use_shared_checked_numeric_contracts() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "small",
        Type::named("int8"),
        Value::Int(IntegerValue::from_typed_signed(-7, IntegerKind::Int8).unwrap()),
    );
    env.define_typed("half", Type::named("float32"), Value::Float(2.5));

    assert_eq!(
        runtime
            .evaluate_call(
                &CallTarget::Name("round".to_string()),
                &[mir_arg(None, Operand::Place("small".to_string()))],
                &mut env,
            )
            .expect("round preserves exact integer values"),
        Value::Int(IntegerValue::from_typed_signed(-7, IntegerKind::Int8).unwrap()),
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &CallTarget::Name("round".to_string()),
                &[mir_arg(None, Operand::Place("half".to_string()))],
                &mut env,
            )
            .expect("round uses ties-to-even"),
        Value::Int(IntegerValue::from_i64(2)),
    );

    let pair = runtime
        .evaluate_call(
            &CallTarget::Name("divmod".to_string()),
            &[
                mir_arg(None, Operand::Place("small".to_string())),
                mir_arg(None, Operand::Int(3)),
            ],
            &mut env,
        )
        .expect("divmod should produce a paired quotient and remainder");
    assert_eq!(
        pair,
        Value::Tuple(TupleValue {
            element_types: vec![Type::named("int8"), Type::named("int8")],
            elements: vec![
                Value::Int(IntegerValue::from_typed_signed(-3, IntegerKind::Int8).unwrap()),
                Value::Int(IntegerValue::from_typed_signed(2, IntegerKind::Int8).unwrap()),
            ],
        })
    );

    let zero = runtime
        .evaluate_call(
            &CallTarget::Name("divmod".to_string()),
            &[
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(0)),
            ],
            &mut env,
        )
        .expect_err("zero divmod divisor must trap");
    assert_eq!(zero.code, "AU4004");
}

#[test]
fn mir_runtime_process_child_methods_cover_timeout_cancel_and_error_edges() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let sleeper = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 5".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("sleeper should spawn");
    let mut failed_wait = enum_payloads(
        runtime
            .evaluate_process_child_method(
                sleeper.clone(),
                "wait",
                &[mir_arg(Some("timeout"), Operand::Duration(-1))],
                &mut env,
            )
            .expect("invalid wait timers should use the Wait.Failed carrier"),
        "Wait",
        "Failed",
    );
    assert_eq!(failed_wait.len(), 1);
    assert_process_invalid_input(failed_wait.remove(0));
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_child_method(
                sleeper.clone(),
                "wait_or_none",
                &[mir_arg(Some("timeout"), Operand::Duration(-1))],
                &mut env,
            )
            .expect("invalid wait_or_none timers should use Result.Err"),
    );
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_child_method(
                sleeper.clone(),
                "wait_ok",
                &[mir_arg(Some("timeout"), Operand::Duration(-1))],
                &mut env,
            )
            .expect("invalid wait_ok timers should use Result.Err"),
    );
    enum_payloads(
        runtime
            .evaluate_process_child_method(
                sleeper.clone(),
                "wait",
                &[mir_arg(Some("timeout"), Operand::Duration(0))],
                &mut env,
            )
            .expect("wait should surface timeout"),
        "Wait",
        "TimedOut",
    );
    enum_payloads(
        result_ok_payload(
            runtime
                .evaluate_process_child_method(
                    sleeper.clone(),
                    "wait_or_none",
                    &[mir_arg(Some("timeout"), Operand::Duration(0))],
                    &mut env,
                )
                .expect("wait_or_none should surface timeout as None"),
        ),
        "Option",
        "None",
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_child_method(sleeper.clone(), "kill", &[], &mut env)
                .expect("kill should succeed")
        ),
        Value::Unit
    );
    enum_payloads(
        runtime
            .evaluate_process_child_method(
                sleeper.clone(),
                "wait",
                &[mir_arg(Some("timeout"), Operand::Duration(1_000_000_000))],
                &mut env,
            )
            .expect("killed child should become waitable"),
        "Wait",
        "Exited",
    );
    let unknown_method = runtime
        .evaluate_process_child_method(sleeper, "missing", &[], &mut env)
        .expect_err("unknown process child methods should fail");
    assert!(unknown_method
        .message
        .contains("unsupported MIR process child method"));

    let failing = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("failing child should spawn");
    enum_payloads(
        runtime
            .evaluate_process_child_method(
                failing,
                "wait_ok",
                &[mir_arg(Some("timeout"), Operand::Duration(1_000_000_000))],
                &mut env,
            )
            .expect("wait_ok should return a Result"),
        "Result",
        "Err",
    );

    let terminable = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 5".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("terminable child should spawn");
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_child_method(terminable.clone(), "terminate", &[], &mut env)
                .expect("terminate should succeed")
        ),
        Value::Unit
    );
    let _ = runtime.evaluate_process_child_method(
        terminable.clone(),
        "wait",
        &[mir_arg(Some("timeout"), Operand::Duration(1_000_000_000))],
        &mut env,
    );
    let _ = runtime.evaluate_process_child_method(terminable, "close", &[], &mut env);

    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let cancelled_context = group.child_cancellation();
    group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancelled_context,
    );
    let cancelled_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 5".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("cancelled-runtime child should spawn");
    enum_payloads(
        cancelled_runtime
            .evaluate_process_child_method(
                cancelled_child.clone(),
                "wait",
                &[mir_arg(Some("timeout"), Operand::Duration(1_000_000_000))],
                &mut env,
            )
            .expect("wait should observe cancellation"),
        "Wait",
        "Cancelled",
    );
    let _ =
        cancelled_runtime.evaluate_process_child_method(cancelled_child, "close", &[], &mut env);
}

#[test]
fn mir_runtime_process_resource_members_cover_completed_errors_and_pipe_edges() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(b"payload".to_vec()),
    );
    env.define_typed(
        "negative",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(-1)),
    );

    let failed_completed = ProcessCompletedValue::new(
        Value::EnumVariant(EnumVariantValue {
            enum_name: "ExitStatus".to_string(),
            variant_name: "Exited".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(7))],
        }),
        vec![0xff],
        vec![0xfe],
    );
    assert_eq!(
        runtime
            .evaluate_process_completed_method(failed_completed.clone(), "success", &[], &mut env)
            .expect("completed success should evaluate"),
        Value::Bool(false)
    );
    assert!(runtime
        .evaluate_process_completed_method(failed_completed.clone(), "stdout", &[], &mut env)
        .expect_err("invalid stdout utf-8 should be rejected")
        .message
        .contains("invalid utf-8"));
    assert!(runtime
        .evaluate_process_completed_method(failed_completed.clone(), "stderr", &[], &mut env)
        .expect_err("invalid stderr utf-8 should be rejected")
        .message
        .contains("invalid utf-8"));
    assert_result_err(
        runtime
            .evaluate_process_completed_method(failed_completed, "check", &[], &mut env)
            .expect("failed process check should return Result.Err"),
    );

    let eof_child = ProcessChildValue::spawn(
        vec!["/bin/sh".to_string(), "-c".to_string(), String::new()],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("eof process should spawn");
    let eof_stdout = eof_child.stdout().expect("eof stdout should be piped");
    enum_payloads(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(
                    eof_stdout.clone(),
                    "read_line",
                    &[mir_arg(Some("timeout"), Operand::Duration(1_000_000_000))],
                    &mut env,
                )
                .expect("eof read_line should succeed"),
        ),
        "Option",
        "None",
    );
    enum_payloads(
        result_ok_payload(
            runtime
                .evaluate_process_pipe_method(
                    eof_stdout,
                    "read_bytes",
                    &[
                        mir_arg(Some("max_bytes"), Operand::Int(8)),
                        mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                    ],
                    &mut env,
                )
                .expect("eof read_bytes should succeed"),
        ),
        "Option",
        "None",
    );
    let _ = runtime.evaluate_process_child_method(eof_child, "wait", &[], &mut env);

    let reader_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 0.05".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("reader process should spawn");
    let closed_reader = reader_child
        .stdout()
        .expect("reader stdout should be piped");
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_pipe_method(
                closed_reader.clone(),
                "read_line",
                &[mir_arg(Some("timeout"), Operand::Duration(-1))],
                &mut env,
            )
            .expect("invalid read_line timers should use Result.Err"),
    );
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_pipe_method(
                closed_reader.clone(),
                "read_bytes",
                &[
                    mir_arg(Some("max_bytes"), Operand::Int(8)),
                    mir_arg(Some("timeout"), Operand::Duration(-1)),
                ],
                &mut env,
            )
            .expect("invalid read_bytes timers should use Result.Err"),
    );
    assert_eq!(
        runtime
            .evaluate_process_pipe_method(closed_reader.clone(), "close", &[], &mut env)
            .expect("reader pipe close should succeed"),
        Value::Unit
    );
    assert_result_err(
        runtime
            .evaluate_process_pipe_method(closed_reader.clone(), "read_all", &[], &mut env)
            .expect("closed read_all should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_process_pipe_method(
                closed_reader.clone(),
                "read_line",
                &[mir_arg(Some("timeout"), Operand::Duration(1_000_000_000))],
                &mut env,
            )
            .expect("closed read_line should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_process_pipe_method(
                closed_reader.clone(),
                "read_bytes",
                &[
                    mir_arg(Some("max_bytes"), Operand::Int(8)),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("closed read_bytes should return Result.Err"),
    );
    assert!(runtime
        .evaluate_process_pipe_method(
            closed_reader,
            "read_bytes",
            &[
                mir_arg(Some("max_bytes"), Operand::Place("negative".to_string())),
                mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
            ],
            &mut env,
        )
        .expect_err("negative process pipe read sizes should fail")
        .message
        .contains("non-negative"));
    let _ = runtime.evaluate_process_child_method(reader_child, "wait", &[], &mut env);

    let writer_child = ProcessChildValue::spawn(
        vec!["/bin/cat".to_string()],
        None,
        Vec::new(),
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("writer process should spawn");
    let closed_writer = writer_child.stdin().expect("writer stdin should be piped");
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_pipe_method(
                closed_writer.clone(),
                "write_all",
                &[
                    mir_arg(Some("text"), Operand::String("payload".to_string())),
                    mir_arg(Some("timeout"), Operand::Duration(-1)),
                ],
                &mut env,
            )
            .expect("invalid write_all timers should use Result.Err"),
    );
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_pipe_method(
                closed_writer.clone(),
                "write_bytes",
                &[
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                    mir_arg(Some("timeout"), Operand::Duration(-1)),
                ],
                &mut env,
            )
            .expect("invalid write_bytes timers should use Result.Err"),
    );
    assert_eq!(
        runtime
            .evaluate_process_pipe_method(closed_writer.clone(), "close", &[], &mut env)
            .expect("writer pipe close should succeed"),
        Value::Unit
    );
    assert_result_err(
        runtime
            .evaluate_process_pipe_method(
                closed_writer.clone(),
                "write_all",
                &[
                    mir_arg(Some("text"), Operand::String("closed".to_string())),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("closed write_all should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_process_pipe_method(
                closed_writer.clone(),
                "write_bytes",
                &[
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("closed write_bytes should return Result.Err"),
    );
    assert_result_err(
        runtime
            .evaluate_process_pipe_method(closed_writer.clone(), "flush", &[], &mut env)
            .expect("closed flush should return Result.Err"),
    );
    assert!(runtime
        .evaluate_process_pipe_method(closed_writer, "missing", &[], &mut env)
        .expect_err("unknown process pipe methods should fail")
        .message
        .contains("unsupported MIR process pipe method"));
    let _ = runtime.evaluate_process_child_method(writer_child, "wait", &[], &mut env);
}

#[test]
fn mir_runtime_process_supervisor_methods_cover_start_wait_and_cancel_edges() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "exit_command",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        string_vec_value(&["/bin/sh", "-c", "exit 0"]),
    );
    env.define_typed(
        "sleep_command",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        string_vec_value(&["/bin/sh", "-c", "sleep 5"]),
    );
    let stdio_variant = |variant_name: &str| {
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.Stdio".to_string(),
            variant_name: variant_name.to_string(),
            payloads: Vec::new(),
        })
    };
    env.define_typed(
        "supervisor_cwd",
        Type::named("Option[str]"),
        option_some(Value::String(
            std::env::temp_dir().to_string_lossy().into_owned(),
        )),
    );
    env.define_typed(
        "supervisor_env",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("str")],
        ),
        string_map_value(&[("AURA_SUPERVISOR_TEST", "1")]),
    );
    env.define_typed(
        "stdio_null",
        Type::named("process.Stdio"),
        stdio_variant("Null"),
    );
    env.define_typed(
        "stdio_pipe",
        Type::named("process.Stdio"),
        stdio_variant("Pipe"),
    );
    env.define_typed(
        "stdio_inherit",
        Type::named("process.Stdio"),
        stdio_variant("Inherit"),
    );
    env.define_typed(
        "restart_never",
        Type::named("process.RestartPolicy"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "process.RestartPolicy".to_string(),
            variant_name: "Never".to_string(),
            payloads: Vec::new(),
        }),
    );

    let supervisor = ProcessSupervisorValue::new();
    assert_process_invalid_input_result(
        runtime
            .evaluate_process_supervisor_method(
                supervisor.clone(),
                "start",
                &[
                    mir_arg(Some("name"), Operand::String("invalid-backoff".to_string())),
                    mir_arg(Some("command"), Operand::Place("exit_command".to_string())),
                    mir_arg(Some("backoff"), Operand::Duration(-1)),
                ],
                &mut env,
            )
            .expect("invalid supervisor backoff should use Result.Err"),
    );
    let missing_command = runtime
        .evaluate_process_supervisor_method(
            supervisor.clone(),
            "start",
            &[mir_arg(
                Some("name"),
                Operand::String("missing-command".to_string()),
            )],
            &mut env,
        )
        .expect_err("supervisor start should require a command");
    assert!(missing_command
        .message
        .contains("missing MIR argument `command`"));
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(
                    supervisor.clone(),
                    "start",
                    &[
                        mir_arg(Some("name"), Operand::String("oneshot".to_string())),
                        mir_arg(Some("command"), Operand::Place("exit_command".to_string())),
                    ],
                    &mut env,
                )
                .expect("supervisor start should succeed with defaulted optional args")
        ),
        Value::Unit
    );
    enum_payloads(
        runtime
            .evaluate_process_supervisor_method(
                supervisor.clone(),
                "wait",
                &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                &mut env,
            )
            .expect("supervisor wait should surface an event"),
        "SupervisorWait",
        "Event",
    );
    enum_payloads(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(
                    supervisor.clone(),
                    "wait_or_none",
                    &[mir_arg(Some("timeout"), Operand::Duration(0))],
                    &mut env,
                )
                .expect("empty supervisor wait_or_none should return Result.Ok"),
        ),
        "Option",
        "None",
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(
                    supervisor.clone(),
                    "start",
                    &[
                        mir_arg(Some("name"), Operand::String("configured".to_string())),
                        mir_arg(Some("command"), Operand::Place("exit_command".to_string())),
                        mir_arg(Some("cwd"), Operand::Place("supervisor_cwd".to_string())),
                        mir_arg(Some("env"), Operand::Place("supervisor_env".to_string())),
                        mir_arg(Some("stdin"), Operand::Place("stdio_null".to_string())),
                        mir_arg(Some("stdout"), Operand::Place("stdio_pipe".to_string())),
                        mir_arg(Some("stderr"), Operand::Place("stdio_inherit".to_string())),
                        mir_arg(Some("restart"), Operand::Place("restart_never".to_string())),
                        mir_arg(Some("backoff"), Operand::Duration(1_000_000)),
                        mir_arg(Some("max_restarts"), Operand::Int(1)),
                        mir_arg(Some("group"), Operand::Bool(false)),
                    ],
                    &mut env,
                )
                .expect("supervisor start should accept all optional args")
        ),
        Value::Unit
    );
    enum_payloads(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(
                    supervisor.clone(),
                    "wait_or_none",
                    &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                    &mut env,
                )
                .expect("supervisor wait_or_none should surface ready events"),
        ),
        "Option",
        "Some",
    );

    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(
                    supervisor.clone(),
                    "start",
                    &[
                        mir_arg(Some("name"), Operand::String("dupe".to_string())),
                        mir_arg(Some("command"), Operand::Place("sleep_command".to_string())),
                    ],
                    &mut env,
                )
                .expect("supervisor start should accept a long-running child")
        ),
        Value::Unit
    );
    enum_payloads(
        runtime
            .evaluate_process_supervisor_method(
                supervisor.clone(),
                "start",
                &[
                    mir_arg(Some("name"), Operand::String("dupe".to_string())),
                    mir_arg(Some("command"), Operand::Place("sleep_command".to_string())),
                ],
                &mut env,
            )
            .expect("duplicate supervisor starts should return Result.Err"),
        "Result",
        "Err",
    );
    assert_eq!(
        result_ok_payload(
            runtime
                .evaluate_process_supervisor_method(supervisor.clone(), "stop", &[], &mut env)
                .expect("supervisor stop should clean up running children")
        ),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_process_supervisor_method(supervisor, "close", &[], &mut env)
            .expect("supervisor close should succeed"),
        Value::Unit
    );

    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let cancelled_context = group.child_cancellation();
    group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancelled_context,
    );
    let cancelled_supervisor = ProcessSupervisorValue::new();
    assert_eq!(
        result_ok_payload(
            cancelled_runtime
                .evaluate_process_supervisor_method(
                    cancelled_supervisor.clone(),
                    "start",
                    &[
                        mir_arg(Some("name"), Operand::String("cancelled".to_string())),
                        mir_arg(Some("command"), Operand::Place("sleep_command".to_string())),
                    ],
                    &mut env,
                )
                .expect("cancelled-runtime supervisor start should still register children")
        ),
        Value::Unit
    );
    enum_payloads(
        cancelled_runtime
            .evaluate_process_supervisor_method(
                cancelled_supervisor.clone(),
                "wait",
                &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                &mut env,
            )
            .expect("supervisor wait should observe cancellation"),
        "SupervisorWait",
        "Cancelled",
    );
    enum_payloads(
        cancelled_runtime
            .evaluate_process_supervisor_method(
                cancelled_supervisor.clone(),
                "wait_or_none",
                &[mir_arg(Some("timeout"), Operand::Duration(5_000_000_000))],
                &mut env,
            )
            .expect("cancelled supervisor wait_or_none should return Result.Err"),
        "Result",
        "Err",
    );
    let _ = cancelled_runtime.evaluate_process_supervisor_method(
        cancelled_supervisor.clone(),
        "stop",
        &[],
        &mut env,
    );
    let _ = cancelled_runtime.evaluate_process_supervisor_method(
        cancelled_supervisor,
        "close",
        &[],
        &mut env,
    );
}

#[test]
fn mir_source_filesystem_reports_invalid_utf8_and_sorted_directory_entries() {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let temp_root = std::env::temp_dir().join(format!(
        "aura-mir-source-fs-{}-{timestamp}",
        std::process::id()
    ));
    let entries_path = temp_root.join("entries");
    std::fs::create_dir_all(&entries_path).expect("source filesystem directory should be created");
    std::fs::write(temp_root.join("invalid.bin"), [0xff, 0xfe])
        .expect("invalid UTF-8 fixture should be written");
    std::fs::write(entries_path.join("zeta.txt"), "z").expect("zeta fixture should be written");
    std::fs::write(entries_path.join("alpha.txt"), "a").expect("alpha fixture should be written");

    let source = format!(
        r#"import fs
import io

def main() -> int32:
    match fs.read_to_string("{}"):
        case Result.Err(io.Error.InvalidData):
            print("invalid-data")
        case Result.Err(error):
            print(error)
            return 1
        case Result.Ok(_):
            return 2

    match fs.read_dir("{}"):
        case Result.Ok(entries):
            print(entries)
            return 0
        case Result.Err(error):
            print(error)
            return 3
"#,
        temp_root.join("invalid.bin").display(),
        entries_path.display(),
    );
    let output = crate::run_source(&source)
        .unwrap_or_else(|error| panic!("filesystem source should run through MIR: {error}"));
    assert_eq!(output.value, Value::Int(IntegerValue::zero()));
    assert_eq!(output.stdout, "invalid-data\n[alpha.txt, zeta.txt]\n");

    std::fs::remove_dir_all(&temp_root).expect("source filesystem fixtures should be removed");
}

#[test]
fn mir_runtime_builtin_io_calls_cover_process_filesystem_and_network_paths() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let stdio_null = call_name(&mut runtime, "process::null", &[], &mut env)
        .expect("process.null() should succeed");
    let stdio_pipe = call_name(&mut runtime, "process::pipe", &[], &mut env)
        .expect("process.pipe() should succeed");
    let stdio_inherit = call_name(&mut runtime, "process::inherit", &[], &mut env)
        .expect("process.inherit() should succeed");
    assert!(matches!(
        call_name(&mut runtime, "process::supervisor", &[], &mut env)
            .expect("process.supervisor() should succeed"),
        Value::ProcessSupervisor(_)
    ));

    env.define_typed(
        "empty_cmd",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        string_vec_value(&[]),
    );
    env.define_typed(
        "child_cmd",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        string_vec_value(&["/bin/sh", "-c", "printf child"]),
    );
    env.define_typed("cwd", Type::named("Option[str]"), option_none());
    env.define_typed(
        "env_map",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("str")],
        ),
        string_map_value(&[]),
    );
    env.define_typed(
        "stdio_null",
        Type::named("process.Stdio"),
        stdio_null.clone(),
    );
    env.define_typed(
        "stdio_pipe",
        Type::named("process.Stdio"),
        stdio_pipe.clone(),
    );
    env.define_typed("stdio_inherit", Type::named("process.Stdio"), stdio_inherit);

    let start_no_command = call_name(
        &mut runtime,
        "process::start",
        &[
            mir_arg(Some("command"), Operand::Place("empty_cmd".to_string())),
            mir_arg(Some("cwd"), Operand::Place("cwd".to_string())),
            mir_arg(Some("env"), Operand::Place("env_map".to_string())),
            mir_arg(Some("stdin"), Operand::Place("stdio_null".to_string())),
            mir_arg(Some("stdout"), Operand::Place("stdio_null".to_string())),
            mir_arg(Some("stderr"), Operand::Place("stdio_null".to_string())),
            mir_arg(Some("group"), Operand::Bool(false)),
        ],
        &mut env,
    )
    .expect("process.start should return a Result for empty commands");
    let start_error = enum_payloads(start_no_command, "Result", "Err").remove(0);
    enum_payloads(start_error, "Error", "NoCommand");

    let child = result_ok_payload(
        call_name(
            &mut runtime,
            "process::start",
            &[
                mir_arg(Some("command"), Operand::Place("child_cmd".to_string())),
                mir_arg(Some("cwd"), Operand::Place("cwd".to_string())),
                mir_arg(Some("env"), Operand::Place("env_map".to_string())),
                mir_arg(Some("stdin"), Operand::Place("stdio_null".to_string())),
                mir_arg(Some("stdout"), Operand::Place("stdio_pipe".to_string())),
                mir_arg(Some("stderr"), Operand::Place("stdio_null".to_string())),
                mir_arg(Some("group"), Operand::Bool(false)),
            ],
            &mut env,
        )
        .expect("process.start should spawn a child"),
    );
    match child {
        Value::ProcessChild(child) => child.close(),
        other => panic!("expected process child, found {other:?}"),
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let temp_root = std::env::temp_dir().join(format!(
        "aura-mir-builtin-{}-{timestamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temp_root).expect("temp root should be created");
    let read_path = temp_root.join("read.txt");
    let write_path = temp_root.join("write.txt");
    let bytes_path = temp_root.join("bytes.bin");
    let dir_path = temp_root.join("items");
    std::fs::write(&read_path, "hello").expect("read fixture should be written");

    env.define_typed(
        "read_path",
        Type::named("str"),
        Value::String(read_path.to_string_lossy().into_owned()),
    );
    env.define_typed(
        "write_path",
        Type::named("str"),
        Value::String(write_path.to_string_lossy().into_owned()),
    );
    env.define_typed(
        "bytes_path",
        Type::named("str"),
        Value::String(bytes_path.to_string_lossy().into_owned()),
    );
    env.define_typed(
        "dir_path",
        Type::named("str"),
        Value::String(dir_path.to_string_lossy().into_owned()),
    );
    env.define_typed(
        "text",
        Type::named("str"),
        Value::String("hello".to_string()),
    );
    env.define_typed(
        "suffix",
        Type::named("str"),
        Value::String("-again".to_string()),
    );
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(b"ab".to_vec()),
    );

    assert_eq!(
        call_name(
            &mut runtime,
            "fs::exists",
            &[mir_arg(
                Some("path"),
                Operand::Place("read_path".to_string())
            )],
            &mut env,
        )
        .expect("fs.exists should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::read_to_string",
                &[mir_arg(
                    Some("path"),
                    Operand::Place("read_path".to_string())
                )],
                &mut env,
            )
            .expect("fs.read_to_string should succeed")
        ),
        Value::String("hello".to_string())
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::read_bytes",
                &[mir_arg(
                    Some("path"),
                    Operand::Place("read_path".to_string())
                )],
                &mut env,
            )
            .expect("fs.read_bytes should succeed")
        ),
        bytes_vec_value(b"hello".to_vec())
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::write_string",
                &[
                    mir_arg(Some("path"), Operand::Place("write_path".to_string())),
                    mir_arg(Some("text"), Operand::Place("text".to_string())),
                ],
                &mut env,
            )
            .expect("fs.write_string should succeed")
        ),
        Value::Unit
    );
    let write_text_error = call_name(
        &mut runtime,
        "fs::write_string",
        &[
            mir_arg(Some("path"), Operand::Place("write_path".to_string())),
            mir_arg(Some("text"), Operand::Int(7)),
        ],
        &mut env,
    )
    .expect_err("fs.write_string should reject non-string text");
    assert!(write_text_error
        .message
        .contains("expects `str` for `text`"));
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::append_string",
                &[
                    mir_arg(Some("path"), Operand::Place("write_path".to_string())),
                    mir_arg(Some("text"), Operand::Place("suffix".to_string())),
                ],
                &mut env,
            )
            .expect("fs.append_string should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        std::fs::read_to_string(&write_path).expect("write fixture should be readable"),
        "hello-again"
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::write_bytes",
                &[
                    mir_arg(Some("path"), Operand::Place("bytes_path".to_string())),
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                ],
                &mut env,
            )
            .expect("fs.write_bytes should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::append_bytes",
                &[
                    mir_arg(Some("path"), Operand::Place("bytes_path".to_string())),
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                ],
                &mut env,
            )
            .expect("fs.append_bytes should succeed")
        ),
        Value::Unit
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::read_bytes",
                &[mir_arg(
                    Some("path"),
                    Operand::Place("bytes_path".to_string())
                )],
                &mut env,
            )
            .expect("fs.read_bytes should read appended bytes")
        ),
        bytes_vec_value(b"abab".to_vec())
    );
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::create_dir",
                &[mir_arg(
                    Some("path"),
                    Operand::Place("dir_path".to_string())
                )],
                &mut env,
            )
            .expect("fs.create_dir should succeed")
        ),
        Value::Unit
    );
    assert!(matches!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::read_dir",
                &[mir_arg(
                    Some("path"),
                    Operand::Place("dir_path".to_string())
                )],
                &mut env,
            )
            .expect("fs.read_dir should succeed")
        ),
        Value::Vec(_)
    ));
    assert_eq!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "fs::remove_file",
                &[mir_arg(
                    Some("path"),
                    Operand::Place("read_path".to_string())
                )],
                &mut env,
            )
            .expect("fs.remove_file should succeed")
        ),
        Value::Unit
    );
    for builtin in ["fs::open", "fs::create", "fs::append"] {
        match result_ok_payload(
            call_name(
                &mut runtime,
                builtin,
                &[mir_arg(
                    Some("path"),
                    Operand::Place("write_path".to_string()),
                )],
                &mut env,
            )
            .expect("file constructor should return a Result"),
        ) {
            Value::File(file) => file.close(),
            other => panic!("expected file from {builtin}, found {other:?}"),
        }
    }
    let open_type_error = call_name(
        &mut runtime,
        "fs::open",
        &[mir_arg(Some("path"), Operand::Bool(false))],
        &mut env,
    )
    .expect_err("fs.open should reject non-string paths");
    assert!(open_type_error.message.contains("expects `str`"));

    let listener = result_ok_payload(
        call_name(
            &mut runtime,
            "net::listen",
            &[mir_arg(
                Some("address"),
                Operand::String("127.0.0.1:0".to_string()),
            )],
            &mut env,
        )
        .expect("net.listen should return a Result"),
    );
    let tcp_listener = match listener {
        Value::TcpListener(listener) => listener,
        other => panic!("expected tcp listener, found {other:?}"),
    };
    let tcp_address = tcp_listener
        .local_addr()
        .expect("tcp listener address should be available");
    env.define_typed(
        "tcp_address",
        Type::named("str"),
        Value::String(tcp_address),
    );
    let tcp_server = {
        let listener = tcp_listener.clone();
        thread::spawn(move || {
            for _ in 0..2 {
                let stream = listener
                    .accept(
                        Some(StdDuration::from_secs(2)),
                        Some(&CancellationContext::default()),
                    )
                    .expect("tcp server should accept");
                stream.close();
            }
        })
    };
    for builtin in ["net::connect", "net::connect_timeout"] {
        let args = if builtin == "net::connect" {
            vec![mir_arg(
                Some("address"),
                Operand::Place("tcp_address".to_string()),
            )]
        } else {
            vec![
                mir_arg(Some("address"), Operand::Place("tcp_address".to_string())),
                mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
            ]
        };
        match result_ok_payload(
            call_name(&mut runtime, builtin, &args, &mut env)
                .expect("tcp connect builtin should return a Result"),
        ) {
            Value::TcpStream(stream) => stream.close(),
            other => panic!("expected tcp stream from {builtin}, found {other:?}"),
        }
    }
    tcp_server.join().expect("tcp server should join");
    tcp_listener.close();
    let connect_type_error = call_name(
        &mut runtime,
        "net::connect",
        &[mir_arg(Some("address"), Operand::Bool(true))],
        &mut env,
    )
    .expect_err("net.connect should reject non-string addresses");
    assert!(connect_type_error.message.contains("expects `str`"));

    match result_ok_payload(
        call_name(
            &mut runtime,
            "net::udp_bind",
            &[mir_arg(
                Some("address"),
                Operand::String("127.0.0.1:0".to_string()),
            )],
            &mut env,
        )
        .expect("net.udp_bind should return a Result"),
    ) {
        Value::UdpSocket(socket) => socket.close(),
        other => panic!("expected udp socket, found {other:?}"),
    }

    #[cfg(unix)]
    {
        let socket_path = std::path::PathBuf::from(format!(
            "/tmp/aumir-{}-{}.sock",
            std::process::id(),
            timestamp % 1_000_000
        ));
        let _ = std::fs::remove_file(&socket_path);
        let socket_text = socket_path.to_string_lossy().into_owned();
        let unix_listener = result_ok_payload(
            call_name(
                &mut runtime,
                "net::unix_listen",
                &[mir_arg(Some("path"), Operand::String(socket_text.clone()))],
                &mut env,
            )
            .expect("net.unix_listen should return a Result"),
        );
        let unix_listener = match unix_listener {
            Value::UnixListener(listener) => listener,
            other => panic!("expected unix listener, found {other:?}"),
        };
        let unix_server = {
            let listener = unix_listener.clone();
            thread::spawn(move || {
                for _ in 0..2 {
                    let stream = listener
                        .accept(
                            Some(StdDuration::from_secs(2)),
                            Some(&CancellationContext::default()),
                        )
                        .expect("unix server should accept");
                    stream.close();
                }
            })
        };
        for builtin in ["net::unix_connect", "net::unix_connect_timeout"] {
            let args = if builtin == "net::unix_connect" {
                vec![mir_arg(Some("path"), Operand::String(socket_text.clone()))]
            } else {
                vec![
                    mir_arg(Some("path"), Operand::String(socket_text.clone())),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ]
            };
            match result_ok_payload(
                call_name(&mut runtime, builtin, &args, &mut env)
                    .expect("unix connect builtin should return a Result"),
            ) {
                Value::UnixStream(stream) => stream.close(),
                other => panic!("expected unix stream from {builtin}, found {other:?}"),
            }
        }
        unix_server.join().expect("unix server should join");
        unix_listener.close();
        let _ = std::fs::remove_file(&socket_path);
    }

    let http_listener = result_ok_payload(
        call_name(
            &mut runtime,
            "net::http_listen",
            &[mir_arg(
                Some("address"),
                Operand::String("127.0.0.1:0".to_string()),
            )],
            &mut env,
        )
        .expect("net.http_listen should return a Result"),
    );
    let http_listener = match http_listener {
        Value::HttpListener(listener) => listener,
        other => panic!("expected http listener, found {other:?}"),
    };
    let http_address = http_listener
        .local_addr()
        .expect("http listener address should be available");
    let http_server = {
        let listener = http_listener.clone();
        thread::spawn(move || {
            for response_text in ["text-ok", "text-timeout-ok", "bytes-ok", "bytes-timeout-ok"] {
                let exchange = listener
                    .accept(
                        Some(StdDuration::from_secs(2)),
                        Some(&CancellationContext::default()),
                    )
                    .expect("http server should accept");
                exchange
                    .respond_text(200, response_text, Vec::new())
                    .expect("http response should write");
            }
        })
    };
    env.define_typed(
        "http_url",
        Type::named("str"),
        Value::String(format!("http://{http_address}/builtin")),
    );
    env.define_typed(
        "headers",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("str")],
        ),
        string_map_value(&[("Content-Type", "text/plain")]),
    );
    assert!(matches!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "net::http_request_text",
                &[
                    mir_arg(Some("method"), Operand::String("POST".to_string())),
                    mir_arg(Some("url"), Operand::Place("http_url".to_string())),
                    mir_arg(Some("body"), Operand::String("body".to_string())),
                    mir_arg(Some("headers"), Operand::Place("headers".to_string())),
                ],
                &mut env,
            )
            .expect("http text request should return a Result")
        ),
        Value::HttpResponse(_)
    ));
    assert!(matches!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "net::http_request_text_timeout",
                &[
                    mir_arg(Some("method"), Operand::String("POST".to_string())),
                    mir_arg(Some("url"), Operand::Place("http_url".to_string())),
                    mir_arg(Some("body"), Operand::String("body".to_string())),
                    mir_arg(Some("headers"), Operand::Place("headers".to_string())),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("http text timeout request should return a Result")
        ),
        Value::HttpResponse(_)
    ));
    assert!(matches!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "net::http_request_bytes",
                &[
                    mir_arg(Some("method"), Operand::String("POST".to_string())),
                    mir_arg(Some("url"), Operand::Place("http_url".to_string())),
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                    mir_arg(Some("headers"), Operand::Place("headers".to_string())),
                ],
                &mut env,
            )
            .expect("http bytes request should return a Result")
        ),
        Value::HttpResponse(_)
    ));
    assert!(matches!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "net::http_request_bytes_timeout",
                &[
                    mir_arg(Some("method"), Operand::String("POST".to_string())),
                    mir_arg(Some("url"), Operand::Place("http_url".to_string())),
                    mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
                    mir_arg(Some("headers"), Operand::Place("headers".to_string())),
                    mir_arg(Some("timeout"), Operand::Duration(1_000_000_000)),
                ],
                &mut env,
            )
            .expect("http bytes request should return a Result")
        ),
        Value::HttpResponse(_)
    ));
    http_server.join().expect("http server should join");
    http_listener.close();

    for builtin in [
        "net::tls_listen",
        "net::tls_connect",
        "net::tls_connect_timeout",
    ] {
        let args = if builtin == "net::tls_listen" {
            vec![
                mir_arg(Some("address"), Operand::String("127.0.0.1:0".to_string())),
                mir_arg(
                    Some("cert_pem_path"),
                    Operand::String("missing-cert.pem".to_string()),
                ),
                mir_arg(
                    Some("key_pem_path"),
                    Operand::String("missing-key.pem".to_string()),
                ),
            ]
        } else if builtin == "net::tls_connect" {
            vec![
                mir_arg(Some("address"), Operand::String("127.0.0.1:9".to_string())),
                mir_arg(
                    Some("server_name"),
                    Operand::String("localhost".to_string()),
                ),
                mir_arg(
                    Some("ca_pem_path"),
                    Operand::String("missing-ca.pem".to_string()),
                ),
            ]
        } else {
            vec![
                mir_arg(Some("address"), Operand::String("127.0.0.1:9".to_string())),
                mir_arg(
                    Some("server_name"),
                    Operand::String("localhost".to_string()),
                ),
                mir_arg(
                    Some("ca_pem_path"),
                    Operand::String("missing-ca.pem".to_string()),
                ),
                mir_arg(Some("timeout"), Operand::Duration(1_000_000)),
            ]
        };
        enum_payloads(
            call_name(&mut runtime, builtin, &args, &mut env)
                .expect("tls builtin should return a Result"),
            "Result",
            "Err",
        );
    }

    assert!(matches!(
        result_ok_payload(
            call_name(
                &mut runtime,
                "net::websocket_listen",
                &[mir_arg(
                    Some("address"),
                    Operand::String("127.0.0.1:0".to_string()),
                )],
                &mut env,
            )
            .expect("websocket listen should return a Result")
        ),
        Value::WebSocketListener(_)
    ));
    for builtin in ["net::websocket_connect", "net::websocket_connect_timeout"] {
        let args = if builtin == "net::websocket_connect" {
            vec![mir_arg(
                Some("url"),
                Operand::String("not a websocket url".to_string()),
            )]
        } else {
            vec![
                mir_arg(
                    Some("url"),
                    Operand::String("not a websocket url".to_string()),
                ),
                mir_arg(Some("timeout"), Operand::Duration(1_000_000)),
            ]
        };
        enum_payloads(
            call_name(&mut runtime, builtin, &args, &mut env)
                .expect("websocket connect builtin should return a Result"),
            "Result",
            "Err",
        );
    }

    let unknown = runtime
        .evaluate_builtin_io_call("unknown::call", Vec::new())
        .expect_err("unknown builtin I/O calls should report diagnostics");
    assert!(unknown.message.contains("unsupported builtin I/O call"));
    let _ = std::fs::remove_dir_all(&temp_root);
}

#[test]
fn mir_runtime_builtin_io_error_results_cover_filesystem_and_network_edges() {
    fn expect_result_err(runtime: &mut MirRuntime, env: &mut Env, name: &str, args: Vec<MirArg>) {
        enum_payloads(
            call_name(runtime, name, &args, env)
                .unwrap_or_else(|error| panic!("{name} should return Result.Err: {error:?}")),
            "Result",
            "Err",
        );
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let temp_root = std::env::temp_dir().join(format!(
        "aura-mir-runtime-io-errors-{timestamp}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&temp_root);
    std::fs::create_dir_all(&temp_root).expect("temp root should be created");
    let occupied_file = temp_root.join("occupied.txt");
    std::fs::write(&occupied_file, "occupied").expect("occupied file should be written");
    let missing_file = temp_root.join("missing.txt");

    let dir_path = temp_root.to_string_lossy().into_owned();
    let occupied_path = occupied_file.to_string_lossy().into_owned();
    let missing_path = missing_file.to_string_lossy().into_owned();
    env.define_typed(
        "bytes",
        Type::Named("list".to_string(), vec![Type::named("uint8")]),
        bytes_vec_value(b"bytes".to_vec()),
    );

    let write_path_error = call_name(
        &mut runtime,
        "fs::write_string",
        &[
            mir_arg(Some("path"), Operand::Bool(false)),
            mir_arg(Some("text"), Operand::String("text".to_string())),
        ],
        &mut env,
    )
    .expect_err("fs.write_string should reject non-string paths");
    assert!(write_path_error
        .message
        .contains("expects `str` for `path`"));

    expect_result_err(
        &mut runtime,
        &mut env,
        "fs::write_string",
        vec![
            mir_arg(Some("path"), Operand::String(dir_path.clone())),
            mir_arg(Some("text"), Operand::String("text".to_string())),
        ],
    );
    expect_result_err(
        &mut runtime,
        &mut env,
        "fs::write_bytes",
        vec![
            mir_arg(Some("path"), Operand::String(dir_path.clone())),
            mir_arg(Some("bytes"), Operand::Place("bytes".to_string())),
        ],
    );
    expect_result_err(
        &mut runtime,
        &mut env,
        "fs::create_dir",
        vec![mir_arg(
            Some("path"),
            Operand::String(occupied_path.clone()),
        )],
    );
    expect_result_err(
        &mut runtime,
        &mut env,
        "fs::read_dir",
        vec![mir_arg(
            Some("path"),
            Operand::String(occupied_path.clone()),
        )],
    );
    expect_result_err(
        &mut runtime,
        &mut env,
        "fs::open",
        vec![mir_arg(Some("path"), Operand::String(missing_path.clone()))],
    );

    for builtin in [
        "net::connect",
        "net::connect_timeout",
        "net::listen",
        "net::udp_bind",
    ] {
        let mut args = vec![mir_arg(
            Some("address"),
            Operand::String("not a socket address".to_string()),
        )];
        if builtin == "net::connect_timeout" {
            args.push(mir_arg(Some("timeout"), Operand::Duration(1_000_000)));
        }
        expect_result_err(&mut runtime, &mut env, builtin, args);
    }

    let listen_type_error = call_name(
        &mut runtime,
        "net::listen",
        &[mir_arg(Some("address"), Operand::Bool(false))],
        &mut env,
    )
    .expect_err("net.listen should reject non-string addresses");
    assert!(listen_type_error.message.contains("expects `str`"));

    #[cfg(unix)]
    {
        expect_result_err(
            &mut runtime,
            &mut env,
            "net::unix_listen",
            vec![mir_arg(
                Some("path"),
                Operand::String(occupied_path.clone()),
            )],
        );
        expect_result_err(
            &mut runtime,
            &mut env,
            "net::unix_connect",
            vec![mir_arg(Some("path"), Operand::String(missing_path.clone()))],
        );
        expect_result_err(
            &mut runtime,
            &mut env,
            "net::unix_connect_timeout",
            vec![
                mir_arg(Some("path"), Operand::String(missing_path.clone())),
                mir_arg(Some("timeout"), Operand::Duration(1_000_000)),
            ],
        );
    }

    let _ = std::fs::remove_dir_all(&temp_root);
}

#[test]
fn mir_runtime_process_run_builtin_captures_stdio_under_scheduler() {
    let output = crate::runtime_value::run_lightweight_root_task(|| {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        env.define_typed(
            "run_cmd",
            Type::Named("list".to_string(), vec![Type::named("str")]),
            string_vec_value(&["/bin/sh", "-c", "printf out; printf err >&2"]),
        );
        env.define_typed("cwd", Type::named("Option[str]"), option_none());
        env.define_typed(
            "env_map",
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            ),
            string_map_value(&[]),
        );
        let stdio_null = call_name(&mut runtime, "process::null", &[], &mut env)?;
        let stdout_pipe = call_name(&mut runtime, "process::pipe", &[], &mut env)?;
        let stderr_pipe = call_name(&mut runtime, "process::pipe", &[], &mut env)?;
        env.define_typed("stdio_null", Type::named("process.Stdio"), stdio_null);
        env.define_typed("stdout_pipe", Type::named("process.Stdio"), stdout_pipe);
        env.define_typed("stderr_pipe", Type::named("process.Stdio"), stderr_pipe);
        call_name(
            &mut runtime,
            "process::run",
            &[
                mir_arg(Some("command"), Operand::Place("run_cmd".to_string())),
                mir_arg(Some("cwd"), Operand::Place("cwd".to_string())),
                mir_arg(Some("env"), Operand::Place("env_map".to_string())),
                mir_arg(Some("stdin"), Operand::Place("stdio_null".to_string())),
                mir_arg(Some("stdout"), Operand::Place("stdout_pipe".to_string())),
                mir_arg(Some("stderr"), Operand::Place("stderr_pipe".to_string())),
                mir_arg(Some("timeout"), Operand::Duration(2_000_000_000)),
                mir_arg(Some("group"), Operand::Bool(false)),
            ],
            &mut env,
        )
    })
    .expect("process.run should execute inside the lightweight scheduler");

    let completed = match result_ok_payload(output) {
        Value::ProcessCompleted(completed) => completed,
        other => panic!("expected process completed value, found {other:?}"),
    };
    assert_eq!(completed.stdout_bytes(), b"out".to_vec());
    assert_eq!(completed.stderr_bytes(), b"err".to_vec());
}

#[test]
fn mir_runtime_process_builtins_cover_spawn_timeout_and_cancelled_edges() {
    fn expect_process_error_variant(value: Value, variant_name: &str) {
        let error = enum_payloads(value, "Result", "Err").remove(0);
        enum_payloads(error, "Error", variant_name);
    }

    fn install_process_env(runtime: &mut MirRuntime, env: &mut Env, command: &[&str]) {
        env.define_typed(
            "command",
            Type::Named("list".to_string(), vec![Type::named("str")]),
            string_vec_value(command),
        );
        env.define_typed("cwd", Type::named("Option[str]"), option_none());
        env.define_typed(
            "env_map",
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            ),
            string_map_value(&[]),
        );
        let stdio_null = call_name(runtime, "process::null", &[], env)
            .expect("process.null should construct stdio");
        env.define_typed("stdio_null", Type::named("process.Stdio"), stdio_null);
    }

    fn process_args(timeout: Option<i128>) -> Vec<MirArg> {
        let mut args = vec![
            mir_arg(Some("command"), Operand::Place("command".to_string())),
            mir_arg(Some("cwd"), Operand::Place("cwd".to_string())),
            mir_arg(Some("env"), Operand::Place("env_map".to_string())),
            mir_arg(Some("stdin"), Operand::Place("stdio_null".to_string())),
            mir_arg(Some("stdout"), Operand::Place("stdio_null".to_string())),
            mir_arg(Some("stderr"), Operand::Place("stdio_null".to_string())),
        ];
        if let Some(timeout) = timeout {
            args.push(mir_arg(Some("timeout"), Operand::Duration(timeout)));
        }
        args.push(mir_arg(Some("group"), Operand::Bool(false)));
        args
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    install_process_env(
        &mut runtime,
        &mut env,
        &["/__definitely_missing_aura_process_builtin__"],
    );
    expect_process_error_variant(
        call_name(
            &mut runtime,
            "process::start",
            &process_args(None),
            &mut env,
        )
        .expect("process.start spawn failures should return Result.Err"),
        "Spawn",
    );
    expect_process_error_variant(
        call_name(
            &mut runtime,
            "process::run",
            &process_args(Some(1_000_000_000)),
            &mut env,
        )
        .expect("process.run spawn failures should return Result.Err"),
        "Spawn",
    );

    let mut timeout_runtime = test_runtime();
    let mut timeout_env = Env::default();
    install_process_env(
        &mut timeout_runtime,
        &mut timeout_env,
        &["/bin/sh", "-c", "sleep 1"],
    );
    expect_process_error_variant(
        call_name(
            &mut timeout_runtime,
            "process::run",
            &process_args(Some(0)),
            &mut timeout_env,
        )
        .expect("process.run timeouts should return Result.Err"),
        "TimedOut",
    );

    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let cancelled_context = group.child_cancellation();
    group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancelled_context,
    );
    let mut cancelled_env = Env::default();
    install_process_env(
        &mut cancelled_runtime,
        &mut cancelled_env,
        &["/bin/sh", "-c", "sleep 1"],
    );
    expect_process_error_variant(
        call_name(
            &mut cancelled_runtime,
            "process::run",
            &process_args(Some(1_000_000_000)),
            &mut cancelled_env,
        )
        .expect("process.run cancellations should return Result.Err"),
        "Cancelled",
    );
}

#[test]
fn mir_runtime_member_call_dispatch_covers_builtin_runtime_and_trait_receivers() {
    let mut runtime = test_runtime();
    runtime.functions.insert(
        "widget_render".to_string(),
        MirFunction {
            name: "widget_render".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("str"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(Operand::String("widget".to_string())),
            }],
        },
    );
    runtime.functions.insert(
        "status_label".to_string(),
        MirFunction {
            name: "status_label".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::named("str"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(Operand::String("done".to_string())),
            }],
        },
    );
    runtime.classes.insert(
        "Widget".to_string(),
        MirClass {
            name: "Widget".to_string(),
            type_params: Vec::new(),
            fields: Vec::new(),
            methods: vec![MirMethod {
                name: "render".to_string(),
                function_name: "widget_render".to_string(),
                receiver: Some(crate::mir::MirReceiverKind::Borrow),
            }],
        },
    );
    runtime.trait_impls.push(MirTraitImpl {
        trait_name: "Label".to_string(),
        trait_args: Vec::new(),
        for_type: Type::named("Status"),
        methods: vec![MirMethod {
            name: "label".to_string(),
            function_name: "status_label".to_string(),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
        }],
    });

    let mut env = Env::default();
    env.define_typed(
        "number",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(7)),
    );
    env.define_typed("ratio", Type::named("float64"), Value::Float(4.0));
    env.define_typed("flag", Type::named("bool"), Value::Bool(true));
    env.define_typed(
        "text",
        Type::named("str"),
        Value::String("Aura".to_string()),
    );
    env.define_typed(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: vec![
                Value::Int(IntegerValue::from_signed(1)),
                Value::Int(IntegerValue::from_signed(2)),
            ],
        }),
    );
    env.define_typed(
        "counts",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        Value::Map(crate::runtime_value::MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![(
                Value::String("count".to_string()),
                Value::Int(IntegerValue::from_signed(1)),
            )],
        }),
    );
    env.define_typed(
        "seen",
        Type::Named("set".to_string(), vec![Type::named("str")]),
        Value::Set(crate::runtime_value::SetValue {
            element_type: Type::named("str"),
            elements: vec![Value::String("ready".to_string())],
        }),
    );
    env.define_typed(
        "jobs",
        Type::Named("Queue".to_string(), vec![Type::named("int32")]),
        Value::Channel(ChannelValue::new()),
    );
    env.define_typed(
        "task",
        Type::Named("Task".to_string(), vec![Type::named("bool")]),
        Value::Task(TaskValue::from_handle(std::thread::spawn(|| {
            Ok(Value::Bool(true))
        }))),
    );
    env.define_typed(
        "group",
        Type::named("TaskGroup"),
        Value::TaskGroup(TaskGroupValue::new(&CancellationContext::default())),
    );
    env.define_typed(
        "widget",
        Type::named("Widget"),
        Value::Instance(InstanceValue {
            class_name: "Widget".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    env.define_typed(
        "status",
        Type::named("Status"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Done".to_string(),
            payloads: Vec::new(),
        }),
    );
    env.define_typed("unit", Type::Unit, Value::Unit);

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("ratio".to_string()),
                    field: "sqrt".to_string(),
                    receiver_place: Some("ratio".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("float.sqrt() should succeed"),
        Value::Float(2.0)
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("number".to_string()),
                    field: "to_string".to_string(),
                    receiver_place: Some("number".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("int.to_string() should succeed"),
        Value::String("7".to_string())
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("flag".to_string()),
                    field: "to_string".to_string(),
                    receiver_place: Some("flag".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("bool.to_string() should succeed"),
        Value::String("true".to_string())
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("text".to_string()),
                    field: "contains".to_string(),
                    receiver_place: Some("text".to_string()),
                },
                &[mir_arg(None, Operand::String("ur".to_string()))],
                &mut env,
            )
            .expect("string member calls should dispatch"),
        Value::Bool(true)
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("values".to_string()),
                    field: "insert".to_string(),
                    receiver_place: Some("values".to_string()),
                },
                &[
                    mir_arg(None, Operand::Int(1)),
                    mir_arg(None, Operand::Int(9)),
                ],
                &mut env,
            )
            .expect("vec member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("counts".to_string()),
                    field: "clear".to_string(),
                    receiver_place: Some("counts".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("map member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("seen".to_string()),
                    field: "add".to_string(),
                    receiver_place: Some("seen".to_string()),
                },
                &[mir_arg(None, Operand::String("go".to_string()))],
                &mut env,
            )
            .expect("set member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("jobs".to_string()),
                    field: "close".to_string(),
                    receiver_place: Some("jobs".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("channel member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("task".to_string()),
                    field: "result".to_string(),
                    receiver_place: Some("task".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("task member calls should dispatch"),
        task_result_ready(Value::Bool(true))
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("group".to_string()),
                    field: "cancel".to_string(),
                    receiver_place: Some("group".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("task-group member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("widget".to_string()),
                    field: "render".to_string(),
                    receiver_place: Some("widget".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("class member calls should dispatch"),
        Value::String("widget".to_string())
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("status".to_string()),
                    field: "label".to_string(),
                    receiver_place: Some("status".to_string()),
                },
                &[],
                &mut env,
            )
            .expect("runtime type fallback trait dispatch should succeed"),
        Value::String("done".to_string())
    );

    let unsupported = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("unit".to_string()),
                field: "missing".to_string(),
                receiver_place: Some("unit".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("unsupported members should fail cleanly");
    assert!(unsupported
        .message
        .contains("unsupported MIR member call `missing`"));
}

#[test]
fn mir_runtime_builtin_error_surface_covers_additional_builtin_branches() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "huge_unsigned",
        Type::named("uint128"),
        Value::Int(IntegerValue::from_literal((i128::MAX as u128) + 1)),
    );
    env.define_typed(
        "min_signed",
        Type::named("int128"),
        Value::Int(IntegerValue::from_signed(i128::MIN)),
    );
    env.define_typed(
        "word",
        Type::named("str"),
        Value::String("aura".to_string()),
    );
    env.define_typed("flag", Type::named("bool"), Value::Bool(true));
    env.define_typed(
        "negative_duration",
        Type::named("Duration"),
        Value::Duration(-1),
    );

    let sleep_range = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("sleep".to_string()),
            &[mir_arg(
                None,
                Operand::Place("negative_duration".to_string()),
            )],
            &mut env,
        )
        .expect_err("sleep() should reject negative durations");
    assert!(sleep_range
        .message
        .contains("sleep(...) must be non-negative"));

    let sleep_unsigned_range = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("sleep".to_string()),
            &[mir_arg(None, Operand::Place("huge_unsigned".to_string()))],
            &mut env,
        )
        .expect_err("sleep() should reject unsigned values outside signed timer range");
    assert!(sleep_unsigned_range
        .message
        .contains("`sleep(...)` expects a duration value"));

    let sleep_integer = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("sleep".to_string()),
            &[mir_arg(None, Operand::Int(0))],
            &mut env,
        )
        .expect_err("sleep() should reject untyped integer durations");
    assert!(sleep_integer
        .message
        .contains("`sleep(...)` expects a duration value"));

    let sleep_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("sleep".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("sleep() should reject non-duration values");
    assert!(sleep_type
        .message
        .contains("expects a duration value in MIR runtime"));

    let abs_overflow = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("abs".to_string()),
            &[mir_arg(None, Operand::Place("min_signed".to_string()))],
            &mut env,
        )
        .expect_err("abs() should reject signed overflow");
    assert!(abs_overflow
        .message
        .contains("overflowed the signed integer range"));

    let abs_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("abs".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("abs() should reject non-numeric values");
    assert!(abs_type
        .message
        .contains("expects an integer or float value"));

    let parse_int64_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("parse_int64".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("parse_int64() should reject non-strings");
    assert!(parse_int64_type
        .message
        .contains("expects `str`, found `true`"));

    let parse_int32_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("parse_int32".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("parse_int32() should reject non-strings");
    assert!(parse_int32_type
        .message
        .contains("expects `str`, found `true`"));

    let parse_float64_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("parse_float64".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("parse_float64() should reject non-strings");
    assert!(parse_float64_type
        .message
        .contains("expects `str`, found `true`"));

    let io_write_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("io::write".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("io.write() should reject non-string text");
    assert!(io_write_type
        .message
        .contains("`io.write(...)` expects `str`, found `true`"));

    let fs_exists_type = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("fs::exists".to_string()),
            &[mir_arg(None, Operand::Place("flag".to_string()))],
            &mut env,
        )
        .expect_err("fs.exists() should reject non-string paths");
    assert!(fs_exists_type
        .message
        .contains("`fs.exists(...)` expects `str`, found `true`"));

    let unknown = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("missing".to_string()),
            &[],
            &mut env,
        )
        .expect_err("unknown MIR functions should fail");
    assert!(unknown.message.contains("unknown MIR function `missing`"));

    let queue_name = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("Queue".to_string()),
            &[MirArg {
                name: Some("size".to_string()),
                value: Operand::Int(1),
                writeback_place: None,
            }],
            &mut env,
        )
        .expect_err("Queue() should reject unknown named arguments");
    assert!(queue_name
        .message
        .contains("expects an optional `capacity=` argument"));

    let queue_capacity = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("Queue".to_string()),
            &[MirArg {
                name: Some("capacity".to_string()),
                value: Operand::Int(0),
                writeback_place: None,
            }],
            &mut env,
        )
        .expect_err("Queue() should reject non-positive capacities");
    assert!(queue_capacity
        .message
        .contains("expects a positive `int32`"));

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Name("parse_int32".to_string()),
                &[mir_arg(None, Operand::Place("word".to_string()))],
                &mut env,
            )
            .expect("parse_int32() should still return Result.Err for invalid strings"),
        result_err(Value::String("invalid digit found in string".to_string()))
    );
}

#[test]
fn mir_runtime_member_error_surface_covers_remaining_dispatch_branches() {
    let mut runtime = test_runtime();
    runtime.classes.insert(
        "Empty".to_string(),
        MirClass {
            name: "Empty".to_string(),
            type_params: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
        },
    );
    runtime.classes.insert(
        "Broken".to_string(),
        MirClass {
            name: "Broken".to_string(),
            type_params: Vec::new(),
            fields: Vec::new(),
            methods: vec![MirMethod {
                name: "render".to_string(),
                function_name: "missing_impl".to_string(),
                receiver: Some(crate::mir::MirReceiverKind::Borrow),
            }],
        },
    );
    runtime.trait_impls.push(MirTraitImpl {
        trait_name: "Render".to_string(),
        trait_args: Vec::new(),
        for_type: Type::named("Status"),
        methods: vec![MirMethod {
            name: "render".to_string(),
            function_name: "missing_trait_impl".to_string(),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
        }],
    });

    let mut env = Env::default();
    env.define_typed("ratio", Type::named("float64"), Value::Float(4.0));
    env.define_typed(
        "number",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(7)),
    );
    env.define_typed("flag", Type::named("bool"), Value::Bool(true));
    env.define_typed(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
    );
    env.define_typed(
        "ghost",
        Type::named("Ghost"),
        Value::Instance(InstanceValue {
            class_name: "Ghost".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    env.define_typed(
        "empty",
        Type::named("Empty"),
        Value::Instance(InstanceValue {
            class_name: "Empty".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    env.define_typed(
        "broken",
        Type::named("Broken"),
        Value::Instance(InstanceValue {
            class_name: "Broken".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    env.define_typed(
        "status",
        Type::named("Status"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: Vec::new(),
        }),
    );

    let sqrt_args = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("ratio".to_string()),
                field: "sqrt".to_string(),
                receiver_place: Some("ratio".to_string()),
            },
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("sqrt() should reject extra arguments");
    assert!(sqrt_args.message.contains("`sqrt` does not take arguments"));

    let int_to_string_args = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("number".to_string()),
                field: "to_string".to_string(),
                receiver_place: Some("number".to_string()),
            },
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("int.to_string() should reject extra arguments");
    assert!(int_to_string_args
        .message
        .contains("`to_string` does not take arguments"));

    let bool_to_string_args = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("flag".to_string()),
                field: "to_string".to_string(),
                receiver_place: Some("flag".to_string()),
            },
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("bool.to_string() should reject extra arguments");
    assert!(bool_to_string_args
        .message
        .contains("`to_string` does not take arguments"));

    let len_args = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("values".to_string()),
                field: "len".to_string(),
                receiver_place: Some("values".to_string()),
            },
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("len() should reject extra arguments");
    assert!(len_args.message.contains("`len` does not take arguments"));

    let push_no_place = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("values".to_string()),
                field: "append".to_string(),
                receiver_place: None,
            },
            &[mir_arg(None, Operand::Int(9))],
            &mut env,
        )
        .expect_err("append() should require a mutable receiver place");
    assert!(push_no_place
        .message
        .contains("requires a mutable list place"));

    let internal_index_args = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("values".to_string()),
                field: "__index".to_string(),
                receiver_place: Some("values".to_string()),
            },
            &[mir_arg(None, Operand::Int(0))],
            &mut env,
        )
        .expect_err("internal __index should enforce operand count");
    assert!(internal_index_args
        .message
        .contains("requires index, line, and column operands"));

    let internal_set_args = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("values".to_string()),
                field: "__set_index".to_string(),
                receiver_place: Some("values".to_string()),
            },
            &[
                mir_arg(None, Operand::Int(0)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(2)),
            ],
            &mut env,
        )
        .expect_err("internal __set_index should enforce operand count");
    assert!(internal_set_args
        .message
        .contains("requires index, value, line, and column operands"));

    let unsupported_vector_method = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("values".to_string()),
                field: "mystery".to_string(),
                receiver_place: Some("values".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("unknown vector methods should fail");
    assert!(unsupported_vector_method
        .message
        .contains("unsupported vector method `mystery`"));

    let unknown_class = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("ghost".to_string()),
                field: "render".to_string(),
                receiver_place: Some("ghost".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("unknown classes should fail");
    assert!(unknown_class.message.contains("unknown MIR class `Ghost`"));

    let missing_class_method = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("empty".to_string()),
                field: "render".to_string(),
                receiver_place: Some("empty".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("missing class methods should fail");
    assert!(missing_class_method
        .message
        .contains("class `Empty` has no MIR method `render`"));

    let missing_method_body = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("broken".to_string()),
                field: "render".to_string(),
                receiver_place: Some("broken".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("missing method bodies should fail");
    assert!(missing_method_body
        .message
        .contains("unknown MIR method body `missing_impl`"));

    let missing_trait_method_body = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("status".to_string()),
                field: "render".to_string(),
                receiver_place: Some("status".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("missing trait method bodies should fail");
    assert!(missing_trait_method_body
        .message
        .contains("unknown MIR method body `missing_trait_impl`"));
}

#[test]
fn mir_runtime_mutating_member_calls_write_back_receivers_and_params() {
    let mut runtime = test_runtime();
    runtime.functions.insert(
        "counter_replace".to_string(),
        MirFunction {
            name: "counter_replace".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
            params: vec![MirParam {
                name: "amount".to_string(),
                passing: crate::mir::MirReceiverKind::BorrowMut,
                ty: Type::named("int32"),
                default_function: None,
            }],
            local_types: Vec::new(),
            return_type: Type::Unit,
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "self.value".to_string(),
                        value: Rvalue::Use(Operand::Int(42)),
                    },
                    Instruction::Assign {
                        target: "amount".to_string(),
                        value: Rvalue::Use(Operand::Int(17)),
                    },
                ],
                terminator: Terminator::Return(Operand::Unit),
            }],
        },
    );
    runtime.functions.insert(
        "counter_borrow_only".to_string(),
        MirFunction {
            name: "counter_borrow_only".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::Unit,
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(Operand::Unit),
            }],
        },
    );
    runtime.functions.insert(
        "status_mark".to_string(),
        MirFunction {
            name: "status_mark".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
            params: vec![MirParam {
                name: "flag".to_string(),
                passing: crate::mir::MirReceiverKind::BorrowMut,
                ty: Type::named("bool"),
                default_function: None,
            }],
            local_types: Vec::new(),
            return_type: Type::Unit,
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![Instruction::Assign {
                    target: "flag".to_string(),
                    value: Rvalue::Use(Operand::Bool(false)),
                }],
                terminator: Terminator::Return(Operand::Unit),
            }],
        },
    );
    runtime.functions.insert(
        "status_borrow_only".to_string(),
        MirFunction {
            name: "status_borrow_only".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
            params: Vec::new(),
            local_types: Vec::new(),
            return_type: Type::Unit,
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Return(Operand::Unit),
            }],
        },
    );
    runtime.classes.insert(
        "Counter".to_string(),
        MirClass {
            name: "Counter".to_string(),
            type_params: Vec::new(),
            fields: vec![crate::mir::MirClassField {
                name: "value".to_string(),
                ty: Type::named("int32"),
            }],
            methods: vec![
                MirMethod {
                    name: "replace".to_string(),
                    function_name: "counter_replace".to_string(),
                    receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
                },
                MirMethod {
                    name: "broken".to_string(),
                    function_name: "counter_borrow_only".to_string(),
                    receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
                },
            ],
        },
    );
    runtime.trait_impls.push(MirTraitImpl {
        trait_name: "Mark".to_string(),
        trait_args: Vec::new(),
        for_type: Type::named("Status"),
        methods: vec![
            MirMethod {
                name: "mark".to_string(),
                function_name: "status_mark".to_string(),
                receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
            },
            MirMethod {
                name: "broken".to_string(),
                function_name: "status_borrow_only".to_string(),
                receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
            },
        ],
    });

    let mut env = Env::default();
    env.define_typed(
        "counter",
        Type::named("Counter"),
        Value::Instance(InstanceValue {
            class_name: "Counter".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(1)),
            )]),
        }),
    );
    env.define_typed(
        "amount",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(3)),
    );
    env.define_typed(
        "status",
        Type::named("Status"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: Vec::new(),
        }),
    );
    env.define_typed("flag", Type::named("bool"), Value::Bool(true));

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("counter".to_string()),
                    field: "replace".to_string(),
                    receiver_place: Some("counter".to_string()),
                },
                &[MirArg {
                    name: None,
                    value: Operand::Place("amount".to_string()),
                    writeback_place: Some("amount".to_string()),
                }],
                &mut env,
            )
            .expect("mutable class member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(
        env.read_place("counter.value"),
        Ok(Value::Int(IntegerValue::from_signed(42)))
    );
    assert_eq!(
        env.read_place("amount"),
        Ok(Value::Int(IntegerValue::from_signed(17)))
    );

    let missing_class_update = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("counter".to_string()),
                field: "broken".to_string(),
                receiver_place: Some("counter".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("mutable class metadata must be matched by function metadata");
    assert!(missing_class_update
        .message
        .contains("mutable MIR method `broken` did not return an updated receiver"));

    assert_eq!(
        runtime
            .evaluate_call(
                &crate::mir::CallTarget::Member {
                    object: Operand::Place("status".to_string()),
                    field: "mark".to_string(),
                    receiver_place: Some("status".to_string()),
                },
                &[MirArg {
                    name: None,
                    value: Operand::Place("flag".to_string()),
                    writeback_place: Some("flag".to_string()),
                }],
                &mut env,
            )
            .expect("mutable trait member calls should dispatch"),
        Value::Unit
    );
    assert_eq!(env.read_place("flag"), Ok(Value::Bool(false)));

    let missing_trait_update = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Member {
                object: Operand::Place("status".to_string()),
                field: "broken".to_string(),
                receiver_place: Some("status".to_string()),
            },
            &[],
            &mut env,
        )
        .expect_err("mutable trait metadata must be matched by function metadata");
    assert!(missing_trait_update
        .message
        .contains("mutable MIR method `broken` did not return an updated receiver"));
}

#[test]
fn mir_runtime_range_and_type_substitution_helpers_cover_remaining_paths() {
    let range = build_range(vec![
        EvaluatedMirArg {
            ty: None,
            name: None,
            value: Value::Int(IntegerValue::from_signed(2)),
            writeback_place: None,
        },
        EvaluatedMirArg {
            ty: None,
            name: None,
            value: Value::Int(IntegerValue::from_signed(5)),
            writeback_place: None,
        },
    ])
    .expect("range should build");
    assert_eq!(range, Value::Range(RangeValue { start: 2, end: 5 }));
    let named_range = build_range(vec![EvaluatedMirArg {
        ty: None,
        name: Some("stop".to_string()),
        value: Value::Int(IntegerValue::from_signed(3)),
        writeback_place: None,
    }])
    .expect("named stop should build range from zero");
    assert_eq!(named_range, Value::Range(RangeValue { start: 0, end: 3 }));
    let range_error = build_range(vec![EvaluatedMirArg {
        ty: None,
        name: Some("unknown".to_string()),
        value: Value::Int(IntegerValue::from_signed(1)),
        writeback_place: None,
    }])
    .expect_err("unknown range argument should fail");
    assert!(range_error.message.contains("unknown MIR `range` argument"));
    let named_start_stop_range = build_range(vec![
        EvaluatedMirArg {
            ty: None,
            name: Some("start".to_string()),
            value: Value::Int(IntegerValue::from_signed(4)),
            writeback_place: None,
        },
        EvaluatedMirArg {
            ty: None,
            name: Some("stop".to_string()),
            value: Value::Int(IntegerValue::from_signed(9)),
            writeback_place: None,
        },
    ])
    .expect("named start and stop should build range");
    assert_eq!(
        named_start_stop_range,
        Value::Range(RangeValue { start: 4, end: 9 })
    );
    let non_int_range_error = build_range(vec![EvaluatedMirArg {
        ty: None,
        name: None,
        value: Value::String("5".to_string()),
        writeback_place: None,
    }])
    .expect_err("range should reject non-integer arguments");
    assert!(non_int_range_error
        .message
        .contains("requires integer arguments"));
    let too_many_range_args = build_range(vec![
        EvaluatedMirArg {
            ty: None,
            name: None,
            value: Value::Int(IntegerValue::from_signed(1)),
            writeback_place: None,
        },
        EvaluatedMirArg {
            ty: None,
            name: None,
            value: Value::Int(IntegerValue::from_signed(2)),
            writeback_place: None,
        },
        EvaluatedMirArg {
            ty: None,
            name: None,
            value: Value::Int(IntegerValue::from_signed(3)),
            writeback_place: None,
        },
    ])
    .expect_err("range should reject more than two positional arguments");
    assert!(too_many_range_args
        .message
        .contains("takes at most two arguments"));
    let missing_stop_range = build_range(vec![EvaluatedMirArg {
        ty: None,
        name: Some("start".to_string()),
        value: Value::Int(IntegerValue::from_signed(1)),
        writeback_place: None,
    }])
    .expect_err("range should require a stop endpoint");
    assert!(missing_stop_range.message.contains("requires `stop`"));

    let mut substitutions = HashMap::new();
    collect_runtime_type_substitutions(
        &Type::Named(
            "dict".to_string(),
            vec![
                Type::TypeParam("K".to_string()),
                Type::TypeParam("V".to_string()),
            ],
        ),
        &Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        &mut substitutions,
    );
    assert_eq!(substitutions.get("K"), Some(&Type::named("str")));
    assert_eq!(substitutions.get("V"), Some(&Type::named("int32")));
    collect_runtime_type_substitutions(
        &Type::Named(
            "list".to_string(),
            vec![Type::TypeParam("Ignored".to_string())],
        ),
        &Type::named("str"),
        &mut substitutions,
    );
    collect_runtime_type_substitutions(
        &Type::Named(
            "list".to_string(),
            vec![Type::TypeParam("Ignored".to_string())],
        ),
        &Type::Named("set".to_string(), vec![Type::named("str")]),
        &mut substitutions,
    );
    collect_runtime_type_substitutions(&Type::Unit, &Type::Unit, &mut substitutions);
    assert!(!substitutions.contains_key("Ignored"));
    collect_runtime_type_substitutions(
        &Type::Tuple(vec![
            Type::TypeParam("TupleLeft".to_string()),
            Type::Named(
                "list".to_string(),
                vec![Type::TypeParam("TupleRight".to_string())],
            ),
        ]),
        &Type::Tuple(vec![
            Type::named("str"),
            Type::Named("list".to_string(), vec![Type::named("int64")]),
        ]),
        &mut substitutions,
    );
    assert_eq!(substitutions.get("TupleLeft"), Some(&Type::named("str")));
    assert_eq!(substitutions.get("TupleRight"), Some(&Type::named("int64")));
    collect_runtime_type_substitutions(
        &Type::Tuple(vec![Type::TypeParam("WrongArity".to_string())]),
        &Type::Tuple(Vec::new()),
        &mut substitutions,
    );
    assert!(!substitutions.contains_key("WrongArity"));
    collect_runtime_type_substitutions(
        &Type::Tuple(vec![Type::TypeParam("TupleOnly".to_string())]),
        &Type::named("str"),
        &mut substitutions,
    );
    assert!(
        !substitutions.contains_key("TupleOnly"),
        "a nominal actual type must not satisfy a structural tuple generic"
    );

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(
        &Type::Named(
            "Result".to_string(),
            vec![
                Type::TypeParam("T".to_string()),
                Type::TypeParam("E".to_string()),
            ],
        ),
        &mut collected,
    );
    assert_eq!(
        collected,
        BTreeSet::from(["E".to_string(), "T".to_string()])
    );
    collect_type_params_from_type(
        &Type::Tuple(vec![
            Type::TypeParam("TupleT".to_string()),
            Type::named("str"),
        ]),
        &mut collected,
    );
    assert!(collected.contains("TupleT"));
    collect_type_params_from_type(&Type::TypeParam("Direct".to_string()), &mut collected);
    collect_type_params_from_type(&Type::Unit, &mut collected);
    assert!(collected.contains("Direct"));

    assert_eq!(
        eval_ordering(
            crate::ast::BinaryOp::Less,
            Value::Int(IntegerValue::from_signed(1)),
            Value::Int(IntegerValue::from_signed(2)),
        )
        .expect("int ordering should work"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_ordering(
            crate::ast::BinaryOp::LessEq,
            Value::Float(1.0),
            Value::Float(1.0),
        )
        .expect("float <= ordering should work"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_ordering(
            crate::ast::BinaryOp::Greater,
            Value::Float(2.0),
            Value::Float(1.0),
        )
        .expect("float > ordering should work"),
        Value::Bool(true)
    );
    assert_eq!(
        eval_ordering(
            crate::ast::BinaryOp::GreaterEq,
            Value::Float(2.0),
            Value::Float(2.0),
        )
        .expect("float >= ordering should work"),
        Value::Bool(true)
    );
    let ordering_error = eval_ordering(
        crate::ast::BinaryOp::Less,
        Value::Bool(true),
        Value::Bool(false),
    )
    .expect_err("non-numeric ordering should fail");
    assert!(ordering_error
        .message
        .contains("matching numeric or Duration operands"));
}

#[test]
fn serialized_mir_helper_reports_invalid_payloads() {
    let error = run_serialized_mir(
        b"{not-json}",
        "/tmp/test.au",
        "def main() -> int32:\n    return 0\n",
    )
    .expect_err("invalid embedded MIR should fail");
    assert!(error.message.contains("failed to deserialize embedded MIR"));
}

#[test]
fn mir_runtime_try_error_conversion_helpers_cover_context_and_from_paths() {
    let mut runtime = test_runtime();
    let no_context = runtime
        .convert_try_error_via_from(Value::String("boom".to_string()), &Type::named("str"))
        .expect_err("try error conversion should require a Result return context");
    assert!(no_context
        .message
        .contains("only allowed inside a function returning `Result`"));

    runtime.return_type_stack.push(Type::named("int32"));
    let non_result_context = runtime
        .convert_try_error_via_from(Value::String("boom".to_string()), &Type::named("str"))
        .expect_err("try error conversion should reject non-Result return types");
    assert!(non_result_context
        .message
        .contains("only allowed inside a function returning `Result`"));
    runtime.return_type_stack.pop();

    runtime.return_type_stack.push(Type::Named(
        "Result".to_string(),
        vec![Type::named("int32"), Type::named("bool")],
    ));
    let mismatch = runtime
        .convert_try_error_via_from(Value::String("boom".to_string()), &Type::named("str"))
        .expect_err("try error conversion should reject unrelated error types");
    assert!(mismatch
        .message
        .contains("does not match enclosing `Result`"));
    runtime.return_type_stack.pop();

    let lookup_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: vec![
                MirTraitImpl {
                    trait_name: "Display".to_string(),
                    trait_args: vec![Type::named("int32")],
                    for_type: Type::named("str"),
                    methods: Vec::new(),
                },
                MirTraitImpl {
                    trait_name: "From".to_string(),
                    trait_args: Vec::new(),
                    for_type: Type::named("str"),
                    methods: Vec::new(),
                },
                MirTraitImpl {
                    trait_name: "From".to_string(),
                    trait_args: vec![Type::named("int32")],
                    for_type: Type::named("bool"),
                    methods: Vec::new(),
                },
                MirTraitImpl {
                    trait_name: "From".to_string(),
                    trait_args: vec![Type::named("bool")],
                    for_type: Type::named("str"),
                    methods: Vec::new(),
                },
                MirTraitImpl {
                    trait_name: "From".to_string(),
                    trait_args: vec![Type::named("int32")],
                    for_type: Type::named("str"),
                    methods: vec![MirMethod {
                        name: "from".to_string(),
                        function_name: "missing_from_body".to_string(),
                        receiver: None,
                    }],
                },
            ],
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    assert!(lookup_runtime
        .find_from_trait_impl_method(&Type::named("int32"), &Type::named("str"))
        .is_none());

    let from_function = MirFunction {
        name: "from_int_error".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "value".to_string(),
            passing: crate::mir::MirReceiverKind::Value,
            ty: Type::named("int32"),
            default_function: None,
        }],
        local_types: Vec::new(),
        return_type: Type::named("str"),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::String("converted".to_string())),
        }],
    };
    let mut converting_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![from_function],
            classes: Vec::new(),
            trait_impls: vec![MirTraitImpl {
                trait_name: "From".to_string(),
                trait_args: vec![Type::named("int32")],
                for_type: Type::named("str"),
                methods: vec![MirMethod {
                    name: "from".to_string(),
                    function_name: "from_int_error".to_string(),
                    receiver: None,
                }],
            }],
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    converting_runtime.return_type_stack.push(Type::Named(
        "Result".to_string(),
        vec![Type::named("int32"), Type::named("str")],
    ));
    assert_eq!(
        converting_runtime
            .convert_try_error_via_from(
                Value::Int(IntegerValue::from_signed(7)),
                &Type::named("int32")
            )
            .expect("From-based try error conversion should run the impl method"),
        Value::String("converted".to_string())
    );
}

#[test]
fn trait_impl_lookup_and_top_level_run_helpers_cover_runtime_paths() {
    let render_method = MirMethod {
        name: "render".to_string(),
        function_name: "render_impl".to_string(),
        receiver: Some(crate::mir::MirReceiverKind::Borrow),
    };
    let runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: vec![
                MirTraitImpl {
                    trait_name: "Render".to_string(),
                    trait_args: Vec::new(),
                    for_type: Type::Named(
                        "Box".to_string(),
                        vec![Type::TypeParam("T".to_string())],
                    ),
                    methods: vec![render_method.clone()],
                },
                MirTraitImpl {
                    trait_name: "Render".to_string(),
                    trait_args: Vec::new(),
                    for_type: Type::named("Widget"),
                    methods: vec![render_method.clone()],
                },
                MirTraitImpl {
                    trait_name: "Preview".to_string(),
                    trait_args: Vec::new(),
                    for_type: Type::named("Widget"),
                    methods: vec![render_method.clone()],
                },
                MirTraitImpl {
                    trait_name: "Display".to_string(),
                    trait_args: Vec::new(),
                    for_type: Type::named("Widget"),
                    methods: vec![MirMethod {
                        name: "display".to_string(),
                        function_name: "display_impl".to_string(),
                        receiver: Some(crate::mir::MirReceiverKind::Borrow),
                    }],
                },
            ],
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    assert_eq!(
        runtime
            .find_trait_impl_method(
                &Type::Named("Box".to_string(), vec![Type::named("int32")]),
                "render",
            )
            .map(|method| method.function_name.as_str()),
        Some("render_impl")
    );
    assert_eq!(
        runtime
            .find_trait_impl_method_for_class_name("Widget", "display")
            .map(|method| method.function_name.as_str()),
        Some("display_impl")
    );
    assert!(
        runtime
            .find_trait_impl_method_for_class_name("Widget", "render")
            .is_none(),
        "ambiguous class-name trait lookups should return None",
    );
    assert!(runtime
        .find_trait_impl_method(&Type::named("Missing"), "render")
        .is_none());
    assert!(runtime
        .find_trait_impl_method_for_class_name("Missing", "render")
        .is_none());

    assert_eq!(
        MirRuntime::infer_value_type(&Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        })),
        Some(Type::Named("list".to_string(), vec![Type::named("int32")]))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::Set(crate::runtime_value::SetValue {
            element_type: Type::named("str"),
            elements: vec![Value::String("ready".to_string())],
        })),
        Some(Type::Named("set".to_string(), vec![Type::named("str")]))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::Map(crate::runtime_value::MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![(
                Value::String("count".to_string()),
                Value::Int(IntegerValue::from_signed(1)),
            )],
        })),
        Some(Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")]
        ))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&option_none()),
        Some(Type::Named("Option".to_string(), vec![Type::Unit]))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&result_err(Value::String("oops".to_string()))),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::Unit, Type::named("str")]
        ))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&send_error_closed(Value::Bool(true))),
        Some(Type::Named(
            "SendError".to_string(),
            vec![Type::named("bool")]
        ))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::EnumVariant(EnumVariantValue {
            enum_name: "SendError".to_string(),
            variant_name: "Cancelled".to_string(),
            payloads: vec![Value::String("payload".to_string())],
        })),
        Some(Type::Named(
            "SendError".to_string(),
            vec![Type::named("str")]
        ))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::EnumVariant(EnumVariantValue {
            enum_name: "Wait".to_string(),
            variant_name: "TimedOut".to_string(),
            payloads: Vec::new(),
        })),
        Some(Type::Named("process.Wait".to_string(), Vec::new()))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::EnumVariant(EnumVariantValue {
            enum_name: "Error".to_string(),
            variant_name: "TimedOut".to_string(),
            payloads: Vec::new(),
        })),
        Some(Type::Named("process.Error".to_string(), Vec::new()))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::EnumVariant(EnumVariantValue {
            enum_name: "Stdio".to_string(),
            variant_name: "Pipe".to_string(),
            payloads: Vec::new(),
        })),
        Some(Type::Named("process.Stdio".to_string(), Vec::new()))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::Channel(ChannelValue::new())),
        None
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::Task(TaskValue::from_handle(std::thread::spawn(
            || Ok(Value::Unit)
        )))),
        None
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::TaskGroup(TaskGroupValue::new(
            &CancellationContext::default()
        ))),
        None
    );

    let top_level_module =
        crate::lower_source_to_mir("print(1)\n").expect("top-level script should lower");
    let stdout = Arc::new(Mutex::new(String::new()));
    let mut top_level_runtime = MirRuntime::new(
        top_level_module,
        stdout.clone(),
        CancellationContext::default(),
    );
    assert_eq!(
        top_level_runtime
            .run_main()
            .expect("top-level script should execute"),
        Value::Int(IntegerValue::zero())
    );
    assert_eq!(stdout.lock().unwrap().as_str(), "1\n");

    let mut missing_entrypoint_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: vec![MirClass {
                name: "Box".to_string(),
                type_params: vec!["T".to_string()],
                fields: vec![crate::mir::MirClassField {
                    name: "value".to_string(),
                    ty: Type::TypeParam("T".to_string()),
                }],
                methods: Vec::new(),
            }],
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    assert_eq!(
        missing_entrypoint_runtime.infer_instance_type(&InstanceValue {
            class_name: "Box".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(9)),
            )]),
        }),
        Some(Type::Named("Box".to_string(), vec![Type::named("int64")]))
    );
    let entrypoint_error = missing_entrypoint_runtime
        .run_main()
        .expect_err("missing entrypoints should fail");
    assert!(entrypoint_error
        .message
        .contains("no `main` function or top-level script statements were found"));
}

#[test]
fn mir_owned_slice_runtime_uses_scalar_bounds_and_preserves_au4003_spans() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "negative_start",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(-3)),
    );

    let slice_args =
        |start: Operand, has_start: bool, end: Operand, has_end: bool, line: u128, column: u128| {
            vec![
                mir_arg(None, start),
                mir_arg(None, Operand::Bool(has_start)),
                mir_arg(None, end),
                mir_arg(None, Operand::Bool(has_end)),
                mir_arg(None, Operand::Int(line)),
                mir_arg(None, Operand::Int(column)),
            ]
        };

    let vector = VecValue {
        element_type: Type::named("str"),
        elements: ["zero", "one", "two", "three"]
            .into_iter()
            .map(|value| Value::String(value.to_string()))
            .collect(),
    };
    let source_elements = vector.elements.as_ptr();
    env.define_typed(
        "source_vector",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        Value::Vec(vector),
    );
    let sliced = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("source_vector".to_string()),
                field: "__slice".to_string(),
                receiver_place: None,
            },
            &slice_args(
                Operand::Place("negative_start".to_string()),
                true,
                Operand::Int(0),
                false,
                8,
                13,
            ),
            &mut env,
        )
        .expect("MIR Vec slicing should normalize a negative start");
    let Value::Vec(sliced) = sliced else {
        panic!("expected owned Vec slice");
    };
    assert_eq!(sliced.element_type, Type::named("str"));
    assert_eq!(
        sliced.elements,
        ["one", "two", "three"]
            .into_iter()
            .map(|value| Value::String(value.to_string()))
            .collect::<Vec<_>>()
    );
    assert_ne!(
        sliced.elements.as_ptr(),
        source_elements,
        "MIR Vec slicing must produce fresh owned storage"
    );
    let Value::Vec(source_after_slice) = env
        .place_ref("source_vector")
        .expect("the borrowed slice source must remain in its place")
    else {
        panic!("expected source Vec");
    };
    assert_eq!(
        source_after_slice.elements.as_ptr(),
        source_elements,
        "MIR slicing a Vec place must not consume or replace its source storage"
    );
    assert_eq!(source_after_slice.elements.len(), 4);

    let source_text = "aé🎉e\u{301}".to_string();
    let source_text_bytes = source_text.as_ptr();
    env.define_typed(
        "source_text",
        Type::named("str"),
        Value::String(source_text),
    );
    let sliced = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("source_text".to_string()),
                field: "__slice".to_string(),
                receiver_place: None,
            },
            &slice_args(Operand::Int(1), true, Operand::Int(4), true, 9, 7),
            &mut env,
        )
        .expect("MIR str slicing should count Unicode scalar values");
    let Value::String(sliced) = sliced else {
        panic!("expected owned str slice");
    };
    assert_eq!(sliced, "é🎉e");
    assert_ne!(
        sliced.as_ptr(),
        source_text_bytes,
        "MIR str slicing must allocate a fresh owned result"
    );
    let Value::String(source_after_slice) = env
        .place_ref("source_text")
        .expect("the borrowed str source must remain in its place")
    else {
        panic!("expected source str");
    };
    assert_eq!(source_after_slice.as_ptr(), source_text_bytes);

    let out_of_range = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("source_text".to_string()),
                field: "__slice".to_string(),
                receiver_place: None,
            },
            &slice_args(Operand::Int(0), false, Operand::Int(6), true, 11, 5),
            &mut env,
        )
        .expect_err("MIR str slicing must reject rather than clamp");
    assert_eq!(out_of_range.code, "AU4003");
    assert_eq!(out_of_range.message, "slice end `6` is outside `0..=5`");
    assert_eq!(out_of_range.span, Some(Span::new(11, 5)));

    let reversed = runtime
        .evaluate_vec_method(
            VecValue {
                element_type: Type::named("int32"),
                elements: vec![
                    Value::Int(IntegerValue::from_signed(1)),
                    Value::Int(IntegerValue::from_signed(2)),
                    Value::Int(IntegerValue::from_signed(3)),
                ],
            },
            "__slice",
            None,
            &slice_args(Operand::Int(3), true, Operand::Int(1), true, 12, 4),
            &mut env,
        )
        .expect_err("MIR Vec slicing must reject reversed normalized bounds");
    assert_eq!(reversed.code, "AU4003");
    assert_eq!(
        reversed.message,
        "slice start `3` is greater than slice end `1`"
    );
    assert_eq!(reversed.span, Some(Span::new(12, 4)));

    let temporary_reversed = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::String("temporary".to_string()),
                field: "__slice".to_string(),
                receiver_place: None,
            },
            &slice_args(Operand::Int(8), true, Operand::Int(2), true, 13, 3),
            &mut env,
        )
        .expect_err("temporary str slicing should use the owned fallback");
    assert_eq!(temporary_reversed.code, "AU4003");
    assert_eq!(
        temporary_reversed.message,
        "slice start `8` is greater than slice end `2`"
    );
    assert_eq!(temporary_reversed.span, Some(Span::new(13, 3)));

    env.define_typed("not_sliceable", Type::named("bool"), Value::Bool(true));
    let unsupported_place = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Place("not_sliceable".to_string()),
                field: "__slice".to_string(),
                receiver_place: None,
            },
            &slice_args(Operand::Int(0), false, Operand::Int(0), false, 14, 2),
            &mut env,
        )
        .expect_err("malformed MIR must reject a non-sliceable place");
    assert_eq!(
        unsupported_place.message,
        "unsupported MIR member call `__slice` on `true`"
    );

    let unsupported_temporary = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Bool(false),
                field: "__slice".to_string(),
                receiver_place: None,
            },
            &slice_args(Operand::Int(0), false, Operand::Int(0), false, 15, 2),
            &mut env,
        )
        .expect_err("malformed MIR must reject a non-sliceable temporary");
    assert_eq!(
        unsupported_temporary.message,
        "unsupported MIR member call `__slice` on `false`"
    );
}

#[test]
fn mir_slice_internal_abi_rejects_malformed_endpoint_and_presence_operands() {
    fn evaluated(value: Value) -> EvaluatedMirArg {
        EvaluatedMirArg {
            name: None,
            value,
            ty: None,
            writeback_place: None,
        }
    }

    let runtime = test_runtime();
    let valid = || {
        vec![
            evaluated(Value::Int(IntegerValue::from_signed(0))),
            evaluated(Value::Bool(false)),
            evaluated(Value::Int(IntegerValue::from_signed(0))),
            evaluated(Value::Bool(false)),
            evaluated(Value::Int(IntegerValue::from_signed(1))),
            evaluated(Value::Int(IntegerValue::from_signed(1))),
        ]
    };
    let too_wide = || {
        Value::Int(
            IntegerValue::from_typed_unsigned(u128::MAX, IntegerKind::Uint128)
                .expect("uint128::MAX is a valid typed integer"),
        )
    };

    let wrong_count = runtime
        .mir_slice_args(Vec::new())
        .expect_err("the private ABI requires exactly six operands");
    assert!(wrong_count.message.contains("requires start, has_start"));

    for (index, replacement, expected) in [
        (
            0,
            Value::String("start".to_string()),
            "internal slice start operand must be an integer",
        ),
        (
            0,
            too_wide(),
            "slice start is outside the supported signed range",
        ),
        (
            1,
            Value::Int(IntegerValue::from_signed(1)),
            "internal slice has_start operand must be bool",
        ),
        (
            2,
            Value::String("end".to_string()),
            "internal slice end operand must be an integer",
        ),
        (
            2,
            too_wide(),
            "slice end is outside the supported signed range",
        ),
        (
            3,
            Value::Int(IntegerValue::from_signed(1)),
            "internal slice has_end operand must be bool",
        ),
    ] {
        let mut values = valid();
        values[index] = evaluated(replacement);
        let error = runtime
            .mir_slice_args(values)
            .expect_err("malformed private slice operands must be rejected");
        assert_eq!(error.message, expected);
    }
}

#[test]
fn mir_runtime_collection_string_and_task_helpers_cover_remaining_paths() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: vec![
                Value::Int(IntegerValue::from_signed(1)),
                Value::Int(IntegerValue::from_signed(2)),
            ],
        }),
    );
    env.define_typed(
        "other",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_signed(3))],
        }),
    );
    env.define_typed(
        "texts",
        Type::Named("list".to_string(), vec![Type::named("str")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("str"),
            elements: vec![
                Value::String("one".to_string()),
                Value::String("two".to_string()),
            ],
        }),
    );
    env.define_typed(
        "mapping",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        Value::Map(crate::runtime_value::MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![(
                Value::String("count".to_string()),
                Value::Int(IntegerValue::from_signed(1)),
            )],
        }),
    );
    env.define_typed(
        "mapping_other",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        Value::Map(crate::runtime_value::MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![(
                Value::String("next".to_string()),
                Value::Int(IntegerValue::from_signed(9)),
            )],
        }),
    );
    env.define_typed(
        "flags",
        Type::Named("set".to_string(), vec![Type::named("str")]),
        Value::Set(crate::runtime_value::SetValue {
            element_type: Type::named("str"),
            elements: vec![Value::String("ready".to_string())],
        }),
    );
    env.define_typed(
        "jobs",
        Type::Named("Queue".to_string(), vec![Type::named("int32")]),
        Value::Channel(ChannelValue::new()),
    );
    let vec_from_env = |env: &Env, place: &str| match env.read_place(place).unwrap() {
        Value::Vec(vector) => vector,
        other => panic!("expected vec at `{place}`, found {other:?}"),
    };
    let map_from_env = |env: &Env, place: &str| match env.read_place(place).unwrap() {
        Value::Map(map) => map,
        other => panic!("expected map at `{place}`, found {other:?}"),
    };
    let channel_from_env = |env: &Env, place: &str| match env.read_place(place).unwrap() {
        Value::Channel(channel) => channel,
        other => panic!("expected channel at `{place}`, found {other:?}"),
    };

    let vec_len = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "len",
            Some("values"),
            &[],
            &mut env,
        )
        .expect("vec len should succeed");
    assert_eq!(vec_len, Value::Int(IntegerValue::from_signed(2)));

    let vec_empty = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "is_empty",
            Some("values"),
            &[],
            &mut env,
        )
        .expect("vec is_empty should succeed");
    assert_eq!(vec_empty, Value::Bool(false));

    let vec_clone = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "copy",
            Some("values"),
            &[],
            &mut env,
        )
        .expect("list copy should succeed");
    match vec_clone {
        Value::Vec(vector) => assert_eq!(vector.elements.len(), 2),
        other => panic!("expected list copy, found {other:?}"),
    }

    let vec_get = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "get",
            Some("values"),
            &[mir_arg(Some("index"), Operand::Int(0))],
            &mut env,
        )
        .expect("vec get should succeed");
    assert_eq!(
        vec_get,
        option_some(Value::Int(IntegerValue::from_signed(1)))
    );

    let vec_contains = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "contains",
            Some("values"),
            &[mir_arg(Some("value"), Operand::Int(2))],
            &mut env,
        )
        .expect("vec contains should succeed");
    assert_eq!(vec_contains, Value::Bool(true));

    let vec_is_empty_args = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "is_empty",
            Some("values"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("vec is_empty should reject arguments");
    assert!(vec_is_empty_args
        .message
        .contains("`is_empty` does not take arguments"));
    let vec_clone_args = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "copy",
            Some("values"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("list copy should reject arguments");
    assert!(vec_clone_args
        .message
        .contains("`copy` does not take arguments"));
    let vec_pop_at_index = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "pop",
            Some("values"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect("list pop should accept an optional index");
    assert_eq!(vec_pop_at_index, Value::Int(IntegerValue::from_signed(2)));
    let vec_pop_no_place = runtime
        .evaluate_vec_method(vec_from_env(&mut env, "values"), "pop", None, &[], &mut env)
        .expect_err("vec pop should require a receiver place");
    assert!(vec_pop_no_place
        .message
        .contains("requires a mutable list place"));
    let vec_set_index_no_place = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "__set_index",
            None,
            &[
                mir_arg(None, Operand::Int(0)),
                mir_arg(None, Operand::Int(7)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ],
            &mut env,
        )
        .expect_err("internal indexed vector assignment should require a receiver place");
    assert!(vec_set_index_no_place
        .message
        .contains("requires a mutable list place"));
    let vec_swap_no_place = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "swap",
            None,
            &[
                mir_arg(Some("first"), Operand::Int(0)),
                mir_arg(Some("second"), Operand::Int(0)),
            ],
            &mut env,
        )
        .expect_err("vec swap should require a receiver place");
    assert!(vec_swap_no_place
        .message
        .contains("requires a mutable list place"));
    let vec_clear_args = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "clear",
            Some("values"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("vec clear should reject arguments");
    assert!(vec_clear_args
        .message
        .contains("`clear` does not take arguments"));
    let vec_reverse_args = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "reverse",
            Some("values"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("vec reverse should reject arguments");
    assert!(vec_reverse_args
        .message
        .contains("`reverse` does not take arguments"));
    let vec_extend_no_place = runtime
        .evaluate_vec_method(
            vec_from_env(&mut env, "values"),
            "extend",
            None,
            &[mir_arg(Some("other"), Operand::Place("other".to_string()))],
            &mut env,
        )
        .expect_err("vec extend should require a receiver place");
    assert!(vec_extend_no_place
        .message
        .contains("requires a mutable list place"));

    let map_len = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "len",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("map len should succeed");
    assert_eq!(map_len, Value::Int(IntegerValue::from_signed(1)));

    let map_empty = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "is_empty",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("map is_empty should succeed");
    assert_eq!(map_empty, Value::Bool(false));

    let map_clone = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "copy",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("dict copy should succeed");
    match map_clone {
        Value::Map(map) => assert_eq!(map.entries.len(), 1),
        other => panic!("expected dict copy, found {other:?}"),
    }

    let map_get = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "get",
            Some("mapping"),
            &[mir_arg(Some("key"), Operand::String("count".to_string()))],
            &mut env,
        )
        .expect("map get should succeed");
    assert_eq!(
        map_get,
        option_some(Value::Int(IntegerValue::from_signed(1)))
    );
    let map_get_missing = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "get",
            Some("mapping"),
            &[mir_arg(Some("key"), Operand::String("missing".to_string()))],
            &mut env,
        )
        .expect("missing map get should return Option.None");
    assert_eq!(map_get_missing, option_none());

    let map_values = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "values",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("map values should succeed");
    match map_values {
        Value::Vec(values) => assert_eq!(values.elements.len(), 1),
        other => panic!("expected vec of values, found {other:?}"),
    }

    let map_keys = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "keys",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("map keys should succeed");
    match map_keys {
        Value::Vec(keys) => assert_eq!(keys.elements.len(), 1),
        other => panic!("expected vec of keys, found {other:?}"),
    }

    let map_contains = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "contains_key",
            Some("mapping"),
            &[mir_arg(Some("key"), Operand::String("count".to_string()))],
            &mut env,
        )
        .expect("map contains_key should succeed");
    assert_eq!(map_contains, Value::Bool(true));
    let map_missing_contains = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "contains_key",
            Some("mapping"),
            &[mir_arg(Some("key"), Operand::String("missing".to_string()))],
            &mut env,
        )
        .expect("map contains_key should return false for missing keys");
    assert_eq!(map_missing_contains, Value::Bool(false));
    let map_index = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "__index",
            Some("mapping"),
            &[
                mir_arg(None, Operand::String("count".to_string())),
                mir_arg(None, Operand::Int(2)),
                mir_arg(None, Operand::Int(3)),
            ],
            &mut env,
        )
        .expect("internal map indexing should succeed for existing keys");
    assert_eq!(map_index, Value::Int(IntegerValue::from_signed(1)));

    let map_len_args = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "len",
            Some("mapping"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("map len should reject arguments");
    assert!(map_len_args
        .message
        .contains("`len` does not take arguments"));
    let map_empty_args = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "is_empty",
            Some("mapping"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("map is_empty should reject arguments");
    assert!(map_empty_args
        .message
        .contains("`is_empty` does not take arguments"));
    let map_clone_args = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "copy",
            Some("mapping"),
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("dict copy should reject arguments");
    assert!(map_clone_args
        .message
        .contains("`copy` does not take arguments"));
    let set_len = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "len",
            Some("flags"),
            &[],
            &mut env,
        )
        .expect("set len should succeed");
    assert_eq!(set_len, Value::Int(IntegerValue::from_signed(1)));

    let set_empty = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "is_empty",
            Some("flags"),
            &[],
            &mut env,
        )
        .expect("set is_empty should succeed");
    assert_eq!(set_empty, Value::Bool(false));

    let set_clone = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "copy",
            Some("flags"),
            &[],
            &mut env,
        )
        .expect("set copy should succeed");
    match set_clone {
        Value::Set(set) => assert_eq!(set.elements.len(), 1),
        other => panic!("expected set copy, found {other:?}"),
    }

    let set_contains = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "contains",
            Some("flags"),
            &[mir_arg(Some("value"), Operand::String("ready".to_string()))],
            &mut env,
        )
        .expect("set contains should succeed");
    assert_eq!(set_contains, Value::Bool(true));

    let set_index = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "__index_option",
            Some("flags"),
            &[mir_arg(Some("index"), Operand::Int(0))],
            &mut env,
        )
        .expect("set __index_option should succeed");
    assert_eq!(set_index, option_some(Value::String("ready".to_string())));

    let string_len = runtime
        .evaluate_string_method("é🎉e\u{301}".to_string(), "len", &[], &mut env)
        .expect("string len should succeed");
    assert_eq!(string_len, Value::Int(IntegerValue::from_signed(4)));

    let string_byte_len = runtime
        .evaluate_string_method("é🎉e\u{301}".to_string(), "byte_len", &[], &mut env)
        .expect("string byte_len should succeed");
    assert_eq!(string_byte_len, Value::Int(IntegerValue::from_signed(9)));

    let starts_with = runtime
        .evaluate_string_method(
            "Aura".to_string(),
            "starts_with",
            &[mir_arg(Some("text"), Operand::String("Aur".to_string()))],
            &mut env,
        )
        .expect("string starts_with should succeed");
    assert_eq!(starts_with, Value::Bool(true));

    let ends_with = runtime
        .evaluate_string_method(
            "Aura".to_string(),
            "ends_with",
            &[mir_arg(Some("text"), Operand::String("ura".to_string()))],
            &mut env,
        )
        .expect("string ends_with should succeed");
    assert_eq!(ends_with, Value::Bool(true));

    let split = runtime
        .evaluate_string_method(
            "au-ra-test".to_string(),
            "split",
            &[mir_arg(Some("text"), Operand::String("-".to_string()))],
            &mut env,
        )
        .expect("string split should succeed");
    match split {
        Value::Vec(parts) => assert_eq!(parts.elements.len(), 3),
        other => panic!("expected split vec, found {other:?}"),
    }

    let replace = runtime
        .evaluate_string_method(
            "Aura".to_string(),
            "replace",
            &[
                mir_arg(Some("from"), Operand::String("Aur".to_string())),
                mir_arg(Some("to"), Operand::String("Our".to_string())),
            ],
            &mut env,
        )
        .expect("string replace should succeed");
    assert_eq!(replace, Value::String("Oura".to_string()));

    let lower = runtime
        .evaluate_string_method("AuRa".to_string(), "to_lower", &[], &mut env)
        .expect("string to_lower should succeed");
    assert_eq!(lower, Value::String("aura".to_string()));

    let upper = runtime
        .evaluate_string_method("AuRa".to_string(), "to_upper", &[], &mut env)
        .expect("string to_upper should succeed");
    assert_eq!(upper, Value::String("AURA".to_string()));

    let suffix = runtime
        .evaluate_string_method(
            "prefix-value".to_string(),
            "strip_suffix",
            &[mir_arg(Some("text"), Operand::String("-value".to_string()))],
            &mut env,
        )
        .expect("string strip_suffix should succeed");
    match suffix {
        Value::EnumVariant(variant) => assert_eq!(variant.variant_name, "Some"),
        other => panic!("expected option result, found {other:?}"),
    }

    let trim = runtime
        .evaluate_string_method("  Aura  ".to_string(), "trim", &[], &mut env)
        .expect("string trim should succeed");
    assert_eq!(trim, Value::String("Aura".to_string()));

    let string_clone = runtime
        .evaluate_string_method("Aura".to_string(), "clone", &[], &mut env)
        .expect("string clone should succeed");
    assert_eq!(string_clone, Value::String("Aura".to_string()));

    let send = runtime
        .evaluate_channel_method(
            channel_from_env(&mut env, "jobs"),
            "put",
            &[mir_arg(Some("value"), Operand::Int(5))],
            &mut env,
        )
        .expect("queue put should succeed");
    match send {
        Value::EnumVariant(variant) => assert_eq!(variant.variant_name, "Ok"),
        other => panic!("expected Result from send, found {other:?}"),
    }

    let recv = runtime
        .evaluate_channel_method(channel_from_env(&mut env, "jobs"), "get", &[], &mut env)
        .expect("queue get should succeed");
    assert_eq!(
        recv,
        Value::EnumVariant(EnumVariantValue {
            enum_name: "QueueReceive".to_string(),
            variant_name: "Item".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(5))],
        })
    );

    let close = runtime
        .evaluate_channel_method(channel_from_env(&mut env, "jobs"), "close", &[], &mut env)
        .expect("channel close should succeed");
    assert_eq!(close, Value::Unit);

    let assert_send_error = |value: Value, variant_name: &str, expected_payload: i128| {
        let mut result_payloads = enum_payloads(value, "Result", "Err");
        assert_eq!(result_payloads.len(), 1);
        let mut send_payloads = enum_payloads(result_payloads.remove(0), "SendError", variant_name);
        assert_eq!(send_payloads.len(), 1);
        assert_eq!(
            send_payloads.remove(0),
            Value::Int(IntegerValue::from_signed(expected_payload))
        );
    };

    let closed_channel = ChannelValue::new();
    closed_channel.close();
    assert_send_error(
        runtime
            .evaluate_channel_method(
                closed_channel.clone(),
                "put",
                &[mir_arg(Some("value"), Operand::Int(6))],
                &mut env,
            )
            .expect("closed queue put should return a send error"),
        "Closed",
        6,
    );
    assert_send_error(
        runtime
            .evaluate_channel_method(
                closed_channel,
                "try_put",
                &[mir_arg(Some("value"), Operand::Int(7))],
                &mut env,
            )
            .expect("closed queue try_put should return a send error"),
        "Closed",
        7,
    );

    let full_channel = ChannelValue::with_capacity(0);
    assert_send_error(
        runtime
            .evaluate_channel_method(
                full_channel.clone(),
                "try_put",
                &[mir_arg(Some("value"), Operand::Int(8))],
                &mut env,
            )
            .expect("full queue try_put should return a send error"),
        "Full",
        8,
    );
    assert_send_error(
        runtime
            .evaluate_channel_method(
                full_channel,
                "put",
                &[
                    mir_arg(Some("value"), Operand::Int(9)),
                    mir_arg(Some("timeout"), Operand::Duration(0)),
                ],
                &mut env,
            )
            .expect("timed out queue put should return a send error"),
        "TimedOut",
        9,
    );

    let cancellation_group = TaskGroupValue::new(&CancellationContext::default());
    cancellation_group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancellation_group.child_cancellation(),
    );
    assert_send_error(
        cancelled_runtime
            .evaluate_channel_method(
                ChannelValue::with_capacity(0),
                "put",
                &[mir_arg(Some("value"), Operand::Int(10))],
                &mut env,
            )
            .expect("cancelled queue put should return a send error"),
        "Cancelled",
        10,
    );

    let assert_queue_receive_variant =
        |value: Value, variant_name: &str| enum_payloads(value, "QueueReceive", variant_name);

    let empty_get_or_none = runtime
        .evaluate_channel_method(ChannelValue::new(), "get_or_none", &[], &mut env)
        .expect("empty get_or_none should return Option.None immediately");
    assert_eq!(empty_get_or_none, option_none());

    let queued_for_get_or_none = ChannelValue::new();
    queued_for_get_or_none
        .try_send_result(Value::Int(IntegerValue::from_signed(12)))
        .expect("queue should accept a value");
    assert_eq!(
        runtime
            .evaluate_channel_method(queued_for_get_or_none, "get_or_none", &[], &mut env,)
            .expect("queued get_or_none should return Option.Some"),
        option_some(Value::Int(IntegerValue::from_signed(12)))
    );

    let closed_get_or_none = ChannelValue::new();
    closed_get_or_none.close();
    assert_eq!(
        runtime
            .evaluate_channel_method(closed_get_or_none, "get_or_none", &[], &mut env)
            .expect("closed get_or_none should return Option.None"),
        option_none()
    );
    assert_eq!(
        cancelled_runtime
            .evaluate_channel_method(ChannelValue::new(), "get_or_none", &[], &mut env)
            .expect("cancelled get_or_none should return Option.None"),
        option_none()
    );

    let queued_for_get_or = ChannelValue::new();
    queued_for_get_or
        .try_send_result(Value::Int(IntegerValue::from_signed(14)))
        .expect("queue should accept a value");
    assert_eq!(
        runtime
            .evaluate_channel_method(
                queued_for_get_or,
                "get_or",
                &[mir_arg(Some("default"), Operand::Int(99))],
                &mut env,
            )
            .expect("queued get_or should return the queued value"),
        Value::Int(IntegerValue::from_signed(14))
    );
    assert_eq!(
        runtime
            .evaluate_channel_method(
                ChannelValue::new(),
                "get_or",
                &[mir_arg(Some("default"), Operand::Int(99))],
                &mut env,
            )
            .expect("empty get_or should return the fallback immediately"),
        Value::Int(IntegerValue::from_signed(99))
    );
    let closed_get_or = ChannelValue::new();
    closed_get_or.close();
    assert_eq!(
        runtime
            .evaluate_channel_method(
                closed_get_or,
                "get_or",
                &[mir_arg(Some("default"), Operand::Int(100))],
                &mut env,
            )
            .expect("closed get_or should return the fallback"),
        Value::Int(IntegerValue::from_signed(100))
    );
    assert_eq!(
        cancelled_runtime
            .evaluate_channel_method(
                ChannelValue::new(),
                "get_or",
                &[mir_arg(Some("default"), Operand::Int(101))],
                &mut env,
            )
            .expect("cancelled get_or should return the fallback"),
        Value::Int(IntegerValue::from_signed(101))
    );

    let iteration_arg_error = runtime
        .evaluate_channel_method(ChannelValue::new(), "__get_in_task_group", &[], &mut env)
        .expect_err("internal task-group get helper should enforce arity");
    assert!(iteration_arg_error
        .message
        .contains("expects one task-group"));

    let iteration_type_error = runtime
        .evaluate_channel_method(
            ChannelValue::new(),
            "__get_in_task_group",
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("internal task-group get helper should require a task group");
    assert!(iteration_type_error
        .message
        .contains("expected `TaskGroup`"));

    let mut iteration_env = Env::default();
    iteration_env.define_typed(
        "iter_group",
        Type::named("TaskGroup"),
        Value::TaskGroup(TaskGroupValue::new(&CancellationContext::default())),
    );
    let closed_iteration_channel = ChannelValue::new();
    closed_iteration_channel.close();
    assert!(assert_queue_receive_variant(
        runtime
            .evaluate_channel_method(
                closed_iteration_channel,
                "__get_in_task_group",
                &[mir_arg(None, Operand::Place("iter_group".to_string()))],
                &mut iteration_env,
            )
            .expect("closed task-group iteration helper should return Closed"),
        "Closed",
    )
    .is_empty());

    let registered_arg_error = runtime
        .evaluate_channel_method(
            ChannelValue::new(),
            "__get_with_registered_producers",
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("registered-producer helper should reject arguments");
    assert!(registered_arg_error
        .message
        .contains("expects no arguments"));
    let closed_registered_channel = ChannelValue::new();
    closed_registered_channel.close();
    assert!(assert_queue_receive_variant(
        runtime
            .evaluate_channel_method(
                closed_registered_channel,
                "__get_with_registered_producers",
                &[],
                &mut env,
            )
            .expect("closed registered-producer helper should return Closed"),
        "Closed",
    )
    .is_empty());

    let insert = runtime
        .evaluate_vec_method(
            match env.read_place("values").unwrap() {
                Value::Vec(vector) => vector,
                other => panic!("expected vec, found {other:?}"),
            },
            "insert",
            Some("values"),
            &[
                mir_arg(Some("index"), Operand::Int(1)),
                mir_arg(Some("value"), Operand::Int(99)),
            ],
            &mut env,
        )
        .expect("vec insert should succeed");
    assert_eq!(insert, Value::Unit);

    let reverse = runtime
        .evaluate_vec_method(
            match env.read_place("values").unwrap() {
                Value::Vec(vector) => vector,
                other => panic!("expected vec, found {other:?}"),
            },
            "reverse",
            Some("values"),
            &[],
            &mut env,
        )
        .expect("vec reverse should succeed");
    assert_eq!(reverse, Value::Unit);

    let extend = runtime
        .evaluate_vec_method(
            match env.read_place("values").unwrap() {
                Value::Vec(vector) => vector,
                other => panic!("expected vec, found {other:?}"),
            },
            "extend",
            Some("values"),
            &[mir_arg(Some("other"), Operand::Place("other".to_string()))],
            &mut env,
        )
        .expect("vec extend should succeed");
    assert_eq!(extend, Value::Unit);

    let clear = runtime
        .evaluate_vec_method(
            match env.read_place("values").unwrap() {
                Value::Vec(vector) => vector,
                other => panic!("expected vec, found {other:?}"),
            },
            "clear",
            Some("values"),
            &[],
            &mut env,
        )
        .expect("vec clear should succeed");
    assert_eq!(clear, Value::Unit);

    let vec_error = runtime
        .evaluate_vec_method(
            match env.read_place("texts").unwrap() {
                Value::Vec(vector) => vector,
                other => panic!("expected vec, found {other:?}"),
            },
            "mystery",
            Some("texts"),
            &[],
            &mut env,
        )
        .expect_err("unsupported vec method should fail");
    assert!(vec_error.message.contains("unsupported vector method"));

    let map_items = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "items",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("map items should succeed");
    match map_items {
        Value::Vec(items) => assert_eq!(
            items.elements,
            vec![Value::Tuple(TupleValue {
                element_types: vec![Type::named("str"), Type::named("int32")],
                elements: vec![
                    Value::String("count".to_string()),
                    Value::Int(IntegerValue::from_signed(1)),
                ],
            })]
        ),
        other => panic!("expected vec, found {other:?}"),
    }

    let map_update = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "update",
            Some("mapping"),
            &[mir_arg(
                Some("other"),
                Operand::Place("mapping_other".to_string()),
            )],
            &mut env,
        )
        .expect("dict update should succeed");
    assert_eq!(map_update, Value::Unit);

    let map_set_existing = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "set",
            Some("mapping"),
            &[
                mir_arg(Some("key"), Operand::String("count".to_string())),
                mir_arg(Some("value"), Operand::Int(4)),
            ],
            &mut env,
        )
        .expect("map set should replace existing keys");
    assert_eq!(
        map_set_existing,
        option_some(Value::Int(IntegerValue::from_signed(1)))
    );

    runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "__set_index",
            Some("mapping"),
            &[
                mir_arg(None, Operand::String("count".to_string())),
                mir_arg(None, Operand::Int(5)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ],
            &mut env,
        )
        .expect("internal map indexed assignment should update existing keys");
    let map_set_index_no_place = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "__set_index",
            None,
            &[
                mir_arg(None, Operand::String("count".to_string())),
                mir_arg(None, Operand::Int(6)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(1)),
            ],
            &mut env,
        )
        .expect_err("internal map indexed assignment should require a receiver place");
    assert!(map_set_index_no_place
        .message
        .contains("requires a mutable dict place"));
    env.define_typed(
        "mapping_update",
        Type::Named(
            "dict".to_string(),
            vec![Type::named("str"), Type::named("int32")],
        ),
        Value::Map(crate::runtime_value::MapValue {
            key_type: Type::named("str"),
            value_type: Type::named("int32"),
            entries: vec![(
                Value::String("count".to_string()),
                Value::Int(IntegerValue::from_signed(9)),
            )],
        }),
    );
    runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "update",
            Some("mapping"),
            &[mir_arg(
                Some("other"),
                Operand::Place("mapping_update".to_string()),
            )],
            &mut env,
        )
        .expect("dict update should update existing keys");
    let map_update_no_place = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "update",
            None,
            &[mir_arg(
                Some("other"),
                Operand::Place("mapping_other".to_string()),
            )],
            &mut env,
        )
        .expect_err("dict update should require a receiver place");
    assert!(map_update_no_place
        .message
        .contains("requires a mutable dict place"));
    let unsupported_map_method = runtime
        .evaluate_map_method(
            map_from_env(&mut env, "mapping"),
            "mystery",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect_err("unknown map methods should fail");
    assert!(unsupported_map_method
        .message
        .contains("unsupported dict method `mystery`"));

    let map_set_new = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "set",
            Some("mapping"),
            &[
                mir_arg(Some("key"), Operand::String("fresh".to_string())),
                mir_arg(Some("value"), Operand::Int(5)),
            ],
            &mut env,
        )
        .expect("map set should insert missing keys");
    assert_eq!(map_set_new, option_none());

    let map_remove_missing = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "remove",
            Some("mapping"),
            &[mir_arg(Some("key"), Operand::String("missing".to_string()))],
            &mut env,
        )
        .expect("map remove should return Option.None for missing keys");
    assert_eq!(map_remove_missing, option_none());

    let map_remove_existing = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "remove",
            Some("mapping"),
            &[mir_arg(Some("key"), Operand::String("fresh".to_string()))],
            &mut env,
        )
        .expect("map remove should return the removed value");
    assert_eq!(
        map_remove_existing,
        option_some(Value::Int(IntegerValue::from_signed(5)))
    );

    let map_set_index = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "__set_index",
            Some("mapping"),
            &[
                mir_arg(None, Operand::String("indexed".to_string())),
                mir_arg(None, Operand::Int(7)),
                mir_arg(None, Operand::Int(2)),
                mir_arg(None, Operand::Int(3)),
            ],
            &mut env,
        )
        .expect("internal map indexed assignment should insert or update keys");
    assert_eq!(map_set_index, Value::Unit);

    let map_clear = runtime
        .evaluate_map_method(
            match env.read_place("mapping").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "clear",
            Some("mapping"),
            &[],
            &mut env,
        )
        .expect("map clear should succeed");
    assert_eq!(map_clear, Value::Unit);

    let map_error = runtime
        .evaluate_map_method(
            match env.read_place("mapping_other").unwrap() {
                Value::Map(map) => map,
                other => panic!("expected map, found {other:?}"),
            },
            "update",
            Some("mapping_other"),
            &[mir_arg(Some("other"), Operand::Int(7))],
            &mut env,
        )
        .expect_err("dict update should reject non-dict values");
    assert!(map_error
        .message
        .contains("requires another `dict[K, V]` value"));

    let set_add = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "add",
            Some("flags"),
            &[mir_arg(Some("value"), Operand::String("go".to_string()))],
            &mut env,
        )
        .expect("set add should succeed");
    assert_eq!(set_add, Value::Unit);

    let set_remove = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "remove",
            Some("flags"),
            &[mir_arg(Some("value"), Operand::String("ready".to_string()))],
            &mut env,
        )
        .expect("set remove should succeed");
    assert_eq!(set_remove, Value::Unit);

    let set_add_existing = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "add",
            Some("flags"),
            &[mir_arg(Some("value"), Operand::String("go".to_string()))],
            &mut env,
        )
        .expect("set add should accept duplicate values");
    assert_eq!(set_add_existing, Value::Unit);

    let set_remove_missing = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "remove",
            Some("flags"),
            &[mir_arg(
                Some("value"),
                Operand::String("missing".to_string()),
            )],
            &mut env,
        )
        .expect_err("set remove should reject missing values");
    assert_eq!(set_remove_missing.code, "AU4008");

    let set_error = runtime
        .evaluate_set_method(
            match env.read_place("flags").unwrap() {
                Value::Set(set) => set,
                other => panic!("expected set, found {other:?}"),
            },
            "unknown",
            Some("flags"),
            &[],
            &mut env,
        )
        .expect_err("unsupported set method should fail");
    assert!(set_error.message.contains("unsupported set method"));

    let contains = runtime
        .evaluate_string_method(
            "aura".to_string(),
            "contains",
            &[mir_arg(Some("text"), Operand::String("ur".to_string()))],
            &mut env,
        )
        .expect("string contains should succeed");
    assert_eq!(contains, Value::Bool(true));

    let join = runtime
        .evaluate_string_method(
            ", ".to_string(),
            "join",
            &[mir_arg(Some("parts"), Operand::Place("texts".to_string()))],
            &mut env,
        )
        .expect("string join should succeed");
    assert_eq!(join, Value::String("one, two".to_string()));

    let strip = runtime
        .evaluate_string_method(
            "prefix-value".to_string(),
            "strip_prefix",
            &[mir_arg(
                Some("text"),
                Operand::String("prefix-".to_string()),
            )],
            &mut env,
        )
        .expect("string strip_prefix should succeed");
    match strip {
        Value::EnumVariant(variant) => assert_eq!(variant.variant_name, "Some"),
        other => panic!("expected option result, found {other:?}"),
    }

    let string_error = runtime
        .evaluate_string_method(
            "aura".to_string(),
            "contains",
            &[mir_arg(Some("text"), Operand::Bool(true))],
            &mut env,
        )
        .expect_err("string contains should reject non-string args");
    assert!(string_error.message.contains("requires a `str`"));

    let join_error = runtime
        .evaluate_string_method(
            ", ".to_string(),
            "join",
            &[mir_arg(Some("parts"), Operand::Int(1))],
            &mut env,
        )
        .expect_err("string join should reject non-vectors");
    assert!(join_error.message.contains("requires `list[str]`"));

    env.define_typed(
        "non_string_parts",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
    );

    for (field, args, expected) in [
        (
            "len",
            vec![mir_arg(None, Operand::Int(1))],
            "`len` does not take arguments",
        ),
        (
            "starts_with",
            vec![mir_arg(Some("text"), Operand::Bool(true))],
            "`starts_with` requires a `str` argument",
        ),
        (
            "ends_with",
            vec![mir_arg(Some("text"), Operand::Bool(true))],
            "`ends_with` requires a `str` argument",
        ),
        (
            "split",
            vec![mir_arg(Some("text"), Operand::Bool(true))],
            "`split` requires a `str` argument",
        ),
        (
            "replace",
            vec![
                mir_arg(Some("from"), Operand::Bool(true)),
                mir_arg(Some("to"), Operand::String("x".to_string())),
            ],
            "`replace` requires `str` for `from`",
        ),
        (
            "replace",
            vec![
                mir_arg(Some("from"), Operand::String("a".to_string())),
                mir_arg(Some("to"), Operand::Bool(true)),
            ],
            "`replace` requires `str` for `to`",
        ),
        (
            "to_lower",
            vec![mir_arg(None, Operand::Int(1))],
            "`to_lower` does not take arguments",
        ),
        (
            "to_upper",
            vec![mir_arg(None, Operand::Int(1))],
            "`to_upper` does not take arguments",
        ),
        (
            "join",
            vec![mir_arg(
                Some("parts"),
                Operand::Place("non_string_parts".to_string()),
            )],
            "`join` requires `list[str]`",
        ),
        (
            "add",
            vec![mir_arg(Some("other"), Operand::Bool(true))],
            "`add` requires a `str` argument",
        ),
        (
            "strip_prefix",
            vec![mir_arg(Some("text"), Operand::Bool(true))],
            "`strip_prefix` requires a `str` argument",
        ),
        (
            "strip_suffix",
            vec![mir_arg(Some("text"), Operand::Bool(true))],
            "`strip_suffix` requires a `str` argument",
        ),
        (
            "trim",
            vec![mir_arg(None, Operand::Int(1))],
            "`trim` does not take arguments",
        ),
        (
            "clone",
            vec![mir_arg(None, Operand::Int(1))],
            "`clone` does not take arguments",
        ),
        ("missing", Vec::new(), "unsupported string method `missing`"),
    ] {
        let error = runtime
            .evaluate_string_method("aura".to_string(), field, &args, &mut env)
            .expect_err("string helper edge should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in diagnostic, got `{}`",
            error.message
        );
    }

    let task_clone = runtime
        .evaluate_task_method(
            TaskValue::from_handle(std::thread::spawn(|| Ok(Value::Unit))),
            "clone",
            &[],
            &mut env,
        )
        .expect_err("task clone should be unsupported");
    assert!(task_clone
        .message
        .contains("unsupported task method `clone`"));

    let task_join = runtime
        .evaluate_task_method(
            TaskValue::from_handle(std::thread::spawn(|| Ok(Value::Bool(true)))),
            "result",
            &[],
            &mut env,
        )
        .expect("task result should succeed");
    assert_eq!(task_join, task_result_ready(Value::Bool(true)));

    let task_error = runtime
        .evaluate_task_method(
            TaskValue::from_handle(std::thread::spawn(|| Ok(Value::Unit))),
            "cancel",
            &[],
            &mut env,
        )
        .expect_err("unsupported task method should fail");
    assert!(task_error.message.contains("unsupported task method"));

    let group = TaskGroupValue::new(&CancellationContext::default());
    let cancel = runtime
        .evaluate_task_group_method(group.clone(), "cancel", &[], &mut env)
        .expect("task-group cancel should succeed");
    assert_eq!(cancel, Value::Unit);

    let no_target = runtime
        .evaluate_task_group_method(group.clone(), "start", &[], &mut env)
        .expect_err("task-group start should reject empty args");
    assert!(no_target.message.contains("expects a target function"));

    let bad_target = runtime
        .evaluate_task_group_method(
            group,
            "start",
            &[mir_arg(Some("target"), Operand::Int(3))],
            &mut env,
        )
        .expect_err("task-group start should stay in MIR lowering");
    assert!(bad_target
        .message
        .contains("should lower to MIR `Spawn` directly"));
}

#[test]
fn mir_runtime_normalizes_negative_vec_indices_for_every_indexed_operation() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let vec_type = Type::Named("list".to_string(), vec![Type::named("int32")]);
    env.define_typed(
        "values",
        vec_type,
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: [10, 20, 30, 40]
                .into_iter()
                .map(|value| Value::Int(IntegerValue::from_signed(value)))
                .collect(),
        }),
    );
    for magnitude in 1..=5 {
        env.define_typed(
            format!("negative_{magnitude}"),
            Type::named("int32"),
            Value::Int(IntegerValue::from_signed(-i128::from(magnitude))),
        );
    }
    let negative = |magnitude| Operand::Place(format!("negative_{magnitude}"));
    let read_vec = |env: &Env| match env.read_place("values").expect("values should exist") {
        Value::Vec(vector) => vector,
        other => panic!("expected Vec, found {other:?}"),
    };

    let indexed = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "__index",
            Some("values"),
            &[
                mir_arg(None, negative(1)),
                mir_arg(None, Operand::Int(3)),
                mir_arg(None, Operand::Int(9)),
            ],
            &mut env,
        )
        .expect("negative indexed read should normalize");
    assert_eq!(indexed, Value::Int(IntegerValue::from_signed(40)));

    runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "__set_index",
            Some("values"),
            &[
                mir_arg(None, negative(2)),
                mir_arg(None, Operand::Int(35)),
                mir_arg(None, Operand::Int(4)),
                mir_arg(None, Operand::Int(5)),
            ],
            &mut env,
        )
        .expect("negative indexed write should normalize");

    let gotten = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "get",
            Some("values"),
            &[mir_arg(Some("index"), negative(2))],
            &mut env,
        )
        .expect("negative get should normalize");
    assert_eq!(
        gotten,
        option_some(Value::Int(IntegerValue::from_signed(35)))
    );
    let missing = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "get",
            Some("values"),
            &[mir_arg(Some("index"), negative(5))],
            &mut env,
        )
        .expect("too-negative get should preserve Option behavior");
    assert_eq!(missing, option_none());

    let replaced = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "set",
            Some("values"),
            &[
                mir_arg(Some("index"), negative(4)),
                mir_arg(Some("value"), Operand::Int(11)),
            ],
            &mut env,
        )
        .expect("negative set should normalize");
    assert_eq!(replaced, Value::Int(IntegerValue::from_signed(10)));

    let popped = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "pop",
            Some("values"),
            &[mir_arg(Some("index"), negative(2))],
            &mut env,
        )
        .expect("negative pop should normalize");
    assert_eq!(popped, Value::Int(IntegerValue::from_signed(35)));

    let swapped = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "swap",
            Some("values"),
            &[
                mir_arg(Some("first"), negative(1)),
                mir_arg(Some("second"), negative(3)),
            ],
            &mut env,
        )
        .expect("negative swap indices should normalize");
    assert_eq!(swapped, Value::Unit);

    let inserted = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "insert",
            Some("values"),
            &[
                mir_arg(Some("index"), negative(1)),
                mir_arg(Some("value"), Operand::Int(99)),
            ],
            &mut env,
        )
        .expect("insert(-1, value) should insert before the final element");
    assert_eq!(inserted, Value::Unit);

    let final_values = read_vec(&mut env)
        .elements
        .into_iter()
        .map(|value| match value {
            Value::Int(value) => value.as_i128().expect("test integers should be signed"),
            other => panic!("expected integer, found {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(final_values, vec![40, 20, 99, 11]);

    for (field, args, expected) in [
        (
            "set",
            vec![
                mir_arg(Some("index"), negative(5)),
                mir_arg(Some("value"), Operand::Int(1)),
            ],
            "list set index `-5` is out of bounds for length `4`",
        ),
        (
            "pop",
            vec![mir_arg(Some("index"), negative(5))],
            "list pop index `-5` is out of bounds for length `4`",
        ),
        (
            "swap",
            vec![
                mir_arg(Some("first"), negative(5)),
                mir_arg(Some("second"), negative(1)),
            ],
            "list swap indices `-5` and `-1` are out of bounds for length `4`",
        ),
    ] {
        let error = runtime
            .evaluate_vec_method(read_vec(&mut env), field, Some("values"), &args, &mut env)
            .expect_err("too-negative trapping Vec operation should fail");
        assert_eq!(error.message, expected);
    }

    let read_error = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "__index",
            Some("values"),
            &[
                mir_arg(None, negative(5)),
                mir_arg(None, Operand::Int(8)),
                mir_arg(None, Operand::Int(6)),
            ],
            &mut env,
        )
        .expect_err("too-negative indexed read should trap");
    assert_eq!(
        read_error.message,
        "list index `-5` is out of bounds for length `4`"
    );
    assert_eq!(read_error.span, Some(crate::diag::Span::new(8, 6)));

    let write_error = runtime
        .evaluate_vec_method(
            read_vec(&mut env),
            "__set_index",
            Some("values"),
            &[
                mir_arg(None, negative(5)),
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(9)),
                mir_arg(None, Operand::Int(7)),
            ],
            &mut env,
        )
        .expect_err("too-negative indexed write should trap");
    assert_eq!(
        write_error.message,
        "list index `-5` is out of bounds for length `4`"
    );
    assert_eq!(write_error.span, Some(crate::diag::Span::new(9, 7)));

    env.define_typed(
        "empty",
        Type::Named("list".to_string(), vec![Type::named("int32")]),
        Value::Vec(crate::runtime_value::VecValue {
            element_type: Type::named("int32"),
            elements: Vec::new(),
        }),
    );
    let empty_insert = runtime
        .evaluate_vec_method(
            match env.read_place("empty").expect("empty should exist") {
                Value::Vec(vector) => vector,
                other => panic!("expected Vec, found {other:?}"),
            },
            "insert",
            Some("empty"),
            &[
                mir_arg(Some("index"), negative(1)),
                mir_arg(Some("value"), Operand::Int(7)),
            ],
            &mut env,
        )
        .expect("insert clamps a too-negative index to the start of an empty list");
    assert_eq!(empty_insert, Value::Unit);
    assert_eq!(
        read_vec(&mut env).elements,
        vec![
            Value::Int(IntegerValue::from_signed(40)),
            Value::Int(IntegerValue::from_signed(20)),
            Value::Int(IntegerValue::from_signed(99)),
            Value::Int(IntegerValue::from_signed(11)),
        ]
    );
    let Value::Vec(empty) = env.read_place("empty").expect("empty list should remain") else {
        panic!("expected list");
    };
    assert_eq!(
        empty.elements,
        vec![Value::Int(IntegerValue::from_signed(7))]
    );
}

#[test]
fn mir_runtime_index_helpers_cover_error_paths() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed("flag", Type::named("bool"), Value::Bool(true));
    env.define_typed(
        "text",
        Type::named("str"),
        Value::String("aura".to_string()),
    );

    let negative = runtime
        .mir_index_from_value(Value::Int(IntegerValue::from_signed(-1)))
        .expect_err("negative indices should fail");
    assert!(negative.message.contains("cannot be negative"));

    let non_integer = runtime
        .mir_index_from_value(Value::Bool(true))
        .expect_err("non-integer indices should fail");
    assert!(non_integer
        .message
        .contains("list indices must be integers"));

    let vec_missing_place = runtime
        .evaluate_vec_method(
            crate::runtime_value::VecValue {
                element_type: Type::named("int32"),
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            },
            "clear",
            None,
            &[],
            &mut env,
        )
        .expect_err("mutable vec methods should require a receiver place");
    assert!(vec_missing_place
        .message
        .contains("requires a mutable list place"));

    for (field, args, expected) in [
        (
            "set",
            vec![
                mir_arg(Some("index"), Operand::Int(0)),
                mir_arg(Some("value"), Operand::Int(1)),
            ],
            "`set` requires a mutable list place",
        ),
        (
            "remove",
            vec![mir_arg(Some("value"), Operand::Int(1))],
            "`remove` requires a mutable list place",
        ),
        (
            "swap",
            vec![
                mir_arg(Some("first"), Operand::Int(0)),
                mir_arg(Some("second"), Operand::Int(1)),
            ],
            "list swap indices `0` and `1` are out of bounds for length `1`",
        ),
        (
            "insert",
            vec![
                mir_arg(Some("index"), Operand::Int(0)),
                mir_arg(Some("value"), Operand::Int(1)),
            ],
            "`insert` requires a mutable list place",
        ),
        (
            "reverse",
            Vec::new(),
            "`reverse` requires a mutable list place",
        ),
        (
            "extend",
            vec![mir_arg(Some("other"), Operand::Bool(true))],
            "`extend` requires another `list[T]` value",
        ),
    ] {
        let error = runtime
            .evaluate_vec_method(
                crate::runtime_value::VecValue {
                    element_type: Type::named("int32"),
                    elements: vec![Value::Int(IntegerValue::from_signed(1))],
                },
                field,
                None,
                &args,
                &mut env,
            )
            .expect_err("vector helper edge should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in diagnostic, got `{}`",
            error.message
        );
    }

    let internal_index_oob = runtime
        .evaluate_vec_method(
            crate::runtime_value::VecValue {
                element_type: Type::named("int32"),
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            },
            "__index",
            Some("missing"),
            &[
                mir_arg(None, Operand::Int(5)),
                mir_arg(None, Operand::Int(9)),
                mir_arg(None, Operand::Int(2)),
            ],
            &mut env,
        )
        .expect_err("internal list indexing should report out-of-bounds spans");
    assert!(internal_index_oob.message.contains("out of bounds"));
    assert_eq!(internal_index_oob.span, Some(crate::diag::Span::new(9, 2)));

    let internal_set_oob = runtime
        .evaluate_vec_method(
            crate::runtime_value::VecValue {
                element_type: Type::named("int32"),
                elements: vec![Value::Int(IntegerValue::from_signed(1))],
            },
            "__set_index",
            Some("missing"),
            &[
                mir_arg(None, Operand::Int(5)),
                mir_arg(None, Operand::Int(7)),
                mir_arg(None, Operand::Int(4)),
                mir_arg(None, Operand::Int(6)),
            ],
            &mut env,
        )
        .expect_err("internal indexed assignment should report out-of-bounds spans");
    assert!(internal_set_oob.message.contains("out of bounds"));
    assert_eq!(internal_set_oob.span, Some(crate::diag::Span::new(4, 6)));

    let map_value = || crate::runtime_value::MapValue {
        key_type: Type::named("str"),
        value_type: Type::named("int32"),
        entries: vec![(
            Value::String("count".to_string()),
            Value::Int(IntegerValue::from_signed(1)),
        )],
    };
    for (field, args, receiver_place, expected) in [
        (
            "__index",
            Vec::new(),
            Some("mapping"),
            "internal map indexing requires key, line, and column operands",
        ),
        (
            "__set_index",
            Vec::new(),
            Some("mapping"),
            "internal map indexed assignment requires key, value, line, and column operands",
        ),
        (
            "remove",
            vec![mir_arg(Some("key"), Operand::String("count".to_string()))],
            None,
            "`remove` requires a mutable dict place",
        ),
        (
            "keys",
            vec![mir_arg(None, Operand::Int(1))],
            Some("mapping"),
            "`keys` does not take arguments",
        ),
        (
            "values",
            vec![mir_arg(None, Operand::Int(1))],
            Some("mapping"),
            "`values` does not take arguments",
        ),
        (
            "items",
            vec![mir_arg(None, Operand::Int(1))],
            Some("mapping"),
            "`items` does not take arguments",
        ),
        (
            "clear",
            vec![mir_arg(None, Operand::Int(1))],
            Some("mapping"),
            "`clear` does not take arguments",
        ),
    ] {
        let error = runtime
            .evaluate_map_method(map_value(), field, receiver_place, &args, &mut env)
            .expect_err("map helper edge should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in diagnostic, got `{}`",
            error.message
        );
    }

    let map_missing_place = runtime
        .evaluate_map_method(
            crate::runtime_value::MapValue {
                key_type: Type::named("str"),
                value_type: Type::named("int32"),
                entries: vec![],
            },
            "clear",
            None,
            &[],
            &mut env,
        )
        .expect_err("mutable map methods should require a receiver place");
    assert!(map_missing_place
        .message
        .contains("requires a mutable map place"));

    let set_missing_place = runtime
        .evaluate_set_method(
            crate::runtime_value::SetValue {
                element_type: Type::named("str"),
                elements: vec![],
            },
            "add",
            None,
            &[mir_arg(Some("value"), Operand::String("go".to_string()))],
            &mut env,
        )
        .expect_err("mutable set methods should require a receiver place");
    assert!(set_missing_place
        .message
        .contains("requires a mutable set place"));

    let set_value = || crate::runtime_value::SetValue {
        element_type: Type::named("str"),
        elements: vec![Value::String("ready".to_string())],
    };
    for (field, args, receiver_place, expected) in [
        (
            "len",
            vec![mir_arg(None, Operand::Int(1))],
            Some("flags"),
            "`len` does not take arguments",
        ),
        (
            "is_empty",
            vec![mir_arg(None, Operand::Int(1))],
            Some("flags"),
            "`is_empty` does not take arguments",
        ),
        (
            "copy",
            vec![mir_arg(None, Operand::Int(1))],
            Some("flags"),
            "`copy` does not take arguments",
        ),
        (
            "remove",
            vec![mir_arg(Some("value"), Operand::String("ready".to_string()))],
            None,
            "`remove` requires a mutable set place",
        ),
        (
            "missing",
            Vec::new(),
            Some("flags"),
            "unsupported set method",
        ),
    ] {
        let error = runtime
            .evaluate_set_method(set_value(), field, receiver_place, &args, &mut env)
            .expect_err("set helper edge should fail");
        assert!(
            error.message.contains(expected),
            "expected `{expected}` in diagnostic, got `{}`",
            error.message
        );
    }
}

#[test]
fn mir_runtime_operator_and_task_helpers_cover_additional_branches() {
    let mut runtime = test_runtime();
    let span = Some(Span::new(4, 5));

    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::And,
                Value::Bool(true),
                Value::Bool(false),
                None,
            )
            .expect("bool and should evaluate"),
        Value::Bool(false)
    );
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Or,
                Value::Bool(false),
                Value::Bool(true),
                None,
            )
            .expect("bool or should evaluate"),
        Value::Bool(true)
    );
    let bad_and = runtime
        .eval_binary(
            crate::ast::BinaryOp::And,
            Value::Bool(true),
            Value::Int(IntegerValue::from_signed(1)),
            None,
        )
        .expect_err("non-bool logical operands should fail");
    assert!(bad_and.message.contains("must both have type `bool`"));
    let bad_or = runtime
        .eval_binary(
            crate::ast::BinaryOp::Or,
            Value::Bool(false),
            Value::Int(IntegerValue::from_signed(1)),
            None,
        )
        .expect_err("non-bool logical operands should fail");
    assert!(bad_or.message.contains("must both have type `bool`"));

    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Add,
                Value::String("au".to_string()),
                Value::String("ra".to_string()),
                None,
            )
            .expect("string addition should concatenate"),
        Value::String("aura".to_string())
    );
    let overflow = runtime
        .eval_binary(
            crate::ast::BinaryOp::Add,
            Value::Int(IntegerValue::from_literal(u128::MAX)),
            Value::Int(IntegerValue::from_signed(1)),
            span,
        )
        .expect_err("integer overflow should fail");
    assert!(overflow.message.contains("integer overflow"));
    let overflow_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Add,
            Value::Int(IntegerValue::from_literal(u128::MAX)),
            Value::Int(IntegerValue::from_signed(1)),
            None,
        )
        .expect_err("integer overflow without a source span should fail");
    assert!(overflow_without_span.message.contains("integer overflow"));
    let bad_add = runtime
        .eval_binary(
            crate::ast::BinaryOp::Add,
            Value::Bool(true),
            Value::String("x".to_string()),
            None,
        )
        .expect_err("unsupported add operands should fail");
    assert!(bad_add.message.contains("matching supported operand types"));

    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Sub,
                Value::Int(IntegerValue::from_signed(9)),
                Value::Int(IntegerValue::from_signed(4)),
                None,
            )
            .expect("integer subtraction should evaluate"),
        Value::Int(IntegerValue::from_signed(5))
    );
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Sub,
                Value::Float(7.5),
                Value::Float(2.0),
                None,
            )
            .expect("float subtraction should evaluate"),
        Value::Float(5.5)
    );
    let sub_overflow = runtime
        .eval_binary(
            crate::ast::BinaryOp::Sub,
            Value::Int(IntegerValue::from_signed(i128::MIN)),
            Value::Int(IntegerValue::from_signed(1)),
            span,
        )
        .expect_err("integer subtraction overflow should fail");
    assert!(sub_overflow.message.contains("integer overflow"));
    let sub_overflow_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Sub,
            Value::Int(IntegerValue::from_signed(i128::MIN)),
            Value::Int(IntegerValue::from_signed(1)),
            None,
        )
        .expect_err("integer subtraction overflow without a source span should fail");
    assert!(sub_overflow_without_span
        .message
        .contains("integer overflow"));
    let bad_sub = runtime
        .eval_binary(
            crate::ast::BinaryOp::Sub,
            Value::String("x".to_string()),
            Value::String("y".to_string()),
            None,
        )
        .expect_err("string subtraction should fail");
    assert!(bad_sub.message.contains("matching numeric operands"));

    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Mul,
                Value::Int(IntegerValue::from_signed(6)),
                Value::Int(IntegerValue::from_signed(7)),
                None,
            )
            .expect("integer multiplication should evaluate"),
        Value::Int(IntegerValue::from_signed(42))
    );
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Mul,
                Value::Float(2.5),
                Value::Float(4.0),
                None,
            )
            .expect("float multiplication should evaluate"),
        Value::Float(10.0)
    );
    let mul_overflow = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mul,
            Value::Int(IntegerValue::from_literal(u128::MAX)),
            Value::Int(IntegerValue::from_signed(2)),
            span,
        )
        .expect_err("integer multiplication overflow should fail");
    assert!(mul_overflow.message.contains("integer overflow"));
    let mul_overflow_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mul,
            Value::Int(IntegerValue::from_literal(u128::MAX)),
            Value::Int(IntegerValue::from_signed(2)),
            None,
        )
        .expect_err("integer multiplication overflow without a source span should fail");
    assert!(mul_overflow_without_span
        .message
        .contains("integer overflow"));
    let bad_mul = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mul,
            Value::Bool(true),
            Value::Bool(false),
            None,
        )
        .expect_err("bool multiplication should fail");
    assert!(bad_mul.message.contains("matching numeric operands"));

    let div_zero = runtime
        .eval_binary(
            crate::ast::BinaryOp::Div,
            Value::Int(IntegerValue::from_signed(4)),
            Value::Int(IntegerValue::from_signed(0)),
            span,
        )
        .expect_err("division by zero should fail");
    assert!(div_zero.message.contains("division by zero"));
    let div_zero_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Div,
            Value::Int(IntegerValue::from_signed(4)),
            Value::Int(IntegerValue::from_signed(0)),
            None,
        )
        .expect_err("division by zero without a source span should fail");
    assert!(div_zero_without_span.message.contains("division by zero"));
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Div,
                Value::Int(IntegerValue::from_signed(9)),
                Value::Int(IntegerValue::from_signed(3)),
                None,
            )
            .expect("integer division should evaluate"),
        Value::Int(IntegerValue::from_signed(3))
    );
    let bad_div = runtime
        .eval_binary(
            crate::ast::BinaryOp::Div,
            Value::String("x".to_string()),
            Value::String("y".to_string()),
            None,
        )
        .expect_err("string division should fail");
    assert!(bad_div.message.contains("matching numeric operands"));
    let float_div_zero = runtime
        .eval_binary(
            crate::ast::BinaryOp::Div,
            Value::Float(7.5),
            Value::Float(0.0),
            span,
        )
        .expect_err("float division by zero should fail");
    assert!(float_div_zero.message.contains("division by zero"));
    let float_div_zero_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Div,
            Value::Float(7.5),
            Value::Float(0.0),
            None,
        )
        .expect_err("float division by zero without a source span should fail");
    assert!(float_div_zero_without_span
        .message
        .contains("division by zero"));
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Div,
                Value::Float(7.5),
                Value::Float(2.5),
                None,
            )
            .expect("float division should evaluate"),
        Value::Float(3.0)
    );

    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::FloorDiv,
                Value::Int(IntegerValue::from_signed(-7)),
                Value::Int(IntegerValue::from_signed(3)),
                None,
            )
            .expect("integer floor division should round toward negative infinity"),
        Value::Int(IntegerValue::from_signed(-3))
    );
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::FloorDiv,
                Value::Float(7.5),
                Value::Float(-2.0),
                None,
            )
            .expect("float floor division should round toward negative infinity"),
        Value::Float(-4.0)
    );
    let floor_div_zero_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::FloorDiv,
            Value::Int(IntegerValue::from_signed(7)),
            Value::Int(IntegerValue::zero()),
            None,
        )
        .expect_err("integer floor division by zero should fail without a source span");
    assert!(floor_div_zero_without_span
        .message
        .contains("division by zero"));
    let float_floor_div_zero_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::FloorDiv,
            Value::Float(7.5),
            Value::Float(0.0),
            None,
        )
        .expect_err("float floor division by zero should fail without a source span");
    assert!(float_floor_div_zero_without_span
        .message
        .contains("division by zero"));
    let float_floor_div_negative_zero = runtime
        .eval_binary(
            crate::ast::BinaryOp::FloorDiv,
            Value::Float(7.5),
            Value::Float(-0.0),
            span,
        )
        .expect_err("float floor division by negative zero should fail");
    assert!(float_floor_div_negative_zero
        .message
        .contains("division by zero"));

    let mod_zero_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mod,
            Value::Int(IntegerValue::from_signed(7)),
            Value::Int(IntegerValue::from_signed(0)),
            None,
        )
        .expect_err("integer remainder by zero without a source span should fail");
    assert!(mod_zero_without_span.message.contains("division by zero"));
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Mod,
                Value::Int(IntegerValue::from_signed(7)),
                Value::Int(IntegerValue::from_signed(3)),
                None,
            )
            .expect("integer remainder should evaluate"),
        Value::Int(IntegerValue::from_signed(1))
    );
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Mod,
                Value::Float(7.5),
                Value::Float(2.0),
                None,
            )
            .expect("float remainder should evaluate"),
        Value::Float(1.5)
    );
    let bad_mod = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mod,
            Value::Bool(true),
            Value::Bool(false),
            None,
        )
        .expect_err("bool remainder should fail");
    assert!(bad_mod.message.contains("matching numeric operands"));

    let float_mod_zero = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mod,
            Value::Float(7.5),
            Value::Float(0.0),
            span,
        )
        .expect_err("float remainder by zero should fail");
    assert!(float_mod_zero.message.contains("division by zero"));
    let float_mod_zero_without_span = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mod,
            Value::Float(7.5),
            Value::Float(0.0),
            None,
        )
        .expect_err("float remainder by zero without a source span should fail");
    assert!(float_mod_zero_without_span
        .message
        .contains("division by zero"));
    let float_mod_negative_zero = runtime
        .eval_binary(
            crate::ast::BinaryOp::Mod,
            Value::Float(7.5),
            Value::Float(-0.0),
            span,
        )
        .expect_err("float remainder by negative zero should fail");
    assert!(float_mod_negative_zero.message.contains("division by zero"));

    let task = TaskValue::from_handle(std::thread::spawn(|| Ok(Value::Bool(true))));
    let mut env = Env::default();
    let clone_error = runtime
        .evaluate_task_method(task.clone(), "clone", &[], &mut env)
        .expect_err("task clone should be unsupported");
    assert!(clone_error
        .message
        .contains("unsupported task method `clone`"));
    let join_args = runtime
        .evaluate_task_method(
            task.clone(),
            "result",
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("result should reject arguments");
    assert!(join_args
        .message
        .contains("`result(timeout=...)` expects `Duration`, found `1`"));
    let bad_task_member = runtime
        .evaluate_task_method(task, "missing", &[], &mut env)
        .expect_err("unknown task members should fail");
    assert!(bad_task_member
        .message
        .contains("unsupported task method `missing`"));

    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let mut env = Env::default();
    assert_eq!(
        runtime
            .evaluate_task_group_method(group.clone(), "cancel", &[], &mut env)
            .expect("group cancel should succeed"),
        Value::Unit
    );
    let spawn_error = runtime
        .evaluate_task_group_method(group.clone(), "start", &[], &mut env)
        .expect_err("group start should reject empty arg lists");
    assert!(spawn_error.message.contains("expects a target function"));
    let bad_group_member = runtime
        .evaluate_task_group_method(group, "missing", &[], &mut env)
        .expect_err("unknown task-group members should fail");
    assert!(bad_group_member
        .message
        .contains("unsupported task-group method `missing`"));
}

#[test]
fn mir_runtime_duration_arithmetic_is_checked_exact_and_ordered() {
    let runtime = test_runtime();
    let int = |value| Value::Int(IntegerValue::from_signed(value));

    for (op, left, right, expected) in [
        (
            crate::ast::BinaryOp::Add,
            Value::Duration(7),
            Value::Duration(5),
            Value::Duration(12),
        ),
        (
            crate::ast::BinaryOp::Sub,
            Value::Duration(7),
            Value::Duration(9),
            Value::Duration(-2),
        ),
        (
            crate::ast::BinaryOp::Mul,
            Value::Duration(7),
            int(-3),
            Value::Duration(-21),
        ),
        (
            crate::ast::BinaryOp::Mul,
            int(-3),
            Value::Duration(7),
            Value::Duration(-21),
        ),
        (
            crate::ast::BinaryOp::FloorDiv,
            Value::Duration(7),
            int(-3),
            Value::Duration(-3),
        ),
        (
            crate::ast::BinaryOp::FloorDiv,
            Value::Duration(-7),
            int(3),
            Value::Duration(-3),
        ),
    ] {
        assert_eq!(
            runtime
                .eval_binary(op, left, right, None)
                .expect("supported Duration arithmetic should evaluate"),
            expected
        );
    }

    for (op, expected) in [
        (crate::ast::BinaryOp::Less, true),
        (crate::ast::BinaryOp::LessEq, true),
        (crate::ast::BinaryOp::Greater, false),
        (crate::ast::BinaryOp::GreaterEq, false),
    ] {
        assert_eq!(
            runtime
                .eval_binary(op, Value::Duration(-1), Value::Duration(1), None)
                .expect("Duration ordering should evaluate"),
            Value::Bool(expected)
        );
    }
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::Eq,
                Value::Duration(1),
                Value::Duration(1),
                None,
            )
            .expect("Duration equality should evaluate"),
        Value::Bool(true)
    );
    assert_eq!(
        runtime
            .eval_binary(
                crate::ast::BinaryOp::NotEq,
                Value::Duration(1),
                Value::Duration(2),
                None,
            )
            .expect("Duration inequality should evaluate"),
        Value::Bool(true)
    );

    for (op, left, right, message) in [
        (
            crate::ast::BinaryOp::Add,
            Value::Duration(i128::MAX),
            Value::Duration(1),
            "duration overflow",
        ),
        (
            crate::ast::BinaryOp::Sub,
            Value::Duration(i128::MIN),
            Value::Duration(1),
            "duration overflow",
        ),
        (
            crate::ast::BinaryOp::Mul,
            Value::Duration(i128::MAX),
            int(2),
            "duration overflow",
        ),
        (
            crate::ast::BinaryOp::FloorDiv,
            Value::Duration(1),
            int(0),
            "division by zero",
        ),
        (
            crate::ast::BinaryOp::FloorDiv,
            Value::Duration(i128::MIN),
            int(-1),
            "duration overflow",
        ),
    ] {
        let error = runtime
            .eval_binary(op, left, right, Some(Span::new(7, 9)))
            .expect_err("invalid Duration arithmetic should trap");
        assert!(
            error.message.contains(message),
            "expected `{message}`, got `{}`",
            error.message
        );
        assert_eq!(error.span, Some(Span::new(7, 9)));
    }
}

#[test]
fn mir_runtime_task_result_or_helpers_cover_nonblocking_shortcuts() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let ready_task = TaskValue::from_handle(std::thread::spawn(|| Ok(Value::Bool(true))));
    match ready_task
        .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
        .expect("one-second task deadline should fit")
    {
        crate::runtime_value::TaskWaitStatus::Ready(Ok(Value::Bool(true))) => {}
        other => panic!("expected ready bool task, got {other:?}"),
    }

    let maybe_ready = runtime
        .evaluate_task_method(ready_task.clone(), "result_or_none", &[], &mut env)
        .expect("completed result_or_none should use cached task result");
    assert_eq!(
        enum_payloads(maybe_ready, "Option", "Some"),
        vec![Value::Bool(true)]
    );
    assert_eq!(
        runtime
            .evaluate_task_method(
                ready_task,
                "result_or",
                &[mir_arg(None, Operand::Bool(false))],
                &mut env,
            )
            .expect("completed result_or should use cached task result"),
        Value::Bool(true)
    );

    let root_result = crate::runtime_value::run_lightweight_root_task(|| {
        let cancelled_task =
            crate::runtime_value::spawn_lightweight_task(|| -> crate::diag::Result<Value> {
                crate::runtime_value::cancel_current_lightweight_task_boundary()
            })?;
        match cancelled_task
            .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None)
            .expect("one-second task deadline should fit")
        {
            crate::runtime_value::TaskWaitStatus::Cancelled => {}
            other => panic!("expected cancelled lightweight task, got {other:?}"),
        }

        let mut runtime = test_runtime();
        let mut env = Env::default();
        assert_eq!(
            runtime.evaluate_task_method(
                cancelled_task.clone(),
                "result_or_none",
                &[],
                &mut env
            )?,
            option_none()
        );
        assert_eq!(
            runtime.evaluate_task_method(
                cancelled_task,
                "result_or",
                &[mir_arg(None, Operand::String("fallback".to_string()))],
                &mut env,
            )?,
            Value::String("fallback".to_string())
        );
        Ok(Value::Unit)
    })
    .expect("cancelled lightweight task shortcuts should evaluate");
    assert_eq!(root_result, Value::Unit);

    let cancellation = CancellationContext::default();
    let group = TaskGroupValue::new(&cancellation);
    let cancelled_runtime_context = group.child_cancellation();
    group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancelled_runtime_context,
    );
    let blocker = ChannelValue::new();
    let unblocker = blocker.clone();
    let pending_task = TaskValue::from_handle(std::thread::spawn(move || {
        let _ = unblocker.recv_with_cancellation(None, None);
        Ok(Value::Unit)
    }));
    assert_eq!(
        cancelled_runtime
            .evaluate_task_method(pending_task.clone(), "result_or_none", &[], &mut env)
            .expect("cancelled runtimes should return Option.None"),
        option_none()
    );
    assert_eq!(
        cancelled_runtime
            .evaluate_task_method(
                pending_task.clone(),
                "result_or",
                &[mir_arg(None, Operand::String("fallback".to_string()))],
                &mut env,
            )
            .expect("cancelled runtimes should return the fallback"),
        Value::String("fallback".to_string())
    );
    blocker.close();
    let _ =
        pending_task.wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None);
}

#[test]
fn mir_runtime_single_consumer_task_results_claim_every_observing_attempt() {
    let mut runtime = test_runtime();
    let mut env = Env::default();

    let repeatable = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Ok(Value::Bool(true))),
        true,
    );
    for _ in 0..2 {
        assert_eq!(
            enum_payloads(
                runtime
                    .join_task(repeatable.clone(), Some(StdDuration::from_secs(1)))
                    .expect("repeatable task results should remain observable"),
                "TaskResult",
                "Ready",
            ),
            vec![Value::Bool(true)]
        );
    }

    let error_task = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Err(Diagnostic::new("task failed"))),
        false,
    );
    enum_payloads(
        runtime
            .join_task(error_task.clone(), Some(StdDuration::from_secs(1)))
            .expect("the first error observation should consume the right"),
        "TaskResult",
        "Error",
    );
    let repeated_error = runtime
        .join_task(error_task, Some(StdDuration::from_secs(1)))
        .expect_err("an error outcome must still consume a non-repeatable result");
    assert_eq!(repeated_error.code, "AU4001");

    let timeout_blocker = ChannelValue::new();
    let timeout_unblocker = timeout_blocker.clone();
    let timed_task = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(move || {
            let _ = timeout_unblocker.recv_with_cancellation(None, None);
            Ok(Value::String("late".to_string()))
        }),
        false,
    );
    enum_payloads(
        runtime
            .join_task(timed_task.clone(), Some(StdDuration::ZERO))
            .expect("the first timed observation should return a TaskResult"),
        "TaskResult",
        "TimedOut",
    );
    let repeated_timeout = runtime
        .join_task(timed_task.clone(), Some(StdDuration::ZERO))
        .expect_err("a timeout must consume a non-repeatable observation right");
    assert_eq!(repeated_timeout.code, "AU4001");
    timeout_blocker.close();

    let default_blocker = ChannelValue::new();
    let default_unblocker = default_blocker.clone();
    let default_task = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(move || {
            let _ = default_unblocker.recv_with_cancellation(None, None);
            Ok(Value::String("late".to_string()))
        }),
        false,
    );
    assert_eq!(
        runtime
            .evaluate_task_method(
                default_task.clone(),
                "result_or",
                &[mir_arg(None, Operand::String("fallback".to_string()))],
                &mut env,
            )
            .expect("the first default observation should succeed"),
        Value::String("fallback".to_string())
    );
    let repeated_default = runtime
        .evaluate_task_method(default_task, "result_or_none", &[], &mut env)
        .expect_err("a default outcome must consume a non-repeatable observation right");
    assert_eq!(repeated_default.code, "AU4001");
    default_blocker.close();

    let parent = CancellationContext::default();
    let group = TaskGroupValue::new(&parent);
    let cancelled_context = group.child_cancellation();
    group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancelled_context,
    );
    let cancel_blocker = ChannelValue::new();
    let cancel_unblocker = cancel_blocker.clone();
    let cancelled_task = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(move || {
            let _ = cancel_unblocker.recv_with_cancellation(None, None);
            Ok(Value::String("late".to_string()))
        }),
        false,
    );
    enum_payloads(
        cancelled_runtime
            .join_task(cancelled_task.clone(), None)
            .expect("the first cancelled observation should return a TaskResult"),
        "TaskResult",
        "Cancelled",
    );
    let repeated_cancel = cancelled_runtime
        .join_task(cancelled_task, None)
        .expect_err("cancellation must consume a non-repeatable observation right");
    assert_eq!(repeated_cancel.code, "AU4001");
    cancel_blocker.close();
}

#[test]
fn mir_runtime_wait_helpers_claim_distinct_nonrepeatable_tasks_before_observing() {
    let mut runtime = test_runtime();
    let duplicate = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Ok(Value::String("ready".to_string()))),
        false,
    );
    let duplicate_error = runtime
        .wait_any(
            vec![duplicate.clone(), duplicate.clone()],
            Some(StdDuration::from_secs(1)),
        )
        .expect_err("one wait call must reject duplicate non-repeatable aliases");
    assert_eq!(duplicate_error.code, "AU4001");
    let repeated = runtime
        .join_task(duplicate, Some(StdDuration::from_secs(1)))
        .expect_err("the failed duplicate attempt must consume the observation right");
    assert_eq!(repeated.code, "AU4001");

    let duplicate_all = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Ok(Value::String("once".to_string()))),
        false,
    );
    let duplicate_all_error = runtime
        .wait_all(
            vec![duplicate_all.clone(), duplicate_all],
            Some(StdDuration::from_secs(1)),
        )
        .expect_err("wait_all must not deliver one non-repeatable result twice");
    assert_eq!(duplicate_all_error.code, "AU4001");

    let selected = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Ok(Value::String("selected".to_string()))),
        false,
    );
    enum_payloads(
        runtime
            .wait_any(vec![selected.clone()], Some(StdDuration::from_secs(1)))
            .expect("wait_any should deliver one uniquely claimed result"),
        "WaitAny",
        "Ready",
    );
    let repeated = runtime
        .join_task(selected, Some(StdDuration::from_secs(1)))
        .expect_err("wait_any must consume the selected task result");
    assert_eq!(repeated.code, "AU4001");

    let first = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Ok(Value::String("first".to_string()))),
        false,
    );
    let second = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(|| Err(Diagnostic::new("second failed"))),
        false,
    );
    enum_payloads(
        runtime
            .wait_all(
                vec![first.clone(), second.clone()],
                Some(StdDuration::from_secs(1)),
            )
            .expect("wait_all should report the task error as a value"),
        "WaitAll",
        "Error",
    );
    for claimed in [first, second] {
        let error = runtime
            .join_task(claimed, Some(StdDuration::from_secs(1)))
            .expect_err("wait_all must claim every result before waiting");
        assert_eq!(error.code, "AU4001");
    }
}

#[test]
fn mir_runtime_select_adapter_preserves_source_order_and_queue_losers() {
    let first = ChannelValue::new();
    let second = ChannelValue::new();
    first
        .send(Value::String("first".to_string()))
        .expect("first queue should accept its item");
    second
        .send(Value::String("second".to_string()))
        .expect("second queue should accept its item");

    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "first",
        Type::Named("Queue".to_string(), vec![Type::named("str")]),
        Value::Channel(first),
    );
    env.define_typed(
        "second",
        Type::Named("Queue".to_string(), vec![Type::named("str")]),
        Value::Channel(second.clone()),
    );
    let selected = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[
                mir_arg(None, Operand::Place("first".to_string())),
                mir_arg(None, Operand::Place("second".to_string())),
            ],
            &mut env,
        )
        .expect("the lowest ready queue index should win");
    let selected = enum_payloads(selected, "SelectOutcome", "Queue");
    assert_eq!(
        selected[0],
        Value::Int(IntegerValue::from_signed(0)),
        "the result index is the original argument position"
    );
    assert_eq!(
        enum_payloads(selected[1].clone(), "QueueReceive", "Item"),
        vec![Value::String("first".to_string())]
    );
    assert_eq!(
        second.try_recv(),
        crate::runtime_value::TryRecvResult::Value(Value::String("second".to_string())),
        "a simultaneously ready losing queue must not lose its item"
    );

    let closed = ChannelValue::new();
    closed.close();
    env.define_typed(
        "closed",
        Type::Named("Queue".to_string(), vec![Type::named("str")]),
        Value::Channel(closed),
    );
    let selected = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[mir_arg(None, Operand::Place("closed".to_string()))],
            &mut env,
        )
        .expect("a closed queue should be immediately ready");
    let selected = enum_payloads(selected, "SelectOutcome", "Queue");
    assert_eq!(selected[0], Value::Int(IntegerValue::from_signed(0)));
    assert!(enum_payloads(selected[1].clone(), "QueueReceive", "Closed").is_empty());
}

#[test]
fn mir_runtime_select_adapter_reports_task_outcomes_and_claims_losers() {
    let ready = TaskValue::from_handle(std::thread::spawn(|| {
        Ok(Value::String("ready".to_string()))
    }));
    let failed =
        TaskValue::from_handle(std::thread::spawn(|| Err(Diagnostic::new("select failed"))));
    while ready.completed_result_observed().is_none()
        || failed.completed_result_observed().is_none()
    {
        std::thread::yield_now();
    }
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "ready",
        Type::Named("Task".to_string(), vec![Type::named("str")]),
        Value::Task(ready),
    );
    env.define_typed(
        "failed",
        Type::Named("Task".to_string(), vec![Type::named("str")]),
        Value::Task(failed),
    );
    let selected = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[
                mir_arg(None, Operand::Place("ready".to_string())),
                mir_arg(None, Operand::Place("failed".to_string())),
            ],
            &mut env,
        )
        .expect("the lowest-index completed task should win");
    let selected = enum_payloads(selected, "SelectOutcome", "Task");
    assert_eq!(selected[0], Value::Int(IntegerValue::from_signed(0)));
    assert_eq!(
        enum_payloads(selected[1].clone(), "TaskResult", "Ready"),
        vec![Value::String("ready".to_string())]
    );

    let failed_only =
        TaskValue::from_handle(std::thread::spawn(|| Err(Diagnostic::new("task error"))));
    env.define_typed(
        "failed_only",
        Type::Named("Task".to_string(), vec![Type::named("str")]),
        Value::Task(failed_only),
    );
    let failed_outcome = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[mir_arg(None, Operand::Place("failed_only".to_string()))],
            &mut env,
        )
        .expect("a failed child is a typed select outcome");
    let failed_outcome = enum_payloads(failed_outcome, "SelectOutcome", "Task");
    assert_eq!(
        enum_payloads(failed_outcome[1].clone(), "TaskResult", "Error"),
        vec![Value::String("task error".to_string())]
    );

    let blocker = ChannelValue::new();
    let unblocker = blocker.clone();
    let losing = TaskValue::from_handle_with_result_repeatability(
        std::thread::spawn(move || {
            let _ = unblocker.recv_with_cancellation(None, None);
            Ok(Value::String("late".to_string()))
        }),
        false,
    );
    env.define_typed(
        "losing",
        Type::Named("Task".to_string(), vec![Type::named("str")]),
        Value::Task(losing.clone()),
    );
    let deadline = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[
                mir_arg(None, Operand::Duration(0)),
                mir_arg(None, Operand::MovePlace("losing".to_string())),
            ],
            &mut env,
        )
        .expect("an immediate deadline should beat a pending task");
    assert_eq!(
        enum_payloads(deadline, "SelectOutcome", "Deadline"),
        vec![Value::Int(IntegerValue::from_signed(0))]
    );
    blocker.close();
    let repeated = runtime
        .join_task(losing, Some(StdDuration::from_secs(1)))
        .expect_err("select must abandon a losing non-repeatable task observation right");
    assert_eq!(repeated.code, "AU4001");
}

#[test]
fn mir_runtime_select_adapter_distinguishes_child_and_current_task_cancellation() {
    let child_outcome = crate::runtime_value::run_lightweight_root_task(|| {
        let child =
            crate::runtime_value::spawn_lightweight_task(|| -> crate::diag::Result<Value> {
                crate::runtime_value::cancel_current_lightweight_task_boundary()
            })?;
        while child.completed_result_observed().is_none() {
            crate::runtime_value::yield_now_with_runtime_scheduler();
        }
        let mut runtime = test_runtime();
        let mut env = Env::default();
        env.define_typed(
            "child",
            Type::Named("Task".to_string(), vec![Type::Unit]),
            Value::Task(child),
        );
        runtime.evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[mir_arg(None, Operand::Place("child".to_string()))],
            &mut env,
        )
    })
    .expect("a cancelled child should be returned as its TaskResult");
    let child_outcome = enum_payloads(child_outcome, "SelectOutcome", "Task");
    assert!(enum_payloads(child_outcome[1].clone(), "TaskResult", "Cancelled").is_empty());

    let parent = CancellationContext::default();
    let group = TaskGroupValue::new(&parent);
    let cancellation = group.child_cancellation();
    group.cancel();
    let ready = ChannelValue::new();
    ready
        .send(Value::String("preserved".to_string()))
        .expect("queue should accept the ready value");
    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancellation,
    );
    let mut env = Env::default();
    env.define_typed(
        "ready",
        Type::Named("Queue".to_string(), vec![Type::named("str")]),
        Value::Channel(ready.clone()),
    );
    let cancelled = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[mir_arg(None, Operand::Place("ready".to_string()))],
            &mut env,
        )
        .expect("current-task cancellation should be a select result");
    assert!(enum_payloads(cancelled, "SelectOutcome", "Cancelled").is_empty());
    assert_eq!(
        ready.try_recv(),
        crate::runtime_value::TryRecvResult::Value(Value::String("preserved".to_string())),
        "cancellation wins before a ready source and must not consume it"
    );
}

#[test]
fn mir_runtime_select_adapter_rejects_invalid_deadlines_and_named_sources() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    for invalid in [-1, i128::MAX] {
        let error = runtime
            .evaluate_call(
                &CallTarget::Name("select".to_string()),
                &[mir_arg(None, Operand::Duration(invalid))],
                &mut env,
            )
            .expect_err("invalid select deadlines must trap");
        assert_eq!(error.code, "AU4001");
    }

    let named = runtime
        .evaluate_call(
            &CallTarget::Name("select".to_string()),
            &[mir_arg(Some("source"), Operand::Duration(0))],
            &mut env,
        )
        .expect_err("malformed MIR must not smuggle a named select source");
    assert_eq!(named.code, "AU4001");
    assert!(named.message.contains("positional source"));
}

#[test]
fn mir_runtime_select_adapter_validates_typed_source_descriptors_before_arbitration() {
    fn select_error(runtime: &mut MirRuntime, env: &mut Env, source_names: &[&str]) -> Diagnostic {
        let args = source_names
            .iter()
            .map(|name| mir_arg(None, Operand::Place((*name).to_string())))
            .collect::<Vec<_>>();
        runtime
            .evaluate_call(&CallTarget::Name("select".to_string()), &args, env)
            .expect_err("malformed MIR select descriptors must trap")
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();

    let missing_type_queue = ChannelValue::new();
    missing_type_queue
        .send(Value::String("ready".to_string()))
        .expect("the malformed queue should be ready so a missing validation fails promptly");
    env.values.insert(
        "missing_type".to_string(),
        Value::Channel(missing_type_queue),
    );
    let missing = select_error(&mut runtime, &mut env, &["missing_type"]);
    assert_eq!(missing.code, "AU4001");
    assert!(missing.message.contains("missing source type metadata"));

    env.define_typed(
        "wrong_type",
        Type::named("str"),
        Value::Channel(ChannelValue::new()),
    );
    let wrong = select_error(&mut runtime, &mut env, &["wrong_type"]);
    assert_eq!(wrong.code, "AU4001");
    assert!(wrong.message.contains("`Queue[T]`"));
    assert!(wrong.message.contains("`Task[T]`"));
    assert!(wrong.message.contains("`Duration`"));

    env.define_typed(
        "wrong_queue_arity",
        Type::named("Queue"),
        Value::Channel(ChannelValue::new()),
    );
    let wrong_arity = select_error(&mut runtime, &mut env, &["wrong_queue_arity"]);
    assert_eq!(wrong_arity.code, "AU4001");
    assert!(wrong_arity.message.contains("Queue[T]"));

    env.define_typed(
        "wrong_task_arity",
        Type::named("Task"),
        Value::Task(TaskValue::from_handle(std::thread::spawn(|| {
            Ok(Value::Unit)
        }))),
    );
    let wrong_task_arity = select_error(&mut runtime, &mut env, &["wrong_task_arity"]);
    assert_eq!(wrong_task_arity.code, "AU4001");
    assert!(wrong_task_arity.message.contains("Task[T]"));

    env.define_typed(
        "wrong_duration_arity",
        Type::Named("Duration".to_string(), vec![Type::named("str")]),
        Value::Duration(0),
    );
    let wrong_duration_arity = select_error(&mut runtime, &mut env, &["wrong_duration_arity"]);
    assert_eq!(wrong_duration_arity.code, "AU4001");
    assert!(wrong_duration_arity
        .message
        .contains("malformed Duration descriptor"));

    env.define_typed(
        "queue_value_mismatch",
        Type::Named("Queue".to_string(), vec![Type::named("str")]),
        Value::Duration(0),
    );
    let queue_mismatch = select_error(&mut runtime, &mut env, &["queue_value_mismatch"]);
    assert_eq!(queue_mismatch.code, "AU4001");
    assert!(queue_mismatch.message.contains("Queue[str]"));
    assert!(queue_mismatch.message.contains("queue runtime value"));

    env.define_typed(
        "task_value_mismatch",
        Type::Named("Task".to_string(), vec![Type::named("str")]),
        Value::Duration(0),
    );
    let task_mismatch = select_error(&mut runtime, &mut env, &["task_value_mismatch"]);
    assert_eq!(task_mismatch.code, "AU4001");
    assert!(task_mismatch.message.contains("Task[str]"));
    assert!(task_mismatch.message.contains("task runtime value"));

    env.define_typed(
        "duration_value_mismatch",
        Type::named("Duration"),
        Value::Channel(ChannelValue::new()),
    );
    let duration_mismatch = select_error(&mut runtime, &mut env, &["duration_value_mismatch"]);
    assert_eq!(duration_mismatch.code, "AU4001");
    assert!(duration_mismatch.message.contains("Duration"));
    assert!(duration_mismatch.message.contains("duration runtime value"));

    env.define_typed(
        "queue_string",
        Type::Named("Queue".to_string(), vec![Type::named("str")]),
        Value::Channel(ChannelValue::new()),
    );
    env.define_typed(
        "queue_int",
        Type::Named("Queue".to_string(), vec![Type::named("int32")]),
        Value::Channel(ChannelValue::new()),
    );
    let mixed_queues = select_error(&mut runtime, &mut env, &["queue_string", "queue_int"]);
    assert_eq!(mixed_queues.code, "AU4001");
    assert!(mixed_queues.message.contains("common Queue payload type"));
    assert!(mixed_queues.message.contains("str"));
    assert!(mixed_queues.message.contains("int32"));

    env.define_typed(
        "task_string",
        Type::Named("Task".to_string(), vec![Type::named("str")]),
        Value::Task(TaskValue::from_handle(std::thread::spawn(|| {
            Ok(Value::String("ready".to_string()))
        }))),
    );
    env.define_typed(
        "task_bool",
        Type::Named("Task".to_string(), vec![Type::named("bool")]),
        Value::Task(TaskValue::from_handle(std::thread::spawn(|| {
            Ok(Value::Bool(true))
        }))),
    );
    let mixed_tasks = select_error(&mut runtime, &mut env, &["task_string", "task_bool"]);
    assert_eq!(mixed_tasks.code, "AU4001");
    assert!(mixed_tasks.message.contains("common Task result type"));
    assert!(mixed_tasks.message.contains("str"));
    assert!(mixed_tasks.message.contains("bool"));
}

#[test]
fn mir_runtime_wait_helpers_cover_task_lists_ready_error_timeout_and_cancel_paths() {
    let mut runtime = test_runtime();
    let ready_task = TaskValue::from_handle(std::thread::spawn(|| Ok(Value::Bool(true))));
    let task_list = Value::Vec(VecValue {
        element_type: Type::Named("Task".to_string(), vec![Type::named("bool")]),
        elements: vec![Value::Task(ready_task.clone())],
    });
    assert_eq!(
        runtime
            .expect_task_list(&task_list, "wait_any(...)")
            .expect("task vectors should decode")
            .len(),
        1
    );
    let non_vec = runtime
        .expect_task_list(&Value::Bool(true), "wait_any(...)")
        .expect_err("non-vector task lists should fail");
    assert!(non_vec.message.contains("expects `list[Task[T]]`"));
    let non_task_list = Value::Vec(VecValue {
        element_type: Type::named("int32"),
        elements: vec![Value::Int(IntegerValue::from_signed(1))],
    });
    let non_task = runtime
        .expect_task_list(&non_task_list, "wait_any(...)")
        .expect_err("task vectors with non-task elements should fail");
    assert!(non_task.message.contains("expects `list[Task[T]]`"));

    assert_eq!(
        enum_payloads(
            runtime
                .join_task(ready_task.clone(), Some(StdDuration::from_secs(1)))
                .expect("ready task should join"),
            "TaskResult",
            "Ready",
        ),
        vec![Value::Bool(true)]
    );
    assert_eq!(
        enum_payloads(
            runtime
                .wait_any(vec![ready_task.clone()], Some(StdDuration::from_secs(1)))
                .expect("ready wait_any should succeed"),
            "WaitAny",
            "Ready",
        ),
        vec![Value::Int(IntegerValue::from_signed(0)), Value::Bool(true)]
    );
    let wait_all_ready = enum_payloads(
        runtime
            .wait_all(vec![ready_task], Some(StdDuration::from_secs(1)))
            .expect("ready wait_all should succeed"),
        "WaitAll",
        "Ready",
    );
    match wait_all_ready.as_slice() {
        [Value::Vec(values)] => assert_eq!(values.elements, vec![Value::Bool(true)]),
        other => panic!("expected WaitAll.Ready vector payload, found {other:?}"),
    }

    let error_task =
        TaskValue::from_handle(std::thread::spawn(|| Err(Diagnostic::new("task failed"))));
    assert_eq!(
        enum_payloads(
            runtime
                .join_task(error_task.clone(), Some(StdDuration::from_secs(1)))
                .expect("error task should join as TaskResult.Error"),
            "TaskResult",
            "Error",
        ),
        vec![Value::String("task failed".to_string())]
    );
    assert_eq!(
        enum_payloads(
            runtime
                .wait_any(vec![error_task.clone()], Some(StdDuration::from_secs(1)))
                .expect("error wait_any should return WaitAny.Error"),
            "WaitAny",
            "Error",
        ),
        vec![
            Value::Int(IntegerValue::from_signed(0)),
            Value::String("task failed".to_string())
        ]
    );
    let wait_all_error_task =
        TaskValue::from_handle(std::thread::spawn(|| Err(Diagnostic::new("all failed"))));
    assert_eq!(
        enum_payloads(
            runtime
                .wait_all(vec![wait_all_error_task], Some(StdDuration::from_secs(1)),)
                .expect("error wait_all should return WaitAll.Error"),
            "WaitAll",
            "Error",
        ),
        vec![
            Value::Int(IntegerValue::from_signed(0)),
            Value::String("all failed".to_string())
        ]
    );

    let timeout_blocker = ChannelValue::new();
    let timeout_unblocker = timeout_blocker.clone();
    let pending_task = TaskValue::from_handle(std::thread::spawn(move || {
        let _ = timeout_unblocker.recv_with_cancellation(None, None);
        Ok(Value::Unit)
    }));
    enum_payloads(
        runtime
            .join_task(pending_task.clone(), Some(StdDuration::ZERO))
            .expect("timed out join_task should return a TaskResult"),
        "TaskResult",
        "TimedOut",
    );
    enum_payloads(
        runtime
            .wait_any(vec![pending_task.clone()], Some(StdDuration::ZERO))
            .expect("timed out wait_any should return a WaitAny value"),
        "WaitAny",
        "TimedOut",
    );
    enum_payloads(
        runtime
            .wait_all(vec![pending_task.clone()], Some(StdDuration::ZERO))
            .expect("timed out wait_all should return a WaitAll value"),
        "WaitAll",
        "TimedOut",
    );
    timeout_blocker.close();
    let _ =
        pending_task.wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None);

    enum_payloads(
        runtime
            .wait_any(Vec::new(), None)
            .expect("empty wait_any should time out immediately"),
        "WaitAny",
        "TimedOut",
    );

    let parent = CancellationContext::default();
    let group = TaskGroupValue::new(&parent);
    let cancelled_context = group.child_cancellation();
    group.cancel();
    let mut cancelled_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        cancelled_context,
    );
    enum_payloads(
        cancelled_runtime
            .wait_any(Vec::new(), None)
            .expect("empty wait_any should observe cancellation"),
        "WaitAny",
        "Cancelled",
    );

    let cancel_blocker = ChannelValue::new();
    let cancel_unblocker = cancel_blocker.clone();
    let cancelled_task = TaskValue::from_handle(std::thread::spawn(move || {
        let _ = cancel_unblocker.recv_with_cancellation(None, None);
        Ok(Value::Unit)
    }));
    enum_payloads(
        cancelled_runtime
            .join_task(cancelled_task.clone(), None)
            .expect("cancelled join_task should return a TaskResult"),
        "TaskResult",
        "Cancelled",
    );
    enum_payloads(
        cancelled_runtime
            .wait_any(vec![cancelled_task.clone()], None)
            .expect("cancelled wait_any should return a WaitAny value"),
        "WaitAny",
        "Cancelled",
    );
    enum_payloads(
        cancelled_runtime
            .wait_all(vec![cancelled_task.clone()], None)
            .expect("cancelled wait_all should return a WaitAll value"),
        "WaitAll",
        "Cancelled",
    );
    cancel_blocker.close();
    let _ = cancelled_task
        .wait_result_with_cancellation_observed(Some(StdDuration::from_secs(1)), None);
}

#[test]
fn mir_runtime_print_tolerates_poisoned_stdout_lock() {
    let stdout = Arc::new(Mutex::new(String::new()));
    let poisoned = stdout.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.lock().expect("poison setup lock should succeed");
        panic!("poison stdout lock");
    })
    .join();

    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        stdout.clone(),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    let value = Value::Int(IntegerValue::from_signed(3));
    env.define_typed("value", Type::named("int32"), value.clone());

    let printed = runtime
        .evaluate_call(
            &crate::mir::CallTarget::Name("print".to_string()),
            &[mir_arg(Some("value"), Operand::Place("value".to_string()))],
            &mut env,
        )
        .expect("poisoned stdout should not panic");
    assert_eq!(printed, Value::Unit);
    assert_eq!(
        stdout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str(),
        "3\n"
    );
}

#[test]
fn mir_runtime_io_write_streams_to_stdout_sink() {
    let stdout = Arc::new(Mutex::new(String::new()));
    let streamed = Arc::new(Mutex::new(String::new()));
    let sink_output = streamed.clone();
    let sink = Arc::new(move |chunk: &str| {
        sink_output
            .lock()
            .expect("sink output should lock")
            .push_str(chunk);
    });
    let mut runtime = MirRuntime::new_with_stdout_sink(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        stdout.clone(),
        Some(sink),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    env.define_typed(
        "text",
        Type::named("str"),
        Value::String("hello".to_string()),
    );

    runtime
        .evaluate_call(
            &CallTarget::Name("io::write".to_string()),
            &[mir_arg(Some("text"), Operand::Place("text".to_string()))],
            &mut env,
        )
        .expect("io.write should succeed");

    assert_eq!(*stdout.lock().expect("stdout should lock"), "hello");
    assert_eq!(*streamed.lock().expect("sink output should lock"), "hello");
}

#[test]
fn mir_runtime_range_rejects_unsigned_endpoints_outside_signed_index_space() {
    let error = build_range(vec![EvaluatedMirArg {
        ty: None,
        name: Some("stop".to_string()),
        value: Value::Int(IntegerValue::from_literal((i128::MAX as u128) + 1)),
        writeback_place: None,
    }])
    .expect_err("oversized unsigned range endpoints should fail");
    assert!(error
        .message
        .contains("must fit in signed index space in MIR runtime"));
    let start_error = build_range(vec![
        EvaluatedMirArg {
            ty: None,
            name: Some("start".to_string()),
            value: Value::Int(IntegerValue::from_literal((i128::MAX as u128) + 1)),
            writeback_place: None,
        },
        EvaluatedMirArg {
            ty: None,
            name: Some("stop".to_string()),
            value: Value::Int(IntegerValue::from_signed(1)),
            writeback_place: None,
        },
    ])
    .expect_err("oversized unsigned range starts should fail");
    assert!(start_error
        .message
        .contains("start must fit in signed index space"));
}

#[test]
fn mir_runtime_terminator_and_cleanup_helpers_cover_branch_and_error_paths() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    let mut loop_state = HashMap::new();
    let mut cleanup_stack = Vec::new();

    env.define_typed("cond", Type::named("bool"), Value::Bool(true));
    match runtime
        .execute_terminator(
            "entry",
            &Terminator::Branch {
                condition: Operand::Place("cond".to_string()),
                then_label: "then".to_string(),
                else_label: "else".to_string(),
            },
            &mut env,
            &mut loop_state,
            &mut cleanup_stack,
        )
        .expect("bool branch should succeed")
    {
        super::BlockOutcome::Goto(label) => assert_eq!(label, "then"),
        super::BlockOutcome::Return(_) => panic!("expected goto"),
    }

    env.define_typed(
        "not_bool",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(1)),
    );
    let branch_error = match runtime.execute_terminator(
        "entry",
        &Terminator::Branch {
            condition: Operand::Place("not_bool".to_string()),
            then_label: "then".to_string(),
            else_label: "else".to_string(),
        },
        &mut env,
        &mut loop_state,
        &mut cleanup_stack,
    ) {
        Ok(_) => panic!("non-bool branches should fail"),
        Err(error) => error,
    };
    assert!(branch_error.message.contains("must evaluate to `bool`"));

    env.define_typed(
        "iter",
        Type::named("Range"),
        Value::Range(RangeValue { start: 0, end: 2 }),
    );
    env.define_typed(
        "item",
        Type::named("int32"),
        Value::Int(IntegerValue::zero()),
    );
    match runtime
        .execute_terminator(
            "loop",
            &Terminator::ForRange {
                binding: "item".to_string(),
                iterable: Operand::Place("iter".to_string()),
                body_label: "body".to_string(),
                exit_label: "exit".to_string(),
            },
            &mut env,
            &mut loop_state,
            &mut cleanup_stack,
        )
        .expect("range loop should start")
    {
        super::BlockOutcome::Goto(label) => assert_eq!(label, "body"),
        super::BlockOutcome::Return(_) => panic!("expected goto"),
    }
    assert_eq!(
        env.read_place("item")
            .expect("loop binding should be written"),
        Value::Int(IntegerValue::zero())
    );
    let _ = runtime.execute_terminator(
        "loop",
        &Terminator::ForRange {
            binding: "item".to_string(),
            iterable: Operand::Place("iter".to_string()),
            body_label: "body".to_string(),
            exit_label: "exit".to_string(),
        },
        &mut env,
        &mut loop_state,
        &mut cleanup_stack,
    );
    match runtime
        .execute_terminator(
            "loop",
            &Terminator::ForRange {
                binding: "item".to_string(),
                iterable: Operand::Place("iter".to_string()),
                body_label: "body".to_string(),
                exit_label: "exit".to_string(),
            },
            &mut env,
            &mut loop_state,
            &mut cleanup_stack,
        )
        .expect("range loop should exit")
    {
        super::BlockOutcome::Goto(label) => assert_eq!(label, "exit"),
        super::BlockOutcome::Return(_) => panic!("expected goto"),
    }

    env.define_typed(
        "bad_iter",
        Type::named("int32"),
        Value::Int(IntegerValue::zero()),
    );
    let range_error = match runtime.execute_terminator(
        "bad-loop",
        &Terminator::ForRange {
            binding: "item".to_string(),
            iterable: Operand::Place("bad_iter".to_string()),
            body_label: "body".to_string(),
            exit_label: "exit".to_string(),
        },
        &mut env,
        &mut loop_state,
        &mut cleanup_stack,
    ) {
        Ok(_) => panic!("non-range iterables should fail"),
        Err(error) => error,
    };
    assert!(range_error.message.contains("requires a `Range`"));

    env.define_typed(
        "status",
        Type::named("Status"),
        Value::EnumVariant(crate::runtime_value::EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: Vec::new(),
        }),
    );
    match runtime
        .execute_terminator(
            "match",
            &Terminator::Match {
                scrutinee: Operand::Place("status".to_string()),
                arms: vec![
                    MirMatchArm {
                        enum_name: None,
                        variant_name: Some("Ready".to_string()),
                        label: "ready".to_string(),
                        wildcard: false,
                    },
                    MirMatchArm {
                        enum_name: None,
                        variant_name: None,
                        label: "wild".to_string(),
                        wildcard: true,
                    },
                ],
                otherwise: "other".to_string(),
            },
            &mut env,
            &mut loop_state,
            &mut cleanup_stack,
        )
        .expect("match should select a branch")
    {
        super::BlockOutcome::Goto(label) => assert_eq!(label, "ready"),
        super::BlockOutcome::Return(_) => panic!("expected goto"),
    }

    let match_error = match runtime.execute_terminator(
        "match",
        &Terminator::Match {
            scrutinee: Operand::Place("bad_iter".to_string()),
            arms: Vec::new(),
            otherwise: "other".to_string(),
        },
        &mut env,
        &mut loop_state,
        &mut cleanup_stack,
    ) {
        Ok(_) => panic!("non-enum matches should fail"),
        Err(error) => error,
    };
    assert!(match_error.message.contains("expected an enum value"));

    let unreachable = match runtime.execute_terminator(
        "dead",
        &Terminator::Unreachable,
        &mut env,
        &mut loop_state,
        &mut cleanup_stack,
    ) {
        Ok(_) => panic!("unreachable terminators should fail"),
        Err(error) => error,
    };
    assert!(unreachable
        .message
        .contains("reached unreachable MIR block"));

    let underflow = runtime
        .pop_cleanup("resource", &mut Vec::new(), &mut env, false)
        .expect_err("missing cleanup entries should fail");
    assert!(underflow.message.contains("cleanup stack underflow"));

    let mut mismatched_stack = vec!["other".to_string()];
    let mismatch = runtime
        .pop_cleanup("resource", &mut mismatched_stack, &mut env, false)
        .expect_err("mismatched cleanup entries should fail");
    assert!(mismatch.message.contains("cleanup stack mismatch"));
}

#[test]
fn mir_runtime_entrypoint_call_and_type_helpers_cover_remaining_edges() {
    assert_eq!(
        run_native_entry(
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
        ),
        1
    );

    let source = "def main() -> int32:\n    return 0\n";
    let mir = crate::lower_source_to_mir(source).expect("source should lower");
    let mir_json = serde_json::to_vec(&mir).expect("mir should serialize");
    let bad_utf8 = [0xffu8];
    assert_eq!(
        run_native_entry(
            mir_json.as_ptr(),
            mir_json.len(),
            bad_utf8.as_ptr(),
            bad_utf8.len(),
            source.as_ptr(),
            source.len(),
        ),
        1
    );
    assert_eq!(
        run_native_entry(
            mir_json.as_ptr(),
            mir_json.len(),
            b"/tmp/test.au".as_ptr(),
            b"/tmp/test.au".len(),
            bad_utf8.as_ptr(),
            bad_utf8.len(),
        ),
        1
    );
    let tiny = [b'x'];
    assert_eq!(
        run_native_entry(
            tiny.as_ptr(),
            (1 << 30) + 1,
            b"/tmp/test.au".as_ptr(),
            b"/tmp/test.au".len(),
            source.as_ptr(),
            source.len(),
        ),
        1
    );

    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: vec![MirClass {
                name: "Pair".to_string(),
                type_params: vec!["T".to_string(), "U".to_string()],
                fields: vec![crate::mir::MirClassField {
                    name: "left".to_string(),
                    ty: Type::TypeParam("T".to_string()),
                }],
                methods: Vec::new(),
            }],
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );

    assert_eq!(
        runtime.infer_instance_type(&InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::from([(
                "left".to_string(),
                Value::Int(IntegerValue::from_signed(9)),
            )]),
        }),
        Some(Type::Named(
            "Pair".to_string(),
            vec![Type::named("int64"), Type::named("Unknown")],
        )),
    );
    assert_eq!(
        runtime.infer_instance_type(&InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::new(),
        }),
        None
    );
    assert_eq!(
        runtime.infer_runtime_value_type(&option_none()),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("Unknown")]
        )),
    );
    assert_eq!(
        runtime.infer_runtime_value_type(&result_ok(Value::Int(IntegerValue::from_signed(4)))),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("int64"), Type::named("Unknown")],
        )),
    );

    let mut env = Env::default();
    env.define_typed(
        "pair",
        Type::Named(
            "Pair".to_string(),
            vec![Type::named("int32"), Type::named("bool")],
        ),
        Value::Instance(InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::from([(
                "left".to_string(),
                Value::Int(IntegerValue::from_signed(1)),
            )]),
        }),
    );
    env.define_typed(
        "number",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(2)),
    );
    let pair_type = Type::Named(
        "Pair".to_string(),
        vec![Type::named("int32"), Type::named("bool")],
    );
    env.define_typed(
        "wrapped",
        Type::Tuple(vec![pair_type.clone()]),
        Value::Tuple(TupleValue {
            element_types: vec![pair_type],
            elements: vec![Value::Instance(InstanceValue {
                class_name: "Pair".to_string(),
                fields: BTreeMap::from([(
                    "left".to_string(),
                    Value::Int(IntegerValue::from_signed(3)),
                )]),
            })],
        }),
    );
    assert_eq!(
        runtime.resolve_place_type("pair", &mut env),
        Some(Type::Named(
            "Pair".to_string(),
            vec![Type::named("int32"), Type::named("bool")]
        ))
    );
    assert_eq!(
        runtime.resolve_place_type("pair.left", &mut env),
        Some(Type::named("int32"))
    );
    assert_eq!(
        runtime.resolve_place_type("wrapped.0.left", &mut env),
        Some(Type::named("int32"))
    );
    assert_eq!(runtime.resolve_place_type("number.value", &mut env), None);
    runtime
        .validate_value_fits_type(&Value::Bool(true), &Type::named("int32"), None)
        .expect("non-integer values are ignored by integer-width validation");
    let overflow = runtime
        .validate_value_fits_type(
            &Value::Int(IntegerValue::from_signed(999)),
            &Type::named("int8"),
            None,
        )
        .expect_err("overflowing integers should fail validation");
    assert!(overflow.message.contains("does not fit in `int8`"));
    assert_eq!(
        runtime
            .coerce_value_to_type(
                Value::Int(IntegerValue::from_signed(7)),
                &Type::named("float64"),
                None
            )
            .expect("int-to-float coercion should work"),
        Value::Float(7.0)
    );
    assert_eq!(
        runtime
            .coerce_value_to_type(Value::Float(7.0), &Type::named("int32"), None)
            .expect("float-to-int coercion should work"),
        Value::Int(IntegerValue::from_signed(7))
    );

    let missing_receiver = MirFunction {
        name: "touch".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(crate::mir::MirReceiverKind::Borrow),
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let receiver_error = match runtime.call_function(&missing_receiver, None, Vec::new()) {
        Ok(_) => panic!("receiver methods should require an explicit receiver"),
        Err(error) => error,
    };
    assert!(receiver_error.message.contains("missing its receiver"));

    let borrow_mut = MirFunction {
        name: "mutate".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
        params: vec![MirParam {
            name: "value".to_string(),
            passing: crate::mir::MirReceiverKind::BorrowMut,
            ty: Type::named("int32"),
            default_function: None,
        }],
        local_types: vec![MirLocalType {
            name: "temp".to_string(),
            ty: Type::named("int32"),
        }],
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![
                Instruction::Assign {
                    target: "temp".to_string(),
                    value: Rvalue::Use(Operand::Int(4)),
                },
                Instruction::Eval {
                    value: Operand::Place("temp".to_string()),
                },
            ],
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let group = TaskGroupValue::new(&CancellationContext::default());
    env.define_typed(
        "group",
        Type::named("TaskGroup"),
        Value::TaskGroup(group.clone()),
    );
    let outcome = runtime
        .call_function(
            &borrow_mut,
            Some(Value::Instance(InstanceValue {
                class_name: "Pair".to_string(),
                fields: BTreeMap::from([(
                    "left".to_string(),
                    Value::Int(IntegerValue::from_signed(8)),
                )]),
            })),
            vec![EvaluatedMirArg {
                ty: None,
                name: None,
                value: Value::Int(IntegerValue::from_signed(11)),
                writeback_place: Some("value".to_string()),
            }],
        )
        .expect("borrow-mut functions should return updated writebacks");
    assert_eq!(
        outcome.updated_receiver,
        Some(Value::Instance(InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::from([(
                "left".to_string(),
                Value::Int(IntegerValue::from_signed(8)),
            )]),
        }))
    );
    assert_eq!(
        outcome.updated_params,
        vec![(0, Value::Int(IntegerValue::from_signed(11)))]
    );
    let mut cleanup_stack = Vec::new();
    let mut safepoint_fuel = crate::mir::MIR_LOOP_SAFEPOINT_INTERVAL;
    runtime
        .execute_instruction(
            &Instruction::PushCleanup {
                place: "group".to_string(),
            },
            &mut env,
            &mut cleanup_stack,
            &mut safepoint_fuel,
        )
        .expect("push cleanup should succeed");
    assert_eq!(cleanup_stack, vec!["group".to_string()]);
    runtime
        .execute_instruction(
            &Instruction::PopCleanup {
                place: "group".to_string(),
                cancel_before_cleanup: true,
            },
            &mut env,
            &mut cleanup_stack,
            &mut safepoint_fuel,
        )
        .expect("pop cleanup should run the resource cleanup path");
    assert!(cleanup_stack.is_empty());

    let bad_entry = MirFunction {
        name: "broken".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "missing".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let execute_error = runtime
        .execute_function(&bad_entry, &mut Env::default())
        .expect_err("missing block labels should fail execution");
    assert!(execute_error
        .message
        .contains("unknown MIR block `missing`"));

    let cleanup_function = MirFunction {
        name: "cleanup".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "current".to_string(),
        blocks: vec![
            BasicBlock {
                label: "inner".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::ForRange {
                    binding: "i".to_string(),
                    iterable: Operand::Place("iter".to_string()),
                    body_label: "body".to_string(),
                    exit_label: "after".to_string(),
                },
            },
            BasicBlock {
                label: "current".to_string(),
                instructions: Vec::new(),
                terminator: Terminator::Goto("after".to_string()),
            },
        ],
    };
    let mut loop_state =
        HashMap::from([("inner".to_string(), 1i128), ("current".to_string(), 2i128)]);
    MirRuntime::clear_exited_for_range_states(
        &cleanup_function,
        "current",
        "after",
        &mut loop_state,
    );
    assert!(!loop_state.contains_key("inner"));
    assert!(loop_state.contains_key("current"));
}

#[test]
fn mir_runtime_entrypoint_and_env_helpers_cover_write_stream_and_place_edges() {
    let mut sink = Vec::new();
    write_stream(&mut sink, "aura").expect("write_stream should write into Vec sinks");
    assert_eq!(
        String::from_utf8(sink).expect("sink should be UTF-8"),
        "aura"
    );

    let source = "def main() -> int32:\n    print(1)\n    return 7\n";
    let module = crate::lower_source_to_mir(source).expect("source should lower to MIR");
    let mir_json = serde_json::to_vec(&module).expect("MIR should serialize");
    assert_eq!(
        super::run_serialized_mir_entrypoint(&mir_json, "/tmp/entry.au", source),
        7
    );
    assert_eq!(
        super::run_serialized_mir_entrypoint(b"{not json", "/tmp/entry.au", source),
        1
    );

    let env = Env::default();
    let empty_place = env
        .read_place("")
        .expect_err("empty places should be rejected");
    assert!(empty_place.message.contains("empty MIR place"));
    let missing_place = env
        .read_place("missing")
        .expect_err("missing places should be rejected");
    assert!(missing_place.message.contains("unknown MIR place"));

    let mut env = Env::default();
    env.define_typed("flag", Type::named("bool"), Value::Bool(true));
    let non_instance = env
        .read_place("flag.value")
        .expect_err("scalar field access should fail");
    assert!(non_instance
        .message
        .contains("cannot access field `value` on non-instance MIR place `flag.value`"));

    env.define_typed(
        "pair",
        Type::named("Pair"),
        Value::Instance(InstanceValue {
            class_name: "Pair".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    let missing_field = env
        .read_place("pair.value")
        .expect_err("missing fields should fail");
    assert!(missing_field
        .message
        .contains("class `Pair` has no field `value` in MIR place `pair.value`"));

    let empty_write = env
        .write_place("", Value::Unit)
        .expect_err("empty roots should be rejected");
    assert!(empty_write.message.contains("empty MIR place"));
}

#[test]
fn mir_runtime_cleanup_and_rvalue_helpers_cover_remaining_error_paths() {
    let close_fn = MirFunction {
        name: "close_managed".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let close_borrow_fn = MirFunction {
        name: "close_borrow_managed".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: Some(crate::mir::MirReceiverKind::Borrow),
        params: Vec::new(),
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let managed_class = MirClass {
        name: "Managed".to_string(),
        type_params: Vec::new(),
        fields: vec![crate::mir::MirClassField {
            name: "value".to_string(),
            ty: Type::named("int32"),
        }],
        methods: vec![MirMethod {
            name: "close".to_string(),
            function_name: "close_managed".to_string(),
            receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
        }],
    };
    let borrow_managed_class = MirClass {
        name: "BorrowManaged".to_string(),
        type_params: Vec::new(),
        fields: Vec::new(),
        methods: vec![MirMethod {
            name: "close".to_string(),
            function_name: "close_borrow_managed".to_string(),
            receiver: Some(crate::mir::MirReceiverKind::Borrow),
        }],
    };
    let worker_class = MirClass {
        name: "Worker".to_string(),
        type_params: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
    };
    let broken_class = MirClass {
        name: "Broken".to_string(),
        type_params: Vec::new(),
        fields: Vec::new(),
        methods: vec![MirMethod {
            name: "close".to_string(),
            function_name: "missing_body".to_string(),
            receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
        }],
    };
    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![close_fn, close_borrow_fn],
            classes: vec![
                managed_class,
                borrow_managed_class,
                worker_class,
                broken_class,
            ],
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    env.define_typed(
        "count",
        Type::named("int32"),
        Value::Int(IntegerValue::from_signed(1)),
    );
    let non_resource = runtime
        .run_cleanup_place("count", &mut env, false)
        .expect_err("non-resource cleanup targets should fail");
    assert!(non_resource.message.contains("is not a managed resource"));

    env.define_typed(
        "completed",
        Type::named("process.Completed"),
        Value::ProcessCompleted(ProcessCompletedValue::new(
            Value::EnumVariant(EnumVariantValue {
                enum_name: "process.ExitStatus".to_string(),
                variant_name: "Exited".to_string(),
                payloads: vec![Value::Int(IntegerValue::from_signed(0))],
            }),
            Vec::new(),
            Vec::new(),
        )),
    );
    runtime
        .run_cleanup_place("completed", &mut env, false)
        .expect("completed process values should be harmless cleanup resources");

    let pipe_child = ProcessChildValue::spawn(
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf pipe-cleanup".to_string(),
        ],
        None,
        Vec::new(),
        ProcessStdioConfig::Null,
        ProcessStdioConfig::Pipe,
        ProcessStdioConfig::Null,
        false,
    )
    .expect("process child should spawn with piped stdout");
    let stdout_pipe = pipe_child.stdout().expect("child stdout should be piped");
    pipe_child.wait(
        Some(StdDuration::from_secs(2)),
        Some(&CancellationContext::default()),
    );
    env.define_typed(
        "pipe",
        Type::named("process.Pipe"),
        Value::ProcessPipe(stdout_pipe.clone()),
    );
    runtime
        .run_cleanup_place("pipe", &mut env, false)
        .expect("process pipe cleanup should close the pipe");

    env.define_typed(
        "ghost",
        Type::named("Ghost"),
        Value::Instance(InstanceValue {
            class_name: "Ghost".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    let unknown_class = runtime
        .run_cleanup_place("ghost", &mut env, false)
        .expect_err("unknown MIR classes should fail cleanup");
    assert!(unknown_class.message.contains("unknown MIR class `Ghost`"));

    env.define_typed(
        "worker",
        Type::named("Worker"),
        Value::Instance(InstanceValue {
            class_name: "Worker".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    let missing_close = runtime
        .run_cleanup_place("worker", &mut env, false)
        .expect_err("classes without close should fail");
    assert!(missing_close
        .message
        .contains("cannot be used with MIR `with` because it has no `close` method"));

    env.define_typed(
        "broken",
        Type::named("Broken"),
        Value::Instance(InstanceValue {
            class_name: "Broken".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    let missing_body = runtime
        .run_cleanup_place("broken", &mut env, false)
        .expect_err("missing method bodies should fail");
    assert!(missing_body
        .message
        .contains("unknown MIR method body `missing_body`"));

    env.define_typed(
        "managed",
        Type::named("Managed"),
        Value::Instance(InstanceValue {
            class_name: "Managed".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(3)),
            )]),
        }),
    );
    runtime
        .run_cleanup_place("managed", &mut env, false)
        .expect("managed resources with close methods should clean up");

    env.define_typed(
        "borrow_managed",
        Type::named("BorrowManaged"),
        Value::Instance(InstanceValue {
            class_name: "BorrowManaged".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    runtime
        .run_cleanup_place("borrow_managed", &mut env, false)
        .expect("borrowed close receivers should clean up without receiver writeback");

    match runtime
        .evaluate_rvalue(
            &Rvalue::Unary {
                op: crate::ast::UnaryOp::Not,
                value: Operand::Bool(true),
                span: Span::new(1, 1),
            },
            &mut env,
        )
        .expect("MIR unary not should evaluate booleans")
    {
        super::RvalueOutcome::Value(value) => assert_eq!(value, Value::Bool(false)),
        _ => panic!("expected unary value outcome"),
    }
    match runtime
        .evaluate_rvalue(
            &Rvalue::Unary {
                op: crate::ast::UnaryOp::Neg,
                value: Operand::Int(4),
                span: Span::new(1, 1),
            },
            &mut env,
        )
        .expect("MIR unary neg should evaluate integers")
    {
        super::RvalueOutcome::Value(value) => {
            assert_eq!(value, Value::Int(IntegerValue::from_signed(-4)))
        }
        _ => panic!("expected unary value outcome"),
    }
    match runtime
        .evaluate_rvalue(
            &Rvalue::Unary {
                op: crate::ast::UnaryOp::Neg,
                value: Operand::Float(-1.5),
                span: Span::new(1, 1),
            },
            &mut env,
        )
        .expect("MIR unary neg should evaluate floats")
    {
        super::RvalueOutcome::Value(value) => assert_eq!(value, Value::Float(1.5)),
        _ => panic!("expected unary value outcome"),
    }
    let not_type = match runtime.evaluate_rvalue(
        &Rvalue::Unary {
            op: crate::ast::UnaryOp::Not,
            value: Operand::Int(1),
            span: Span::new(1, 1),
        },
        &mut env,
    ) {
        Ok(_) => panic!("MIR unary not should reject non-booleans"),
        Err(error) => error,
    };
    assert!(not_type.message.contains("`not` expects `bool`"));
    let neg_type = match runtime.evaluate_rvalue(
        &Rvalue::Unary {
            op: crate::ast::UnaryOp::Neg,
            value: Operand::String("nope".to_string()),
            span: Span::new(1, 1),
        },
        &mut env,
    ) {
        Ok(_) => panic!("MIR unary neg should reject non-numeric values"),
        Err(error) => error,
    };
    assert!(neg_type
        .message
        .contains("unary `-` expects a numeric value"));

    let try_non_result = match runtime.evaluate_rvalue(
        &Rvalue::Try {
            value: Operand::Int(1),
        },
        &mut env,
    ) {
        Ok(_) => panic!("MIR try should require Result values"),
        Err(error) => error,
    };
    assert!(try_non_result
        .message
        .contains("MIR `try` requires a `Result` value"));

    env.define_typed(
        "option_value",
        Type::Named("Option".to_string(), vec![Type::named("int32")]),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Option".to_string(),
            variant_name: "Some".to_string(),
            payloads: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
    );
    let try_wrong_enum = match runtime.evaluate_rvalue(
        &Rvalue::Try {
            value: Operand::Place("option_value".to_string()),
        },
        &mut env,
    ) {
        Ok(_) => panic!("MIR try should require Result enum values"),
        Err(error) => error,
    };
    assert!(try_wrong_enum
        .message
        .contains("MIR `try` requires a `Result` value"));

    env.define_typed(
        "ok_result",
        Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")],
        ),
        result_ok(Value::Int(IntegerValue::from_signed(8))),
    );
    match runtime
        .evaluate_rvalue(
            &Rvalue::Try {
                value: Operand::Place("ok_result".to_string()),
            },
            &mut env,
        )
        .expect("MIR try should unwrap Result.Ok payloads")
    {
        super::RvalueOutcome::Value(value) => {
            assert_eq!(value, Value::Int(IntegerValue::from_signed(8)))
        }
        _ => panic!("expected try value outcome"),
    }

    env.define_typed(
        "err_result",
        Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")],
        ),
        result_err(Value::String("boom".to_string())),
    );
    runtime.return_type_stack.push(Type::Named(
        "Result".to_string(),
        vec![Type::named("int32"), Type::named("str")],
    ));
    match runtime
        .evaluate_rvalue(
            &Rvalue::Try {
                value: Operand::Place("err_result".to_string()),
            },
            &mut env,
        )
        .expect("MIR try should return Result.Err payloads")
    {
        super::RvalueOutcome::Return(Value::EnumVariant(variant)) => {
            assert_eq!(variant.enum_name, "Result");
            assert_eq!(variant.variant_name, "Err");
            assert_eq!(variant.payloads, vec![Value::String("boom".to_string())]);
        }
        _ => panic!("expected try return outcome"),
    }
    runtime.return_type_stack.pop();

    env.define_typed(
        "broken_result",
        Type::Named(
            "Result".to_string(),
            vec![Type::named("int32"), Type::named("str")],
        ),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Result".to_string(),
            variant_name: "Ok".to_string(),
            payloads: Vec::new(),
        }),
    );
    let invalid_payload = match runtime.evaluate_rvalue(
        &Rvalue::Try {
            value: Operand::Place("broken_result".to_string()),
        },
        &mut env,
    ) {
        Ok(_) => panic!("invalid Result payloads should fail"),
        Err(error) => error,
    };
    assert!(invalid_payload
        .message
        .contains("encountered an invalid `Result` payload"));

    let non_enum_payload = match runtime.evaluate_rvalue(
        &Rvalue::VariantPayload {
            scrutinee: Operand::Int(1),
            variant_name: "Some".to_string(),
            index: 0,
        },
        &mut env,
    ) {
        Ok(_) => panic!("variant payload extraction should require enum values"),
        Err(error) => error,
    };
    assert!(non_enum_payload.message.contains("expected an enum value"));

    env.define_typed(
        "payload_status",
        Type::named("Status"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: vec![Value::String("ok".to_string())],
        }),
    );
    match runtime
        .evaluate_rvalue(
            &Rvalue::VariantPayload {
                scrutinee: Operand::Place("payload_status".to_string()),
                variant_name: "Ready".to_string(),
                index: 0,
            },
            &mut env,
        )
        .expect("variant payload extraction should return existing payloads")
    {
        super::RvalueOutcome::Value(value) => assert_eq!(value, Value::String("ok".to_string())),
        _ => panic!("expected variant payload value outcome"),
    }

    env.define_typed(
        "status",
        Type::named("Status"),
        Value::EnumVariant(EnumVariantValue {
            enum_name: "Status".to_string(),
            variant_name: "Ready".to_string(),
            payloads: Vec::new(),
        }),
    );
    let no_payload = match runtime.evaluate_rvalue(
        &Rvalue::VariantPayload {
            scrutinee: Operand::Place("status".to_string()),
            variant_name: "Ready".to_string(),
            index: 0,
        },
        &mut env,
    ) {
        Ok(_) => panic!("unit variants should reject payload extraction"),
        Err(error) => error,
    };
    assert!(no_payload.message.contains("does not carry a payload"));

    let member_on_int = match runtime.evaluate_rvalue(
        &Rvalue::Member {
            object: Operand::Int(1),
            field: "value".to_string(),
        },
        &mut env,
    ) {
        Ok(_) => panic!("member access on scalars should fail"),
        Err(error) => error,
    };
    assert!(member_on_int
        .message
        .contains("cannot access field `value` on non-instance value"));

    env.define_typed(
        "empty_instance",
        Type::named("Managed"),
        Value::Instance(InstanceValue {
            class_name: "Managed".to_string(),
            fields: BTreeMap::new(),
        }),
    );
    let missing_field = match runtime.evaluate_rvalue(
        &Rvalue::Member {
            object: Operand::Place("empty_instance".to_string()),
            field: "missing".to_string(),
        },
        &mut env,
    ) {
        Ok(_) => panic!("missing fields should fail member access"),
        Err(error) => error,
    };
    assert!(missing_field.message.contains("has no field `missing`"));
}

#[test]
fn mir_runtime_env_and_entry_helpers_cover_additional_branch_paths() {
    let mut env = Env::default();
    env.define_typed(
        "root",
        Type::named("Box"),
        Value::Instance(InstanceValue {
            class_name: "Box".to_string(),
            fields: BTreeMap::from([(
                "value".to_string(),
                Value::Int(IntegerValue::from_signed(4)),
            )]),
        }),
    );
    assert_eq!(
        env.read_place("root.value")
            .expect("nested place should read"),
        Value::Int(IntegerValue::from_signed(4))
    );
    let missing_place = env
        .write_place("missing.value", Value::Bool(true))
        .expect_err("missing MIR roots should fail");
    assert!(missing_place
        .message
        .contains("unknown MIR place `missing.value`"));
    let empty_write = env
        .write_place("", Value::Bool(true))
        .expect_err("empty MIR roots should be rejected");
    assert!(empty_write.message.contains("empty MIR place"));

    let runtime = test_runtime();
    assert_eq!(
        runtime
            .resolve_place_type("root", &mut env)
            .expect("root type should resolve"),
        Type::named("Box")
    );
    assert!(runtime.resolve_place_type("root.value", &mut env).is_none());
    assert!(runtime.resolve_place_type("missing", &mut env).is_none());

    let typed_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: vec![MirClass {
                name: "Box".to_string(),
                type_params: Vec::new(),
                fields: vec![crate::mir::MirClassField {
                    name: "value".to_string(),
                    ty: Type::named("int32"),
                }],
                methods: Vec::new(),
            }],
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    assert_eq!(
        typed_runtime.resolve_place_type("root.value", &mut env),
        Some(Type::named("int32"))
    );

    let mut no_top_level = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![MirFunction {
                name: "main".to_string(),
                module_name: "<test>".to_string(),
                source_path: None,
                span: Span::new(1, 1),
                receiver: None,
                params: Vec::new(),
                local_types: Vec::new(),
                return_type: Type::named("int32"),
                entry: "entry".to_string(),
                blocks: vec![BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(9)),
                }],
            }],
            classes: vec![MirClass {
                name: "Box".to_string(),
                type_params: vec!["T".to_string()],
                fields: vec![crate::mir::MirClassField {
                    name: "value".to_string(),
                    ty: Type::TypeParam("T".to_string()),
                }],
                methods: Vec::new(),
            }],
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    assert_eq!(
        no_top_level.run_main().expect("main should execute"),
        Value::Int(IntegerValue::from_signed(9))
    );
    assert_eq!(
        MirRuntime::infer_value_type(&Value::ModuleNamespace(
            crate::runtime_value::ModuleNamespaceValue {
                path: "pkg.tools".to_string(),
            },
        )),
        None
    );

    let mut needs_receiver = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![MirFunction {
                name: "update".to_string(),
                module_name: "<test>".to_string(),
                source_path: None,
                span: Span::new(1, 1),
                receiver: Some(crate::mir::MirReceiverKind::BorrowMut),
                params: Vec::new(),
                local_types: Vec::new(),
                return_type: Type::Unit,
                entry: "entry".to_string(),
                blocks: vec![BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Unit),
                }],
            }],
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let update = needs_receiver
        .functions
        .get("update")
        .cloned()
        .expect("update function should exist");
    let receiver_error = match needs_receiver.call_function(&update, None, Vec::new()) {
        Ok(_) => panic!("missing MIR receivers should fail"),
        Err(error) => error,
    };
    assert!(receiver_error.message.contains("missing its receiver"));

    let panic_code = run_native_entry(
        std::ptr::null(),
        0,
        std::ptr::null(),
        0,
        std::ptr::null(),
        0,
    );
    assert_eq!(panic_code, 1);

    let missing_main_runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: Some(MirFunction {
                name: "<top-level>".to_string(),
                module_name: "<test>".to_string(),
                source_path: None,
                span: Span::new(1, 1),
                receiver: None,
                params: Vec::new(),
                local_types: Vec::new(),
                return_type: Type::named("int32"),
                entry: "entry".to_string(),
                blocks: vec![BasicBlock {
                    label: "entry".to_string(),
                    instructions: Vec::new(),
                    terminator: Terminator::Return(Operand::Int(0)),
                }],
            }),
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    assert_eq!(
        missing_main_runtime.resolve_place_type("missing", &Env::default()),
        None
    );
}

#[test]
fn mir_assert_fail_preserves_default_custom_empty_and_whitespace_messages() {
    for (message, expected) in [
        (None, "assertion failed"),
        (Some(Operand::String("custom".to_string())), "custom"),
        (Some(Operand::String(String::new())), ""),
        (Some(Operand::String(" \t ".to_string())), " \t "),
    ] {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        let mut loop_state = HashMap::new();
        let mut cleanups = Vec::new();
        let result = runtime.execute_terminator(
            "entry",
            &Terminator::AssertFail {
                message,
                captures: Vec::new(),
                span: Span::new(7, 9),
            },
            &mut env,
            &mut loop_state,
            &mut cleanups,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("assert-fail terminators must trap"),
        };
        assert_eq!(error.code, "AU4001");
        assert_eq!(error.message, expected);
        assert_eq!(error.span, Some(Span::new(7, 9)));
    }

    let mut runtime = test_runtime();
    let mut env = Env::default();
    let mut loop_state = HashMap::new();
    let mut cleanups = Vec::new();
    let error = match runtime.execute_terminator(
        "entry",
        &Terminator::AssertFail {
            message: Some(Operand::Int(17)),
            captures: Vec::new(),
            span: Span::new(11, 13),
        },
        &mut env,
        &mut loop_state,
        &mut cleanups,
    ) {
        Ok(_) => panic!("malformed MIR assertion messages must still be strings"),
        Err(error) => error,
    };
    assert_eq!(error.code, "AU4001");
    assert_eq!(
        error.message,
        "MIR assertion message must evaluate to `str`, found `17`"
    );
    assert_eq!(error.span, Some(Span::new(11, 13)));
}

#[test]
fn mir_assert_fail_attaches_typed_bounded_operands_in_source_order() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "left_rendered",
        Type::named("str"),
        Value::String("41".to_string()),
    );
    env.define_typed(
        "right_rendered",
        Type::named("str"),
        Value::String("é".repeat(3_000)),
    );
    let mut loop_state = HashMap::new();
    let mut cleanups = Vec::new();
    let result = runtime.execute_terminator(
        "entry",
        &Terminator::AssertFail {
            message: Some(Operand::String("values differ".to_string())),
            captures: vec![
                AssertionCapture {
                    label: "left".to_string(),
                    ty: Type::named("int64"),
                    value: Operand::MovePlace("left_rendered".to_string()),
                },
                AssertionCapture {
                    label: "right".to_string(),
                    ty: Type::named("str"),
                    value: Operand::MovePlace("right_rendered".to_string()),
                },
            ],
            span: Span::new(8, 5),
        },
        &mut env,
        &mut loop_state,
        &mut cleanups,
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("a detailed assertion terminator must trap"),
    };

    assert_eq!(error.code, "AU4001");
    assert_eq!(error.message, "values differ");
    assert_eq!(error.span, Some(Span::new(8, 5)));
    assert_eq!(error.assertion_operands.len(), 2);
    assert_eq!(error.assertion_operands[0].label, "left");
    assert_eq!(error.assertion_operands[0].r#type, "int64");
    assert_eq!(error.assertion_operands[0].value, "41");
    assert!(!error.assertion_operands[0].truncated);
    assert_eq!(error.assertion_operands[1].label, "right");
    assert_eq!(error.assertion_operands[1].r#type, "str");
    assert!(error.assertion_operands[1].truncated);
    assert!(error.assertion_operands[1]
        .value
        .ends_with("... (truncated)"));
    assert!(error.assertion_operands[1].value.len() <= 4_096);
    assert!(env.place_ref("left_rendered").is_err());
    assert!(env.place_ref("right_rendered").is_err());
}

#[test]
fn mir_assert_fail_rejects_malformed_capture_shapes_and_values() {
    let execute = |captures| {
        let mut runtime = test_runtime();
        let mut env = Env::default();
        let mut loop_state = HashMap::new();
        let mut cleanups = Vec::new();
        let result = runtime.execute_terminator(
            "entry",
            &Terminator::AssertFail {
                message: None,
                captures,
                span: Span::new(3, 7),
            },
            &mut env,
            &mut loop_state,
            &mut cleanups,
        );
        match result {
            Err(error) => error,
            Ok(_) => panic!("malformed assertion captures must be rejected"),
        }
    };

    let cardinality = execute(vec![AssertionCapture {
        label: "left".to_string(),
        ty: Type::named("int64"),
        value: Operand::String("1".to_string()),
    }]);
    assert_eq!(cardinality.code, "AU4001");
    assert_eq!(
        cardinality.message,
        "MIR assertion captures must contain zero or two operands, found 1"
    );

    let labels = execute(vec![
        AssertionCapture {
            label: "first".to_string(),
            ty: Type::named("int64"),
            value: Operand::String("1".to_string()),
        },
        AssertionCapture {
            label: "second".to_string(),
            ty: Type::named("int64"),
            value: Operand::String("2".to_string()),
        },
    ]);
    assert_eq!(labels.code, "AU4001");
    assert_eq!(
        labels.message,
        "MIR assertion captures use invalid labels `first` and `second`"
    );

    let value = execute(vec![
        AssertionCapture {
            label: "left".to_string(),
            ty: Type::named("int64"),
            value: Operand::Int(41),
        },
        AssertionCapture {
            label: "right".to_string(),
            ty: Type::named("int64"),
            value: Operand::String("42".to_string()),
        },
    ]);
    assert_eq!(value.code, "AU4001");
    assert_eq!(
        value.message,
        "MIR assertion capture `left` must evaluate to rendered `str`, found `41`"
    );
}

#[test]
fn mir_assert_fail_moves_an_owned_message_only_on_the_failure_path() {
    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "message",
        Type::named("str"),
        Value::String("owned message".to_string()),
    );
    let mut loop_state = HashMap::new();
    let mut cleanups = Vec::new();
    let result = runtime.execute_terminator(
        "entry",
        &Terminator::AssertFail {
            message: Some(Operand::MovePlace("message".to_string())),
            captures: Vec::new(),
            span: Span::new(3, 5),
        },
        &mut env,
        &mut loop_state,
        &mut cleanups,
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("assert-fail terminators must trap"),
    };
    assert_eq!(error.message, "owned message");
    assert!(
        env.place_ref("message").is_err(),
        "an owned assertion message must be consumed exactly once"
    );
}

#[test]
fn source_assertion_custom_message_is_lazy_and_borrows_a_bare_string() {
    let message = "borrowed assertion message that is long enough to own an allocation";
    let source = format!(
        r#"
def unselected_message() -> str:
    print("message must stay lazy")
    return "unselected"

def main():
    message = "{message}"
    assert true, unselected_message()
    assert false, message
"#
    );
    let module =
        crate::lower_source_to_mir(&source).expect("custom-message assertions should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should be lowered");
    let failures = main
        .blocks
        .iter()
        .filter_map(|block| match &block.terminator {
            Terminator::AssertFail { message, span, .. } => Some((message, span)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    assert!(
        matches!(failures[0].0, Some(Operand::Place(_))),
        "the lazy call result should remain owned by its failure block"
    );
    assert_eq!(
        failures[1],
        (
            &Some(Operand::Place("message".to_string())),
            &Span::new(9, 5)
        ),
        "a bare non-copy message is borrowed rather than moved by assertion lowering"
    );

    let mut isolated_runtime = test_runtime();
    let mut env = Env::default();
    let owned_message = message.to_string();
    let allocation = owned_message.as_ptr();
    env.define_typed("message", Type::named("str"), Value::String(owned_message));
    let mut loop_state = HashMap::new();
    let mut cleanups = Vec::new();
    let clone_count = super::mir_value_clone_count();
    let result = isolated_runtime.execute_terminator(
        "assert_fail",
        &Terminator::AssertFail {
            message: failures[1].0.clone(),
            captures: Vec::new(),
            span: *failures[1].1,
        },
        &mut env,
        &mut loop_state,
        &mut cleanups,
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("the source-lowered assertion terminator should trap"),
    };
    assert_eq!(error.code, "AU4001");
    assert_eq!(error.message, message);
    assert_eq!(
        super::mir_value_clone_count(),
        clone_count + 1,
        "a borrowed message should be snapshotted exactly once for the owned diagnostic"
    );
    match env
        .place_ref("message")
        .expect("a borrowed assertion message must remain in its source place")
    {
        Value::String(value) => assert_eq!(
            value.as_ptr(),
            allocation,
            "assertion diagnostics must not move or replace a borrowed str allocation"
        ),
        other => panic!("expected str, found {other:?}"),
    }

    let stdout = Arc::new(Mutex::new(String::new()));
    let mut runtime = MirRuntime::new(module, stdout.clone(), CancellationContext::default());
    let clone_count = super::mir_value_clone_count();
    let error = runtime
        .run_main()
        .expect_err("the selected source assertion should trap");
    assert_eq!(error.code, "AU4001");
    assert_eq!(error.message, message);
    assert_eq!(
        stdout.lock().unwrap().as_str(),
        "",
        "the true assertion must not evaluate its message call"
    );
    assert_eq!(
        super::mir_value_clone_count(),
        clone_count + 1,
        "only the selected borrowed message should be snapshotted"
    );
}

#[test]
fn mir_function_value_executes_dynamic_defaults_and_capability_handoffs() {
    let module = crate::lower_source_to_mir(
        r#"
class Counter:
    value: int32

def increment(counter: mut Counter) -> None:
    counter.value += 1

def consume(value: own str) -> int64:
    return value.len()

def mark(label: str, value: int32) -> int32:
    print(label)
    return value

def with_default(value: int32 = mark("fresh-default", 40)) -> int32:
    return value + 2

def double(value: int32) -> int32:
    return value * 2

def choose(use_double: bool) -> def(int32) -> int32:
    return double if use_double else with_default

def main():
    mut counter = Counter(value=0)
    mutator = increment
    consumer = consume
    selected = with_default
    dynamic = choose(true)
    text = "owned"

    mutator(counter)
    print(counter.value)
    print(consumer(text))
    print(selected())
    print(selected())
    print(dynamic(9))
    with group = TaskGroup():
        dynamic_task = group.start(dynamic, 7)
        default_task = group.start(selected)
        print("after-start")
        print(dynamic_task.result_or(-1, timeout=1s))
        print(default_task.result_or(-1, timeout=1s))
"#,
    )
    .expect("dynamic function values should lower");
    let output = crate::run_mir(&module).expect("dynamic function values should execute");
    assert_eq!(
        output.stdout,
        "1\n5\nfresh-default\n42\nfresh-default\n42\n18\nfresh-default\nafter-start\n14\n42\n",
        "mut writeback, own consumption, fresh defaults, selected targets, and task capture must agree"
    );
}

#[test]
fn mir_function_value_member_and_index_task_targets_execute_selected_functions() {
    let module = crate::lower_source_to_mir(
        r#"
class Holder:
    callback: def(int32) -> int32

def double(value: int32) -> int32:
    return value * 2

def main():
    holder = Holder(callback=double)
    callbacks: list[def(int32) -> int32] = [double]
    with group = TaskGroup():
        field_task = group.start(holder.callback, 5)
        index_task = group.start(callbacks[0], 6)
        print(field_task.result_or(-1, timeout=1s))
        print(index_task.result_or(-1, timeout=1s))
"#,
    )
    .expect("stored function values should lower as dynamic task targets");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let starts = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::StartTask { function, args, .. },
                ..
            } => Some((function, args)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 2);
    assert!(
        starts.iter().all(|(function, args)| {
            matches!(function, Operand::Place(_))
                && args.len() == 1
                && args[0].writeback_place.is_none()
        }),
        "field and indexed task targets must be evaluated into dynamic function-value places"
    );

    let output = crate::run_mir(&module).expect("stored function-value tasks should execute");
    assert_eq!(output.stdout, "10\n12\n");
}

#[test]
fn mir_function_value_runtime_moves_owned_args_writes_back_mut_args_and_traps_bad_calls() {
    let string_type = Type::named("str");
    let consume_signature = Type::Function {
        params: vec![crate::sema::FunctionParamContract {
            name: "value".to_string(),
            ty: string_type.clone(),
            passing: crate::ast::ReceiverKind::Value,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(string_type.clone()),
    };
    let consume = MirFunction {
        name: "consume".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "value".to_string(),
            passing: crate::mir::MirReceiverKind::Value,
            ty: string_type.clone(),
            default_function: None,
        }],
        local_types: Vec::new(),
        return_type: string_type.clone(),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::MovePlace("value".to_string())),
        }],
    };

    let int_type = Type::named("int32");
    let mutate_signature = Type::Function {
        params: vec![crate::sema::FunctionParamContract {
            name: "value".to_string(),
            ty: int_type.clone(),
            passing: crate::ast::ReceiverKind::BorrowMut,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(Type::Unit),
    };
    let mutate = MirFunction {
        name: "increment".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "value".to_string(),
            passing: crate::mir::MirReceiverKind::BorrowMut,
            ty: int_type.clone(),
            default_function: None,
        }],
        local_types: vec![MirLocalType {
            name: "next".to_string(),
            ty: int_type.clone(),
        }],
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![
                Instruction::Assign {
                    target: "next".to_string(),
                    value: Rvalue::Binary {
                        op: crate::ast::BinaryOp::Add,
                        left: Operand::Place("value".to_string()),
                        right: Operand::Int(1),
                        span: Span::new(1, 1),
                    },
                },
                Instruction::Assign {
                    target: "value".to_string(),
                    value: Rvalue::Use(Operand::Place("next".to_string())),
                },
            ],
            terminator: Terminator::Return(Operand::Unit),
        }],
    };

    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![consume, mutate],
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    env.define_typed(
        "consumer",
        consume_signature.clone(),
        super::mir_function_value("consume", &consume_signature),
    );
    let text = "owned dynamic argument".to_string();
    let allocation = text.as_ptr();
    env.define_typed("text", string_type, Value::String(text));
    let consumed = runtime
        .evaluate_call(
            &CallTarget::Value(Operand::Place("consumer".to_string())),
            &[MirArg {
                name: None,
                value: Operand::MovePlace("text".to_string()),
                writeback_place: None,
            }],
            &mut env,
        )
        .expect("an own indirect argument should reach the selected function");
    match consumed {
        Value::String(value) => assert_eq!(
            value.as_ptr(),
            allocation,
            "the owned allocation should transfer through the dynamic call"
        ),
        other => panic!("expected the consumed str, found {other:?}"),
    }
    assert!(
        env.place_ref("text").is_err(),
        "an own indirect argument must leave its source consumed"
    );

    env.define_typed(
        "mutator",
        mutate_signature.clone(),
        super::mir_function_value("increment", &mutate_signature),
    );
    env.define_typed(
        "counter",
        int_type,
        Value::Int(IntegerValue::from_signed(41)),
    );
    assert_eq!(
        runtime
            .evaluate_call(
                &CallTarget::Value(Operand::Place("mutator".to_string())),
                &[MirArg {
                    name: None,
                    value: Operand::Place("counter".to_string()),
                    writeback_place: Some("counter".to_string()),
                }],
                &mut env,
            )
            .expect("a mut indirect call should write its updated argument back"),
        Value::Unit
    );
    assert_eq!(
        env.place_ref("counter")
            .expect("counter should remain live"),
        &Value::Int(IntegerValue::from_signed(42))
    );

    let missing_writeback = runtime
        .evaluate_call(
            &CallTarget::Value(Operand::Place("mutator".to_string())),
            &[MirArg {
                name: None,
                value: Operand::Place("counter".to_string()),
                writeback_place: None,
            }],
            &mut env,
        )
        .expect_err("malformed MIR cannot discard a mutable capability writeback");
    assert_eq!(
        missing_writeback.message,
        "mutable borrowed MIR parameter `value` requires a writeback place"
    );
    assert_eq!(
        env.place_ref("counter")
            .expect("a rejected writeback must preserve the caller's value"),
        &Value::Int(IntegerValue::from_signed(42))
    );

    let not_callable = runtime
        .evaluate_call(&CallTarget::Value(Operand::Bool(true)), &[], &mut env)
        .expect_err("malformed MIR cannot call a non-function value");
    assert_eq!(
        not_callable.message,
        "indirect MIR call expected a function value, found `true`"
    );
}

#[test]
fn mir_function_value_runtime_rejects_missing_targets_defaults_and_malformed_task_args() {
    let int_type = Type::named("int32");
    let param = |name: &str, default_function: Option<&str>| MirParam {
        name: name.to_string(),
        passing: crate::mir::MirReceiverKind::Value,
        ty: int_type.clone(),
        default_function: default_function.map(str::to_string),
    };
    let function = |name: &str, params: Vec<MirParam>| MirFunction {
        name: name.to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params,
        local_types: Vec::new(),
        return_type: Type::Unit,
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::Unit),
        }],
    };
    let required = function("required", vec![param("left", None), param("right", None)]);
    let broken_default = function(
        "broken_default",
        vec![param("value", Some("missing_default"))],
    );
    let worker = function("worker", Vec::new());
    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![required, broken_default, worker],
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let mut env = Env::default();

    let missing_arg = runtime
        .evaluate_call(
            &CallTarget::Value(test_function_operand(
                "required",
                vec![int_type.clone(), int_type.clone()],
                Type::Unit,
            )),
            &[],
            &mut env,
        )
        .expect_err("a malformed indirect call cannot omit a required parameter");
    assert_eq!(
        missing_arg.message,
        "missing MIR argument `left` for function `required`"
    );

    let missing_default = runtime
        .evaluate_call(
            &CallTarget::Value(test_function_operand(
                "broken_default",
                vec![int_type.clone()],
                Type::Unit,
            )),
            &[],
            &mut env,
        )
        .expect_err("a serialized function cannot reference an absent default helper");
    assert_eq!(
        missing_default.message,
        "unknown MIR default function `missing_default` for `broken_default`"
    );

    let missing_target = runtime
        .evaluate_call(
            &CallTarget::Value(test_function_operand("missing", Vec::new(), Type::Unit)),
            &[],
            &mut env,
        )
        .expect_err("an indirect function value must name a function in the module");
    assert_eq!(missing_target.message, "unknown MIR function `missing`");

    let group = TaskGroupValue::new(&CancellationContext::default());
    env.define_typed("group", Type::named("TaskGroup"), Value::TaskGroup(group));
    env.define_typed("not_group", Type::named("bool"), Value::Bool(false));
    let group_operand = Operand::Place("group".to_string());
    let invalid_group_operand = Operand::Place("not_group".to_string());
    let worker_operand = test_function_operand("worker", Vec::new(), Type::Unit);
    let required_operand =
        test_function_operand("required", vec![int_type.clone(), int_type], Type::Unit);
    fn request<'a>(
        group: &'a Operand,
        function: &'a Operand,
        args: &'a [MirArg],
    ) -> super::StartTaskRequest<'a> {
        super::StartTaskRequest {
            returns_handle: false,
            result_is_repeatable: true,
            stack_size: None,
            task_group: group,
            function,
            args,
            spawn_span: Span::new(1, 1),
        }
    }

    let non_callable = runtime
        .start_task(request(&group_operand, &Operand::Bool(true), &[]), &mut env)
        .expect_err("a malformed task start cannot use a non-function target");
    assert_eq!(
        non_callable.message,
        "MIR task start expected a function value, found `true`"
    );

    let missing_task_target = runtime
        .start_task(
            request(
                &group_operand,
                &test_function_operand("missing", Vec::new(), Type::Unit),
                &[],
            ),
            &mut env,
        )
        .expect_err("a task function value must name a function in the module");
    assert_eq!(
        missing_task_target.message,
        "unknown MIR function `missing`"
    );

    let invalid_group = runtime
        .start_task(
            request(&invalid_group_operand, &worker_operand, &[]),
            &mut env,
        )
        .expect_err("a task start must receive a TaskGroup runtime value");
    assert_eq!(
        invalid_group.message,
        "MIR task start requires a task-group value"
    );

    let malformed_args = [
        (
            vec![mir_arg(Some("other"), Operand::Int(1))],
            "unknown MIR argument `other`",
        ),
        (
            vec![
                mir_arg(Some("left"), Operand::Int(1)),
                mir_arg(Some("left"), Operand::Int(2)),
            ],
            "duplicate MIR argument `left`",
        ),
        (
            vec![
                mir_arg(Some("left"), Operand::Int(1)),
                mir_arg(None, Operand::Int(2)),
            ],
            "positional MIR argument cannot follow a named argument",
        ),
        (
            vec![
                mir_arg(None, Operand::Int(1)),
                mir_arg(None, Operand::Int(2)),
                mir_arg(None, Operand::Int(3)),
            ],
            "too many MIR arguments",
        ),
    ];
    for (args, expected) in malformed_args {
        let error = runtime
            .start_task(request(&group_operand, &required_operand, &args), &mut env)
            .expect_err("malformed task arguments must be rejected before spawning");
        assert_eq!(error.message, expected);
    }
}

#[test]
fn mir_function_value_runtime_type_parameter_discovery_descends_into_signatures() {
    let contract = |ty: Type| crate::sema::FunctionParamContract {
        name: String::new(),
        ty,
        passing: crate::ast::ReceiverKind::Value,
        has_default: false,
        default_erased: true,
    };
    let signature = Type::Function {
        params: vec![contract(Type::TypeParam("CallbackInput".to_string()))],
        return_type: Box::new(Type::Function {
            params: vec![contract(Type::named("str"))],
            return_type: Box::new(Type::TypeParam("CallbackOutput".to_string())),
        }),
    };

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(&signature, &mut collected);

    assert_eq!(
        collected,
        BTreeSet::from(["CallbackInput".to_string(), "CallbackOutput".to_string(),]),
        "runtime generic discovery must include function parameters and nested returns"
    );
}

#[test]
fn mir_function_value_traps_report_selected_and_default_public_frames() {
    let selected = crate::lower_source_to_mir(
        r#"
def crash(value: int32) -> int32:
    return 10 // value

def identity(value: int32) -> int32:
    return value

def choose(crash_now: bool) -> def(int32) -> int32:
    return crash if crash_now else identity

def invoke(callback: def(int32) -> int32, value: int32) -> int32:
    return callback(value)

def main():
    selected = choose(true)
    print(invoke(selected, 0))
"#,
    )
    .expect("a runtime-selected failing function should lower");
    let selected_error =
        crate::run_mir(&selected).expect_err("the runtime-selected function should trap");
    assert_eq!(selected_error.code, "AU4004");
    assert_eq!(selected_error.message, "division by zero");
    assert_eq!(
        selected_error
            .call_frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        vec!["crash", "invoke", "main"],
        "the dynamic target must appear as the active callee, not as an anonymous trampoline"
    );

    let default = crate::lower_source_to_mir(
        r#"
def crash_default() -> int32:
    return 1 // 0

def with_default(value: int32 = crash_default()) -> int32:
    return value

def main():
    callback = with_default
    print(callback())
"#,
    )
    .expect("a failing function-value default should lower");
    let default_error =
        crate::run_mir(&default).expect_err("the selected function's default should trap");
    assert_eq!(default_error.code, "AU4004");
    assert_eq!(default_error.message, "division by zero");
    assert_eq!(
        default_error
            .call_frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        vec!["crash_default", "with_default", "main"],
        "default helpers must report their public declaration frame rather than an internal name"
    );
    assert!(default_error
        .call_frames
        .iter()
        .all(|frame| !frame.function.contains("::__default_")));
}

#[test]
fn mir_runtime_closure_environment_is_by_value_repeatable_and_one_shot_when_consuming() {
    let int_type = Type::named("int64");
    let string_type = Type::named("str");
    let shared_int_param = |name: &str| MirParam {
        name: name.to_string(),
        passing: crate::mir::MirReceiverKind::Borrow,
        ty: int_type.clone(),
        default_function: None,
    };
    let owned_string_param = |name: &str| MirParam {
        name: name.to_string(),
        passing: crate::mir::MirReceiverKind::Value,
        ty: string_type.clone(),
        default_function: None,
    };
    let repeatable_body = MirFunction {
        name: "main::__lambda_1".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(2, 16),
        receiver: None,
        params: vec![
            shared_int_param("__capture_offset"),
            shared_int_param("value"),
        ],
        local_types: vec![MirLocalType {
            name: "sum".to_string(),
            ty: int_type.clone(),
        }],
        return_type: int_type.clone(),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: vec![Instruction::Assign {
                target: "sum".to_string(),
                value: Rvalue::Binary {
                    op: crate::ast::BinaryOp::Add,
                    left: Operand::Place("__capture_offset".to_string()),
                    right: Operand::Place("value".to_string()),
                    span: Span::new(2, 36),
                },
            }],
            terminator: Terminator::Return(Operand::Place("sum".to_string())),
        }],
    };
    let consuming_body = MirFunction {
        name: "main::__lambda_2".to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(4, 16),
        receiver: None,
        params: vec![owned_string_param("__capture_text")],
        local_types: Vec::new(),
        return_type: string_type.clone(),
        entry: "entry".to_string(),
        blocks: vec![BasicBlock {
            label: "entry".to_string(),
            instructions: Vec::new(),
            terminator: Terminator::Return(Operand::MovePlace("__capture_text".to_string())),
        }],
    };
    let mut runtime = MirRuntime::new(
        MirModule {
            constants: Vec::new(),
            functions: vec![repeatable_body, consuming_body],
            classes: Vec::new(),
            trait_impls: Vec::new(),
            top_level: None,
        },
        Arc::new(Mutex::new(String::new())),
        CancellationContext::default(),
    );
    let mut env = Env::default();
    env.define_typed(
        "offset",
        int_type.clone(),
        Value::Int(IntegerValue::from_signed(40)),
    );
    env.define_typed(
        "text",
        string_type.clone(),
        Value::String("captured".to_string()),
    );

    let repeatable_signature = Type::Closure {
        params: Box::new(vec![crate::sema::FunctionParamContract {
            name: "value".to_string(),
            ty: int_type.clone(),
            passing: crate::ast::ReceiverKind::Borrow,
            has_default: false,
            default_erased: false,
        }]),
        return_type: Box::new(int_type.clone()),
        captures: Box::new(vec![crate::sema::ClosureCapture {
            name: "__capture_offset".to_string(),
            ty: int_type.clone(),
            mode: crate::sema::ClosureCaptureMode::Copy,
            span: Span::new(2, 16),
        }]),
        call_kind: crate::sema::ClosureCallKind::Repeatable,
    };
    let repeatable = runtime
        .evaluate_rvalue(
            &Rvalue::Closure {
                function: "main::__lambda_1".to_string(),
                signature: repeatable_signature.clone(),
                captures: vec![MirClosureCapture {
                    name: "__capture_offset".to_string(),
                    value: Operand::Place("offset".to_string()),
                    ty: int_type.clone(),
                    passing: MirReceiverKind::Value,
                    source_place: None,
                    resolve_source_at_capture: false,
                }],
                consuming: false,
            },
            &mut env,
        )
        .expect("a repeatable closure should capture its environment");
    let super::RvalueOutcome::Value(repeatable) = repeatable else {
        panic!("closure construction cannot return from its enclosing function");
    };
    env.define_typed("repeatable", repeatable_signature, repeatable);
    for expected in [42, 43] {
        let value = runtime
            .evaluate_call(
                &CallTarget::Value(Operand::Place("repeatable".to_string())),
                &[MirArg {
                    name: None,
                    value: Operand::Int((expected - 40) as u128),
                    writeback_place: None,
                }],
                &mut env,
            )
            .expect("a read-only closure must be callable repeatedly");
        assert_eq!(value, Value::Int(IntegerValue::from_signed(expected)));
    }
    assert_eq!(
        env.place_ref("offset")
            .expect("copy capture source stays live"),
        &Value::Int(IntegerValue::from_signed(40)),
        "copy captures are copied into the closure environment"
    );

    let consuming_signature = Type::Closure {
        params: Box::new(Vec::new()),
        return_type: Box::new(string_type),
        captures: Box::new(vec![crate::sema::ClosureCapture {
            name: "__capture_text".to_string(),
            ty: Type::named("str"),
            mode: crate::sema::ClosureCaptureMode::Move,
            span: Span::new(4, 16),
        }]),
        call_kind: crate::sema::ClosureCallKind::Consuming,
    };
    let consuming = runtime
        .evaluate_rvalue(
            &Rvalue::Closure {
                function: "main::__lambda_2".to_string(),
                signature: consuming_signature.clone(),
                captures: vec![MirClosureCapture {
                    name: "__capture_text".to_string(),
                    value: Operand::MovePlace("text".to_string()),
                    ty: Type::named("str"),
                    passing: MirReceiverKind::Value,
                    source_place: None,
                    resolve_source_at_capture: false,
                }],
                consuming: true,
            },
            &mut env,
        )
        .expect("a consuming closure should move its environment");
    let super::RvalueOutcome::Value(consuming) = consuming else {
        panic!("closure construction cannot return from its enclosing function");
    };
    assert!(
        env.place_ref("text").is_err(),
        "non-Copy capture construction must consume the source place"
    );
    env.define_typed("consuming", consuming_signature, consuming);
    assert_eq!(
        runtime
            .evaluate_call(
                &CallTarget::Value(Operand::Place("consuming".to_string())),
                &[],
                &mut env,
            )
            .expect("the first consuming closure call should transfer its environment"),
        Value::String("captured".to_string())
    );
    let repeated = runtime
        .evaluate_call(
            &CallTarget::Value(Operand::Place("consuming".to_string())),
            &[],
            &mut env,
        )
        .expect_err("a consuming closure environment is single-use");
    assert_eq!(
        repeated.message,
        "closure `main::__lambda_2` has already consumed its captured environment"
    );
}

#[test]
fn mir_closure_callbacks_keep_defaults_capture_snapshots_and_nested_environments() {
    let module = crate::lower_source_to_mir(
        r#"
def mark_default() -> int64:
    print("default")
    return 5

def invoke(callback: def(int64) -> int64, value: int64 = mark_default()) -> int64:
    return callback(value)

def main():
    print(invoke(lambda value: value + 1))

    offset: int64 = 7
    add_offset: def(int64) -> int64 = lambda value: value + offset
    values: list[int64] = [1, 3]
    print(values.map(add_offset))

    nested: def() -> int64 = lambda: add_offset(3)
    print(nested())
"#,
    )
    .expect("supported lambda callback and nested-capture positions should lower");

    let output = crate::run_mir(&module)
        .expect("nested repeatable closure environments should remain callable");
    assert_eq!(
        output.stdout, "default\n6\n[8, 10]\n10\n",
        "default binding, compiler-known callbacks, and nested environments must retain their observable order and captures"
    );
}

#[test]
fn mir_closure_task_failure_surfaces_after_handoff_cleanup_with_task_ancestry() {
    let module = crate::lower_source_to_mir(
        r#"
def crash(label: str) -> int64:
    print(label)
    return 1 // 0

def main():
    payload = "task-handoff"
    worker: def() -> int64 = lambda: crash(payload)
    with group = TaskGroup():
        task = group.start(worker)
"#,
    )
    .expect("an owned closure capture should lower as a task target");

    let error = crate::run_mir(&module)
        .expect_err("group cleanup must surface an unobserved closure-task failure");
    assert_eq!(error.code, "AU4004", "{error:?}");
    assert_eq!(error.message, "division by zero");
    assert_eq!(
        error.partial_stdout(),
        Some("task-handoff\n"),
        "the child must receive and use the owned capture before its failure"
    );
    assert_eq!(
        error
            .call_frames
            .first()
            .map(|frame| frame.function.as_str()),
        Some("crash"),
        "the failing public callee must remain the innermost runtime frame"
    );
    assert!(
        error
            .call_frames
            .iter()
            .any(|frame| frame.function.contains("::__lambda_")),
        "the closure body must remain visible in the child call stack"
    );
    assert_eq!(error.task_ancestry.len(), 1);
    assert_eq!(error.task_ancestry[0].parent_function, "main");
    assert!(
        error.task_ancestry[0].task_function.contains("::__lambda_"),
        "cleanup must preserve which closure task failed"
    );
}

#[test]
fn mir_closure_behavior_preserves_mut_writeback_owned_args_nested_moves_and_fresh_defaults() {
    let module = crate::lower_source_to_mir(
        r#"
def mark_default() -> int64:
    print("fresh-default")
    return 3

def add_default(value: int64, delta: int64 = mark_default()) -> int64:
    return value + delta

def decorate(prefix: str, value: own str) -> str:
    return f"{prefix}:{value}"

def main():
    offset: int64 = 4
    push_offset: def(mut list[int64]) -> None = lambda mut values: values.append(offset)
    mut values: list[int64] = [1, 2]
    push_offset(values)
    push_offset(values)
    print(values)

    add_fresh: def() -> int64 = lambda: add_default(offset)
    print(add_fresh())
    print(add_fresh())

    prefix = "tag"
    decorate_value: def(own str) -> str = lambda own value: decorate(prefix, value)
    first = "one"
    second = "two"
    print(decorate_value(first))
    print(decorate_value(second))

    nested: def() -> str = lambda: decorate_value("nested")
    print(nested())
"#,
    )
    .expect("mutable, owned, defaulted, and nested closure calls should lower");

    let output = crate::run_mir(&module)
        .expect("closure calls should preserve writeback and environment ownership");
    assert_eq!(
        output.stdout,
        "[1, 2, 4, 4]\nfresh-default\n7\nfresh-default\n7\ntag:one\ntag:two\ntag:nested\n",
        "mut parameters must write back, own arguments must transfer, defaults must stay fresh, and nested closure moves must retain their environment"
    );
}

#[test]
fn mir_closure_behavior_retry_repeats_capture_and_returns_last_owned_error() {
    let module = crate::lower_source_to_mir(
        r#"
import control

def fail(label: str) -> Result[int64, str]:
    print(label)
    return Result.Err(label.clone())

def main():
    label = "captured-attempt"
    worker: def() -> Result[int64, str] = lambda: fail(label)
    match own control.retry[int64, str](worker):
        case Result.Ok(value):
            print(value)
        case Result.Err(error):
            print(error)
"#,
    )
    .expect("a repeatable capturing retry callback should lower");

    let output =
        crate::run_mir(&module).expect("retry should invoke a read-only environment repeatedly");
    assert_eq!(
        output.stdout,
        "captured-attempt\ncaptured-attempt\ncaptured-attempt\ncaptured-attempt\n",
        "the default retry count must invoke the same captured environment three times and return its last owned error"
    );
}

#[test]
fn mir_closure_behavior_callback_traps_preserve_partial_output_and_public_frames() {
    let module = crate::lower_source_to_mir(
        r#"
def divide(label: str, value: int64) -> int64:
    print(label)
    return 10 // value

def main():
    label = "callback-trap"
    values: list[int64] = [2, 0, 5]
    print("before-map")
    mapped = values.map(lambda value: divide(label, value))
    print(mapped)
"#,
    )
    .expect("a trapping capturing callback should lower");

    let error = crate::run_mir(&module).expect_err("the second callback invocation should trap");
    assert_eq!(error.code, "AU4004");
    assert_eq!(error.message, "division by zero");
    assert_eq!(
        error.partial_stdout(),
        Some("before-map\ncallback-trap\ncallback-trap\n"),
        "completed callback effects must remain visible before the trap"
    );
    let frames = error
        .call_frames
        .iter()
        .map(|frame| frame.function.as_str())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], "divide");
    assert!(
        frames[1].starts_with("main::__lambda_"),
        "the generated closure body must remain visible, found {frames:?}"
    );
    assert_eq!(frames[2], "main");
}

#[test]
fn mir_closure_behavior_consuming_trap_follows_owned_transfer_with_complete_frames() {
    let module = crate::lower_source_to_mir(
        r#"
def consume_and_crash(value: own str) -> int64:
    print(value)
    return 1 // 0

def main():
    payload = "direct-owned"
    fail_once: def() -> int64 = lambda: consume_and_crash(payload)
    print("before-call")
    print(fail_once())
"#,
    )
    .expect("a consuming trapping closure should lower");

    let error =
        crate::run_mir(&module).expect_err("the closure should trap after moving its capture");
    assert_eq!(error.code, "AU4004");
    assert_eq!(error.message, "division by zero");
    assert_eq!(
        error.partial_stdout(),
        Some("before-call\ndirect-owned\n"),
        "the owned capture must reach the callee before the trap"
    );
    let frames = error
        .call_frames
        .iter()
        .map(|frame| frame.function.as_str())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], "consume_and_crash");
    assert!(frames[1].starts_with("main::__lambda_"));
    assert_eq!(frames[2], "main");
}

#[test]
fn mir_closure_behavior_explicit_stack_tasks_handoff_captures_and_cleanup() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    events = Queue[str]()
    producer = events
    label = "stack-worker"
    worker: def() -> Result[None, SendError[str]] = lambda: producer.put(label)

    offset: int64 = 4
    add_offset: def(int64) -> int64 = lambda value: value + offset
    with group = TaskGroup():
        task = group.start_with_stack(262144, add_offset, 5)
        group.start_soon_with_stack(262144, worker)
        print(task.result_or(-1, timeout=1s))
    print(events.get_or("missing", timeout=1s))
"#,
    )
    .expect("an explicit-stack closure task should lower");

    let output =
        crate::run_mir(&module).expect("group cleanup should await a fire-and-forget closure task");
    assert_eq!(
        output.stdout, "9\nstack-worker\n",
        "captured and explicit arguments must bind in order, and the fire-and-forget child must receive both owned captures before cleanup returns"
    );
}

#[test]
fn mir_closure_final_task_results_cover_ready_error_optional_poll_and_cleanup() {
    let module = crate::lower_source_to_mir(
        r#"
def fail(label: str) -> int64:
    print(label)
    return 1 // 0

def print_task_result(value: own TaskResult[int64]):
    match own value:
        case TaskResult.Ready(result):
            print(f"ready:{result}")
        case TaskResult.Error(message):
            print(f"error:{message}")
        case TaskResult.TimedOut:
            print("timedout")
        case TaskResult.Cancelled:
            print("cancelled")

def main():
    offset: int64 = 4
    add_offset: def(int64) -> int64 = lambda value: value + offset

    error_label = "task-error"
    fail_with_label: def() -> int64 = lambda: fail(error_label)

    optional_error_label = "optional-error"
    optional_fail: def() -> int64 = lambda: fail(optional_error_label)

    release = Queue[int64]()
    worker_release = release
    wait_for_release: def() -> int64 = lambda: worker_release.get_or(-1, timeout=1s)

    with group = TaskGroup():
        ready_task = group.start(add_offset, 5)
        print_task_result(ready_task.result(timeout=1s))

        error_task = group.start(fail_with_label)
        print_task_result(error_task.result(timeout=1s))

        optional_error_task = group.start(optional_fail)
        print(optional_error_task.result_or_none(timeout=1s))

        pending_task = group.start(wait_for_release)
        print(pending_task.result_or_none())
        release.put(7)

    print("cleanup-complete")
"#,
    )
    .expect("closure tasks using each result helper should lower");

    let output = crate::run_mir(&module)
        .expect("observed task failures and a released pending child should clean up normally");
    assert_eq!(
        output.stdout,
        "ready:9\ntask-error\nerror:division by zero\noptional-error\nOption.None\nOption.None\ncleanup-complete\n",
        "task results must preserve ready values and owned errors, map errors and pending polls to None, and await the released closure during group cleanup"
    );
}

#[test]
fn mir_closure_selected_entry_and_runtime_hardening_preserve_exact_observable_contracts() {
    let module = crate::lower_source_to_mir(
        r#"
def selected():
    offset: int64 = 4
    callback: def(int64) -> int64 = lambda value: value + offset
    print(callback(5))

def main():
    print("wrong-entry")
"#,
    )
    .expect("a capture-bearing callback in an explicit entry should lower");
    let output = super::run_entry_with_stdout_sink_and_program_args(
        &module,
        Some("selected"),
        None,
        Vec::new(),
    )
    .expect("the selected entry should execute through the ordinary closure runtime");
    assert_eq!(
        output.stdout, "9\n",
        "explicit-entry execution must select the requested function and retain its closure environment"
    );
    let missing_entry = super::run_entry_with_stdout_sink_and_program_args(
        &module,
        Some("missing"),
        None,
        Vec::new(),
    )
    .expect_err("an absent explicit entry must be diagnosed");
    assert_eq!(
        missing_entry.message,
        "no entry function named `missing` was found"
    );

    let mut runtime = test_runtime();
    let mut env = Env::default();
    env.define_typed(
        "huge",
        Type::named("uint128"),
        Value::Int(
            IntegerValue::from_typed_unsigned(u128::MAX, IntegerKind::Uint128)
                .expect("u128::MAX is representable as uint128"),
        ),
    );

    let duration_metadata = runtime
        .evaluate_call(
            &CallTarget::Name("Duration.seconds".to_string()),
            &[mir_arg(None, Operand::Place("huge".to_string()))],
            &mut env,
        )
        .expect_err("Duration constructors must reject non-int64 integer metadata");
    assert_eq!(
        duration_metadata.message,
        "`Duration.seconds` expects `int64`"
    );

    let missing_callable = runtime
        .evaluate_call(
            &CallTarget::Name("missing_callable".to_string()),
            &[],
            &mut env,
        )
        .expect_err("malformed MIR cannot name an absent callable");
    assert_eq!(
        missing_callable.message,
        "unknown MIR function `missing_callable`"
    );

    let private_take = runtime
        .evaluate_call(
            &CallTarget::Member {
                object: Operand::Bool(false),
                field: "__take_index_option".to_string(),
                receiver_place: None,
            },
            &[],
            &mut env,
        )
        .expect_err("owned iteration's private take operation only accepts collections");
    assert_eq!(
        private_take.message,
        "unsupported MIR member call `__take_index_option` on `false`"
    );

    for (object, field, expected) in [
        (
            Operand::Int(1),
            "to_float",
            "`to_float` does not take arguments",
        ),
        (
            Operand::Duration(1),
            "to_ms",
            "`to_ms` does not take arguments",
        ),
    ] {
        let error = runtime
            .evaluate_call(
                &CallTarget::Member {
                    object,
                    field: field.to_string(),
                    receiver_place: None,
                },
                &[mir_arg(None, Operand::Int(0))],
                &mut env,
            )
            .expect_err("argument-free conversion members must reject malformed MIR arguments");
        assert_eq!(error.message, expected);
    }

    let shuffle_type = runtime
        .evaluate_rng_method(
            crate::runtime_value::RngValue::from_seed(7),
            "shuffle",
            &[mir_arg(None, Operand::Bool(true))],
            &mut env,
        )
        .expect_err("Rng.shuffle must reject non-vector runtime values");
    assert_eq!(
        shuffle_type.message,
        "`Rng.shuffle(...)` expects `list[T]`, found `true`"
    );

    env.define_typed(
        "values",
        Type::Named("list".to_string(), vec![Type::named("int64")]),
        Value::Vec(VecValue {
            element_type: Type::named("int64"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        }),
    );
    let shuffle_place = runtime
        .evaluate_rng_method(
            crate::runtime_value::RngValue::from_seed(7),
            "shuffle",
            &[mir_arg(None, Operand::Place("values".to_string()))],
            &mut env,
        )
        .expect_err("Rng.shuffle requires an explicit mutable writeback place in MIR");
    assert_eq!(
        shuffle_place.message,
        "`Rng.shuffle(...)` requires a mutable list place"
    );
    assert_eq!(
        env.place_ref("values")
            .expect("a rejected shuffle must preserve its input"),
        &Value::Vec(VecValue {
            element_type: Type::named("int64"),
            elements: vec![Value::Int(IntegerValue::from_signed(1))],
        })
    );

    let close_args = runtime
        .evaluate_channel_method(
            ChannelValue::new(),
            "close",
            &[mir_arg(None, Operand::Int(1))],
            &mut env,
        )
        .expect_err("Queue.close must reject arguments");
    assert_eq!(close_args.message, "`close` does not take arguments");
    let unknown_queue_member = runtime
        .evaluate_channel_method(ChannelValue::new(), "missing", &[], &mut env)
        .expect_err("unknown Queue members must be diagnosed");
    assert_eq!(
        unknown_queue_member.message,
        "unsupported channel method `missing`"
    );
}
