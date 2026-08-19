use super::*;
use crate::ast::{
    Argument, AssignTarget, BindingPattern, BindingTarget, Expr, ExprKind, ForStmt, LiteralPattern,
    LiteralPatternKind, MapEntryExpr, PassStmt, Pattern, Stmt, TypeRef, VariantPattern,
};
use crate::diag::Span;
use crate::integer::IntegerValue;
use crate::sema::{binary_operator_trait, unary_operator_trait, ModuleNamespace, TraitBound};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn checked_program(source: &str) -> Program {
    crate::check_source(source).expect("source should type check")
}

fn assert_public_boundaries_reject(module: &MirModule, expected: &str) {
    let runtime =
        crate::run_mir(module).expect_err("in-memory public MIR execution must reject invalid MIR");
    assert!(runtime.message.contains(expected), "{}", runtime.message);

    let serialized = serde_json::to_vec(module).expect("invalid MIR should serialize");
    let runtime = crate::run_serialized_mir(&serialized, "<forged>", "")
        .expect_err("serialized public MIR execution must reject invalid MIR");
    assert!(runtime.message.contains(expected), "{}", runtime.message);

    let direct = crate::native_codegen::emit_host_object(module)
        .expect_err("the direct backend must use the common MIR validator");
    assert!(direct.contains(expected), "{direct}");
}

fn add_test_returned_view_callee(
    module: &mut MirModule,
    name: &str,
    origin_ty: Type,
    projections: &[String],
    mutable: bool,
) {
    assert!(!projections.is_empty());
    let return_labels = (0..projections.len())
        .map(|index| format!("{name}_return_{index}"))
        .collect::<Vec<_>>();
    let mut blocks = vec![BasicBlock {
        label: format!("{name}_entry"),
        instructions: Vec::new(),
        terminator: Terminator::Match {
            scrutinee: Operand::Bool(true),
            arms: return_labels
                .iter()
                .map(|label| MirMatchArm {
                    enum_name: None,
                    variant_name: None,
                    wildcard: true,
                    label: label.clone(),
                })
                .collect(),
            otherwise: return_labels[0].clone(),
        },
    }];
    blocks.extend(
        return_labels
            .into_iter()
            .zip(projections)
            .map(|(label, projection)| BasicBlock {
                label,
                instructions: vec![Instruction::ReturnLoan {
                    loan: if projection.is_empty() {
                        "origin".to_string()
                    } else {
                        format!("origin.{projection}")
                    },
                    origin: "origin".to_string(),
                }],
                terminator: Terminator::Return(Operand::Unit),
            }),
    );
    module.functions.push(MirFunction {
        name: name.to_string(),
        module_name: "<test>".to_string(),
        source_path: None,
        span: Span::new(1, 1),
        receiver: None,
        params: vec![MirParam {
            name: "origin".to_string(),
            passing: if mutable {
                MirReceiverKind::BorrowMut
            } else {
                MirReceiverKind::Borrow
            },
            ty: origin_ty.clone(),
            default_function: None,
        }],
        local_types: Vec::new(),
        return_type: origin_ty,
        entry: format!("{name}_entry"),
        blocks,
    });
}

#[test]
fn adr0038_view_lowering_uses_explicit_loan_operations() {
    let module = crate::lower_source_to_mir(
        r#"
class Counter:
    value: int64

def main():
    mut counter = Counter(value=1)
    view mut value = counter.value
    value = 2
"#,
    )
    .expect("view source should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR should exist");
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(
            instruction,
            Instruction::BeginLoan { loan, source, mutable: true }
                if loan == "value" && source == "counter.value"
        )));
}

#[test]
fn adr0038_recursive_returned_view_cycles_use_a_conservative_root_descriptor() {
    let module = crate::lower_source_to_mir(
        r#"
def recurse(value: int64) -> view int64 from value:
    return view recurse(value)

def main():
    value = 1
    view selected = recurse(value)
    print(selected)
"#,
    )
    .expect("a checked recursive returned-view SCC must still lower valid descriptors");
    let returned_loans = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::BeginReturnedLoan { projections, .. } => Some(projections),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!returned_loans.is_empty());
    assert!(
        returned_loans
            .iter()
            .all(|projections| projections == &&vec![String::new()]),
        "recursive projection-less forwarding should conservatively use the origin root: {returned_loans:?}"
    );
}

#[test]
fn adr0038_loan_validator_rejects_capability_forgery_and_unbalanced_paths() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    value = 1
    view parent = value
    view child = parent
    print(child)
"#,
    )
    .expect("valid shared reborrow source should lower");
    validate_loan_flow(&module).expect("source-produced loan flow should validate");

    let mut escalated = module.clone();
    let reborrow = escalated
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
    let error = validate_loan_flow(&escalated)
        .expect_err("serialized MIR cannot escalate a shared parent loan");
    assert!(error.contains("escalates shared parent"), "{error}");

    let mut unterminated = module;
    let block = unterminated
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .find(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::EndLoan { .. }))
        })
        .expect("the lowered source should end its loans");
    block
        .instructions
        .retain(|instruction| !matches!(instruction, Instruction::EndLoan { .. }));
    let error = validate_loan_flow(&unterminated)
        .expect_err("every ordinary return path must end its live loans");
    assert!(error.contains("returns with active loans"), "{error}");

    let mut forged_capture = crate::lower_source_to_mir(
        r#"
def main():
    mut left = [1]
    mut right = [2]
    mut update: def(int64) -> None = lambda [mut left] item: left.append(item)
    update(3)
"#,
    )
    .expect("ordinary mutable closure capture should lower");
    let mut forged_dynamic_capture = forged_capture.clone();
    let resolve_source_at_capture = forged_dynamic_capture
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => captures
                .first_mut()
                .map(|capture| &mut capture.resolve_source_at_capture),
            _ => None,
        })
        .expect("mutable closure capture should retain resolution metadata");
    *resolve_source_at_capture = true;
    let error = validate_loan_flow(&forged_dynamic_capture)
        .expect_err("ordinary captures cannot forge dynamic returned-view resolution");
    assert!(error.contains("active returned-view descriptor"), "{error}");

    let capture = forged_dynamic_capture
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => captures.first_mut(),
            _ => None,
        })
        .expect("the forged closure capture should remain available");
    capture.passing = MirReceiverKind::Value;
    capture.source_place = None;
    let error = validate_loan_flow(&forged_dynamic_capture)
        .expect_err("owned captures cannot request returned-view source resolution");
    assert!(error.contains("value closure capture"), "{error}");

    let source = forged_capture
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => captures
                .first_mut()
                .and_then(|capture| capture.source_place.as_mut()),
            _ => None,
        })
        .expect("mutable closure capture should retain its source");
    *source = "right".to_string();
    let error = validate_loan_flow(&forged_capture)
        .expect_err("serialized MIR cannot redirect a borrowed closure capture");
    assert!(error.contains("has unrelated source `right`"), "{error}");
}

#[test]
fn adr0038_public_mir_rejects_named_function_signature_forgery() {
    let mut forged = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64

def borrow_pair(origin: Pair, fallback: int64 = 0) -> view int64 from origin:
    return view origin.left

def main():
    pair = Pair(left=1)
    view selected = borrow_pair(pair)
    print(selected)
"#,
    )
    .expect("returned-view function-value fixture should lower");

    let call = forged
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Call { callee, args },
                ..
            } if matches!(&*callee, CallTarget::Name(name) if name == "borrow_pair") => {
                Some((callee, args))
            }
            _ => None,
        })
        .expect("the returned-view call should lower");
    call.1.truncate(1);
    *call.0 = CallTarget::Value(Operand::Function {
        name: "borrow_pair".to_string(),
        signature: Box::new(Type::Function {
            params: vec![crate::sema::FunctionParamContract {
                name: "origin".to_string(),
                ty: Type::named("Pair"),
                passing: crate::ast::ReceiverKind::Borrow,
                has_default: false,
                default_erased: false,
            }],
            return_type: Box::new(Type::named("int64")),
        }),
    });

    assert_public_boundaries_reject(&forged, "does not match declaration");
}

#[test]
fn adr0038_public_mir_rejects_mutable_argument_writeback_forgery() {
    let source = r#"
class Pair:
    left: int64

def inspect(origin: Pair, scratch: mut Pair) -> view int64 from origin:
    scratch.left = scratch.left
    return view origin.left

def main():
    origin = Pair(left=1)
    mut scratch = Pair(left=2)
    mut redirect = Pair(left=3)
    view selected = inspect(origin, scratch)
    print(selected)
"#;
    let baseline = crate::lower_source_to_mir(source)
        .expect("mutable returned-view argument fixture should lower");
    validate_loan_flow(&baseline).expect("source-produced mutable writeback should validate");

    let mutate_call =
        |module: &mut MirModule, update: &mut dyn FnMut(&mut CallTarget, &mut Vec<MirArg>)| {
            let (callee, args) = module
                .functions
                .iter_mut()
                .find(|function| function.name == "main")
                .into_iter()
                .flat_map(|function| &mut function.blocks)
                .flat_map(|block| &mut block.instructions)
                .find_map(|instruction| match instruction {
                    Instruction::Assign {
                        value: Rvalue::Call { callee, args },
                        ..
                    } if matches!(&*callee, CallTarget::Name(name) if name == "inspect") => {
                        Some((callee, args))
                    }
                    _ => None,
                })
                .expect("inspect call should lower");
            update(callee, args);
        };

    let mut redirected = baseline.clone();
    mutate_call(&mut redirected, &mut |_, args| {
        args[1].writeback_place = Some("redirect".to_string())
    });
    assert_public_boundaries_reject(&redirected, "unrelated mutable writeback place");

    let mut missing = baseline.clone();
    mutate_call(&mut missing, &mut |_, args| args[1].writeback_place = None);
    assert_public_boundaries_reject(&missing, "requires a mutable writeback place");

    let mut non_mutable = baseline.clone();
    mutate_call(&mut non_mutable, &mut |_, args| {
        args[0].writeback_place = Some("origin".to_string())
    });
    assert_public_boundaries_reject(&non_mutable, "writeback for non-mutable parameter");

    let mut omitted = baseline.clone();
    mutate_call(&mut omitted, &mut |_, args| {
        args.truncate(1);
    });
    assert_public_boundaries_reject(&omitted, "omits required parameter `scratch`");

    let inspect = baseline
        .functions
        .iter()
        .find(|function| function.name == "inspect")
        .expect("inspect declaration should lower");
    let signature = Type::Function {
        params: inspect
            .params
            .iter()
            .map(|param| crate::sema::FunctionParamContract {
                name: param.name.clone(),
                ty: param.ty.clone(),
                passing: match param.passing {
                    MirReceiverKind::Value => crate::ast::ReceiverKind::Value,
                    MirReceiverKind::Borrow => crate::ast::ReceiverKind::Borrow,
                    MirReceiverKind::BorrowMut => crate::ast::ReceiverKind::BorrowMut,
                },
                has_default: param.default_function.is_some(),
                default_erased: false,
            })
            .collect(),
        return_type: Box::new(inspect.return_type.clone()),
    };
    let mut exact_function = baseline;
    mutate_call(&mut exact_function, &mut |callee, args| {
        *callee = CallTarget::Value(Operand::Function {
            name: "inspect".to_string(),
            signature: Box::new(signature.clone()),
        });
        args[1].writeback_place = Some("redirect".to_string());
    });
    assert_public_boundaries_reject(&exact_function, "unrelated mutable writeback place");

    let mut member = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64

class Holder:
    value: int64

    def inspect(self, scratch: mut Pair) -> view int64 from self:
        scratch.left = scratch.left
        return view self.value

def main():
    holder = Holder(value=1)
    mut scratch = Pair(left=2)
    mut redirect = Pair(left=3)
    view selected = holder.inspect(scratch)
    print(selected)
"#,
    )
    .expect("returned-view member argument fixture should lower");
    let args = member
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        args,
                    },
                ..
            } if field == "inspect" => Some(args),
            _ => None,
        })
        .expect("returned-view member call should lower");
    args[0].writeback_place = Some("redirect".to_string());
    assert_public_boundaries_reject(&member, "unrelated mutable writeback place");
}

#[test]
fn adr0038_public_mir_rejects_mutable_capture_escalation_from_shared_input() {
    let mut forged = crate::lower_source_to_mir(
        r#"
def make(source: list[int64]):
    mut local = [1]
    mut update: def(int64) -> None = lambda [mut local] item: local.append(item)
    update(2)
    print(source.len())

def main():
    make([1])
"#,
    )
    .expect("ordinary mutable local capture should lower");
    let capture = forged
        .functions
        .iter_mut()
        .find(|function| function.name == "make")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => captures
                .iter_mut()
                .find(|capture| capture.passing == MirReceiverKind::BorrowMut),
            _ => None,
        })
        .expect("mutable local capture should lower");
    capture.value = Operand::Place("source".to_string());
    capture.source_place = Some("source".to_string());

    assert_public_boundaries_reject(&forged, "escalates shared input `source`");

    let mut forged_unbound_self = forged;
    let capture = forged_unbound_self
        .functions
        .iter_mut()
        .find(|function| function.name == "make")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => captures
                .iter_mut()
                .find(|capture| capture.passing == MirReceiverKind::BorrowMut),
            _ => None,
        })
        .expect("mutable capture should remain available");
    capture.value = Operand::Place("self".to_string());
    capture.source_place = Some("self".to_string());
    assert_public_boundaries_reject(&forged_unbound_self, "escalates shared input `self`");
}

#[test]
fn adr0038_deep_symbolic_returned_view_contracts_fail_closed_without_stack_overflow() {
    const HELPER: &str = "AURA_ADR0038_DEEP_CONTRACT_HELPER";
    const TEST_NAME: &str =
        "mir::tests::adr0038_deep_symbolic_returned_view_contracts_fail_closed_without_stack_overflow";
    if std::env::var_os(HELPER).is_none() {
        for mode in ["mir", "direct"] {
            let output = Command::new(std::env::current_exe().expect("test binary should exist"))
                .arg("--exact")
                .arg(TEST_NAME)
                .arg("--nocapture")
                .env(HELPER, mode)
                .output()
                .expect("isolated deep-contract validation should run");
            assert!(
                output.status.success(),
                "{mode} deep-contract validation crashed or failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }

    let mut module = crate::lower_source_to_mir(
        r#"
def borrow(origin: int64) -> view int64 from origin:
    return view origin

def main():
    value = 1
    view selected = borrow(value)
    print(selected)
"#,
    )
    .expect("deep-contract baseline should lower");
    let function = module
        .functions
        .iter_mut()
        .find(|function| function.name == "borrow")
        .expect("borrow function should lower");
    let block = function
        .blocks
        .iter_mut()
        .find(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::ReturnLoan { .. }))
        })
        .expect("borrow return block should lower");
    let return_loan = block
        .instructions
        .iter_mut()
        .find_map(|instruction| match instruction {
            Instruction::ReturnLoan { loan, .. } => Some(loan),
            _ => None,
        })
        .expect("borrow should retain a ReturnLoan");
    *return_loan = "loan19999".to_string();
    let mut chain = Vec::with_capacity(20_000);
    chain.push(Instruction::BeginLoan {
        loan: "loan0".to_string(),
        source: "origin".to_string(),
        mutable: false,
    });
    for index in 1..20_000 {
        chain.push(Instruction::Reborrow {
            loan: format!("loan{index}"),
            parent: format!("loan{}", index - 1),
            projection: String::new(),
            mutable: false,
        });
    }
    block.instructions.splice(0..0, chain);

    let expected = "symbolic loan descriptor depth limit";
    match std::env::var(HELPER).as_deref() {
        Ok("mir") => {
            let error =
                crate::run_mir(&module).expect_err("deep in-memory MIR contracts must fail closed");
            assert!(error.message.contains(expected), "{}", error.message);
        }
        Ok("direct") => {
            let error = crate::native_codegen::emit_host_object(&module)
                .expect_err("deep direct MIR contracts must fail closed");
            assert!(error.contains(expected), "{error}");
        }
        mode => panic!("unexpected deep-contract helper mode: {mode:?}"),
    }
}

#[test]
fn adr0038_unreachable_return_loans_cannot_expand_callee_authority() {
    let mut forged = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64
    right: int64

def left(origin: Pair) -> view int64 from origin:
    return view origin.left

def main():
    pair = Pair(left=1, right=2)
    view selected = left(pair)
    print(selected)
"#,
    )
    .expect("returned-view dead-block fixture should lower");
    let callee = forged
        .functions
        .iter_mut()
        .find(|function| function.name == "left")
        .expect("left should lower");
    callee.blocks.push(BasicBlock {
        label: "dead_forged_return".to_string(),
        instructions: vec![Instruction::ReturnLoan {
            loan: "origin.right".to_string(),
            origin: "origin".to_string(),
        }],
        terminator: Terminator::Return(Operand::Unit),
    });
    let projections = forged
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::BeginReturnedLoan { projections, .. } => Some(projections),
            _ => None,
        })
        .expect("caller should bind the returned view");
    *projections = vec!["right".to_string()];

    assert_public_boundaries_reject(&forged, "projection contract");
}

#[test]
fn adr0038_every_reachable_returned_view_path_requires_a_loan_handoff() {
    let mut forged = crate::lower_source_to_mir(
        r#"
def borrow(origin: int64) -> view int64 from origin:
    return view origin

def main():
    value = 1
    view selected = borrow(value)
    print(selected)
"#,
    )
    .expect("returned-view path fixture should lower");
    let callee = forged
        .functions
        .iter_mut()
        .find(|function| function.name == "borrow")
        .expect("borrow should lower");
    let original_entry = callee.entry.clone();
    callee.entry = "forged_dispatch".to_string();
    callee.blocks.push(BasicBlock {
        label: "forged_dispatch".to_string(),
        instructions: Vec::new(),
        terminator: Terminator::Branch {
            condition: Operand::Bool(true),
            then_label: original_entry,
            else_label: "forged_plain_return".to_string(),
        },
    });
    callee.blocks.push(BasicBlock {
        label: "forged_plain_return".to_string(),
        instructions: Vec::new(),
        terminator: Terminator::Return(Operand::Unit),
    });

    assert_public_boundaries_reject(&forged, "returns without a returned-loan handoff");
}

#[test]
fn adr0038_generic_trait_member_calls_retain_authoritative_trait_identity() {
    let source = r#"
trait Other:
    def get(self) -> view int64 from self

trait Project:
    def get(self) -> view int64 from self

trait OwnedGet:
    def get(self) -> int64

trait Marker:
    def marker(self) -> int64

trait OtherMut:
    def get_mut(mut self) -> view mut int64 from self

trait ProjectMut:
    def get_mut(mut self) -> view mut int64 from self

class Box:
    left: int64
    right: int64

impl Other for Box:
    def get(self) -> view int64 from self:
        return view self.right

impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.left

impl OwnedGet for int64:
    def get(self) -> int64:
        return self

impl Marker for Box:
    def marker(self) -> int64:
        return 0

impl OtherMut for Box:
    def get_mut(mut self) -> view mut int64 from self:
        return view mut self.right

impl ProjectMut for Box:
    def get_mut(mut self) -> view mut int64 from self:
        return view mut self.left

def forward[T: Project](value: T) -> view int64 from value:
    return view value.get()

def forward_multi[T: Marker + Project](value: T) -> view int64 from value:
    return view value.get()

def update[T: ProjectMut](value: mut T):
    view mut selected = value.get_mut()
    selected = 9

def main():
    box = Box(left=1, right=2)
    view selected = forward(box)
    print(selected)
    view multi = forward_multi(box)
    print(multi)
    mut mutable_box = Box(left=3, right=4)
    update(mutable_box)
    print(mutable_box.left)
    print(mutable_box.right)
"#;
    let module = crate::lower_source_to_mir(source)
        .expect("unambiguous bounded generic returned-view call should lower");
    for (function_name, expected_trait) in [
        ("forward", "Project"),
        ("forward_multi", "Project"),
        ("update", "ProjectMut"),
    ] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .unwrap_or_else(|| panic!("{function_name} should lower"));
        assert!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction,
                    Instruction::Assign {
                        value:
                            Rvalue::Call {
                                callee: CallTarget::TraitMember { trait_name, .. },
                                ..
                            },
                        ..
                    } if trait_name == expected_trait
                )),
            "{function_name} must retain the `{expected_trait}` bound identity in MIR"
        );
    }
    validate_loan_flow(&module)
        .expect("an unrelated same-name trait must not pollute returned-view authority");

    let mut erased_identity = module.clone();
    let callee = erased_identity
        .functions
        .iter_mut()
        .find(|function| function.name == "forward")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Call { callee, .. },
                ..
            } if matches!(&*callee, CallTarget::TraitMember { .. }) => Some(callee),
            _ => None,
        })
        .expect("forward should retain its trait member target");
    let CallTarget::TraitMember {
        object,
        field,
        receiver_place,
        ..
    } = callee.clone()
    else {
        unreachable!("the selected target was checked above")
    };
    *callee = CallTarget::Member {
        object,
        field,
        receiver_place,
    };
    assert_public_boundaries_reject(&erased_identity, "requires an authoritative trait identity");

    let output = crate::run_mir(&module)
        .expect("MIR dispatch must select the trait named by the generic bound");
    assert_eq!(output.stdout, "1\n1\n9\n4\n");
    let serialized =
        serde_json::to_vec(&module).expect("valid bounded-generic MIR should serialize");
    let serialized = crate::run_serialized_mir(&serialized, "<trait-identity>", "")
        .expect("serialized MIR must preserve bounded trait dispatch identity");
    assert_eq!(serialized.stdout, output.stdout);
    crate::native_codegen::emit_host_object(&module)
        .expect("direct codegen must accept the same authoritative trait identity");
}

#[test]
fn adr0038_specialized_generic_returned_views_ignore_same_named_impl_order() {
    let declarations = r#"
trait Project:
    def get(self) -> view int64 from self

trait Other:
    def get(self) -> view int64 from self

class Box:
    left: int64
    right: int64
"#;
    let project_impl = r#"
impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.left
"#;
    let other_impl = r#"
impl Other for Box:
    def get(self) -> view int64 from self:
        return view self.right
"#;
    let program = r#"
def forward[T: Project](item: T) -> view int64 from item:
    return view item.get()

def main():
    box = Box(left=1, right=2)
    view selected = forward[Box](box)
    print(selected)
"#;

    for (order, first_impl, second_impl) in [
        ("project-first", project_impl, other_impl),
        ("other-first", other_impl, project_impl),
    ] {
        let source = format!("{declarations}{first_impl}{second_impl}{program}");
        let module = crate::lower_source_to_mir(&source).unwrap_or_else(|error| {
            panic!("{order} bounded returned-view specialization should lower: {error}")
        });
        validate_loan_flow(&module)
            .unwrap_or_else(|error| panic!("{order} MIR should validate: {error}"));
        let output = crate::run_mir(&module)
            .unwrap_or_else(|error| panic!("{order} MIR execution should succeed: {error}"));
        assert_eq!(output.stdout, "1\n", "{order} selected the wrong trait");

        let serialized = serde_json::to_vec(&module).expect("valid MIR should serialize");
        let serialized = crate::run_serialized_mir(&serialized, "<trait-order>", "")
            .unwrap_or_else(|error| panic!("{order} serialized MIR should run: {error}"));
        assert_eq!(serialized.stdout, "1\n", "{order} serialized drifted");
        crate::native_codegen::emit_host_object(&module)
            .unwrap_or_else(|error| panic!("{order} direct codegen should succeed: {error}"));
    }
}

#[test]
fn adr0038_returned_loan_handoffs_are_bound_to_the_exact_callee_contract() {
    let baseline = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64
    right: int64

def left(pair: Pair) -> view int64 from pair:
    return view pair.left

def main():
    pair = Pair(left=1, right=2)
    other = Pair(left=3, right=4)
    view selected = left(pair)
    print(selected)
"#,
    )
    .expect("returned-view contract fixture should lower");
    validate_loan_flow(&baseline).expect("source-produced returned-view MIR should validate");

    let mutate_handoff =
        |module: &mut MirModule,
         mutate_call: &mut dyn FnMut(&mut CallTarget),
         mutate_loan: &mut dyn FnMut(&mut String, &mut Vec<String>, &mut bool)| {
            let main = module
                .functions
                .iter_mut()
                .find(|function| function.name == "main")
                .expect("main should lower");
            for block in &mut main.blocks {
                for index in 1..block.instructions.len() {
                    let (before, after) = block.instructions.split_at_mut(index);
                    let Instruction::Assign {
                        value: Rvalue::Call { callee, .. },
                        ..
                    } = &mut before[index - 1]
                    else {
                        continue;
                    };
                    let Instruction::BeginReturnedLoan {
                        origin,
                        projections,
                        mutable,
                        ..
                    } = &mut after[0]
                    else {
                        continue;
                    };
                    mutate_call(callee);
                    mutate_loan(origin, projections, mutable);
                    return;
                }
            }
            panic!("fixture should contain an immediately bound returned-view handoff");
        };

    let mut wrong_callee = baseline.clone();
    mutate_handoff(
        &mut wrong_callee,
        &mut |callee| *callee = CallTarget::Name("abs".to_string()),
        &mut |_, _, _| {},
    );
    let error = validate_loan_flow(&wrong_callee)
        .expect_err("a non-view call cannot authorize BeginReturnedLoan");
    assert!(error.contains("does not return a view"), "{error}");
    assert_public_boundaries_reject(&wrong_callee, "does not return a view");

    let mut wrong_origin = baseline.clone();
    mutate_handoff(&mut wrong_origin, &mut |_| {}, &mut |origin, _, _| {
        *origin = "other".to_string()
    });
    let error = validate_loan_flow(&wrong_origin)
        .expect_err("the caller cannot redirect a returned view to another origin");
    assert!(error.contains("bound origin"), "{error}");
    assert_public_boundaries_reject(&wrong_origin, "bound origin");

    let mut escalated = baseline.clone();
    mutate_handoff(&mut escalated, &mut |_| {}, &mut |_, _, mutable| {
        *mutable = true
    });
    let error = validate_loan_flow(&escalated)
        .expect_err("the caller cannot escalate a shared returned-view contract");
    assert!(error.contains("mutable capability"), "{error}");
    assert_public_boundaries_reject(&escalated, "mutable capability");

    let mut redirected_projection = baseline;
    mutate_handoff(
        &mut redirected_projection,
        &mut |_| {},
        &mut |_, projections, _| *projections = vec!["right".to_string()],
    );
    let error = validate_loan_flow(&redirected_projection)
        .expect_err("the caller cannot replace the callee projection footprint");
    assert!(error.contains("projection contract"), "{error}");
    assert_public_boundaries_reject(&redirected_projection, "projection contract");

    let mut nul_projection = wrong_callee;
    mutate_handoff(
        &mut nul_projection,
        &mut |callee| *callee = CallTarget::Name("left".to_string()),
        &mut |_, projections, _| *projections = vec!["left\0right".to_string()],
    );
    let error = validate_loan_flow(&nul_projection)
        .expect_err("NUL cannot alias projection whitelist entries in the direct backend");
    assert!(error.contains("non-canonical MIR identifier"), "{error}");
    assert_public_boundaries_reject(&nul_projection, "non-canonical MIR identifier");
}

#[test]
fn adr0038_returned_loan_handoffs_may_narrow_authorized_projection_alternatives() {
    let mut module =
        crate::lower_source_to_mir("def main():\n    pass\n").expect("baseline MIR should lower");
    add_test_returned_view_callee(
        &mut module,
        "test_choose_tuple_field",
        Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
        &["0".to_string(), "1".to_string()],
        false,
    );
    let main = module
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    main.local_types.extend([
        MirLocalType {
            name: "root".to_string(),
            ty: Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
        },
        MirLocalType {
            name: "handoff".to_string(),
            ty: Type::named("int64"),
        },
        MirLocalType {
            name: "selected".to_string(),
            ty: Type::named("int64"),
        },
    ]);
    main.blocks[0].instructions.extend([
        Instruction::Assign {
            target: "handoff".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Name("test_choose_tuple_field".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place("root".to_string()),
                    writeback_place: None,
                }],
            },
        },
        Instruction::BeginReturnedLoan {
            loan: "selected".to_string(),
            origin: "root".to_string(),
            projections: vec!["0".to_string()],
            mutable: false,
        },
        Instruction::EndLoan {
            loan: "selected".to_string(),
        },
    ]);

    validate_loan_flow(&module)
        .expect("a specialized caller may narrow the callee's conservative alternatives");

    let main = module
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    let projections = main.blocks[0]
        .instructions
        .iter_mut()
        .find_map(|instruction| match instruction {
            Instruction::BeginReturnedLoan { projections, .. } => Some(projections),
            _ => None,
        })
        .expect("returned descriptor should exist");
    *projections = vec!["2".to_string()];
    let error = validate_loan_flow(&module)
        .expect_err("narrowing cannot introduce a projection outside callee authority");
    assert!(error.contains("projection contract"), "{error}");
}

#[test]
fn adr0038_multi_alternative_returned_view_captures_resolve_the_selected_source() {
    fn selected_capture(module: &mut MirModule) -> &mut MirClosureCapture {
        module
            .functions
            .iter_mut()
            .find(|function| function.name == "main")
            .into_iter()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                Instruction::Assign {
                    value: Rvalue::Closure { captures, .. },
                    ..
                } => captures
                    .iter_mut()
                    .find(|capture| capture.name == "selected"),
                _ => None,
            })
            .expect("lambda should capture the returned-view descriptor")
    }

    let baseline = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64
    right: int64

def choose(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def assign(value: mut int64, next: int64):
    value = next

def main():
    mut pair = Pair(left=1, right=2)
    view mut selected = choose(pair, false)
    mut update: def(int64) -> None = lambda [mut selected] next: assign(selected, next)
    update(9)
"#,
    )
    .expect("dynamic returned-view capture fixture should lower");
    validate_loan_flow(&baseline).expect("source-produced dynamic capture should validate");

    let mut unresolved = baseline.clone();
    selected_capture(&mut unresolved).resolve_source_at_capture = false;
    let error = validate_loan_flow(&unresolved)
        .expect_err("multi-alternative descriptors must resolve at capture time");
    assert!(error.contains("selected source at capture"), "{error}");

    let mut statically_redirected = baseline;
    let capture = selected_capture(&mut statically_redirected);
    capture.resolve_source_at_capture = false;
    capture.source_place = Some("pair.left".to_string());
    let error = validate_loan_flow(&statically_redirected)
        .expect_err("a capture cannot replace a dynamic descriptor with one static alternative");
    assert!(error.contains("selected source at capture"), "{error}");
}

#[test]
fn adr0038_returned_view_descendant_captures_resolve_the_selected_source() {
    let module = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64
    right: int64

def choose_mut(pair: mut Pair, left: bool) -> view mut int64 from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def choose_shared(pair: Pair, left: bool) -> view int64 from pair:
    if left:
        return view pair.left
    return view pair.right

def assign(value: mut int64, next: int64):
    value = next

def main():
    mut mutable_pair = Pair(left=1, right=2)
    view mut selected_mut = choose_mut(mutable_pair, false)
    view mut child_mut = selected_mut
    mut update: def(int64) -> None = lambda [mut child_mut] next: assign(child_mut, next)
    update(9)

    shared_pair = Pair(left=3, right=4)
    view selected_shared = choose_shared(shared_pair, true)
    view child_shared = selected_shared
    read: def() -> int64 = lambda [child_shared]: child_shared
    print(read())
"#,
    )
    .expect("descendants of dynamic returned views should lower");

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let captures = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => Some(captures),
            _ => None,
        })
        .flatten()
        .filter(|capture| matches!(capture.name.as_str(), "child_mut" | "child_shared"))
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2, "both descendant captures should lower");
    for capture in captures {
        assert_eq!(capture.source_place.as_deref(), Some(capture.name.as_str()));
        assert!(
            capture.resolve_source_at_capture,
            "{} must retain dynamic returned-view selection",
            capture.name
        );
    }
    validate_loan_flow(&module)
        .expect("descendant captures must preserve their returned-descriptor ancestry");
}

#[test]
fn adr0038_forwarded_returned_call_projections_use_composed_loans() {
    let module = crate::lower_source_to_mir(
        r#"
class Cell:
    value: int64

class Pair:
    left: Cell
    right: Cell

def left_cell(pair: mut Pair) -> view mut Cell from pair:
    return view mut pair.left

def choose_cell(pair: mut Pair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def cell_value(cell: mut Cell) -> view mut int64 from cell:
    return view mut cell.value

def static_forward(pair: mut Pair) -> view mut int64 from pair:
    return view mut left_cell(pair).value

def dynamic_forward(pair: mut Pair, left: bool) -> view mut int64 from pair:
    return view mut choose_cell(pair, left).value

def nested_forward(pair: mut Pair, left: bool) -> view mut int64 from pair:
    return view mut cell_value(choose_cell(pair, left))

def main():
    pass
"#,
    )
    .expect("static, dynamic, and nested returned-view forwarding should lower");

    validate_loan_flow(&module)
        .expect("forwarded child projections must use an immediate call handoff and reborrow");
    for function_name in ["static_forward", "dynamic_forward"] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .expect("forwarding function should lower");
        assert!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(instruction, Instruction::Reborrow { projection, .. } if projection == "value")),
            "{function_name} should compose the returned descriptor with a child reborrow"
        );
    }
}

#[test]
fn adr0038_returned_view_contracts_preserve_local_descriptor_suffixes() {
    let module = crate::lower_source_to_mir(
        r#"
class Cell:
    value: int64

class CellPair:
    left: Cell
    right: Cell

def choose_cell(pair: mut CellPair, left: bool) -> view mut Cell from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def class_child(pair: mut CellPair, left: bool) -> view mut int64 from pair:
    view mut selected = choose_cell(pair, left)
    return view mut selected.value

class TuplePair:
    left: (int64, int64)
    right: (int64, int64)

def choose_tuple(pair: mut TuplePair, left: bool) -> view mut (int64, int64) from pair:
    if left:
        return view mut pair.left
    return view mut pair.right

def tuple_child(pair: mut TuplePair, left: bool) -> view mut int64 from pair:
    view mut selected = choose_tuple(pair, left)
    return view mut selected[1]

def main():
    mut cells = CellPair(left=Cell(value=1), right=Cell(value=2))
    view mut cell_value = class_child(cells, false)
    cell_value = 9
    print(cells)

    mut tuples = TuplePair(left=(3, 4), right=(5, 6))
    view mut tuple_value = tuple_child(tuples, true)
    tuple_value = 8
    print(tuples)
"#,
    )
    .expect("local returned descriptors should allow class and tuple child forwarding");

    for (function_name, expected) in [
        ("class_child", ["left.value", "right.value"]),
        ("tuple_child", ["left.1", "right.1"]),
    ] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .expect("child forwarding function should lower");
        let contract = function_returned_view_contract(function)
            .expect("source-produced returned-view contract should be valid")
            .expect("child forwarding function should expose a returned-view contract");
        assert_eq!(contract.projections.as_ref(), expected.as_slice());
    }

    validate_loan_flow(&module)
        .expect("callers must receive the full class or tuple child projection authority");
    let mir = crate::run_mir(&module).expect("public MIR execution should accept the contract");
    assert_eq!(
        mir.stdout,
        "CellPair(left=Cell(value=1), right=Cell(value=9))\nTuplePair(left=(3, 8), right=(5, 6))\n"
    );
    let serialized = serde_json::to_vec(&module).expect("valid MIR should serialize");
    let serialized = crate::run_serialized_mir(&serialized, "<test>", "")
        .expect("serialized public MIR execution should preserve child authority");
    assert_eq!(serialized.stdout, mir.stdout);
    crate::native_codegen::emit_host_object(&module)
        .expect("the direct backend should accept the same public MIR contract");
}

#[test]
fn adr0038_member_effect_validation_uses_declared_receiver_passing() {
    let mut user_method = crate::lower_source_to_mir(
        r#"
class Counter:
    value: int64

    def inspect(self) -> int64:
        return self.value

    def tweak(mut self) -> int64:
        self.value += 1
        return self.value

def main():
    mut counter = Counter(value=1)
    view held = counter
    print(counter.inspect())
    print(held.value)
"#,
    )
    .expect("user receiver fixture should lower");
    let field = user_method
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                ..
            } if field == "inspect" => Some(field),
            _ => None,
        })
        .expect("inspect call should lower as a member call");
    *field = "tweak".to_string();
    let error = validate_loan_flow(&user_method)
        .expect_err("an arbitrary user method name cannot hide a mutable receiver effect");
    assert!(error.contains("mutates locked place `counter`"), "{error}");

    let mut trait_method = crate::lower_source_to_mir(
        r#"
trait Observe:
    def inspect(self) -> int64
    def adjust(mut self) -> int64

class Counter:
    value: int64

impl Observe for Counter:
    def inspect(self) -> int64:
        return self.value

    def adjust(mut self) -> int64:
        self.value += 1
        return self.value

def main():
    mut counter = Counter(value=1)
    view held = counter
    print(counter.inspect())
    print(held.value)
"#,
    )
    .expect("trait receiver fixture should lower");
    let field = trait_method
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                ..
            } if field == "inspect" => Some(field),
            _ => None,
        })
        .expect("trait inspect call should lower as a member call");
    *field = "adjust".to_string();
    let error = validate_loan_flow(&trait_method)
        .expect_err("an arbitrary trait method name cannot hide a mutable receiver effect");
    assert!(error.contains("mutates locked place `counter`"), "{error}");

    let mut builtin = crate::lower_source_to_mir(
        r#"
def main():
    mut values = [1, 2]
    view held = values
    print(values.contains(1))
    print(held)
"#,
    )
    .expect("builtin receiver fixture should lower");
    let field = builtin
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                ..
            } if field == "contains" => Some(field),
            _ => None,
        })
        .expect("contains call should lower as a member call");
    *field = "reserve".to_string();
    let error = validate_loan_flow(&builtin)
        .expect_err("a builtin omitted from a field allowlist must retain its mutable effect");
    assert!(error.contains("mutates locked place `values`"), "{error}");
}

#[test]
fn adr0038_mutable_member_receivers_cannot_redirect_writeback() {
    let mut module = crate::lower_source_to_mir(
        r#"
class Counter:
    value: int64

    def increment(mut self):
        self.value += 1

def main():
    mut left = Counter(value=1)
    mut right = Counter(value=2)
    left.increment()
"#,
    )
    .expect("mutable receiver fixture should lower");
    let receiver_place = module
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                field,
                                receiver_place,
                                ..
                            },
                        ..
                    },
                ..
            } if field == "increment" => Some(receiver_place),
            _ => None,
        })
        .expect("increment should lower as a member call");
    assert_eq!(receiver_place.as_deref(), Some("left"));
    *receiver_place = Some("right".to_string());

    let error = validate_loan_flow(&module)
        .expect_err("mutable receiver writeback cannot target another valid place");
    assert!(error.contains("redirects receiver `left`"), "{error}");
    assert!(error.contains("writeback place `right`"), "{error}");
    assert_public_boundaries_reject(&module, "redirects receiver `left`");
}

#[test]
fn adr0038_loan_identifiers_and_active_descriptor_names_are_bounded() {
    let make_module = |loan_names: Vec<String>| {
        let mut module =
            crate::lower_source_to_mir("def main():\n    pass\n").expect("baseline MIR");
        let main = module
            .functions
            .iter_mut()
            .find(|function| function.name == "main")
            .expect("main should exist");
        for (index, loan) in loan_names.iter().enumerate() {
            let source = format!("source_{index}");
            main.local_types.push(MirLocalType {
                name: source.clone(),
                ty: Type::TypeParam("T".to_string()),
            });
            main.local_types.push(MirLocalType {
                name: loan.clone(),
                ty: Type::TypeParam("T".to_string()),
            });
            main.blocks[0].instructions.push(Instruction::BeginLoan {
                loan: loan.clone(),
                source,
                mutable: false,
            });
        }
        for loan in loan_names.into_iter().rev() {
            main.blocks[0]
                .instructions
                .push(Instruction::EndLoan { loan });
        }
        module
    };

    for loan in ["bad\0name", "bad\nname", "%t01", "café"] {
        let error = validate_loan_flow(&make_module(vec![loan.to_string()]))
            .expect_err("public MIR loan names must use one canonical printable spelling");
        assert!(error.contains("non-canonical MIR identifier"), "{error}");
    }

    let nul = make_module(vec!["bad\0name".to_string()]);
    let runtime =
        crate::run_mir(&nul).expect_err("in-memory MIR must reject NUL-bearing descriptor names");
    assert!(runtime.message.contains("non-canonical MIR identifier"));
    let serialized = serde_json::to_vec(&nul).expect("NUL-bearing MIR should serialize as JSON");
    let runtime = crate::run_serialized_mir(&serialized, "<nul>", "")
        .expect_err("serialized MIR must reject NUL-bearing descriptor names");
    assert!(runtime.message.contains("non-canonical MIR identifier"));
    let direct = crate::native_codegen::emit_host_object(&nul)
        .expect_err("direct codegen must share canonical identifier validation");
    assert!(direct.contains("non-canonical MIR identifier"), "{direct}");

    let too_many = (0..=1_024).map(|index| format!("loan_{index}")).collect();
    let error = validate_loan_flow(&make_module(too_many))
        .expect_err("active descriptors must be capped before pairwise validation work");
    assert!(error.contains("active loan descriptor limit"), "{error}");

    let long_names = (0..128)
        .map(|index| format!("loan_{index}_{}", "x".repeat(520)))
        .collect();
    let error = validate_loan_flow(&make_module(long_names))
        .expect_err("active descriptor names need a cumulative byte budget");
    assert!(error.contains("active loan-name byte limit"), "{error}");
}

#[test]
fn adr0038_nested_fixed_places_render_without_placeholder_segments() {
    let module = crate::lower_source_to_mir(
        r#"
class Pair:
    left: int64
    right: int64

def main():
    mut wrapper = (Pair(left=1, right=2), 3)
    view mut field = wrapper[0].right
    field = 9
"#,
    )
    .expect("nested fixed place should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(
            instruction,
            Instruction::BeginLoan { source, .. } if source == "wrapper.0.right"
        )));
    assert!(!main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| format!("{instruction:?}").contains("<expr>")));
}

#[test]
fn adr0038_loan_validator_rejects_alias_spelling_moves_and_use_after_end() {
    let make_module = |instructions: Vec<Instruction>| MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "pair".to_string(),
                    ty: Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
                },
                MirLocalType {
                    name: "value".to_string(),
                    ty: Type::named("str"),
                },
                MirLocalType {
                    name: "other".to_string(),
                    ty: Type::named("str"),
                },
                MirLocalType {
                    name: "alias".to_string(),
                    ty: Type::named("str"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions,
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let numeric_alias = make_module(vec![
        Instruction::BeginLoan {
            loan: "left".to_string(),
            source: "pair.01".to_string(),
            mutable: true,
        },
        Instruction::BeginLoan {
            loan: "right".to_string(),
            source: "pair.1".to_string(),
            mutable: true,
        },
        Instruction::EndLoan {
            loan: "right".to_string(),
        },
        Instruction::EndLoan {
            loan: "left".to_string(),
        },
    ]);
    let error = validate_loan_flow(&numeric_alias)
        .expect_err("tuple positions must use one canonical spelling");
    assert!(error.contains("non-canonical"), "{error}");

    let self_shadow = make_module(vec![
        Instruction::BeginLoan {
            loan: "value".to_string(),
            source: "value".to_string(),
            mutable: false,
        },
        Instruction::EndLoan {
            loan: "value".to_string(),
        },
    ]);
    let error = validate_loan_flow(&self_shadow)
        .expect_err("a loan descriptor cannot shadow its own source root");
    assert!(error.contains("shadows its source root"), "{error}");

    let moved = make_module(vec![
        Instruction::BeginLoan {
            loan: "alias".to_string(),
            source: "value".to_string(),
            mutable: false,
        },
        Instruction::Assign {
            target: "other".to_string(),
            value: Rvalue::Use(Operand::MovePlace("value".to_string())),
        },
        Instruction::EndLoan {
            loan: "alias".to_string(),
        },
    ]);
    let error =
        validate_loan_flow(&moved).expect_err("rvalue moves must not consume a loaned source");
    assert!(error.contains("moves locked place"), "{error}");

    let use_after_end = make_module(vec![
        Instruction::BeginLoan {
            loan: "alias".to_string(),
            source: "value".to_string(),
            mutable: false,
        },
        Instruction::EndLoan {
            loan: "alias".to_string(),
        },
        Instruction::Eval {
            value: Operand::Place("alias".to_string()),
        },
    ]);
    let error = validate_loan_flow(&use_after_end)
        .expect_err("ended loan descriptors cannot be used as ordinary places");
    assert!(error.contains("uses ended loan"), "{error}");
}

#[test]
fn adr0038_return_loan_is_linear_and_returned_paths_are_bounded() {
    let mut module =
        crate::lower_source_to_mir("def main():\n    pass\n").expect("baseline MIR should lower");
    let main = module
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    main.local_types.push(MirLocalType {
        name: "origin".to_string(),
        ty: Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
    });
    main.blocks[0].instructions.insert(
        0,
        Instruction::ReturnLoan {
            loan: "origin".to_string(),
            origin: "origin".to_string(),
        },
    );
    let error = validate_loan_flow(&module)
        .expect_err("ReturnLoan cannot manufacture a returned-view contract from a local origin");
    assert!(error.contains("uses non-parameter origin"), "{error}");

    let mut bounded =
        crate::lower_source_to_mir("def main():\n    pass\n").expect("baseline MIR should lower");
    let bounded_projections = vec!["0".to_string(), "1".to_string()];
    add_test_returned_view_callee(
        &mut bounded,
        "test_choose_tuple_field",
        Type::TypeParam("T".to_string()),
        &bounded_projections,
        false,
    );
    let main = bounded
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    main.local_types.push(MirLocalType {
        name: "root".to_string(),
        ty: Type::Tuple(vec![Type::named("int64"), Type::named("int64")]),
    });
    for depth in 0..14 {
        main.local_types.push(MirLocalType {
            name: format!("loan{depth}"),
            ty: Type::named("int64"),
        });
        main.local_types.push(MirLocalType {
            name: format!("handoff{depth}"),
            ty: Type::named("int64"),
        });
        main.blocks[0].instructions.push(Instruction::Assign {
            target: format!("handoff{depth}"),
            value: Rvalue::Call {
                callee: CallTarget::Name("test_choose_tuple_field".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(if depth == 0 {
                        "root".to_string()
                    } else {
                        format!("loan{}", depth - 1)
                    }),
                    writeback_place: None,
                }],
            },
        });
        main.blocks[0]
            .instructions
            .push(Instruction::BeginReturnedLoan {
                loan: format!("loan{depth}"),
                origin: if depth == 0 {
                    "root".to_string()
                } else {
                    format!("loan{}", depth - 1)
                },
                projections: bounded_projections.clone(),
                mutable: false,
            });
    }
    for depth in (0..14).rev() {
        main.blocks[0].instructions.push(Instruction::EndLoan {
            loan: format!("loan{depth}"),
        });
    }
    let error = validate_loan_flow(&bounded)
        .expect_err("returned-view alternatives need a deterministic expansion budget");
    assert!(error.contains("alternative limit"), "{error}");
}

#[test]
fn adr0038_mir_structure_is_validated_before_reachability() {
    let baseline =
        crate::lower_source_to_mir("def main():\n    pass\n").expect("baseline MIR should lower");

    let mut duplicate = baseline.clone();
    let main = duplicate
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    main.blocks.push(BasicBlock {
        label: main.entry.clone(),
        instructions: Vec::new(),
        terminator: Terminator::Return(Operand::Unit),
    });
    let error = validate_loan_flow(&duplicate)
        .expect_err("duplicate block labels must not be hidden by map construction");
    assert!(error.contains("duplicate block label"), "{error}");

    let mut unknown_dead_successor = baseline;
    let main = unknown_dead_successor
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    main.blocks.push(BasicBlock {
        label: "dead".to_string(),
        instructions: Vec::new(),
        terminator: Terminator::Goto("missing".to_string()),
    });
    let error = validate_loan_flow(&unknown_dead_successor)
        .expect_err("unknown successors must be rejected even in unreachable blocks");
    assert!(error.contains("unknown block `missing`"), "{error}");

    let runtime_error = crate::mir_runtime::run(&unknown_dead_successor)
        .expect_err("the MIR backend must use the common structural validator");
    assert!(runtime_error.message.contains("unknown block `missing`"));
    let direct_error = crate::native_codegen::emit_host_object(&unknown_dead_successor)
        .expect_err("the direct backend must use the common structural validator");
    assert!(
        direct_error.contains("unknown block `missing`"),
        "{direct_error}"
    );
}

#[test]
fn adr0038_mir_reborrow_and_returned_projections_are_canonical_and_type_valid() {
    let make_module = |origin_ty: Type,
                       parent_ty: Type,
                       child_ty: Type,
                       classes: Vec<MirClass>,
                       instructions: Vec<Instruction>| MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<test>".to_string(),
            source_path: None,
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "origin".to_string(),
                    ty: origin_ty,
                },
                MirLocalType {
                    name: "parent".to_string(),
                    ty: parent_ty,
                },
                MirLocalType {
                    name: "child".to_string(),
                    ty: child_ty,
                },
                MirLocalType {
                    name: "handoff".to_string(),
                    ty: Type::named("int64"),
                },
            ],
            return_type: Type::Unit,
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions,
                terminator: Terminator::Return(Operand::Unit),
            }],
        }],
        classes,
        trait_impls: Vec::new(),
        top_level: None,
    };
    let tuple_ty = Type::Tuple(vec![Type::named("int64"), Type::named("int64")]);

    for projection in ["01", "2"] {
        let module = make_module(
            tuple_ty.clone(),
            tuple_ty.clone(),
            Type::named("int64"),
            Vec::new(),
            vec![
                Instruction::BeginLoan {
                    loan: "parent".to_string(),
                    source: "origin".to_string(),
                    mutable: false,
                },
                Instruction::Reborrow {
                    loan: "child".to_string(),
                    parent: "parent".to_string(),
                    projection: projection.to_string(),
                    mutable: false,
                },
                Instruction::EndLoan {
                    loan: "child".to_string(),
                },
                Instruction::EndLoan {
                    loan: "parent".to_string(),
                },
            ],
        );
        let error = validate_loan_flow(&module)
            .expect_err("forged tuple reborrow projections must be rejected centrally");
        assert!(
            error.contains(if projection == "01" {
                "non-canonical"
            } else {
                "out of bounds"
            }),
            "{error}"
        );
        if projection == "01" {
            let runtime_error = crate::mir_runtime::run(&module)
                .expect_err("the MIR backend must reject non-canonical reborrows centrally");
            assert!(runtime_error.message.contains("non-canonical"));
            let direct_error = crate::native_codegen::emit_host_object(&module)
                .expect_err("the direct backend must reject non-canonical reborrows centrally");
            assert!(direct_error.contains("non-canonical"), "{direct_error}");
        }
    }

    let pair = MirClass {
        name: "Pair".to_string(),
        type_params: Vec::new(),
        fields: vec![MirClassField {
            name: "left".to_string(),
            ty: Type::named("int64"),
        }],
        methods: Vec::new(),
    };
    let returned = |projections: Vec<String>| {
        let mut module = make_module(
            Type::named("Pair"),
            Type::named("int64"),
            Type::named("int64"),
            vec![pair.clone()],
            vec![
                Instruction::Assign {
                    target: "handoff".to_string(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name("test_pair_projection".to_string()),
                        args: vec![MirArg {
                            name: None,
                            value: Operand::Place("origin".to_string()),
                            writeback_place: None,
                        }],
                    },
                },
                Instruction::BeginReturnedLoan {
                    loan: "parent".to_string(),
                    origin: "origin".to_string(),
                    projections: projections.clone(),
                    mutable: false,
                },
                Instruction::EndLoan {
                    loan: "parent".to_string(),
                },
            ],
        );
        add_test_returned_view_callee(
            &mut module,
            "test_pair_projection",
            Type::named("Pair"),
            &projections,
            false,
        );
        module
    };
    let missing = returned(vec!["left".to_string(), "missing".to_string()]);
    let error = validate_loan_flow(&missing)
        .expect_err("every returned-view alternative must name a real field");
    assert!(error.contains("has no field `missing`"), "{error}");

    validate_loan_flow(&returned(vec!["left".to_string()]))
        .expect("a canonical, type-valid returned-view projection should remain valid");

    let mut self_shadow = returned(vec![String::new()]);
    let instructions = &mut self_shadow.functions[0].blocks[0].instructions;
    let Instruction::BeginReturnedLoan { loan, .. } = &mut instructions[1] else {
        panic!("the probe should contain a returned loan");
    };
    *loan = "origin".to_string();
    let error = validate_loan_flow(&self_shadow)
        .expect_err("a returned loan descriptor cannot shadow its origin root");
    assert!(error.contains("shadows its origin root"), "{error}");
}

#[test]
fn adr0038_reborrow_expansion_has_cumulative_work_and_active_state_budgets() {
    let projections = (0..MAX_VALIDATED_LOAN_ALTERNATIVES)
        .map(|index| index.to_string())
        .collect::<Vec<_>>();
    let long_projection = "field".repeat(104);
    let mut module =
        crate::lower_source_to_mir("def main():\n    pass\n").expect("baseline MIR should lower");
    add_test_returned_view_callee(
        &mut module,
        "test_many_projections",
        Type::TypeParam("T".to_string()),
        &projections,
        false,
    );
    let main = module
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
        .expect("main should exist");
    for name in ["origin", "parent", "first", "second"] {
        main.local_types.push(MirLocalType {
            name: name.to_string(),
            ty: Type::TypeParam("T".to_string()),
        });
    }
    main.local_types.push(MirLocalType {
        name: "handoff".to_string(),
        ty: Type::named("int64"),
    });
    main.blocks[0].instructions.extend([
        Instruction::Assign {
            target: "handoff".to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Name("test_many_projections".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place("origin".to_string()),
                    writeback_place: None,
                }],
            },
        },
        Instruction::BeginReturnedLoan {
            loan: "parent".to_string(),
            origin: "origin".to_string(),
            projections,
            mutable: false,
        },
        Instruction::Reborrow {
            loan: "first".to_string(),
            parent: "parent".to_string(),
            projection: long_projection.clone(),
            mutable: false,
        },
        Instruction::Reborrow {
            loan: "second".to_string(),
            parent: "parent".to_string(),
            projection: long_projection,
            mutable: false,
        },
        Instruction::EndLoan {
            loan: "second".to_string(),
        },
        Instruction::EndLoan {
            loan: "first".to_string(),
        },
        Instruction::EndLoan {
            loan: "parent".to_string(),
        },
    ]);
    let error = validate_loan_flow(&module)
        .expect_err("reborrow expansion must be bounded before all paths are materialized");
    assert!(error.contains("loan-path byte limit"), "{error}");
}

#[test]
fn adr0038_generic_trait_views_and_early_return_cleanup_lower_without_panics() {
    let generic = crate::lower_source_to_mir(
        r#"
trait Project:
    def get(self) -> view int64 from self

class Box:
    value: int64

impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.value

def read[T: Project](item: T) -> int64:
    view alias = item.get()
    return alias + 0

def main():
    print(read(Box(value=7)))
"#,
    )
    .expect("bounded trait returned views should lower through their implementation footprints");
    validate_loan_flow(&generic).expect("generic trait returned-view MIR should validate");

    let early = crate::lower_source_to_mir(
        r#"
def main():
    mut value = 1
    view alias = value
    if false:
        return
    print(alias)
"#,
    )
    .expect("cleanup on an untaken early-return edge must not consume fallthrough lowering state");
    validate_loan_flow(&early).expect("every reachable early-return path should balance its loans");
}

#[test]
fn adr0038_branch_local_view_ids_and_existing_view_capture_sources_are_distinct() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    mut left = 1
    mut right = 2
    if true:
        view mut selected = left
        selected = 10
    else:
        view mut selected = right
        selected = 20
"#,
    )
    .expect("branch-local views should lower");
    let loan_names = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::BeginLoan { loan, .. } => Some(loan.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        loan_names.len(),
        2,
        "branch-local aliases need distinct MIR identities"
    );

    let captured = crate::lower_source_to_mir(
        r#"
def main():
    mut values = [1]
    view mut editable = values
    mut update: def(int64) -> None = lambda [mut editable] next: editable.append(next)
    update(3)
    print(editable)
"#,
    )
    .expect("capturing an existing mutable view should lower as a contained access");
    let capture = captured
        .functions
        .iter()
        .find(|function| function.name == "main")
        .into_iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => captures.first(),
            _ => None,
        })
        .expect("closure capture should exist");
    assert_eq!(capture.source_place.as_deref(), Some("editable"));
}

#[test]
fn adr0038_cleanup_ends_local_loans_before_early_exit() {
    let module = crate::lower_source_to_mir(
        r#"
class Counter:
    value: int64

def read(counter: Counter) -> int64:
    view value = counter.value
    return value + 0

def loop_once(counter: Counter):
    while true:
        view value = counter.value
        print(value)
        break
"#,
    )
    .expect("early-exit loan source should lower");

    for function_name in ["read", "loop_once"] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .expect("lowered function should exist");
        assert!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(instruction, Instruction::EndLoan { loan } if loan == "value")),
            "{function_name} must release its local view on the exited path"
        );
    }
}

#[test]
fn adr0038_returned_view_projection_helpers_cover_every_place_and_control_shape() {
    assert!(place_paths_overlap("pair", "pair.left"));
    assert!(place_paths_overlap("pair.left", "pair"));
    assert!(!place_paths_overlap("pair.left", "pair.right"));
    assert!(!place_paths_overlap("left.value", "right.value"));

    let name = |value: &str| Expr {
        kind: ExprKind::Name(value.to_string()),
        span: Span::new(1, 1),
    };
    let aliases = BTreeMap::from([("alias".to_string(), "left".to_string())]);
    assert_eq!(
        return_view_projection_expr(&name("origin"), "origin", &aliases).as_deref(),
        Some("")
    );
    assert_eq!(
        return_view_projection_expr(&name("alias"), "origin", &aliases).as_deref(),
        Some("left")
    );
    assert_eq!(
        return_view_projection_expr(&name("other"), "origin", &aliases),
        None
    );
    let member = Expr {
        kind: ExprKind::Member {
            object: Box::new(name("origin")),
            field: "right".to_string(),
        },
        span: Span::new(1, 1),
    };
    let grouped = Expr {
        kind: ExprKind::Group(Box::new(member)),
        span: Span::new(1, 1),
    };
    let indexed = Expr {
        kind: ExprKind::Index {
            object: Box::new(grouped),
            index: Box::new(Expr {
                kind: ExprKind::Int(2),
                span: Span::new(1, 1),
            }),
        },
        span: Span::new(1, 1),
    };
    assert_eq!(
        return_view_projection_expr(&indexed, "origin", &aliases).as_deref(),
        Some("right.2")
    );
    for index in [
        ExprKind::Int(u128::MAX),
        ExprKind::Name("index".to_string()),
    ] {
        let invalid = Expr {
            kind: ExprKind::Index {
                object: Box::new(name("origin")),
                index: Box::new(Expr {
                    kind: index,
                    span: Span::new(1, 1),
                }),
            },
            span: Span::new(1, 1),
        };
        assert_eq!(
            return_view_projection_expr(&invalid, "origin", &aliases),
            None
        );
    }
    assert_eq!(
        return_view_projection_expr(
            &Expr {
                kind: ExprKind::Int(1),
                span: Span::new(1, 1),
            },
            "origin",
            &aliases,
        ),
        None
    );

    let module = crate::parse_source(
        r#"
class Pair:
    left: int64
    right: int64

def explore(origin: Pair, condition: bool) -> view int64 from origin:
    view alias = origin.left
    if condition:
        return view alias
    else:
        return view origin.right
    match condition:
        case true:
            return view origin.left
        case _:
            pass
    for ignored in [1]:
        return view origin.right
    with TaskGroup() as group:
        return view origin.left
    while condition:
        return view origin.right
    return view origin

def plain():
    pass
"#,
    )
    .expect("projection collector input should parse");
    let explore = module
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Function(function) if function.name == "explore" => Some(function),
            _ => None,
        })
        .expect("explore function should be present");
    assert_eq!(
        return_view_projections(explore),
        vec!["".to_string(), "left".to_string(), "right".to_string()]
    );
    let plain = module
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Function(function) if function.name == "plain" => Some(function),
            _ => None,
        })
        .expect("plain function should be present");
    assert!(return_view_projections(plain).is_empty());
}

#[test]
fn adr0038_closure_capture_lowering_preserves_each_loan_mode() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    shared = 1
    mut values = [2]
    text = "owned"
    callback: def(int64) -> (int64, None, str) = lambda [shared, mut values, own text] item: (shared + item, values.append(item), text)
    print(callback(3))
"#,
    )
    .expect("all closure capture capabilities should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let captures = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Closure { captures, .. },
                ..
            } => Some(captures),
            _ => None,
        })
        .expect("capturing lambda should emit a closure environment");
    assert!(captures.iter().any(|capture| {
        capture.name == "shared"
            && capture.passing == MirReceiverKind::Borrow
            && capture.source_place.as_deref() == Some("shared")
    }));
    assert!(captures.iter().any(|capture| {
        capture.name == "values"
            && capture.passing == MirReceiverKind::BorrowMut
            && capture.source_place.as_deref() == Some("values")
    }));
    assert!(captures.iter().any(|capture| {
        capture.name == "text"
            && capture.passing == MirReceiverKind::Value
            && capture.source_place.is_none()
            && matches!(capture.value, Operand::MovePlace(_))
    }));
}

#[test]
fn adr0038_returned_method_views_and_immediate_call_reborrows_lower_explicitly() {
    let module = crate::lower_source_to_mir(
        r#"
class Box:
    value: int64

    def identity(mut self) -> view mut Box from self:
        return view mut self

def value(box: mut Box) -> view mut int64 from box:
    return view mut box.value

def bump(value: mut int64):
    value += 1

def main():
    mut box = Box(value=1)
    view mut root = box.identity()
    bump(value(root))
    print(root.value)
"#,
    )
    .expect("self-returned and immediately passed views should lower");
    let instructions = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::ReturnLoan { loan, origin } if loan == "self" && origin == "self"
    )));
    assert!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction, Instruction::BeginReturnedLoan { .. }))
            .count()
            >= 2
    );
}

#[test]
fn adr0038_module_qualified_returned_views_keep_exported_origin_metadata() {
    let unique = format!(
        "aura-mir-returned-view-imports-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos()
    );
    let package = std::env::temp_dir().join(unique);
    let src = package.join("src");
    fs::create_dir_all(&src).expect("temporary returned-view package should exist");
    fs::write(
        package.join("Aura.toml"),
        "[package]\nname = \"returned_view_imports\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("temporary package manifest should be writable");
    fs::write(
        src.join("api.au"),
        "public def borrow(values: list[int64]) -> view list[int64] from values:\n    return view values\n",
    )
    .expect("temporary returned-view module should be writable");
    let main_path = src.join("main.au");
    fs::write(
        &main_path,
        "import api\n\ndef main():\n    values = [1]\n    view alias = api.borrow(values)\n    print(alias)\n",
    )
    .expect("temporary returned-view entry should be writable");

    let module = crate::lower_path_to_mir(&main_path)
        .expect("qualified exported returned-view calls should lower");
    let _ = fs::remove_dir_all(&package);
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main MIR should exist");
    assert!(main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(
            instruction,
            Instruction::BeginReturnedLoan { origin, .. } if origin == "values"
        )));
}

#[test]
fn adr0038_imported_returned_view_forwarding_uses_the_declaration_owner() {
    let unique = format!(
        "aura-mir-returned-view-forwarding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos()
    );
    let package = std::env::temp_dir().join(unique);
    let src = package.join("src");
    fs::create_dir_all(&src).expect("temporary forwarding package should exist");
    fs::write(
        package.join("Aura.toml"),
        "[package]\nname = \"returned_view_forwarding\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("temporary package manifest should be writable");
    fs::write(
        src.join("api.au"),
        r#"public class Pair:
    public left: int64
    public right: int64

public def choose(pair: mut Pair, take_left: bool) -> view mut int64 from pair:
    if take_left:
        return view mut pair.left
    return view mut pair.right

public def forward(pair: mut Pair, take_left: bool) -> view mut int64 from pair:
    return view mut choose(pair, take_left)

public trait Other:
    def get(self) -> view int64 from self

public trait Project:
    def get(self) -> view int64 from self

public class Box:
    public value: int64
    public other: int64

impl Other for Box:
    def get(self) -> view int64 from self:
        return view self.other

impl Project for Box:
    def get(self) -> view int64 from self:
        return view self.value

public def trait_forward[T: Project](item: T) -> view int64 from item:
    return view item.get()

def inner(pair: Pair) -> view int64 from pair:
    return view pair.left

public def exact_forward(pair: Pair) -> view int64 from pair:
    return view inner(pair)

public def identity(pair: Pair) -> view Pair from pair:
    return view pair

public def nested_exact_forward(pair: Pair) -> view int64 from pair:
    return view inner((identity(pair)))
"#,
    )
    .expect("temporary returned-view module should be writable");
    let main_path = src.join("main.au");
    fs::write(
        &main_path,
        r#"import api

def inner(pair: api.Pair) -> view int64 from pair:
    return view pair.right

def main():
    mut pair = api.Pair(left=1, right=2)
    view mut selected = api.forward(pair, false)
    selected = 9
    print(pair.right)
    box = api.Box(value=7, other=8)
    view value = api.trait_forward(box)
    print(value)
    mut exact_pair = api.Pair(left=10, right=20)
    view exact = api.exact_forward(exact_pair)
    exact_pair.right = 21
    print(exact)
    print(exact_pair.right)
    mut nested_pair = api.Pair(left=30, right=40)
    view nested_exact = api.nested_exact_forward(nested_pair)
    nested_pair.right = 41
    print(nested_exact)
    print(nested_pair.right)
"#,
    )
    .expect("temporary returned-view entry should be writable");

    let result = (|| {
        let module = crate::lower_path_to_mir(&main_path)?;
        validate_loan_flow(&module).map_err(crate::Diagnostic::new)?;
        crate::mir_runtime::run(&module)
    })();
    let _ = fs::remove_dir_all(&package);
    let output = result.expect("imported forwarding must lower and execute without a panic");
    assert_eq!(output.stdout, "9\n7\n10\n21\n30\n41\n");
}

fn lower_ffi_source(source: &str) -> MirModule {
    let module = crate::parse_source(source).expect("FFI source should parse");
    let program =
        crate::check_module_with_builtin_imports(module).expect("FFI source should type check");
    lower(&program)
}

fn expr(kind: ExprKind) -> Expr {
    Expr {
        kind,
        span: Span::new(1, 1),
    }
}

fn name_expr(name: &str) -> Expr {
    expr(ExprKind::Name(name.to_string()))
}

fn member_expr(object: Expr, field: &str) -> Expr {
    expr(ExprKind::Member {
        object: Box::new(object),
        field: field.to_string(),
    })
}

fn operand_function_name(operand: &Operand) -> Option<&str> {
    match operand {
        Operand::Function { name, .. } => Some(name),
        _ => None,
    }
}

fn type_ref(name: &str) -> TypeRef {
    TypeRef::named(name, Vec::new(), false, Span::new(1, 1))
}

#[test]
fn json_runtime_enums_keep_their_module_qualified_identity() {
    let program = checked_program("def main():\n    pass\n");
    for enum_name in ["Value", "Error"] {
        let enum_info = crate::sema::EnumInfo {
            module_name: "json".to_string(),
            decl: crate::ast::EnumDecl {
                public: true,
                name: enum_name.to_string(),
                type_params: Vec::new(),
                type_param_bounds: std::collections::BTreeMap::new(),
                variants: Vec::new(),
                span: Span::new(1, 1),
            },
            type_param_bounds: std::collections::BTreeMap::new(),
            variants: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            mir_runtime_enum_name(&program, &enum_info),
            format!("json.{enum_name}")
        );
    }
}

fn arg(value: Expr) -> Argument {
    Argument {
        name: None,
        span: value.span,
        value,
    }
}

fn assertion_failures(module: &MirModule) -> Vec<(&MirFunction, &BasicBlock)> {
    module
        .functions
        .iter()
        .flat_map(|function| {
            function.blocks.iter().filter_map(move |block| {
                matches!(block.terminator, Terminator::AssertFail { .. })
                    .then_some((function, block))
            })
        })
        .collect()
}

#[test]
fn assertion_introspection_captures_supported_binary_and_membership_operands() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    values = [1, 2]
    assert (1 == 2)
    assert 1 != 2
    assert 1 < 2
    assert 1 <= 2
    assert 1 > 2
    assert 1 >= 2
    assert 1 in values
"#,
    )
    .expect("supported assertion forms should lower");

    let failures = assertion_failures(&module);
    assert_eq!(failures.len(), 7);
    for (_, block) in &failures[..6] {
        let Terminator::AssertFail { captures, .. } = &block.terminator else {
            unreachable!();
        };
        assert_eq!(
            captures
                .iter()
                .map(|capture| (capture.label.as_str(), &capture.ty))
                .collect::<Vec<_>>(),
            vec![
                ("left", &Type::named("int64")),
                ("right", &Type::named("int64")),
            ]
        );
        assert!(captures
            .iter()
            .all(|capture| matches!(capture.value, Operand::Place(_))));
    }
    let Terminator::AssertFail { captures, .. } = &failures[6].1.terminator else {
        unreachable!();
    };
    assert_eq!(captures[0].label, "item");
    assert_eq!(captures[0].ty, Type::named("int64"));
    assert_eq!(captures[1].label, "collection");
    assert_eq!(
        captures[1].ty,
        Type::Named("list".to_string(), vec![Type::named("int64")])
    );
}

#[test]
fn assertion_introspection_excludes_conditions_without_two_operand_semantics() {
    let module = crate::lower_source_to_mir(
        r#"
def truth() -> bool:
    return true

def main():
    values = [1, 2]
    assert 1 < 2 < 3
    assert 3 not in values
    assert 1 < 2 and 2 < 3
    assert truth()
"#,
    )
    .expect("ordinary assertion forms should lower");

    for (_, block) in assertion_failures(&module) {
        let Terminator::AssertFail { captures, .. } = &block.terminator else {
            unreachable!();
        };
        assert!(captures.is_empty());
    }
}

#[test]
fn assertion_introspection_evaluates_once_left_to_right_and_renders_before_lazy_message() {
    let module = crate::lower_source_to_mir(
        r#"
def left() -> int64:
    return 41

def right() -> int64:
    return 42

def explain() -> str:
    return "different"

def main():
    assert left() == right(), explain()
"#,
    )
    .expect("introspected assertion should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let ordered_calls = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Name(name),
                        ..
                    },
                ..
            } => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ordered_calls, vec!["left", "right", "explain"]);

    let (_, failure) = assertion_failures(&module)
        .into_iter()
        .next()
        .expect("assertion failure block should exist");
    let render_count = failure
        .instructions
        .iter()
        .filter(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::FormatString { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(render_count, 2);
    let Terminator::AssertFail {
        message: Some(message),
        captures,
        ..
    } = &failure.terminator
    else {
        panic!("assertion message and captures should be on the failure edge");
    };
    assert!(matches!(message, Operand::Place(_)));
    assert_eq!(captures.len(), 2);
}

#[test]
fn assertion_introspection_requires_shared_custom_comparison_dispatch() {
    fn capture_count(receiver: &str, rhs: &str) -> usize {
        let source = format!(
            r#"
trait Ord[Rhs]:
    def lt({receiver}, rhs: {rhs} Rhs) -> bool

class Score:
    value: int64

impl Ord[Score] for Score:
    def lt({receiver}, rhs: {rhs} Score) -> bool:
        return self.value < rhs.value

def main():
    assert Score(value=1) < Score(value=2)
"#
        );
        let module =
            crate::lower_source_to_mir(&source).expect("custom comparison contract should lower");
        let failures = assertion_failures(&module);
        let Terminator::AssertFail { captures, .. } = &failures[0].1.terminator else {
            unreachable!();
        };
        captures.len()
    }

    assert_eq!(capture_count("self", ""), 2);
    assert_eq!(capture_count("own self", ""), 0);
    assert_eq!(capture_count("self", "own"), 0);
    assert_eq!(capture_count("own self", "own"), 0);
}

#[test]
fn array_and_builtin_associated_results_retain_checked_types_in_mir() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    values = Array[int32].from_list([1, 2], [2])
    scalar: int32 = 2
    right = values + scalar
    left = scalar - values
    mapped = values.map(lambda value: value.to_float())
    offset: int32 = 1
    captured_mapped = values.map(lambda value: value + offset)
    coordinate: int32 = 0
    item = values[(coordinate)]
    duration = Duration.ms(1)
    bytes: list[uint8] = [65]
    decoded = str.from_bytes(bytes)
    print(right)
    print(left)
    print(mapped)
    print(captured_mapped)
    print(item)
    print(duration)
    print(decoded)
"#,
    )
    .expect("checked Array and associated-function expressions should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let locals = main
        .local_types
        .iter()
        .map(|local| (local.name.as_str(), local.ty.clone()))
        .collect::<BTreeMap<_, _>>();
    let int_array = Type::Named("Array".to_string(), vec![Type::named("int32")]);
    assert_eq!(locals["right"], int_array);
    assert_eq!(locals["left"], int_array);
    assert_eq!(
        locals["mapped"],
        Type::Named("Array".to_string(), vec![Type::named("float64")])
    );
    assert_eq!(
        locals["captured_mapped"],
        Type::Named("Array".to_string(), vec![Type::named("int32")])
    );
    assert_eq!(locals["item"], Type::named("int32"));
    assert_eq!(locals["duration"], Type::named("Duration"));
    assert_eq!(
        locals["decoded"],
        Type::Named(
            "Result".to_string(),
            vec![Type::named("str"), Type::named("bytes.Error")]
        )
    );
}

#[test]
fn extern_calls_lower_to_explicit_abi_metadata_without_synthetic_function_bodies() {
    let module = lower_ffi_source(
        r#"
public extern "C" opaque class Handle
public extern "C" def consume(
    value: int32,
    bytes: mut list[uint8],
    handle: own Handle
) -> int32
public extern "C" def acquire() -> Handle
public extern "C" def close(handle: own Handle) -> None

def main():
    handle = acquire()
    close(handle)
"#,
    );
    assert!(
        module
            .functions
            .iter()
            .all(|function| !matches!(function.name.as_str(), "consume" | "acquire" | "close")),
        "an extern declaration must not acquire an ordinary Aura MIR body"
    );
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (close, close_args) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Extern(call),
                        args,
                    },
                ..
            } if call.symbol == "close" => Some((call, args)),
            _ => None,
        })
        .expect("own-handle extern call should lower");
    assert_eq!(close.params[0].passing, MirReceiverKind::Value);
    assert!(matches!(close_args[0].value, Operand::MovePlace(_)));

    let call_module = lower_ffi_source(
        r#"
public extern "C" def mutate(value: int32, bytes: mut list[uint8]) -> int32

def main():
    mut bytes: list[uint8] = [1, 2]
    answer = mutate(bytes=bytes, value=7)
"#,
    );
    let main = call_module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (extern_call, args) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Extern(call),
                        args,
                    },
                ..
            } => Some((call, args)),
            _ => None,
        })
        .expect("the extern call should retain an explicit MIR target");
    assert_eq!(extern_call.symbol, "mutate");
    assert_eq!(extern_call.abi, "C");
    assert_eq!(extern_call.return_type, Type::named("int32"));
    assert_eq!(
        extern_call
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.passing, param.ty.clone(),))
            .collect::<Vec<_>>(),
        vec![
            ("value", MirReceiverKind::Borrow, Type::named("int32")),
            (
                "bytes",
                MirReceiverKind::BorrowMut,
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
            ),
        ]
    );
    assert_eq!(args.len(), 2, "named arguments bind in declaration order");
    assert!(args[0].writeback_place.is_none());
    assert_eq!(args[1].writeback_place.as_deref(), Some("bytes"));
}

#[test]
fn extern_view_and_shared_handle_calls_preserve_exact_mir_contracts() {
    let module = lower_ffi_source(
        r#"
public extern "C" opaque class Handle
public extern "C" def acquire() -> Handle
public extern "C" def inspect(
    text: str,
    input: list[uint8],
    output: mut list[uint8],
    handle: Handle
) -> uint64

def main():
    mut output: list[uint8] = [0, 0]
    handle = acquire()
    answer = inspect("hi", [1, 2], output, handle)
"#,
    );
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (call, args) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Extern(call),
                        args,
                    },
                ..
            } if call.symbol == "inspect" => Some((call, args)),
            _ => None,
        })
        .expect("view-and-handle extern call should lower");

    assert_eq!(call.abi, "C");
    assert_eq!(call.return_type, Type::named("uint64"));
    assert_eq!(
        call.params
            .iter()
            .map(|param| (param.name.as_str(), param.passing, param.ty.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("text", MirReceiverKind::Borrow, Type::named("str")),
            (
                "input",
                MirReceiverKind::Borrow,
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
            ),
            (
                "output",
                MirReceiverKind::BorrowMut,
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
            ),
            ("handle", MirReceiverKind::Borrow, Type::named("Handle")),
        ]
    );
    assert_eq!(args.len(), 4);
    assert_eq!(
        args.iter()
            .map(|arg| arg.writeback_place.as_deref())
            .collect::<Vec<_>>(),
        vec![None, None, Some("output"), None]
    );
    assert!(
        matches!(args[3].value, Operand::Place(_)),
        "a shared opaque handle must not lower as a consuming move"
    );
}

#[test]
fn imported_extern_calls_lower_from_and_qualified_names_without_wrapper_bodies() {
    let unique = format!(
        "aura-mir-ffi-imports-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos()
    );
    let package = std::env::temp_dir().join(unique);
    let src = package.join("src");
    fs::create_dir_all(&src).expect("temporary FFI package source directory should exist");
    fs::write(
        package.join("Aura.toml"),
        "[package]\nname = \"mir_ffi_imports\"\nversion = \"0.1.0\"\nedition = \"2026\"\nallow_ffi = true\n",
    )
    .expect("temporary FFI manifest should be writable");
    fs::write(
        src.join("api.au"),
        "public extern \"C\" def getpid() -> int32\n",
    )
    .expect("temporary FFI module should be writable");
    let main_path = src.join("main.au");
    fs::write(
        &main_path,
        "from api import getpid\nimport api\n\ndef main():\n    direct = getpid()\n    qualified = api.getpid()\n",
    )
    .expect("temporary FFI entry should be writable");

    let module = crate::lower_path_to_mir(&main_path)
        .expect("from-imported and qualified extern calls should lower");
    let _ = fs::remove_dir_all(&package);
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let extern_calls = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Extern(call),
                        ..
                    },
                ..
            } => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(extern_calls.len(), 2);
    assert!(extern_calls
        .iter()
        .all(|call| call.symbol == "getpid" && call.abi == "C"));
    assert!(module
        .functions
        .iter()
        .all(|function| !function.name.ends_with("getpid")));
}

#[test]
fn tuple_literals_capture_each_element_left_to_right_before_construction() {
    let module = crate::lower_source_to_mir(
        r#"
def first() -> str:
    return "first"

def second() -> str:
    return "second"

def main():
    pair = (first(), second())
"#,
    )
    .expect("tuple literal should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    let first_call = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name),
                        ..
                    },
                    ..
                } if name == "first"
            )
        })
        .expect("first call should lower");
    let second_call = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name),
                        ..
                    },
                    ..
                } if name == "second"
            )
        })
        .expect("second call should lower");
    let tuple_construct = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::TupleLiteral { .. },
                    ..
                }
            )
        })
        .expect("tuple construction should lower");
    assert!(first_call < second_call && second_call < tuple_construct);
    assert!(
        instructions[first_call + 1..second_call]
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Use(Operand::MovePlace(_)),
                        ..
                    }
                )
            }),
        "the first element must be captured before the second expression runs"
    );
}

#[test]
fn tuple_literal_elements_use_the_explicit_tuple_annotation_context() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    pair: (int8, int16) = (1, 2)
"#,
    )
    .expect("annotated tuple literal should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let element_types = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::TupleLiteral { element_types, .. },
                ..
            } => Some(element_types),
            _ => None,
        })
        .expect("tuple literal should be explicit in MIR");
    assert_eq!(
        element_types,
        &vec![Type::named("int8"), Type::named("int16")]
    );
}

#[test]
fn non_copy_destructure_consumes_the_whole_source_then_takes_captured_elements() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    pair = ("left", "right")
    left, right = pair
    print(left)
    print(right)
"#,
    )
    .expect("non-Copy tuple destructure should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Use(Operand::MovePlace(place)),
                        ..
                    } if place == "pair"
                )
            })
            .count(),
        1,
        "the original tuple binding must be consumed exactly once"
    );
    let takes = instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::TupleTakeElement { place, index, .. },
                ..
            } => Some((place, index)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(takes.len(), 2);
    assert!(takes.iter().all(|(place, _)| place.starts_with("%t")));
    assert_eq!(
        takes.iter().map(|(_, index)| **index).collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn copy_tuple_indexing_projects_without_partial_move_mir() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    pair = (10, 20)
    first = pair[0]
    print(first)
"#,
    )
    .expect("Copy tuple index should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let rvalues = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign { value, .. } => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(rvalues.iter().any(|value| {
        matches!(
            value,
            Rvalue::TupleElement {
                index: 0,
                element_type,
                ..
            } if *element_type == Type::named("int64")
        )
    }));
    assert!(!rvalues
        .iter()
        .any(|value| matches!(value, Rvalue::TupleTakeElement { .. })));
}

#[test]
fn projected_source_reads_do_not_register_projection_paths_as_mir_locals() {
    let module = crate::lower_source_to_mir(
        r#"
class User:
    name: str
    id: int32

def main():
    mut user = User(name="Ada", id=1)
    name = user.name
    print(user.id)
    user.name = "Grace"
    print(user.name)
"#,
    )
    .expect("partial-move and reinitialization source should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    assert!(
        main.local_types
            .iter()
            .all(|local| !local.name.contains('.')),
        "projection paths are MIR places, not local identifiers: {:?}",
        main.local_types
    );
    validate_loan_flow(&module).expect("source-generated MIR must pass public MIR validation");
    let output = crate::run_mir(&module).expect("valid source-generated MIR should execute");
    assert_eq!(output.stdout, "1\nGrace\n");
}

#[test]
fn tuple_patterns_lower_through_element_cfg_not_enum_match() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    match (1, 2):
        case (1, value):
            print(value)
        case _:
            pass
"#,
    )
    .expect("tuple pattern should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    assert!(main.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::TupleElement { .. },
                    ..
                }
            )
        })
    }));
    assert!(main
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, Terminator::Branch { .. })));
    assert!(
        !main
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, Terminator::Match { .. })),
        "tuple patterns must not use the enum-only Match terminator"
    );
}

#[test]
fn consuming_bind_only_tuple_patterns_do_not_clone_elements_during_matching() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    match own ("left", "right"):
        case (left, right):
            print(left)
            print(right)
"#,
    )
    .expect("consuming bind-only tuple pattern should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let rvalues = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign { value, .. } => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        rvalues
            .iter()
            .filter(|value| matches!(value, Rvalue::TupleTakeElement { .. }))
            .count(),
        2
    );
    assert!(
        !rvalues
            .iter()
            .any(|value| matches!(value, Rvalue::TupleElement { .. })),
        "binding registration must not clone non-Copy tuple elements"
    );
}

#[test]
fn consuming_mixed_tuple_patterns_take_owned_elements_and_copy_scalar_bindings() {
    let source = r#"
def main():
    match own ("owned", 7, true):
        case (text, number, true):
            print(f"{text}:{number}")
        case _:
            pass
"#;
    let module = crate::lower_source_to_mir(source)
        .expect("a consuming tuple pattern may mix owned and Copy bindings");
    let output = crate::run_mir(&module).expect("the mixed tuple pattern should execute");
    assert_eq!(output.stdout, "owned:7\n");

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let rvalues = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign { value, .. } => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(rvalues.iter().any(|value| {
        matches!(
            value,
            Rvalue::TupleTakeElement {
                index: 0,
                element_type,
                ..
            } if element_type == &Type::named("str")
        )
    }));
    assert!(rvalues.iter().any(|value| {
        matches!(
            value,
            Rvalue::TupleElement {
                index: 1,
                element_type,
                ..
            } if element_type == &Type::named("int64")
        )
    }));
}

#[test]
fn tuple_for_targets_project_the_iteration_value_before_the_body() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    rows = [(1, 2), (3, 4)]
    for left, right in rows:
        print(left + right)
"#,
    )
    .expect("tuple for-target should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let body = main
        .blocks
        .iter()
        .find(|block| block.label.contains("for_body"))
        .expect("for body should lower");
    let projection_indices = body
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::TupleElement { index, .. },
                ..
            } => Some(*index),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(projection_indices, vec![0, 1]);
}

#[test]
fn heterogeneous_ordinary_for_bindings_use_distinct_scoped_typed_slots() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    numbers: list[int64] = [1]
    words: list[str] = ["one"]
    for item in numbers:
        print(item + 10)
    for item in words:
        print(item)
"#,
    )
    .expect("heterogeneous ordinary loops should lower");
    let output = crate::run_mir(&module).expect("heterogeneous ordinary loops should execute");
    assert_eq!(output.stdout, "11\none\n");

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let local_types = main
        .local_types
        .iter()
        .map(|local| (local.name.as_str(), &local.ty))
        .collect::<BTreeMap<_, _>>();
    let bindings = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::VariantPayload {
                        variant_name,
                        index: 0,
                        ..
                    },
            } if variant_name == "Some" => {
                Some((target.as_str(), local_types.get(target.as_str()).copied()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0].1, Some(&Type::named("int64")));
    assert_eq!(bindings[1].1, Some(&Type::named("str")));
    assert!(bindings.iter().all(|(name, _)| name.starts_with("%t")));
    assert_ne!(bindings[0].0, bindings[1].0);
    assert!(!local_types.contains_key("item"));
}

#[test]
fn every_ordinary_for_form_uses_a_fresh_scoped_target_slot() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    jobs = Queue[str]()
    jobs.put("queue")
    jobs.close()
    mut labels = set[str]()
    labels.add("set")
    numbers: list[int64] = [1]
    for item in jobs:
        print(item)
    for item in labels:
        print(item)
    for item in numbers:
        print(item)
    for item in range(1):
        print(item)
"#,
    )
    .expect("all ordinary loop forms should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let local_types = main
        .local_types
        .iter()
        .map(|local| (local.name.as_str(), &local.ty))
        .collect::<BTreeMap<_, _>>();
    let mut targets = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::VariantPayload {
                        variant_name,
                        index: 0,
                        ..
                    },
            } if matches!(variant_name.as_str(), "Item" | "Some") => Some(target.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    targets.extend(
        main.blocks
            .iter()
            .filter_map(|block| match &block.terminator {
                Terminator::ForRange { binding, .. } => Some(binding.as_str()),
                _ => None,
            }),
    );

    assert_eq!(targets.len(), 4);
    assert!(targets.iter().all(|target| target.starts_with("%t")));
    assert_eq!(
        targets.iter().copied().collect::<BTreeSet<_>>().len(),
        targets.len()
    );
    let target_types = targets
        .iter()
        .map(|target| {
            local_types
                .get(target)
                .copied()
                .expect("every loop target slot should retain its type")
        })
        .collect::<Vec<_>>();
    let mut rendered_target_types = target_types
        .iter()
        .map(|ty| ty.to_string())
        .collect::<Vec<_>>();
    rendered_target_types.sort();
    assert_eq!(rendered_target_types, vec!["int64", "int64", "str", "str"]);
}

fn safepoint_blocks(function: &MirFunction) -> Vec<&BasicBlock> {
    function
        .blocks
        .iter()
        .filter(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Safepoint))
        })
        .collect()
}

fn terminator_targets(terminator: &Terminator) -> Vec<&str> {
    match terminator {
        Terminator::Goto(label) => vec![label],
        Terminator::Branch {
            then_label,
            else_label,
            ..
        } => vec![then_label, else_label],
        Terminator::ForRange {
            body_label,
            exit_label,
            ..
        } => vec![body_label, exit_label],
        Terminator::Match {
            arms, otherwise, ..
        } => arms
            .iter()
            .map(|arm| arm.label.as_str())
            .chain(std::iter::once(otherwise.as_str()))
            .collect(),
        Terminator::Return(_) | Terminator::AssertFail { .. } | Terminator::Unreachable => {
            Vec::new()
        }
    }
}

#[test]
fn while_backedge_paths_converge_at_one_safepoint_and_break_bypasses_it() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    mut count = 0
    while count < 10:
        count += 1
        if count == 1:
            continue
        if count == 2:
            break
        print(count)
"#,
    )
    .expect("while loop should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    let safepoints = safepoint_blocks(main);
    assert_eq!(
        safepoints.len(),
        1,
        "one semantic while loop must have exactly one safepoint latch"
    );
    let safepoint = safepoints[0];
    assert!(safepoint.label.contains("while_safepoint"));
    assert_eq!(
        safepoint
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, Instruction::Safepoint))
            .count(),
        1
    );
    let condition_label = match &safepoint.terminator {
        Terminator::Goto(label) => label,
        other => panic!("while safepoint must return to its condition, got {other:?}"),
    };
    let condition = main
        .blocks
        .iter()
        .find(|block| &block.label == condition_label)
        .expect("safepoint target should be the while condition");
    let after_label = match &condition.terminator {
        Terminator::Branch { else_label, .. } => else_label,
        other => panic!("while condition must branch, got {other:?}"),
    };

    let safepoint_predecessors = main
        .blocks
        .iter()
        .filter(|block| {
            terminator_targets(&block.terminator)
                .iter()
                .any(|target| *target == safepoint.label)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        safepoint_predecessors.len(),
        2,
        "normal fallthrough and continue must converge at the same latch"
    );

    let after_predecessors = main
        .blocks
        .iter()
        .filter(|block| {
            terminator_targets(&block.terminator)
                .iter()
                .any(|target| *target == after_label)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        after_predecessors.len(),
        2,
        "the condition's false edge and break should both exit the loop"
    );
    assert!(after_predecessors
        .iter()
        .any(|block| block.label == condition.label));
    let break_predecessor = after_predecessors
        .iter()
        .find(|block| block.label != condition.label)
        .expect("break should add an exit predecessor");
    assert!(matches!(
        &break_predecessor.terminator,
        Terminator::Goto(label) if label == after_label
    ));
    assert!(
        !break_predecessor
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Safepoint)),
        "break must bypass the loop safepoint"
    );
}

#[test]
fn nested_while_loops_have_distinct_single_safepoint_latches() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    mut outer = 0
    while outer < 2:
        mut inner = 0
        while inner < 2:
            inner += 1
        outer += 1
"#,
    )
    .expect("nested while loops should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    let safepoints = safepoint_blocks(main);
    assert_eq!(
        safepoints.len(),
        2,
        "each nested semantic loop needs its own latch"
    );
    let condition_targets = safepoints
        .iter()
        .map(|block| match &block.terminator {
            Terminator::Goto(label) => label.as_str(),
            other => panic!("while safepoint must return to its condition, got {other:?}"),
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        condition_targets.len(),
        2,
        "nested loop latches must target distinct conditions"
    );
    assert!(condition_targets
        .iter()
        .all(|label| label.contains("while_cond")));
}

#[test]
fn every_for_loop_shape_has_exactly_one_safepoint_latch() {
    let module = crate::lower_source_to_mir(
        r#"
def range_loop():
    for value in range(0, 2):
        print(value)

def vec_loop(values: list[int64]):
    for value in values:
        print(value)

def set_loop(values: set[int64]):
    for value in values:
        print(value)

def queue_loop(values: Queue[int64]):
    for value in values:
        print(value)

def enumerate_loop(values: list[int64]):
    for index, value in enumerate(values):
        print(index + value)

def zip_loop(left: list[int64], right: list[int64]):
    for first, second in zip(left, right):
        print(first + second)
"#,
    )
    .expect("all supported for-loop shapes should lower");

    for (function_name, dispatch_fragment) in [
        ("range_loop", "for_iter"),
        ("vec_loop", "for_iter"),
        ("set_loop", "for_iter"),
        ("queue_loop", "for_iter"),
        ("enumerate_loop", "for_lockstep"),
        ("zip_loop", "for_lockstep"),
    ] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .unwrap_or_else(|| panic!("{function_name} should lower"));
        let safepoints = safepoint_blocks(function);
        assert_eq!(
            safepoints.len(),
            1,
            "{function_name} should have exactly one safepoint latch"
        );
        assert_eq!(
            safepoints[0]
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, Instruction::Safepoint))
                .count(),
            1
        );
        assert!(
            matches!(
                &safepoints[0].terminator,
                Terminator::Goto(label) if label.contains(dispatch_fragment)
            ),
            "{function_name}'s latch must return to its iteration dispatch"
        );
    }
}

#[test]
fn queue_iteration_receiver_metadata_tracks_the_materialized_handle() {
    let module = crate::lower_source_to_mir(
        r#"
def drain(jobs: Queue[int64]):
    for job in jobs:
        print(job)
"#,
    )
    .expect("queue iteration should lower");

    validate_loan_flow(&module)
        .expect("source-produced queue iteration must satisfy public MIR validation");
    let drain = module
        .functions
        .iter()
        .find(|function| function.name == "drain")
        .expect("drain should lower");
    let (object, receiver_place) = drain
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                        ..
                    },
                ..
            } if field == INTERNAL_QUEUE_GET_WITH_REGISTERED_PRODUCERS_FIELD => {
                Some((object, receiver_place.as_deref()))
            }
            _ => None,
        })
        .expect("queue iteration should use the internal receive member");
    let Operand::Place(object_place) = object else {
        panic!("queue iteration should call through a materialized place operand")
    };
    assert!(
        object_place.starts_with("%t"),
        "Copy queue handles should be materialized at the loop sequence point"
    );
    assert_eq!(receiver_place, Some(object_place.as_str()));
}

#[test]
fn projected_copy_member_receiver_metadata_tracks_the_materialized_value() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    values = [1]
    print(values[0].to_string())
"#,
    )
    .expect("projected Copy receiver should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (object, receiver_place) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                        ..
                    },
                ..
            } if field == "to_string" => Some((object, receiver_place.as_deref())),
            _ => None,
        })
        .expect("projected scalar should lower through to_string");
    let Operand::Place(object_place) = object else {
        panic!("projected Copy receiver should be materialized into a place operand")
    };
    assert!(object_place.starts_with("%t"));
    assert_eq!(receiver_place, Some(object_place.as_str()));
    validate_loan_flow(&module)
        .expect("source-produced projected Copy member call must satisfy MIR validation");
}

#[test]
fn module_constant_index_receiver_metadata_tracks_the_materialized_collection() {
    let module = crate::lower_source_to_mir(
        r#"
labels: dict[int32, int32] = {3: 30}

def main():
    print(labels[3])
"#,
    )
    .expect("module constant index receiver should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (object, receiver_place) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                        ..
                    },
                ..
            } if field == INTERNAL_MAP_INDEX_FIELD => Some((object, receiver_place.as_deref())),
            _ => None,
        })
        .expect("module constant dictionary should lower through indexed access");
    let Operand::Place(object_place) = object else {
        panic!("module constant collection should be materialized into a place operand")
    };
    assert!(object_place.starts_with("%t"));
    assert_eq!(receiver_place, Some(object_place.as_str()));
    validate_loan_flow(&module)
        .expect("source-produced module constant index call must satisfy MIR validation");
}

#[test]
fn module_constant_comprehension_receiver_metadata_tracks_the_materialized_collection() {
    let module = crate::lower_source_to_mir(
        r#"
values: list[int32] = [1, 2, 3]
squares: list[int32] = [value * value for value in values]

def main():
    print(squares)
"#,
    )
    .expect("module constant comprehension should lower");
    let initializer = module
        .functions
        .iter()
        .find(|function| function.name.ends_with("::squares"))
        .expect("squares initializer should lower");
    let (object, receiver_place) = initializer
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                        ..
                    },
                ..
            } if field == INTERNAL_VEC_INDEX_OPTION_FIELD => {
                Some((object, receiver_place.as_deref()))
            }
            _ => None,
        })
        .expect("comprehension should lower through optional indexed access");
    let Operand::Place(object_place) = object else {
        panic!("module constant collection should be materialized into a place operand")
    };
    assert!(object_place.starts_with("%t"));
    assert_eq!(receiver_place, Some(object_place.as_str()));
    validate_loan_flow(&module)
        .expect("source-produced module constant comprehension must satisfy MIR validation");
}

#[test]
fn owned_operator_receiver_metadata_tracks_the_snapshot_operand() {
    let module = crate::lower_source_to_mir(
        r#"
trait Add[Rhs, Out]:
    def add(own self, rhs: own Rhs) -> Out

copy class Counter:
    value: int32

impl Add[Counter, Counter] for Counter:
    def add(own self, rhs: own Counter) -> Counter:
        return Counter(value=self.value + rhs.value)

def main():
    counter = Counter(value=1)
    print((counter + counter).value)
"#,
    )
    .expect("owned operator receiver should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let (object, receiver_place) = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::TraitMember {
                                object,
                                trait_name,
                                field,
                                receiver_place,
                            },
                        ..
                    },
                ..
            } if trait_name == "Add" && field == "add" => Some((object, receiver_place.as_deref())),
            _ => None,
        })
        .expect("operator should lower through its trait member");
    let Operand::Place(object_place) = object else {
        panic!("Copy value receiver should be snapshotted into a place operand")
    };
    assert!(object_place.starts_with("%t"));
    assert_eq!(receiver_place, Some(object_place.as_str()));
    validate_loan_flow(&module)
        .expect("source-produced owned operator call must satisfy MIR validation");
}

#[test]
fn comprehensions_lower_to_owned_collection_literals_nested_loops_and_insertion_calls() {
    let module = crate::lower_source_to_mir(
        r#"
def build(
    numbers: list[int64],
    other: list[int64]
) -> dict[int64, int64]:
    list_values: list[int64] = [
        left + right
        for left in numbers
        for right in other
        if left != right
    ]
    unique: set[int64] = {value for value in list_values}
    return {value: value * 2 for value in unique}
"#,
    )
    .expect("checked comprehensions should lower through ordinary collection and loop MIR");
    let build = module
        .functions
        .iter()
        .find(|function| function.name == "build")
        .expect("build should lower");

    let mut vec_literals = 0;
    let mut set_literals = 0;
    let mut map_literals = 0;
    let mut append_calls = 0;
    let mut add_calls = 0;
    let mut map_set_calls = 0;
    let mut safepoints = 0;
    for block in &build.blocks {
        for instruction in &block.instructions {
            match instruction {
                Instruction::Safepoint => safepoints += 1,
                Instruction::Assign { value, .. } => match value {
                    Rvalue::VecLiteral { .. } => vec_literals += 1,
                    Rvalue::SetLiteral { .. } => set_literals += 1,
                    Rvalue::MapLiteral { .. } => map_literals += 1,
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    } if field == "append" => append_calls += 1,
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    } if field == "add" => add_calls += 1,
                    Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    } if field == INTERNAL_MAP_SET_INDEX_FIELD => map_set_calls += 1,
                    _ => {}
                },
                _ => {}
            }
        }
    }

    assert_eq!(vec_literals, 1, "list comprehension allocates one result");
    assert_eq!(set_literals, 1, "set comprehension allocates one result");
    assert_eq!(map_literals, 1, "map comprehension allocates one result");
    assert_eq!(append_calls, 1, "list output appends once at its leaf");
    assert_eq!(add_calls, 1, "set output adds once at its leaf");
    assert_eq!(map_set_calls, 1, "map output assigns once at its leaf");
    assert_eq!(
        safepoints, 4,
        "two nested list clauses plus one set and one map clause retain loop safepoints"
    );
}

#[test]
fn comprehensions_preserve_checked_iterator_ownership_and_generated_lambda_context() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    ranged: list[int64] = [value * 3 for value in range(1, 4)]

    tags: set[str] = {"set-owned"}
    copied_tags: list[str] = [tag.clone() for tag in tags]

    pairs = Queue[(str, str)]()
    pairs.put(("queue", "-tuple"))
    pairs.close()
    joined: list[str] = [left + right for left, right in pairs]

    indexed: list[int64] = [
        index * 10 + value
        for index, value in enumerate([4, 5])
    ]
    zipped: list[int64] = [
        left + right
        for left, right in zip([1, 2, 3], [10, 20])
    ]

    offset: int64 = 10
    build: def() -> list[int64] = lambda: [
        value + offset
        for value in [1, 2]
    ]

    print(ranged)
    print(copied_tags)
    print(joined)
    print(indexed)
    print(zipped)
    print(build())
"#,
    )
    .expect("every comprehension iterator and a generated closure body should lower");

    let output =
        crate::run_mir(&module).expect("checked comprehension iterator ownership should execute");
    assert_eq!(
        output.stdout,
        "[3, 6, 9]\n[set-owned]\n[queue-tuple]\n[4, 15]\n[11, 22]\n[11, 12]\n"
    );

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let tuple_takes = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::TupleTakeElement { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        tuple_takes, 2,
        "Queue tuple bindings must transfer both received str elements"
    );

    let tuple_reads = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::TupleElement { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        tuple_reads, 4,
        "enumerate and zip bindings must project their shared or Copy elements"
    );

    let lifted = module
        .functions
        .iter()
        .find(|function| function.name.starts_with("main::__lambda_"))
        .expect("the capturing builder closure should lower");
    assert!(
        lifted.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::VecLiteral { element_type, .. },
                        ..
                    } if *element_type == Type::named("int64")
                )
            })
        }),
        "the generated closure must resolve its owning function's comprehension metadata"
    );
}

#[test]
fn mutable_vec_writeback_precedes_its_single_safepoint_latch() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    mut values: list[int64] = [1, 2]
    for value in mut values:
        value += 1
"#,
    )
    .expect("mutable Vec loop should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    let safepoints = safepoint_blocks(main);
    assert_eq!(
        safepoints.len(),
        1,
        "mutable Vec iteration should have exactly one latch"
    );
    let latch = safepoints[0];
    let writeback_index = latch
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                    ..
                } if field == INTERNAL_VEC_SET_INDEX_FIELD
            )
        })
        .expect("mutable Vec latch must write the element back");
    let safepoint_index = latch
        .instructions
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Safepoint))
        .expect("mutable Vec latch must include a safepoint");
    assert!(
        writeback_index < safepoint_index,
        "mutable element writeback must complete before scheduling can yield"
    );
    assert!(matches!(
        &latch.terminator,
        Terminator::Goto(label) if label.contains("for_iter")
    ));
}

#[test]
fn ordinary_for_target_scope_starts_after_iterable_evaluation() {
    let program = Box::leak(Box::new(checked_program("def main():\n    pass\n")));
    let mut lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::Unit,
        BTreeMap::new(),
    );
    lowerer.local_types.insert(
        "values".to_string(),
        Type::Named("list".to_string(), vec![Type::named("int64")]),
    );
    lowerer.lower_for(&ForStmt {
        target: BindingTarget::Name {
            name: "values".to_string(),
            span: Span::new(1, 1),
        },
        iterable: name_expr("values"),
        borrow_mode: None,
        body: vec![Stmt::Pass(PassStmt {
            span: Span::new(1, 1),
        })],
        span: Span::new(1, 1),
    });

    let iteration_receiver = lowerer
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                        ..
                    },
                ..
            } if field == INTERNAL_VEC_INDEX_OPTION_FIELD => {
                Some((object, receiver_place.as_deref()))
            }
            _ => None,
        })
        .expect("Vec iteration should lower through the indexed option helper");
    assert_eq!(
        iteration_receiver,
        (&Operand::Place("values".to_string()), Some("values"))
    );

    let binding = lowerer
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::VariantPayload {
                        variant_name,
                        index: 0,
                        ..
                    },
            } if variant_name == "Some" => Some(target),
            _ => None,
        })
        .expect("Vec iteration should extract the current element");
    assert!(binding.starts_with("%t"));
    assert_ne!(binding, "values");
}

#[test]
fn owned_slice_lowering_preserves_endpoint_presence_and_source_order() {
    let program = Box::leak(Box::new(checked_program("def main():\n    pass\n")));
    let mut lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::Unit,
        BTreeMap::new(),
    );
    lowerer.local_types.insert(
        "values".to_string(),
        Type::Named("list".to_string(), vec![Type::named("int32")]),
    );
    lowerer
        .local_types
        .insert("start".to_string(), Type::named("int32"));
    lowerer
        .local_types
        .insert("end".to_string(), Type::named("int32"));

    let slice = Expr {
        kind: ExprKind::Slice {
            object: Box::new(name_expr("values")),
            start: Some(Box::new(name_expr("start"))),
            end: Some(Box::new(name_expr("end"))),
            colon_span: Span::new(4, 18),
        },
        span: Span::new(4, 12),
    };
    let lowered = lowerer.lower_expr(&slice);
    assert!(matches!(lowered, Operand::Place(_)));

    let instructions = &lowerer.blocks[lowerer.current_block].instructions;
    let call_index = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value:
                        Rvalue::Call {
                            callee: CallTarget::Member { field, .. },
                            ..
                        },
                    ..
                } if field == INTERNAL_SLICE_FIELD
            )
        })
        .expect("slice lowering should emit the hidden owned-copy member call");
    let (object, receiver_place, args) = match &instructions[call_index] {
        Instruction::Assign {
            value:
                Rvalue::Call {
                    callee:
                        CallTarget::Member {
                            object,
                            receiver_place,
                            ..
                        },
                    args,
                },
            ..
        } => (object, receiver_place, args),
        other => panic!("expected slice member call, found {other:?}"),
    };
    let captured_places = instructions[..call_index]
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value: Rvalue::Use(Operand::Place(source)),
            } if source == "start" || source == "end" => Some((target, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let widened_places = instructions[..call_index]
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::Cast {
                        value: Operand::Place(source),
                        ty,
                        ..
                    },
            } if *ty == Type::named("int64") => Some((target, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(object, &Operand::Place("values".to_string()));
    assert_eq!(receiver_place.as_deref(), Some("values"));
    assert_eq!(
        captured_places
            .iter()
            .map(|(_, source)| source.as_str())
            .collect::<Vec<_>>(),
        vec!["start", "end"],
        "written endpoints must be captured once in source order"
    );
    assert_eq!(args.len(), 6);
    assert_eq!(widened_places.len(), 2);
    assert_eq!(widened_places[0].1, captured_places[0].0);
    assert_eq!(widened_places[1].1, captured_places[1].0);
    assert_eq!(
        args[0].value,
        Operand::Place(widened_places[0].0.to_string())
    );
    assert_eq!(args[1].value, Operand::Bool(true));
    assert_eq!(
        args[2].value,
        Operand::Place(widened_places[1].0.to_string())
    );
    assert_eq!(args[3].value, Operand::Bool(true));
    assert_eq!(args[4].value, Operand::Int(4));
    assert_eq!(args[5].value, Operand::Int(18));
}

#[test]
fn nested_and_copy_tuple_patterns_preserve_binding_ownership() {
    let module = crate::lower_source_to_mir(
        r#"
def nested():
    match own (("left", "right"), "tail"):
        case ((left, right), tail):
            print(left)
            print(right)
            print(tail)

def copied():
    pair = (10, 20)
    match own pair:
        case (left, right):
            print(left + right)
    print(pair[0])

def main():
    nested()
    copied()
"#,
    )
    .expect("nested and Copy tuple patterns should lower");

    let nested = module
        .functions
        .iter()
        .find(|function| function.name == "nested")
        .expect("nested pattern function should lower");
    assert!(
        nested.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::TupleTakeElement { .. },
                        ..
                    }
                )
            })
        }),
        "nested non-Copy bindings must transfer their tuple elements"
    );

    let copied = module
        .functions
        .iter()
        .find(|function| function.name == "copied")
        .expect("Copy pattern function should lower");
    let copied_rvalues = copied
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign { value, .. } => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        copied_rvalues
            .iter()
            .filter(|value| matches!(value, Rvalue::TupleElement { .. }))
            .count()
            >= 3,
        "Copy pattern bindings and the later index must project without consuming the tuple"
    );
    assert!(
        !copied_rvalues
            .iter()
            .any(|value| matches!(value, Rvalue::TupleTakeElement { .. })),
        "matching Copy elements must leave the original tuple readable"
    );
}

#[test]
fn grouped_tuple_index_and_set_destructure_execute_through_mir() {
    let output = crate::run_source(
        r#"
def main():
    pair = (10, 20)
    print(pair[(0)])

    rows: set[(int64, int64)] = {(1, 2)}
    for left, right in rows:
        print(left + right)
"#,
    )
    .expect("grouped tuple indexing and Set tuple targets should run");

    assert_eq!(output.stdout, "10\n3\n");
}

#[test]
fn generic_tuple_type_helpers_preserve_nested_parameters_and_structure() {
    let tuple = Type::Tuple(vec![
        Type::TypeParam("Left".to_string()),
        Type::Named(
            "list".to_string(),
            vec![Type::TypeParam("Right".to_string())],
        ),
    ]);
    let mut collected = BTreeSet::new();
    collect_type_params_from_type(&tuple, &mut collected);
    assert_eq!(
        collected,
        BTreeSet::from(["Left".to_string(), "Right".to_string()]),
        "tuple elements must participate in generic trait-impl specialization"
    );

    let tuple_ref = TypeRef::tuple(
        vec![
            type_ref("int"),
            TypeRef::named("list", vec![type_ref("str")], false, Span::new(1, 1)),
        ],
        false,
        Span::new(1, 1),
    );
    assert_eq!(
        lower_type_ref(&tuple_ref),
        Type::Tuple(vec![
            Type::named("int64"),
            Type::Named("list".to_string(), vec![Type::named("str")]),
        ])
    );
}

#[test]
fn d3_mir_canonicalizes_int_and_defaults_unhinted_integer_values_to_int64() {
    let lowerer = trait_lowerer();

    assert_eq!(lower_type_ref(&type_ref("int")), Type::named("int64"));
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Int(7))),
        Some(Type::named("int64"))
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Unary {
            op: UnaryOp::Neg,
            expr: Box::new(expr(ExprKind::Int(7))),
        })),
        Some(Type::named("int64"))
    );
    assert_eq!(
        lowerer.infer_option_some_call_type(&expr(ExprKind::Int(7))),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("int64")]
        ))
    );
    assert_eq!(
        lowerer.infer_operand_type(&Operand::Int(7)),
        Some(Type::named("int64"))
    );
}

#[test]
fn duration_nanoseconds_lower_to_an_exact_i128_operand() {
    let exact = i128::MAX - 123;
    let mut lowerer = trait_lowerer();

    assert_eq!(
        lowerer.lower_expr(&expr(ExprKind::DurationNanos(exact))),
        Operand::Duration(exact)
    );

    let largest_millisecond_count = i128::MAX as u128 / 1_000_000;
    let expected_nanos = i128::try_from(largest_millisecond_count * 1_000_000)
        .expect("largest millisecond literal should fit signed i128 nanoseconds");
    let module = crate::lower_source_to_mir(&format!(
        "def exact() -> Duration:\n    return {largest_millisecond_count}ms\n\ndef main() -> int32:\n    return 0\n"
    ))
    .expect("largest millisecond literal should lower to MIR");
    let exact = module
        .functions
        .iter()
        .find(|function| function.name == "exact")
        .expect("exact should lower");
    assert!(exact.blocks.iter().any(|block| {
        matches!(block.terminator, Terminator::Return(Operand::Duration(value)) if value == expected_nanos)
    }));
}

#[test]
fn duration_constructors_and_conversions_lower_to_canonical_call_targets() {
    let module = crate::lower_source_to_mir(
        r#"
def milliseconds(value: int64) -> float64:
    duration = Duration.seconds(value)
    return duration.to_ms()

def main() -> int32:
    return 0
"#,
    )
    .expect("Duration constructors and conversion methods should lower to MIR");
    let function = module
        .functions
        .iter()
        .find(|function| function.name == "milliseconds")
        .expect("milliseconds helper should lower");
    assert!(function
        .blocks
        .iter()
        .any(
            |block| block.instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name),
                        ..
                    },
                    ..
                } if name == "Duration.seconds"
            ))
        ));
    assert!(function
        .blocks
        .iter()
        .any(
            |block| block.instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                    ..
                } if field == "to_ms"
            ))
        ));
}

#[test]
fn json_dumps_omitted_indent_materializes_option_none_in_checked_mir() {
    let module = crate::lower_source_to_mir(
        r#"
import json

def render(value: json.Value) -> str:
    return json.dumps(value)
"#,
    )
    .expect("json.dumps should lower with its omitted indent default");
    let render = module
        .functions
        .iter()
        .find(|function| function.name == "render")
        .expect("render should lower");

    assert!(render.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::EnumVariant { enum_name, variant_name, .. },
                    ..
                } if enum_name == "Option" && variant_name == "None"
            )
        })
    }));
    assert!(render.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name),
                        args,
                    },
                    ..
                } if name == "json::dumps" && args.len() == 2
            )
        })
    }));
}

#[test]
fn json_parse_dump_and_accessors_execute_through_the_mir_runtime() {
    let module = crate::lower_source_to_mir(
        r#"
import json

def main():
    match json.parse("{\"z\":1.0,\"f\":1.5,\"items\":[true,null,\"x\"]}"):
        case Result.Ok(value):
            print(json.dumps(value))
            print(json.dumps(value, indent=Option.Some(2)))
            print(json.as_int(json.Value.Int(7)))
            print(json.as_float(json.Value.Int(7)))
        case Result.Err(error):
            print(error)

    match json.parse("1e400"):
        case Result.Ok(value):
            print(value)
        case Result.Err(json.Error.NumberOutOfRange(line, column)):
            print(line)
            print(column)
        case Result.Err(error):
            print(error)
"#,
    )
    .expect("dynamic JSON should lower to MIR");
    let output = crate::run_mir(&module).expect("dynamic JSON should execute through MIR");
    assert_eq!(
        output.stdout,
        "{\"f\":1.5,\"items\":[true,null,\"x\"],\"z\":1}\n{\n  \"f\": 1.5,\n  \"items\": [\n    true,\n    null,\n    \"x\"\n  ],\n  \"z\": 1\n}\nOption.Some(7)\nOption.None\n1\n1\n"
    );
}

#[test]
fn json_named_and_default_arguments_preserve_source_evaluation_order() {
    let module = crate::lower_source_to_mir(
        r#"
import json

def value_arg() -> json.Value:
    print("value")
    return json.Value.Null

def indent_arg() -> Option[int64]:
    print("indent")
    return Option.None

def main():
    indent = Option.Some(2)
    print(json.dumps(indent=indent_arg(), value=value_arg()))
    print(json.dumps(json.Value.Null, indent=indent))
    print(indent)
"#,
    )
    .expect("named JSON arguments should lower");
    let output = crate::run_mir(&module).expect("named JSON arguments should execute");
    assert_eq!(output.stdout, "indent\nvalue\nnull\nnull\nOption.Some(2)\n");
}

#[test]
fn json_owned_accessors_accept_rvalue_temporaries() {
    let module = crate::lower_source_to_mir(
        r#"
import json

def main():
    print(json.into_string(json.Value.String("temporary")))
    print(json.into_array(json.Value.Array([json.Value.Null])))
    print(json.into_object(json.Value.Object({"k": json.Value.Bool(true)})))
"#,
    )
    .expect("owned JSON accessors should accept rvalue temporaries");
    let output = crate::run_mir(&module).expect("owned JSON temporaries should execute");
    assert_eq!(
        output.stdout,
        "Option.Some(temporary)\nOption.Some([json.Value.Null])\nOption.Some({k: json.Value.Bool(true)})\n"
    );
}

#[test]
fn json_owned_accessors_lower_noncopy_places_without_snapshot_clones() {
    let module = crate::lower_source_to_mir(
        r#"
import json

def extract(value: own json.Value) -> Option[str]:
    return json.into_string(value)
"#,
    )
    .expect("owned JSON accessors should lower");
    let extract = module
        .functions
        .iter()
        .find(|function| function.name == "extract")
        .expect("extract should lower");

    let argument = extract
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
            } if name == "json::into_string" => args.first(),
            _ => None,
        })
        .expect("json.into_string call should be present");
    assert_eq!(
        argument.value,
        Operand::MovePlace("value".to_string()),
        "an own non-copy place must reach the consuming adapter as an explicit move"
    );
    assert!(
        extract.blocks.iter().all(|block| {
            block.instructions.iter().all(|instruction| {
                !matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Use(Operand::Place(place)),
                        ..
                    } if place == "value"
                )
            })
        }),
        "MIR must not snapshot-clone an own json.Value before extraction"
    );
}

#[test]
fn noncopy_value_flow_lowers_to_explicit_moves_while_copy_payloads_stay_reads() {
    let module = crate::lower_source_to_mir(
        r#"
import json

class Holder:
    value: json.Value

def relay(value: own json.Value) -> json.Value:
    assigned = value
    return assigned

def main():
    text = "payload"
    encoded = json.Value.String(text)
    holder = Holder(encoded)
    relayed = relay(holder.value)
    values = [relayed]
    timeout = 2s
    wrapped = Option.Some(timeout)
    print(timeout)
    print(wrapped)
    print(values)
"#,
    )
    .expect("owned and copy value flow should lower");

    let relay = module
        .functions
        .iter()
        .find(|function| function.name == "relay")
        .expect("relay should lower");
    assert!(relay.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    target,
                    value: Rvalue::Use(Operand::MovePlace(place)),
                } if target == "assigned" && place == "value"
            )
        })
    }));
    assert!(relay.blocks.iter().any(|block| {
        matches!(
            &block.terminator,
            Terminator::Return(Operand::MovePlace(place)) if place == "assigned"
        )
    }));

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let rvalues = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign { value, .. } => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(rvalues.iter().any(|rvalue| {
        matches!(
            rvalue,
            Rvalue::EnumVariant {
                enum_name,
                variant_name,
                payloads,
            } if enum_name == "json.Value"
                && variant_name == "String"
                && payloads == &vec![Operand::MovePlace("text".to_string())]
        )
    }));
    assert!(rvalues.iter().any(|rvalue| {
        matches!(
            rvalue,
            Rvalue::Construct { class_name, fields }
                if class_name == "Holder"
                    && fields.iter().any(|field| {
                        field.name == "value"
                            && field.value == Operand::MovePlace("encoded".to_string())
                    })
        )
    }));
    assert!(rvalues.iter().any(|rvalue| {
        matches!(
            rvalue,
            Rvalue::Call {
                callee: CallTarget::Name(name),
                args,
            } if name == "relay"
                && args.first().is_some_and(|argument| {
                    argument.value == Operand::MovePlace("holder.value".to_string())
                })
        )
    }));
    assert!(rvalues.iter().any(|rvalue| {
        matches!(
            rvalue,
            Rvalue::VecLiteral { elements, .. }
                if elements == &vec![Operand::MovePlace("relayed".to_string())]
        )
    }));
    assert!(rvalues.iter().any(|rvalue| {
        matches!(
            rvalue,
            Rvalue::EnumVariant {
                enum_name,
                variant_name,
                payloads,
            } if enum_name == "Option"
                && variant_name == "Some"
                && payloads
                    .first()
                    .is_some_and(|payload| matches!(payload, Operand::Place(_)))
                && payloads
                    .iter()
                    .all(|payload| !matches!(payload, Operand::MovePlace(_)))
        )
    }));
}

#[test]
fn consuming_match_uses_a_private_owner_and_destructive_payload_operands() {
    let module = crate::lower_source_to_mir(
        r#"
enum Packet:
    Text(str)

def unwrap(packet: own Packet) -> str:
    match own packet:
        case Packet.Text(text):
            return text
"#,
    )
    .expect("consuming match should lower");
    let unwrap = module
        .functions
        .iter()
        .find(|function| function.name == "unwrap")
        .expect("unwrap should lower");

    assert!(unwrap.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Use(Operand::MovePlace(place)),
                    ..
                } if place == "packet"
            )
        })
    }));
    assert!(unwrap.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::VariantPayload {
                        scrutinee: Operand::MovePlace(_),
                        index: 0,
                        ..
                    },
                    ..
                }
            )
        })
    }));
}

#[test]
fn guarded_consuming_or_pattern_commits_with_destructive_payload_operands() {
    let module = crate::lower_source_to_mir(
        r#"
enum Packet:
    Text(str)
    Bytes(str)

def unwrap(packet: own Packet) -> str:
    match own packet:
        case Packet.Text(text) | Packet.Bytes(text) if len(text) > 0:
            return text
        case _:
            return ""
"#,
    )
    .expect("guarded consuming or-pattern should lower");
    let unwrap = module
        .functions
        .iter()
        .find(|function| function.name == "unwrap")
        .expect("unwrap should lower");

    assert!(unwrap.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::VariantPayload {
                        scrutinee: Operand::MovePlace(_),
                        index: 0,
                        ..
                    },
                    ..
                }
            )
        })
    }));
}

#[test]
fn own_user_and_trait_receivers_lower_as_explicit_moves() {
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

def main():
    direct = DirectBox("direct")
    print(direct.take())
    trait_value = TraitBox("trait")
    print(trait_value.take())
"#,
    )
    .expect("own receiver calls should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let moved_receivers = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                object: Operand::MovePlace(place),
                                field,
                                ..
                            },
                        ..
                    },
                ..
            } if field == "take" => Some(place.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(moved_receivers, vec!["direct", "trait_value"]);
}

#[test]
fn queue_and_owned_collection_iteration_lower_destructive_yields() {
    let module = crate::lower_source_to_mir(
        r#"
def consume_vector(values: own list[str]):
    for value in own values:
        print(value)

def consume_set(values: own set[str]):
    for value in own values:
        print(value)

def consume_queue(values: Queue[str]):
    for value in values:
        print(value)
"#,
    )
    .expect("owned collection and queue iteration should lower");

    for function_name in ["consume_vector", "consume_set"] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .unwrap_or_else(|| panic!("{function_name} should lower"));
        assert!(
            function.blocks.iter().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction,
                        Instruction::Assign {
                            value:
                                Rvalue::Call {
                                    callee:
                                        CallTarget::Member {
                                            object: Operand::MovePlace(_),
                                            field,
                                            receiver_place: Some(_),
                                        },
                                    ..
                                },
                            ..
                        } if field == "__take_index_option"
                    )
                })
            }),
            "{function_name} must destructively take from its private collection owner"
        );
        assert!(function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::VariantPayload {
                            scrutinee: Operand::MovePlace(_),
                            index: 0,
                            ..
                        },
                        ..
                    }
                )
            })
        }));
    }

    let queue = module
        .functions
        .iter()
        .find(|function| function.name == "consume_queue")
        .expect("consume_queue should lower");
    assert!(queue.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::VariantPayload {
                        scrutinee: Operand::MovePlace(_),
                        index: 0,
                        ..
                    },
                    ..
                }
            )
        })
    }));
}

#[test]
fn task_group_captures_use_owned_operands_for_bare_and_own_target_params() {
    let module = crate::lower_source_to_mir(
        r#"
def shared_worker(value: str):
    print(value)

def own_worker(value: own str):
    print(value)

def main():
    shared_value = "shared-capture"
    own_value = "own-capture"
    with TaskGroup() as group:
        group.start_soon(shared_worker, shared_value)
        group.start_soon(own_worker, own_value)
"#,
    )
    .expect("TaskGroup captures should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let captures = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::StartTask { args, .. },
                ..
            } => Some(args),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    assert_eq!(
        captures[0][0].value,
        Operand::MovePlace("shared_value".to_string())
    );
    assert_eq!(
        captures[1][0].value,
        Operand::MovePlace("own_value".to_string())
    );
    assert!(captures.iter().all(|args| args.len() == 1
        && args[0].name.is_none()
        && args[0].writeback_place.is_none()));
}

#[test]
fn task_group_start_records_copyability_of_the_result_type() {
    let module = crate::lower_source_to_mir(
        r#"
def duration_worker() -> Duration:
    return Duration.ms(1)

def queue_worker() -> Queue[int32]:
    return Queue[int32]()

def string_worker() -> str:
    return "value"

def vector_worker() -> list[int32]:
    return [1]

def main():
    with TaskGroup() as group:
        group.start(duration_worker)
        group.start(queue_worker)
        group.start(string_worker)
        group.start(vector_worker)
"#,
    )
    .expect("task result copyability should lower");
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
                value:
                    Rvalue::StartTask {
                        function,
                        result_is_copy,
                        ..
                    },
                ..
            } => Some((operand_function_name(function)?, *result_is_copy)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        starts,
        vec![
            ("duration_worker", true),
            ("queue_worker", true),
            ("string_worker", false),
            ("vector_worker", false),
        ]
    );
}

#[test]
fn task_group_generic_starts_preserve_specialized_result_types_and_repeatability() {
    let module = crate::lower_source_to_mir(
        r#"
def relay[T](value: own T) -> T:
    return value

def defaulted[T](value: own Option[T] = None) -> Option[T]:
    return value

class Factory[T]:
    def relay(value: own T) -> T:
        return value

def int_worker() -> int64:
    return 1

def string_worker() -> str:
    return "value"

def main():
    with TaskGroup() as group:
        explicit_int = group.start(relay[int64], 1)
        group.start_soon(relay[str], "value")
        default_int = group.start_with_stack(262144, defaulted[int64])
        group.start_soon_with_stack(262144, relay[Queue[int64]], Queue[int64]())
        static_int = group.start(Factory[int64].relay, 2)
        int_task = group.start(int_worker)
        nested_int = group.start(relay[Task[int64]], int_task)
        string_task = group.start(string_worker)
        nested_string = group.start(relay[Task[str]], string_task)
"#,
    )
    .expect("generic task starts should lower after semantic specialization");
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
                target,
                value:
                    Rvalue::StartTask {
                        returns_handle,
                        result_is_copy,
                        stack_size,
                        function,
                        ..
                    },
            } => Some((
                target.as_str(),
                operand_function_name(function)?,
                *returns_handle,
                *result_is_copy,
                stack_size.is_some(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(starts.len(), 9);
    assert_eq!(
        starts
            .iter()
            .map(|(_, _, returns_handle, result_is_copy, has_stack)| (
                *returns_handle,
                *result_is_copy,
                *has_stack,
            ))
            .collect::<Vec<_>>(),
        vec![
            (true, true, false),
            (false, false, false),
            (true, true, true),
            (false, true, true),
            (true, true, false),
            (true, true, false),
            (true, true, false),
            (true, false, false),
            (true, false, false),
        ]
    );
    assert!(
        starts
            .iter()
            .any(|(_, function, _, _, _)| function == &"Factory.relay"),
        "associated generic task targets should retain their direct symbol"
    );

    let named_types = main
        .local_types
        .iter()
        .filter(|local| {
            matches!(
                local.name.as_str(),
                "explicit_int" | "default_int" | "static_int" | "nested_int" | "nested_string"
            )
        })
        .map(|local| (local.name.as_str(), local.ty.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        named_types["explicit_int"],
        Type::Named("Task".to_string(), vec![Type::named("int64")])
    );
    assert_eq!(
        named_types["default_int"],
        Type::Named(
            "Task".to_string(),
            vec![Type::Named(
                "Option".to_string(),
                vec![Type::named("int64")]
            )]
        )
    );
    assert_eq!(
        named_types["static_int"],
        Type::Named("Task".to_string(), vec![Type::named("int64")])
    );
    assert_eq!(
        named_types["nested_int"],
        Type::Named(
            "Task".to_string(),
            vec![Type::Named("Task".to_string(), vec![Type::named("int64")])]
        )
    );
    assert_eq!(
        named_types["nested_string"],
        Type::Named(
            "Task".to_string(),
            vec![Type::Named("Task".to_string(), vec![Type::named("str")])]
        )
    );
}

#[test]
fn task_target_specialization_preserves_tuple_qualified_and_inferred_types() {
    let module = crate::lower_source_to_mir(
        r#"
import io

def empty[T]() -> Option[T]:
    return Option.None

def pair[A, B](first: own A, second: own B) -> (A, B):
    return (first, second)

def main():
    explicit_label: str = "explicit"
    inferred_label: str = "inferred"
    with TaskGroup() as group:
        tuple_task = group.start(empty[((str, int32),)])
        qualified_task = group.start(empty[io.Error])
        explicit_pair = group.start(pair[str, int64], explicit_label, 2)
        inferred_pair = group.start(pair, inferred_label, 3)
"#,
    )
    .expect("task targets should preserve every supported specialization form");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let named_types = main
        .local_types
        .iter()
        .filter(|local| {
            matches!(
                local.name.as_str(),
                "tuple_task" | "qualified_task" | "explicit_pair" | "inferred_pair"
            )
        })
        .map(|local| (local.name.as_str(), local.ty.clone()))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        named_types["tuple_task"],
        Type::Named(
            "Task".to_string(),
            vec![Type::Named(
                "Option".to_string(),
                vec![Type::Tuple(vec![Type::named("str"), Type::named("int32")])]
            )]
        )
    );
    assert_eq!(
        named_types["qualified_task"],
        Type::Named(
            "Task".to_string(),
            vec![Type::Named(
                "Option".to_string(),
                vec![Type::named("io.Error")]
            )]
        )
    );
    let expected_pair = Type::Named(
        "Task".to_string(),
        vec![Type::Tuple(vec![Type::named("str"), Type::named("int64")])],
    );
    assert_eq!(named_types["explicit_pair"], expected_pair);
    assert_eq!(named_types["inferred_pair"], expected_pair);

    let starts = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::StartTask {
                        result_is_copy,
                        function,
                        args,
                        ..
                    },
                ..
            } => Some((operand_function_name(function)?, *result_is_copy, args)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 4);
    assert_eq!(
        starts
            .iter()
            .map(|(_, result_is_copy, _)| *result_is_copy)
            .collect::<Vec<_>>(),
        vec![false, false, false, false]
    );
    assert_eq!(
        starts[3].2[0].value,
        Operand::MovePlace("inferred_label".to_string()),
        "inferred str capture must use the specialized non-copy parameter type"
    );
    assert!(
        matches!(starts[3].2[1].value, Operand::Place(_)),
        "inferred int64 capture must be materialized in its contextual type"
    );
}

#[test]
fn task_target_context_does_not_reinterpret_ordinary_or_invalid_indices() {
    let lowerer = lowerer_with_imported_modules();
    let indexed_value = expr(ExprKind::Index {
        object: Box::new(name_expr("values")),
        index: Box::new(expr(ExprKind::Int(0))),
    });
    let (base, type_args) = lowerer.task_callable_specialization(&indexed_value);
    assert!(
        std::ptr::eq(base, &indexed_value),
        "an ordinary value index must remain the original callable expression"
    );
    assert!(type_args.is_none());
    assert!(lowerer.resolve_task_start_target(&indexed_value).is_none());

    let invalid = crate::lower_source_to_mir(
        r#"
def main():
    values = [1]
    with TaskGroup() as group:
        group.start(values[0])
"#,
    )
    .expect_err("an indexed value is not a statically resolved task target");
    assert!(
        invalid
            .message
            .contains("task target indexing is not a callable type specialization"),
        "{invalid:?}"
    );
}

#[test]
fn task_target_inference_rejects_conflicting_argument_types_before_lowering() {
    let invalid = crate::lower_source_to_mir(
        r#"
def same[T](left: own T, right: own T) -> T:
    return left

def main():
    label: str = "value"
    with TaskGroup() as group:
        group.start(same, label, 1)
"#,
    )
    .expect_err("one generic target parameter cannot infer two concrete types");
    assert!(
        invalid
            .message
            .contains("conflicting inferred types for `T`: `str` and `int64`"),
        "{invalid:?}"
    );
}

#[test]
fn task_observations_and_waits_move_only_nonrepeatable_rights() {
    let module = crate::lower_source_to_mir(
        r#"
def int_worker() -> int64:
    return 1

def string_worker() -> str:
    return "value"

def queue_worker() -> Queue[int64]:
    return Queue[int64]()

def main():
    with TaskGroup() as group:
        int_result_task = group.start(int_worker)
        int_result = int_result_task.result()
        queue_result_task = group.start(queue_worker)
        queue_result = queue_result_task.result_or_none()
        string_result_task = group.start(string_worker)
        string_result = string_result_task.result_or("")

        int_tasks = [group.start(int_worker)]
        any_int = wait_any(int_tasks)
        string_tasks = [group.start(string_worker)]
        all_strings = wait_all(string_tasks)
"#,
    )
    .expect("task observation ownership should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    let member_receivers = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Member { object, field, .. },
                        ..
                    },
                ..
            } if matches!(field.as_str(), "result" | "result_or_none" | "result_or") => {
                Some((field.as_str(), object))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(member_receivers.len(), 3);
    assert!(matches!(member_receivers[0], ("result", Operand::Place(_))));
    assert!(matches!(
        member_receivers[1],
        ("result_or_none", Operand::Place(_))
    ));
    assert_eq!(
        member_receivers[2],
        (
            "result_or",
            &Operand::MovePlace("string_result_task".to_string())
        )
    );

    let wait_operands = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Name(name),
                        args,
                    },
                ..
            } if matches!(name.as_str(), "wait_any" | "wait_all") => {
                Some((name.as_str(), &args[0].value))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        wait_operands,
        vec![
            ("wait_any", &Operand::Place("int_tasks".to_string())),
            ("wait_all", &Operand::MovePlace("string_tasks".to_string())),
        ]
    );
}

#[test]
fn typed_select_lowers_sources_in_order_and_moves_only_nonrepeatable_task_rights() {
    let module = crate::lower_source_to_mir(
        r#"
def string_worker() -> str:
    return "value"

def main():
    queue = Queue[int64]()
    with TaskGroup() as group:
        single_consumer = group.start(string_worker)
        outcome = select(queue, 1ms, single_consumer)
"#,
    )
    .expect("typed select sources should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let select_args = main
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
            } if name == "select" => Some(args),
            _ => None,
        })
        .expect("select should lower as one variadic MIR call");

    assert_eq!(select_args.len(), 3);
    assert!(select_args.iter().all(|argument| argument.name.is_none()));
    assert!(matches!(select_args[0].value, Operand::Place(_)));
    assert!(matches!(select_args[1].value, Operand::Duration(1_000_000)));
    assert_eq!(
        select_args[2].value,
        Operand::MovePlace("single_consumer".to_string())
    );
}

#[test]
fn typed_select_queue_and_deadline_outcomes_execute_through_mir() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    first = Queue[str]()
    second = Queue[str]()
    first.put("first")
    second.put("second")
    print(select(first, second))
    print(second.get())

    closed = Queue[str]()
    closed.close()
    print(select(closed))
    print(select(0ms, 0ms))
"#,
    )
    .expect("typed select outcomes should lower");
    let output = crate::run_mir(&module).expect("typed select outcomes should execute through MIR");
    assert_eq!(
        output.stdout,
        concat!(
            "SelectOutcome.Queue(0, QueueReceive.Item(first))\n",
            "QueueReceive.Item(second)\n",
            "SelectOutcome.Queue(0, QueueReceive.Closed)\n",
            "SelectOutcome.Deadline(0)\n",
        )
    );
}

#[test]
fn task_group_stack_override_lowers_stack_operand_and_named_target_arguments() {
    let module = crate::lower_source_to_mir(
        r#"
def choose_stack() -> int64:
    return 262144

def worker(left: int32, right: int32) -> int32:
    return left + right

def main() -> int32:
    with TaskGroup() as group:
        task = group.start_with_stack(
            choose_stack(),
            worker,
            right=2,
            left=1
        )
        return task.result_or(-1)
"#,
    )
    .expect("task stack override should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let start = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::StartTask {
                        stack_size, args, ..
                    },
                ..
            } => Some((stack_size, args)),
            _ => None,
        })
        .expect("task start should be explicit MIR");
    assert!(
        matches!(start.0, Some(Operand::Place(place)) if place.starts_with('%')),
        "dynamic stack size should be evaluated once into a sequence-point temporary: {:?}",
        start.0
    );
    assert_eq!(
        start
            .1
            .iter()
            .map(|argument| (argument.name.as_deref(), &argument.value))
            .collect::<Vec<_>>(),
        vec![
            (Some("right"), &Operand::Place("%t3".to_string())),
            (Some("left"), &Operand::Place("%t4".to_string())),
        ],
        "function-value task arguments should retain source order and names until runtime binding"
    );
}

#[test]
fn retained_process_and_http_builtin_arguments_lower_with_owned_operands() {
    let module = crate::lower_source_to_mir(
        r#"
import process
import net

def supervise(supervisor: process.Supervisor, name: own str, command: own list[str], cwd: own Option[str], environment: own dict[str, str], stdin: own process.Stdio, stdout: own process.Stdio, stderr: own process.Stdio, restart: own process.RestartPolicy, backoff: own Duration, max_restarts: own int32, group: own bool):
    supervisor.start(name=name, command=command, cwd=cwd, env=environment, stdin=stdin, stdout=stdout, stderr=stderr, restart=restart, backoff=backoff, max_restarts=max_restarts, group=group)

def respond_text(exchange: net.HttpExchange, status: int32, text: own str, headers: own dict[str, str]):
    exchange.respond_text(status=status, text=text, headers=headers)

def respond_bytes(exchange: net.HttpExchange, status: int32, bytes: own list[uint8], headers: own dict[str, str]):
    exchange.respond_bytes(status=status, bytes=bytes, headers=headers)
"#,
    )
    .expect("retained process and HTTP builtin arguments should lower");

    let member_args = |function_name: &str, member_name: &str| {
        module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .unwrap_or_else(|| panic!("{function_name} should lower"))
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                Instruction::Assign {
                    value:
                        Rvalue::Call {
                            callee: CallTarget::Member { field, .. },
                            args,
                        },
                    ..
                } if field == member_name => Some(args),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{function_name} should call {member_name}"))
    };

    let supervisor_args = member_args("supervise", "start");
    for (name, place) in [
        ("name", "name"),
        ("command", "command"),
        ("cwd", "cwd"),
        ("env", "environment"),
    ] {
        let argument = supervisor_args
            .iter()
            .find(|argument| argument.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("process.Supervisor.start should retain {name}"));
        assert_eq!(
            argument.value,
            Operand::MovePlace(place.to_string()),
            "non-copy process.Supervisor.start argument {name} must be transferred"
        );
    }
    for name in [
        "stdin",
        "stdout",
        "stderr",
        "restart",
        "backoff",
        "max_restarts",
        "group",
    ] {
        let argument = supervisor_args
            .iter()
            .find(|argument| argument.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("process.Supervisor.start should retain {name}"));
        assert!(
            matches!(argument.value, Operand::Place(_)),
            "copy process.Supervisor.start argument {name} must use a value snapshot"
        );
    }

    for (function_name, member_name, owned) in [
        ("respond_text", "respond_text", ["text", "headers"]),
        ("respond_bytes", "respond_bytes", ["bytes", "headers"]),
    ] {
        let args = member_args(function_name, member_name);
        for place in owned {
            let argument = args
                .iter()
                .find(|argument| argument.name.as_deref() == Some(place))
                .unwrap_or_else(|| panic!("{member_name} should bind {place}"));
            assert_eq!(argument.value, Operand::MovePlace(place.to_string()));
        }
        assert!(matches!(
            args.iter()
                .find(|argument| argument.name.as_deref() == Some("status"))
                .expect("status should bind")
                .value,
            Operand::Place(_)
        ));
    }
}

#[test]
fn json_dump_failures_keep_their_documented_mir_trap_codes() {
    let module = crate::lower_source_to_mir(
        r#"
import json

def main():
    print(json.dumps(json.Value.Null, indent=Option.Some(17)))
"#,
    )
    .expect("invalid runtime indent should still lower");
    let error = crate::run_mir(&module).expect_err("invalid JSON indent should trap");
    assert_eq!(error.code, "AU4003");
    assert!(error.message.contains("between 0 and 16"));
}

#[test]
fn random_rng_constructor_and_projected_shuffle_lower_with_mutable_writeback() {
    let module = crate::lower_source_to_mir(
        r#"
import random

class Item:
    label: str

class Holder:
    values: list[Item]

def main() -> int32:
    mut rng = random.Rng(seed=42)
    mut holder = Holder([Item("a"), Item("b"), Item("c")])
    rng.shuffle(values=holder.values)
    return 0
"#,
    )
    .expect("Randomness constructor and projected shuffle should lower to MIR");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    assert!(
        main.local_types
            .iter()
            .any(|local| local.ty == Type::named("random.Rng")),
        "random.Rng constructor temporaries must retain canonical module provenance"
    );
    assert!(
        main.local_types
            .iter()
            .all(|local| local.ty != Type::named("Rng")),
        "random.Rng must never be lowered as a bare-name builtin type"
    );

    assert!(main.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name),
                        ..
                    },
                    ..
                } if name == "random::Rng"
            )
        })
    }));

    let shuffle = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee:
                            CallTarget::Member {
                                field,
                                receiver_place,
                                ..
                            },
                        args,
                    },
                ..
            } if field == "shuffle" => Some((receiver_place, args)),
            _ => None,
        })
        .expect("shuffle call should lower");
    assert_eq!(shuffle.0.as_deref(), Some("rng"));
    assert_eq!(shuffle.1.len(), 1);
    assert_eq!(shuffle.1[0].name.as_deref(), Some("values"));
    assert_eq!(
        shuffle.1[0].writeback_place.as_deref(),
        Some("holder.values")
    );
}

#[test]
fn path_named_random_keeps_local_and_imported_user_rng_classes_out_of_builtin_lowering() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-pass/random.au");
    let program = crate::check_path(&path)
        .expect("a user Rng in an entry module named random should type check normally");
    assert!(!program.classes["Rng"].is_builtin);

    let module = crate::lower_path_to_mir(&path)
        .expect("local and imported user Rng classes should lower as ordinary classes");
    assert!(module.classes.iter().any(|class| class.name == "Rng"));
    assert!(module
        .classes
        .iter()
        .any(|class| class.name == "user_rng_origin_support.random.Rng"));

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("fixture main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Construct { class_name, .. },
            ..
        } if class_name == "Rng"
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Construct { class_name, .. },
            ..
        } if class_name == "user_rng_origin_support.random.Rng"
    )));
    assert!(!instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Call {
                callee: CallTarget::Name(name),
                ..
            },
            ..
        } if name == "random::Rng"
    )));
}

#[test]
fn duration_builtin_operator_matrix_takes_precedence_over_traits() {
    let duration = Type::named("Duration");
    let int64 = Type::named("int64");
    let int32 = Type::named("int32");

    for op in [
        BinaryOp::Add,
        BinaryOp::Sub,
        BinaryOp::Eq,
        BinaryOp::NotEq,
        BinaryOp::Less,
        BinaryOp::LessEq,
        BinaryOp::Greater,
        BinaryOp::GreaterEq,
    ] {
        assert!(is_builtin_binary_operator(op, &duration, &duration));
    }
    assert!(is_builtin_binary_operator(BinaryOp::Mul, &duration, &int64));
    assert!(is_builtin_binary_operator(BinaryOp::Mul, &int64, &duration));
    assert!(is_builtin_binary_operator(
        BinaryOp::FloorDiv,
        &duration,
        &int64
    ));

    for (op, left, right) in [
        (BinaryOp::Div, &duration, &duration),
        (BinaryOp::Mod, &duration, &duration),
        (BinaryOp::FloorDiv, &int64, &duration),
        (BinaryOp::Mul, &duration, &int32),
    ] {
        assert!(!is_builtin_binary_operator(op, left, right));
    }
}

#[test]
fn duration_operators_with_integer_literals_keep_heterogeneous_builtin_types() {
    let source = r#"
def main() -> int32:
    print(1ms // 0)
    print(3 * 1ms)
    print((3 * 1ms).to_ms())
    return 0
"#;
    let program = crate::check_source(source).expect("Duration literal operators should check");
    let module = lower(&program);
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    assert!(main.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Binary {
                        op: BinaryOp::FloorDiv,
                        ..
                    },
                    ..
                }
            )
        })
    }));
    assert!(!main.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                    ..
                } if field == "floor_div"
            )
        })
    }));
    assert!(main.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                    ..
                } if field == "to_ms"
            )
        })
    }));
}

#[test]
fn generic_operator_calls_retain_the_authoritative_trait_identity() {
    let module = crate::lower_source_to_mir(
        r#"
trait Add[Rhs, Out]:
    def add(self, rhs: Rhs) -> Out

trait Neg[Out]:
    def neg(self) -> Out

def add_all[T: Add[T, T]](left: T, right: T) -> T:
    return left + right

def negate[T: Neg[T]](value: T) -> T:
    return -value
"#,
    )
    .expect("generic operator functions should lower to MIR");

    for (function_name, expected_trait, expected_field) in
        [("add_all", "Add", "add"), ("negate", "Neg", "neg")]
    {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .unwrap_or_else(|| panic!("{function_name} should lower"));
        assert!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Call {
                            callee: CallTarget::TraitMember {
                                trait_name,
                                field,
                                ..
                            },
                            ..
                        },
                        ..
                    } if trait_name == expected_trait && field == expected_field
                )),
            "{function_name} must retain the `{expected_trait}` operator-trait identity"
        );
    }

    validate_loan_flow(&module)
        .expect("authoritative generic operator targets should pass common MIR validation");
}

#[test]
fn generic_supertrait_calls_retain_the_declaring_trait_identity() {
    let module = crate::lower_source_to_mir(include_str!(
        "../tests/fixtures/run-pass/supertrait_bound_inherits_methods.au"
    ))
    .expect("supertrait-bound method calls should lower to MIR");
    let total = module
        .functions
        .iter()
        .find(|function| function.name == "total")
        .expect("total should lower");

    for (expected_trait, expected_field) in [("Base", "base"), ("Derived", "derived")] {
        assert!(
            total
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Call {
                            callee: CallTarget::TraitMember {
                                trait_name,
                                field,
                                ..
                            },
                            ..
                        },
                        ..
                    } if trait_name == expected_trait && field == expected_field
                )),
            "`{expected_field}` must retain its declaring `{expected_trait}` trait identity"
        );
    }

    validate_loan_flow(&module)
        .expect("trait-qualified inherited calls should pass common MIR validation");
    let output = crate::run_mir(&module)
        .expect("MIR runtime should resolve the named supertrait implementation");
    assert_eq!(output.stdout, "21\n");
    crate::native_codegen::emit_host_object(&module)
        .expect("direct codegen should resolve the named supertrait implementation");
}

#[test]
fn d6_mir_uses_declaration_resolved_parameter_conventions() {
    let module = crate::lower_source_to_mir(
        r#"
def modes(copy_value: int32, inferred: str, owned: own str, shared: str, mutable: mut str):
    pass

def generic[T](value: T):
    pass

def main() -> int32:
    generic[int32](1)
    return 0
"#,
    )
    .expect("D6 parameter modes should lower to MIR");

    let modes = module
        .functions
        .iter()
        .find(|function| function.name == "modes")
        .expect("modes should lower");
    assert_eq!(
        modes
            .params
            .iter()
            .map(|param| param.passing)
            .collect::<Vec<_>>(),
        // ADR-0022 Q1: `copy_value: int32` is shared like every other bare
        // parameter. Only `own` still passes by value.
        vec![
            MirReceiverKind::Borrow,
            MirReceiverKind::Borrow,
            MirReceiverKind::Value,
            MirReceiverKind::Borrow,
            MirReceiverKind::BorrowMut,
        ]
    );

    let generic = module
        .functions
        .iter()
        .find(|function| function.name == "generic")
        .expect("generic should lower");
    assert_eq!(generic.params[0].passing, MirReceiverKind::Borrow);
}

#[test]
fn d6_shared_default_temporary_lives_through_the_call() {
    let module = crate::lower_source_to_mir(
        r#"
def shared(value: str = "shared") -> str:
    return value.clone()

def owned(value: own str = "owned") -> str:
    return value

def main() -> int32:
    print(shared())
    print(owned())
    return 0
"#,
    )
    .expect("shared and owned defaults should lower");
    let output = crate::run_mir(&module).expect("default temporaries should remain live in calls");
    assert_eq!(output.stdout, "shared\nowned\n");
}

fn named_arg(name: &str, value: Expr) -> Argument {
    Argument {
        name: Some(name.to_string()),
        span: value.span,
        value,
    }
}

fn binding_pattern(name: &str) -> Pattern {
    Pattern::Binding(BindingPattern {
        name: name.to_string(),
        span: Span::new(1, 1),
    })
}

fn variant_pattern(
    enum_name: Option<&str>,
    variant_name: &str,
    subpatterns: Vec<Pattern>,
) -> Pattern {
    Pattern::Variant(VariantPattern {
        enum_name: enum_name.map(str::to_string),
        variant_name: variant_name.to_string(),
        subpatterns,
        span: Span::new(1, 1),
    })
}

fn namespace_from_program(name: &str, path: &str, program: &Program) -> ModuleNamespace {
    let mut functions = program.functions.clone();
    for function in functions.values_mut() {
        function.module_name = path.to_string();
    }
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: name.to_string(),
        path: path.to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: program.classes.clone(),
        enums: program.enums.clone(),
        traits: program.traits.clone(),
        trait_impls: program.trait_impls.clone(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: program.classes.clone(),
        all_enums: program.enums.clone(),
        all_traits: program.traits.clone(),
        imported_modules: program.imported_modules.clone(),
        closures: program.closures.clone(),
        comprehensions: program.comprehensions.clone(),
    }
}

fn lowerer_with_imported_modules() -> Lowerer<'static> {
    let main_source = r#"
def local_helper() -> int32:
    return 1

def main() -> int32:
    return local_helper()
"#;
    let imported_source = r#"
class Thing:
    value: int32
    flag: bool = true

    def zero() -> Thing:
        return Thing(value=0)

    def get(self) -> int32:
        return self.value

class GenericThing[T]:
    def relay(value: own T) -> T:
        return value

    def choose[U](value: own U) -> U:
        return value

enum Status:
    Ok
    Value(int32)

trait RemoteTrait:
    def label(self) -> str

def helper() -> int32:
    return 7

def generic_helper[T](value: own T) -> T:
    return value
"#;

    let mut program = checked_program(main_source);
    let imported = crate::sema::check_with_context(
        crate::parse_source(imported_source).expect("imported helper source should parse"),
        crate::sema::ModuleContext {
            module_name: "pkg.helpers".to_string(),
            ..crate::sema::ModuleContext::default()
        },
    )
    .expect("imported helper source should type check in its owning module");
    let helpers = namespace_from_program("helpers", "pkg.helpers", &imported);
    let mut reexport = helpers.clone();
    reexport.name = "reexport".to_string();
    reexport.path = "pkg.reexport".to_string();
    reexport.functions.clear();
    reexport.classes.clear();
    reexport.enums.clear();
    let mut pkg = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "pkg".to_string(),
        path: "pkg".to_string(),
        source_path: None,
        modules: BTreeMap::from([
            ("helpers".to_string(), helpers.clone()),
            ("reexport".to_string(), reexport.clone()),
        ]),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    pkg.imported_modules
        .insert("helpers".to_string(), helpers.clone());
    pkg.imported_modules
        .insert("reexport".to_string(), reexport.clone());

    let mut current = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "main".to_string(),
        path: "pkg.main".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: program.functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: program.classes.clone(),
        enums: program.enums.clone(),
        traits: program.traits.clone(),
        trait_impls: program.trait_impls.clone(),
        all_functions: program.functions.clone(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: program.classes.clone(),
        all_enums: program.enums.clone(),
        all_traits: program.traits.clone(),
        imported_modules: BTreeMap::from([("pkg".to_string(), pkg.clone())]),
        closures: program.closures.clone(),
        comprehensions: program.comprehensions.clone(),
    };
    current
        .all_classes
        .extend(imported.classes.iter().map(|(k, v)| (k.clone(), v.clone())));
    current
        .all_enums
        .extend(imported.enums.iter().map(|(k, v)| (k.clone(), v.clone())));
    current
        .all_traits
        .extend(imported.traits.iter().map(|(k, v)| (k.clone(), v.clone())));
    current.all_functions.extend(
        imported
            .functions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    program.module_name = "<root>".to_string();
    program.imported_modules = BTreeMap::from([("pkg".to_string(), pkg.clone())]);
    program.module_registry = BTreeMap::from([
        ("pkg".to_string(), pkg),
        ("pkg.helpers".to_string(), helpers),
        ("pkg.reexport".to_string(), reexport),
        ("pkg.main".to_string(), current),
    ]);

    let program = Box::leak(Box::new(program));
    Lowerer::new(
        program,
        "main",
        "pkg.main",
        Type::named("int32"),
        BTreeMap::new(),
    )
}

fn trait_lowerer() -> Lowerer<'static> {
    let source = r#"
trait Add[Rhs, Out]:
    def add(self, rhs: own Rhs) -> Out

trait Neg[Out]:
    def neg(self) -> Out

trait Named:
    def name(self) -> str

trait Reset:
    def reset(mut self)

class User:
    label: str

class Counter:
    value: int32

    def bump(mut self):
        self.value += 1

class Box[T]:
    value: T

enum Status:
    Value(int32)

def make_flag() -> bool:
    return true

impl Named for User:
    def name(self) -> str:
        return self.label.clone()

impl Reset for User:
    def reset(mut self):
        self.label = ""

impl Add[int32, bool] for User:
    def add(self, rhs: own int32) -> bool:
        return rhs > 0

impl Neg[str] for User:
    def neg(self) -> str:
        return self.label.clone()

impl[T: Named] Add[Box[T], Box[T]] for Box[T]:
    def add(self, rhs: own Box[T]) -> Box[T]:
        return rhs
"#;
    let program = Box::leak(Box::new(checked_program(source)));
    Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::from([
            (
                "T".to_string(),
                vec![TraitBound {
                    trait_name: binary_operator_trait(BinaryOp::Add)
                        .expect("add trait should exist")
                        .0
                        .to_string(),
                    trait_args: vec![Type::named("int32"), Type::named("bool")],
                }],
            ),
            (
                "U".to_string(),
                vec![TraitBound {
                    trait_name: unary_operator_trait(UnaryOp::Neg)
                        .expect("neg trait should exist")
                        .0
                        .to_string(),
                    trait_args: vec![Type::named("str")],
                }],
            ),
        ]),
    )
}

fn function_names(module: &MirModule) -> BTreeSet<String> {
    module
        .functions
        .iter()
        .map(|function| function.name.clone())
        .collect()
}

#[test]
fn mir_helper_functions_cover_builtin_ops_and_type_lowering() {
    let enum_program =
        checked_program("enum Flag:\n    Ok\n\ndef main() -> int32:\n    return 0\n");
    assert!(is_known_enum_name(&enum_program, "Flag"));
    assert!(is_known_enum_name(
        &checked_program("def main() -> int32:\n    return 0\n"),
        "Option"
    ));
    assert!(!is_known_enum_name(
        &checked_program("def main() -> int32:\n    return 0\n"),
        "Missing"
    ));

    assert!(is_builtin_unary_operator(
        UnaryOp::Not,
        &Type::named("bool")
    ));
    assert!(is_builtin_unary_operator(
        UnaryOp::Neg,
        &Type::named("float64")
    ));
    assert!(!is_builtin_unary_operator(
        UnaryOp::Not,
        &Type::named("int32")
    ));

    assert!(is_builtin_binary_operator(
        BinaryOp::Add,
        &Type::named("int32"),
        &Type::named("int32")
    ));
    assert!(is_builtin_binary_operator(
        BinaryOp::Add,
        &Type::named("str"),
        &Type::named("str")
    ));
    assert!(is_builtin_binary_operator(
        BinaryOp::And,
        &Type::named("bool"),
        &Type::named("bool")
    ));
    assert!(!is_builtin_binary_operator(
        BinaryOp::Add,
        &Type::named("int32"),
        &Type::named("float64")
    ));

    let mut collected = BTreeSet::new();
    collect_type_params_from_type(
        &Type::Named(
            "dict".to_string(),
            vec![
                Type::TypeParam("K".to_string()),
                Type::Named("list".to_string(), vec![Type::TypeParam("V".to_string())]),
            ],
        ),
        &mut collected,
    );
    assert_eq!(
        collected,
        BTreeSet::from(["K".to_string(), "V".to_string()])
    );
    collect_type_params_from_type(&Type::Unit, &mut collected);
    collect_type_params_from_type(&Type::Module("pkg".to_string()), &mut collected);
    assert_eq!(
        collected,
        BTreeSet::from(["K".to_string(), "V".to_string()])
    );

    let (left_ty, right_ty) = adjusted_binary_operand_types(
        &expr(ExprKind::Int(1)),
        Type::named("int32"),
        &expr(ExprKind::Float(1.0)),
        Type::named("float64"),
    );
    assert_eq!(left_ty, Type::named("float64"));
    assert_eq!(right_ty, Type::named("float64"));

    assert_eq!(default_return_operand(&Type::Unit), Operand::Unit);
    assert_eq!(
        default_return_operand(&Type::named("bool")),
        Operand::Bool(false)
    );
    assert_eq!(
        default_return_operand(&Type::named("float64")),
        Operand::Float(0.0)
    );
    assert_eq!(
        default_return_operand(&Type::named("str")),
        Operand::String(String::new())
    );
    assert_eq!(
        default_return_operand(&Type::named("Duration")),
        Operand::Duration(0)
    );
    assert_eq!(
        default_return_operand(&Type::named("int32")),
        Operand::Int(0)
    );
    assert_eq!(default_return_operand(&Type::named("Thing")), Operand::Unit);

    assert_eq!(
        lower_receiver_kind(ReceiverKind::BorrowMut),
        MirReceiverKind::BorrowMut
    );
    assert_eq!(
        lower_receiver_kind(ReceiverKind::Value),
        MirReceiverKind::Value
    );
    assert_eq!(
        lower_receiver_kind(ReceiverKind::Borrow),
        MirReceiverKind::Borrow
    );
    assert_eq!(
        imported_module_function_name("pkg.tools", "work"),
        "pkg.tools::work"
    );
    assert_eq!(
        format_trait_args(&[Type::named("int32"), Type::named("str")]),
        "[int32, str]"
    );
    assert_eq!(format_trait_args(&[]), "");

    assert_eq!(lower_type_ref(&type_ref("None")), Type::Unit);
    assert_eq!(lower_type_ref(&type_ref("str")), Type::named("str"));
    assert_eq!(
        lower_type_ref(&TypeRef::named(
            "list",
            vec![type_ref("int32")],
            false,
            Span::new(1, 1),
        )),
        Type::Named("list".to_string(), vec![Type::named("int32")])
    );
}

#[test]
fn lowerer_module_resolution_and_rendering_helpers_cover_imported_paths() {
    let mut lowerer = lowerer_with_imported_modules();
    lowerer
        .local_types
        .insert("pkg".to_string(), Type::Module("pkg".to_string()));

    assert_eq!(
        lowerer
            .current_module_namespace()
            .map(|namespace| namespace.path.as_str()),
        Some("pkg.main")
    );
    assert_eq!(
        lowerer
            .module_namespace("pkg.helpers")
            .map(|namespace| namespace.path.as_str()),
        Some("pkg.helpers")
    );
    assert_eq!(
        lowerer
            .trait_info_in_scope("RemoteTrait")
            .map(|info| info.decl.name.as_str()),
        Some("RemoteTrait")
    );
    let mut imported_only_root = ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "pkg".to_string(),
        path: "pkg".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: BTreeMap::new(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: BTreeMap::new(),
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    };
    imported_only_root.imported_modules.insert(
        "helpers".to_string(),
        lowerer.program.module_registry["pkg.helpers"].clone(),
    );
    assert_eq!(
        Lowerer::find_namespace_in_modules(
            &BTreeMap::from([("pkg".to_string(), imported_only_root)]),
            "pkg.helpers",
        )
        .map(|namespace| namespace.path.as_str()),
        Some("pkg.helpers")
    );
    assert_eq!(
        lowerer
            .infer_module_path(&member_expr(name_expr("pkg"), "helpers"))
            .as_deref(),
        Some("pkg.helpers")
    );
    assert_eq!(
        lowerer.qualified_module_item(&member_expr(
            member_expr(name_expr("pkg"), "helpers"),
            "Thing"
        )),
        Some(("pkg.helpers".to_string(), "Thing".to_string()))
    );
    assert_eq!(
        lowerer
            .resolve_function_info("local_helper")
            .map(|info| info.decl.name.as_str()),
        Some("local_helper")
    );
    assert_eq!(
        lowerer
            .resolve_class_info("pkg.helpers.Thing")
            .map(|info| info.decl.name.as_str()),
        Some("Thing")
    );
    assert_eq!(
        lowerer
            .resolve_class_info("pkg.reexport.Thing")
            .map(|info| info.decl.name.as_str()),
        Some("Thing")
    );
    assert_eq!(
        lowerer
            .resolve_class_info("Thing")
            .map(|info| info.decl.name.as_str()),
        Some("Thing")
    );
    assert_eq!(
        lowerer
            .resolve_enum_info("pkg.helpers.Status")
            .map(|info| info.decl.name.as_str()),
        Some("Status")
    );
    assert_eq!(
        lowerer
            .resolve_enum_info("pkg.reexport.Status")
            .map(|info| info.decl.name.as_str()),
        Some("Status")
    );
    assert_eq!(
        lowerer.resolve_pattern_enum_name(
            &VariantPattern {
                enum_name: Some("pkg.reexport.Status".to_string()),
                variant_name: "Ok".to_string(),
                subpatterns: Vec::new(),
                span: Span::new(1, 1),
            },
            None,
        ),
        "pkg.helpers.Status"
    );
    assert_eq!(
        lowerer.render_assign_target(&crate::ast::AssignTarget::Name("value".to_string())),
        "value".to_string()
    );
    assert_eq!(
        lowerer.render_expr_place(&member_expr(name_expr("pkg"), "helpers")),
        "pkg.helpers".to_string()
    );
    assert_eq!(
        lowerer.render_place_expr_option(&name_expr("value")),
        Some("value".to_string())
    );
    assert_eq!(
        lowerer.render_place_expr_option(&expr(ExprKind::Int(1))),
        None
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(member_expr(name_expr("pkg"), "helpers")),
            args: Vec::new(),
        })),
        Some(Type::Module("pkg.helpers".to_string()))
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(member_expr(
                member_expr(name_expr("pkg"), "helpers"),
                "helper",
            )),
            args: Vec::new(),
        })),
        Some(Type::named("int32"))
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(member_expr(
                member_expr(name_expr("pkg"), "helpers"),
                "Thing",
            )),
            args: Vec::new(),
        })),
        Some(Type::named("pkg.helpers.Thing"))
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(member_expr(
                member_expr(name_expr("pkg"), "helpers"),
                "Status",
            )),
            args: Vec::new(),
        })),
        Some(Type::named("pkg.helpers.Status"))
    );
    for (builtin_name, args) in [
        ("Option", vec![type_ref("int32")]),
        ("Result", vec![type_ref("int32"), type_ref("str")]),
        ("SendError", vec![type_ref("int32")]),
        ("Queue", vec![type_ref("str")]),
        ("list", vec![type_ref("int32")]),
        ("set", vec![type_ref("str")]),
        ("dict", vec![type_ref("str"), type_ref("int32")]),
    ] {
        assert_eq!(
            lowerer.infer_expr_type(&expr(ExprKind::Specialize {
                expr: Box::new(name_expr(builtin_name)),
                type_args: args.clone(),
            })),
            Some(Type::Named(
                builtin_name.to_string(),
                args.into_iter().map(|arg| lower_type_ref(&arg)).collect(),
            )),
            "{builtin_name} specialization should infer a builtin generic type"
        );
    }
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Specialize {
            expr: Box::new(expr(ExprKind::Int(7))),
            type_args: Vec::new(),
        })),
        Some(Type::named("int64"))
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Try(Box::new(expr(ExprKind::Int(1)))))),
        None
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Unary {
            op: UnaryOp::Neg,
            expr: Box::new(expr(ExprKind::Bool(true))),
        })),
        None
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(name_expr("wait_any")),
            args: vec![arg(expr(ExprKind::List(vec![expr(ExprKind::Int(1))])))],
        })),
        None
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(name_expr("wait_any")),
            args: vec![arg(expr(ExprKind::Bool(true)))],
        })),
        None
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(name_expr("wait_all")),
            args: vec![arg(expr(ExprKind::List(vec![expr(ExprKind::Int(1))])))],
        })),
        None
    );
    assert_eq!(
        lowerer.infer_expr_type(&expr(ExprKind::Call {
            callee: Box::new(name_expr("wait_all")),
            args: vec![arg(expr(ExprKind::Bool(true)))],
        })),
        None
    );
    let local_static_target = lowerer
        .resolve_task_start_target(&member_expr(name_expr("Thing"), "zero"))
        .expect("unqualified imported class static methods should resolve");
    assert_eq!(
        local_static_target.function_name.as_deref(),
        Some("pkg.helpers::Thing.zero")
    );
    let module_static_target = lowerer
        .resolve_task_start_target(&member_expr(
            member_expr(member_expr(name_expr("pkg"), "helpers"), "Thing"),
            "zero",
        ))
        .expect("module-qualified imported class static methods should resolve");
    assert_eq!(
        module_static_target.function_name.as_deref(),
        Some("pkg.helpers::Thing.zero")
    );
    assert!(
        lowerer
            .resolve_task_start_target(&member_expr(
                member_expr(member_expr(name_expr("pkg"), "helpers"), "Thing"),
                "get",
            ))
            .is_none(),
        "receiver methods are not valid task start targets"
    );
    let module_function_target = lowerer
        .resolve_task_start_target(&member_expr(
            member_expr(name_expr("pkg"), "helpers"),
            "helper",
        ))
        .expect("module-qualified imported functions should resolve");
    assert_eq!(
        module_function_target.function_name.as_deref(),
        Some("pkg.helpers::helper")
    );
    let reexport_function_target = lowerer
        .resolve_task_start_target(&member_expr(
            member_expr(name_expr("pkg"), "reexport"),
            "helper",
        ))
        .expect("all-functions-only imported functions should resolve");
    assert_eq!(
        reexport_function_target.function_name.as_deref(),
        Some("pkg.reexport::helper")
    );
    let specialized_local_function = expr(ExprKind::Specialize {
        expr: Box::new(name_expr("local_helper")),
        type_args: Vec::new(),
    });
    assert_eq!(
        lowerer
            .resolve_task_start_target(&specialized_local_function)
            .expect("specialized local functions should resolve as task targets")
            .function_name
            .as_deref(),
        Some("local_helper")
    );
    let specialized_static_target = expr(ExprKind::Specialize {
        expr: Box::new(member_expr(name_expr("Thing"), "zero")),
        type_args: Vec::new(),
    });
    assert_eq!(
        lowerer
            .resolve_task_start_target(&specialized_static_target)
            .expect("specialized static methods should resolve as task targets")
            .function_name
            .as_deref(),
        Some("pkg.helpers::Thing.zero")
    );
    let specialized_class_object = expr(ExprKind::Specialize {
        expr: Box::new(name_expr("Thing")),
        type_args: Vec::new(),
    });
    assert_eq!(
        lowerer
            .resolve_task_start_target(&member_expr(specialized_class_object, "zero"))
            .expect("static methods on specialized class objects should resolve")
            .function_name
            .as_deref(),
        Some("pkg.helpers::Thing.zero")
    );

    let static_call = expr(ExprKind::Call {
        callee: Box::new(member_expr(
            member_expr(member_expr(name_expr("pkg"), "helpers"), "Thing"),
            "zero",
        )),
        args: Vec::new(),
    });
    assert!(matches!(
        lowerer.lower_expr(&static_call),
        Operand::Place(_)
    ));
    assert!(matches!(
        lowerer.lower_expr(&member_expr(
            member_expr(member_expr(name_expr("pkg"), "helpers"), "Status"),
            "Ok",
        )),
        Operand::Place(_)
    ));
    let module_function_call = expr(ExprKind::Call {
        callee: Box::new(member_expr(
            member_expr(name_expr("pkg"), "helpers"),
            "helper",
        )),
        args: Vec::new(),
    });
    assert!(matches!(
        lowerer.lower_expr(&module_function_call),
        Operand::Place(_)
    ));
    let module_enum_variant_call = expr(ExprKind::Call {
        callee: Box::new(member_expr(
            member_expr(member_expr(name_expr("pkg"), "helpers"), "Status"),
            "Value",
        )),
        args: vec![arg(expr(ExprKind::Int(9)))],
    });
    assert!(matches!(
        lowerer.lower_expr(&module_enum_variant_call),
        Operand::Place(_)
    ));
    for builtin_variant in ["TimedOut", "Full", "Item", "Ready"] {
        let builtin_variant_call = expr(ExprKind::Call {
            callee: Box::new(name_expr(builtin_variant)),
            args: Vec::new(),
        });
        assert!(
            matches!(lowerer.lower_expr(&builtin_variant_call), Operand::Place(_)),
            "{builtin_variant} should lower through the builtin enum fallback"
        );
    }
    let constructor_call = expr(ExprKind::Call {
        callee: Box::new(member_expr(
            member_expr(name_expr("pkg"), "helpers"),
            "Thing",
        )),
        args: vec![arg(expr(ExprKind::Int(5)))],
    });
    assert!(matches!(
        lowerer.lower_expr(&constructor_call),
        Operand::Place(_)
    ));
    let unsupported_call = expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Int(1))),
        args: Vec::new(),
    });
    assert!(matches!(
        lowerer.lower_expr(&unsupported_call),
        Operand::Place(_)
    ));
    let specialized_value = expr(ExprKind::Specialize {
        expr: Box::new(expr(ExprKind::Int(7))),
        type_args: vec![type_ref("int32")],
    });
    assert_eq!(lowerer.lower_expr(&specialized_value), Operand::Int(7));
    let current_instructions = &lowerer.blocks[lowerer.current_block].instructions;
    assert!(current_instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Construct {
                class_name,
                fields,
            },
            ..
        } if class_name == "pkg.helpers.Thing"
            && fields.iter().any(|field| field.name == "value")
            && fields.iter().any(|field| field.name == "flag")
    )));
    assert!(current_instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Call {
                callee: CallTarget::Name(name),
                ..
            },
            ..
        } if name == "pkg.helpers::Thing.zero"
    )));
    assert!(current_instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Call {
                callee: CallTarget::Name(name),
                ..
            },
            ..
        } if name.starts_with("unsupported<")
    )));

    let first_temp = lowerer.new_temp();
    let typed_temp = lowerer.new_typed_temp(Type::named("str"));
    let temp_for_expr = lowerer.new_temp_for_expr(&expr(ExprKind::String("aura".to_string())));
    assert!(first_temp.starts_with("%t"));
    assert!(typed_temp.starts_with("%t"));
    assert!(temp_for_expr.starts_with("%t"));
    assert_eq!(
        lowerer.local_types.get(&typed_temp),
        Some(&Type::named("str"))
    );
    assert_eq!(
        lowerer.local_types.get(&temp_for_expr),
        Some(&Type::named("str"))
    );

    let block = lowerer.new_block("branch");
    let label = lowerer.label(block);
    assert!(label.starts_with("main_branch_"));
    assert!(!lowerer.current_terminated());
    lowerer.emit(Instruction::Eval {
        value: Operand::Int(1),
    });
    lowerer.with_stack.push("resource".to_string());
    lowerer.emit_cleanup_range(0, true);
    lowerer.terminate(Terminator::Return(Operand::Int(0)));
    assert!(lowerer.current_terminated());
    lowerer.switch_to(block);
    let function = lowerer.finish(MirFunctionSpec {
        name: "main".to_string(),
        span: Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        return_type: Type::named("int32"),
        default_return: Operand::Int(0),
    });
    assert_eq!(function.name, "main");
    assert!(function
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .any(|instruction| matches!(
                instruction,
                Instruction::PopCleanup {
                    place,
                    cancel_before_cleanup: true
                } if place == "resource"
        )));
}

#[test]
fn imported_task_targets_preserve_contextual_and_static_specialization() {
    let lowerer = lowerer_with_imported_modules();
    let imported_function = member_expr(member_expr(name_expr("pkg"), "helpers"), "generic_helper");
    let contextual_specialization = expr(ExprKind::Index {
        object: Box::new(imported_function),
        index: Box::new(name_expr("int32")),
    });
    let target = lowerer
        .resolve_task_start_target(&contextual_specialization)
        .expect("an imported generic function should resolve as a task target");
    let target = lowerer.specialize_task_start_target(
        target,
        &[arg(expr(ExprKind::Int(1)))],
        Span::new(1, 1),
    );
    assert_eq!(
        target.function_name.as_deref(),
        Some("pkg.helpers::generic_helper")
    );
    assert_eq!(target.param_types, vec![Type::named("int32")]);
    assert_eq!(target.return_type, Type::named("int32"));

    let imported_class = member_expr(member_expr(name_expr("pkg"), "helpers"), "GenericThing");
    let specialized_class = expr(ExprKind::Specialize {
        expr: Box::new(imported_class),
        type_args: vec![type_ref("str")],
    });
    let static_target = member_expr(specialized_class, "relay");
    let target = lowerer
        .resolve_task_start_target(&static_target)
        .expect("an imported specialized associated method should resolve");
    let target = lowerer.specialize_task_start_target(
        target,
        &[arg(expr(ExprKind::String("value".to_string())))],
        Span::new(1, 1),
    );
    assert_eq!(
        target.function_name.as_deref(),
        Some("pkg.helpers::GenericThing.relay")
    );
    assert_eq!(target.param_types, vec![Type::named("str")]);
    assert_eq!(target.return_type, Type::named("str"));

    let generic_static_target = expr(ExprKind::Index {
        object: Box::new(member_expr(
            expr(ExprKind::Specialize {
                expr: Box::new(member_expr(
                    member_expr(name_expr("pkg"), "helpers"),
                    "GenericThing",
                )),
                type_args: vec![type_ref("str")],
            }),
            "choose",
        )),
        index: Box::new(name_expr("int32")),
    });
    let target = lowerer
        .resolve_task_start_target(&generic_static_target)
        .expect("class and method specializations should both survive import resolution");
    let target = lowerer.specialize_task_start_target(
        target,
        &[arg(expr(ExprKind::Int(7)))],
        Span::new(1, 1),
    );
    assert_eq!(
        target.function_name.as_deref(),
        Some("pkg.helpers::GenericThing.choose")
    );
    assert_eq!(target.param_types, vec![Type::named("int32")]);
    assert_eq!(target.return_type, Type::named("int32"));
}

#[test]
fn imported_module_class_collection_walks_nested_namespaces() {
    let program = checked_program("def main() -> int32:\n    return 0\n");
    let helpers_program = checked_program(
        "\
class Thing:
    value: int32

    def zero() -> Thing:
        return Thing(value=0)
",
    );
    let nested_program = checked_program(
        "\
class Leaf:
    value: int32

def leaf_helper() -> int32:
    return 5
",
    );
    let nested = namespace_from_program("nested", "pkg.helpers.nested", &nested_program);
    let mut helpers = namespace_from_program("helpers", "pkg.helpers", &helpers_program);
    helpers.modules.insert("nested".to_string(), nested.clone());

    let mut classes = Vec::new();
    let mut functions = Vec::new();
    let mut seen_functions = BTreeSet::new();
    let mut seen_classes = BTreeSet::new();
    push_imported_module_classes_from_namespace(
        &program,
        &helpers,
        &mut classes,
        &mut functions,
        &mut seen_functions,
        &mut seen_classes,
    );

    assert!(classes.iter().any(|class| class.name == "Thing"));
    assert!(classes.iter().any(|class| class.name == "Leaf"));
    assert!(functions
        .iter()
        .any(|function| function.name == "pkg.helpers::Thing.zero"));

    let mut imported_functions = Vec::new();
    let mut seen_imported_functions = BTreeSet::new();
    push_imported_module_functions_from_namespace(
        &program,
        &helpers,
        &mut imported_functions,
        &mut seen_imported_functions,
    );
    assert!(imported_functions
        .iter()
        .any(|function| function.name == "pkg.helpers.nested::leaf_helper"));
}

#[test]
fn imported_trait_impl_collection_deduplicates_equivalent_impls() {
    let program = checked_program("def main() -> int32:\n    return 0\n");
    let trait_program = checked_program(
        r#"
trait Named:
    def name(self) -> str

class User:
    label: str

impl Named for User:
    def name(self) -> str:
        return self.label.clone()
"#,
    );
    let first = namespace_from_program("first", "pkg.first", &trait_program);
    let second = namespace_from_program("second", "pkg.second", &trait_program);
    let mut program = program;
    program.module_registry = BTreeMap::from([
        ("pkg.first".to_string(), first),
        ("pkg.second".to_string(), second),
    ]);

    let mut functions = Vec::new();
    let mut trait_impls = Vec::new();
    let mut seen_functions = BTreeSet::new();
    let mut seen_trait_impls = BTreeSet::new();
    push_imported_module_trait_impls(
        &program,
        &mut functions,
        &mut trait_impls,
        &mut seen_functions,
        &mut seen_trait_impls,
    );

    assert_eq!(trait_impls.len(), 1);
    assert_eq!(functions.len(), 1);
}

#[test]
fn imported_class_and_enum_lookup_rejects_ambiguous_unqualified_names() {
    let mut program = checked_program("def main() -> int32:\n    return 0\n");
    let first_program = checked_program(
        r#"
class Thing:
    value: int32

enum Status:
    Ready
"#,
    );
    let second_program = checked_program(
        r#"
class Thing:
    value: int32

enum Status:
    Ready
"#,
    );
    program.imported_modules = BTreeMap::from([
        (
            "first".to_string(),
            namespace_from_program("first", "first", &first_program),
        ),
        (
            "second".to_string(),
            namespace_from_program("second", "second", &second_program),
        ),
    ]);
    let program = Box::leak(Box::new(program));
    let lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::new(),
    );

    assert!(lowerer.resolve_class_info("first.Thing").is_some());
    assert!(lowerer.resolve_enum_info("first.Status").is_some());
    assert!(lowerer.resolve_class_info("Thing").is_none());
    assert!(lowerer.resolve_enum_info("Status").is_none());
}

#[test]
fn mir_types_public_length_members_as_int64() {
    let lowerer = trait_lowerer();
    let cases = [
        (Type::named("str"), "len"),
        (Type::named("str"), "byte_len"),
        (
            Type::Named("list".to_string(), vec![Type::named("str")]),
            "len",
        ),
        (
            Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")],
            ),
            "len",
        ),
        (
            Type::Named("set".to_string(), vec![Type::named("str")]),
            "len",
        ),
    ];

    for (receiver, field) in cases {
        assert_eq!(
            lowerer.builtin_runtime_member_return_type(&receiver, field),
            Some(Type::named("int64")),
            "{receiver}.{field} must lower with an int64 result"
        );
    }
}

#[test]
fn lowerer_trait_and_member_type_helpers_cover_trait_bounds_and_variants() {
    let mut lowerer = trait_lowerer();
    lowerer
        .local_types
        .insert("left".to_string(), Type::TypeParam("T".to_string()));
    lowerer
        .local_types
        .insert("right".to_string(), Type::named("int32"));
    lowerer
        .local_types
        .insert("value".to_string(), Type::TypeParam("U".to_string()));

    assert_eq!(
        lowerer.operator_trait_member_for_binary(
            BinaryOp::Add,
            &name_expr("left"),
            &name_expr("right")
        ),
        binary_operator_trait(BinaryOp::Add)
            .map(|(trait_name, field)| { (trait_name.to_string(), field.to_string()) })
    );
    assert_eq!(
        lowerer.operator_trait_member_for_unary(UnaryOp::Neg, &name_expr("value")),
        unary_operator_trait(UnaryOp::Neg)
            .map(|(trait_name, field)| { (trait_name.to_string(), field.to_string()) })
    );
    assert_eq!(
        lowerer.operator_return_type_for_binary(
            &Type::TypeParam("T".to_string()),
            &Type::named("int32"),
            BinaryOp::Add
        ),
        Some(Type::named("bool"))
    );
    assert_eq!(
        lowerer.operator_return_type_for_unary(&Type::TypeParam("U".to_string()), UnaryOp::Neg),
        Some(Type::named("str"))
    );
    assert_eq!(
        lowerer.operator_return_type_for_binary(
            &Type::named("User"),
            &Type::named("int32"),
            BinaryOp::Add
        ),
        Some(Type::named("bool"))
    );
    assert_eq!(
        lowerer.operator_return_type_for_unary(&Type::named("User"), UnaryOp::Neg),
        Some(Type::named("str"))
    );
    assert_eq!(
        lowerer.operator_return_type_for_unary(&Type::named("User"), UnaryOp::Not),
        None
    );

    let named_bound = TraitBound {
        trait_name: "Named".to_string(),
        trait_args: Vec::new(),
    };
    assert!(lowerer.type_implements_trait_bound(&Type::named("User"), &named_bound));
    assert!(!lowerer.type_implements_trait_bound(&Type::named("str"), &named_bound));
    let bounded_box_add_impl = lowerer
        .program
        .trait_impls
        .iter()
        .find(|trait_impl| {
            trait_impl.trait_name == "Add"
                && matches!(&trait_impl.for_type, Type::Named(name, _) if name == "Box")
        })
        .expect("bounded Box Add impl should be present");
    let box_user = Type::Named("Box".to_string(), vec![Type::named("User")]);
    let box_user_add_bound = TraitBound {
        trait_name: "Add".to_string(),
        trait_args: vec![box_user.clone(), box_user.clone()],
    };
    assert!(lowerer
        .trait_impl_substitutions_for_bound(bounded_box_add_impl, &box_user, &box_user_add_bound)
        .is_some());
    let box_string = Type::Named("Box".to_string(), vec![Type::named("str")]);
    let box_string_add_bound = TraitBound {
        trait_name: "Add".to_string(),
        trait_args: vec![box_string.clone(), box_string.clone()],
    };
    assert!(lowerer
        .trait_impl_substitutions_for_bound(
            bounded_box_add_impl,
            &box_string,
            &box_string_add_bound,
        )
        .is_none());
    assert!(lowerer
        .trait_method_for_receiver(&Type::named("User"), "name")
        .is_some());
    assert!(lowerer
        .trait_impl_method_for_class_name("User", "name")
        .is_some());

    let option_string = Type::Named("Option".to_string(), vec![Type::named("str")]);
    assert_eq!(
        lowerer.builtin_enum_variant_type(&option_string, "Some"),
        Some(option_string.clone())
    );
    assert_eq!(
        lowerer.builtin_enum_variant_type(&option_string, "Missing"),
        None
    );
    let send_error_string = Type::Named("SendError".to_string(), vec![Type::named("str")]);
    assert_eq!(
        lowerer.builtin_enum_variant_type(&send_error_string, "Closed"),
        Some(send_error_string.clone())
    );

    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named("Option".to_string(), vec![Type::named("str")])),
            "Option",
            "Some"
        ),
        Some(vec![Type::named("str")])
    );
    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")]
            )),
            "Result",
            "Err"
        ),
        Some(vec![Type::named("str")])
    );
    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named(
                "SendError".to_string(),
                vec![Type::named("int32")]
            )),
            "SendError",
            "Closed"
        ),
        Some(vec![Type::named("int32")])
    );
    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named(
                "TaskResult".to_string(),
                vec![Type::named("bool")]
            )),
            "TaskResult",
            "Ready"
        ),
        Some(vec![Type::named("bool")])
    );
    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named(
                "WaitAny".to_string(),
                vec![Type::named("str")]
            )),
            "WaitAny",
            "Ready"
        ),
        Some(vec![Type::named("int64"), Type::named("str")])
    );
    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named(
                "QueueReceive".to_string(),
                vec![Type::named("str")]
            )),
            "QueueReceive",
            "TimedOut"
        ),
        Some(Vec::new())
    );
    assert_eq!(
        lowerer.variant_payload_types(
            Some(&Type::Named(
                "WaitAll".to_string(),
                vec![Type::named("str")]
            )),
            "WaitAll",
            "Error"
        ),
        Some(vec![Type::named("int64"), Type::named("str")])
    );
    for (ty, enum_name) in [
        (
            Type::Named("Option".to_string(), vec![Type::named("str")]),
            "Option",
        ),
        (
            Type::Named(
                "Result".to_string(),
                vec![Type::named("int32"), Type::named("str")],
            ),
            "Result",
        ),
        (
            Type::Named("SendError".to_string(), vec![Type::named("int32")]),
            "SendError",
        ),
        (
            Type::Named("QueueReceive".to_string(), vec![Type::named("str")]),
            "QueueReceive",
        ),
        (
            Type::Named("TaskResult".to_string(), vec![Type::named("str")]),
            "TaskResult",
        ),
        (
            Type::Named("WaitAny".to_string(), vec![Type::named("str")]),
            "WaitAny",
        ),
        (
            Type::Named("WaitAll".to_string(), vec![Type::named("str")]),
            "WaitAll",
        ),
    ] {
        assert_eq!(
            lowerer.variant_payload_types(Some(&ty), enum_name, "Missing"),
            None,
            "{enum_name} should reject unknown builtin variants"
        );
    }
    assert_eq!(
        lowerer.variant_payload_types(None, "Status", "Value"),
        Some(vec![Type::named("int32")])
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("str"), "len"),
        Some(Type::named("int64"))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("str"), "byte_len"),
        Some(Type::named("int64"))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("str"), "split"),
        Some(Type::Named("list".to_string(), vec![Type::named("str")]))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::Unit, "to_string"),
        None
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("int32"), "to_string"),
        Some(Type::named("str"))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named("list".to_string(), vec![Type::named("str")]),
            "pop"
        ),
        Some(Type::named("str"))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named("set".to_string(), vec![Type::named("str")]),
            "add"
        ),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")]
            ),
            "items"
        ),
        Some(Type::Named(
            "list".to_string(),
            vec![Type::Tuple(vec![Type::named("str"), Type::named("int32")])]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")]
            ),
            "keys"
        ),
        Some(Type::Named("list".to_string(), vec![Type::named("str")]))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("int32")]
            ),
            "get"
        ),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("int32")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named("Task".to_string(), vec![Type::named("bool")]),
            "result"
        ),
        Some(Type::Named(
            "TaskResult".to_string(),
            vec![Type::named("bool")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named("Task".to_string(), vec![Type::named("bool")]),
            "result_or_none"
        ),
        Some(Type::Named("Option".to_string(), vec![Type::named("bool")]))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named("Queue".to_string(), vec![Type::named("bool")]),
            "get"
        ),
        Some(Type::Named(
            "QueueReceive".to_string(),
            vec![Type::named("bool")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(
            &Type::Named("Queue".to_string(), vec![Type::named("bool")]),
            "put"
        ),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Unit,
                Type::Named("SendError".to_string(), vec![Type::named("bool")])
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("list"), "get"),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("dict"), "keys"),
        Some(Type::Named(
            "list".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("dict"), "values"),
        Some(Type::Named(
            "list".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("dict"), "items"),
        Some(Type::Named(
            "list".to_string(),
            vec![Type::Tuple(vec![
                Type::named("Unknown"),
                Type::named("Unknown")
            ])]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("dict"), "get"),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("Queue"), "get"),
        Some(Type::Named(
            "QueueReceive".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("Queue"), "get_or_none"),
        Some(Type::Named(
            "Option".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("Queue"), "put"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Unit,
                Type::Named("SendError".to_string(), vec![Type::named("Unknown")])
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("Queue"), "try_put"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Unit,
                Type::Named("SendError".to_string(), vec![Type::named("Unknown")])
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("TaskGroup"), "start"),
        Some(Type::Named(
            "Task".to_string(),
            vec![Type::named("Unknown")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("TaskGroup"), "start_soon"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("fs.File"), "read_all"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::named("str"), Type::named("io.Error")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("fs.File"), "read_bytes"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
                Type::named("io.Error")
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("fs.File"), "flush"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::Unit, Type::named("io.Error")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("fs.File"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("process.Completed"), "stdout"),
        Some(Type::named("str"))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.TcpListener"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.TcpStream"), "shutdown_read"),
        Some(Type::Named(
            "Result".to_string(),
            vec![Type::Unit, Type::named("io.Error")]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.TcpStream"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.UdpSocket"), "recv"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Named(
                    "Option".to_string(),
                    vec![Type::Named("list".to_string(), vec![Type::named("uint8")])]
                ),
                Type::named("io.Error")
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.UdpSocket"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.HttpListener"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.HttpExchange"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.HttpResponse"), "reason"),
        Some(Type::named("str"))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.HttpResponse"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.WebSocketListener"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.WebSocket"), "recv_bytes"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Named(
                    "Option".to_string(),
                    vec![Type::Named("list".to_string(), vec![Type::named("uint8")])]
                ),
                Type::named("io.Error")
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.WebSocket"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.UnixListener"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.UnixStream"), "read_exact"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Named("list".to_string(), vec![Type::named("uint8")]),
                Type::named("io.Error")
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.UnixStream"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.TlsListener"), "close"),
        Some(Type::Unit)
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.TlsStream"), "read_line"),
        Some(Type::Named(
            "Result".to_string(),
            vec![
                Type::Named("Option".to_string(), vec![Type::named("str")]),
                Type::named("io.Error")
            ]
        ))
    );
    assert_eq!(
        lowerer.builtin_runtime_member_return_type(&Type::named("net.TlsStream"), "close"),
        Some(Type::Unit)
    );

    let int_vec = Type::Named("list".to_string(), vec![Type::named("int32")]);
    lowerer
        .local_types
        .insert("items".to_string(), int_vec.clone());
    lowerer
        .local_types
        .insert("label".to_string(), Type::named("str"));
    lowerer
        .local_types
        .insert("user".to_string(), Type::named("User"));
    lowerer
        .local_types
        .insert("counter".to_string(), Type::named("Counter"));
    lowerer
        .local_types
        .insert("count".to_string(), Type::named("int32"));
    assert_eq!(
        lowerer.infer_operand_type(&Operand::Place("items".to_string())),
        Some(int_vec)
    );
    assert_eq!(
        lowerer.infer_operand_type(&Operand::Int(7)),
        Some(Type::named("int64"))
    );
    assert_eq!(
        lowerer.infer_operand_type(&Operand::Duration(10)),
        Some(Type::named("Duration"))
    );
    assert_eq!(
        lowerer.infer_operand_type(&Operand::Float(1.5)),
        Some(Type::named("float64"))
    );
    assert_eq!(
        lowerer.infer_operand_type(&Operand::Bool(true)),
        Some(Type::named("bool"))
    );
    assert_eq!(
        lowerer.infer_operand_type(&Operand::String("label".to_string())),
        Some(Type::named("str"))
    );
    assert_eq!(lowerer.infer_operand_type(&Operand::Unit), Some(Type::Unit));
    assert_eq!(
        lowerer.infer_expr_type(&name_expr("make_flag")),
        Some(Type::Function {
            params: Vec::new(),
            return_type: Box::new(Type::named("bool")),
        })
    );
    assert!(lowerer.member_call_mutates_receiver(
        &Operand::Place("items".to_string()),
        None,
        "append"
    ));
    assert!(!lowerer.member_call_mutates_receiver(
        &Operand::Place("label".to_string()),
        None,
        "len"
    ));
    assert!(!lowerer.member_call_mutates_receiver(
        &Operand::Place("items".to_string()),
        None,
        "contains"
    ));
    assert!(lowerer.member_call_mutates_receiver(
        &Operand::Place("user".to_string()),
        None,
        "reset"
    ));
    assert!(lowerer.member_call_mutates_receiver(
        &Operand::Place("counter".to_string()),
        None,
        "bump"
    ));
    assert!(!lowerer.member_call_mutates_receiver(
        &Operand::Place("missing".to_string()),
        None,
        "append"
    ));
    assert!(!lowerer.member_call_mutates_receiver(
        &Operand::Place("count".to_string()),
        None,
        "missing"
    ));
    assert!(lowerer.rvalue_writes_place(
        &Rvalue::Call {
            callee: CallTarget::Member {
                object: Operand::Place("items".to_string()),
                field: "append".to_string(),
                receiver_place: Some("items".to_string()),
            },
            args: Vec::new(),
        },
        "items"
    ));
    assert!(lowerer.rvalue_writes_place(
        &Rvalue::Call {
            callee: CallTarget::Name("borrow_items".to_string()),
            args: vec![MirArg {
                name: None,
                value: Operand::Place("items".to_string()),
                writeback_place: Some("items.length".to_string()),
            }],
        },
        "items"
    ));
    assert!(!lowerer.rvalue_writes_place(&Rvalue::Use(Operand::Unit), "items"));
}

#[test]
fn lower_source_to_mir_covers_broad_control_flow_and_collection_surface() {
    let source = r#"
trait Named:
    def name(self) -> str

class User:
    label: str

impl Named for User:
    def name(self) -> str:
        return self.label.clone()

class Resource:
    closed: bool = false
    def close(mut self):
        self.closed = true

class Counter:
    value: int32

enum Boxed:
    Filled(int32)
    Empty

def worker(value: int32) -> int32:
    return value + 1

def consume[T: Named](value: T) -> str:
    return value.name()

def first_mut(values: own list[int32]) -> int32:
    mut local = values
    for item in mut local:
        return item
    return 0

def main() -> int32:
    mut counter = Counter(value=0)
    positional = Counter(2)
    counter.value += positional.value
    mut values: list[int32] = [1, 2]
    values[0] = 3
    values[0] += 4
    mut counts: dict[str, int32] = {"a": 1}
    counts["b"] = 2
    counts["a"] += 5
    seen = {"a", "b"}
    jobs = Queue[int32]()
    jobs.put(1)
    if true and not false:
        counter.value += values[0]
    match "ok":
        case "ok":
            counter.value += 1
        case _:
            pass
    for i in range(2):
        counter.value += i as int32
    for item in values:
        counter.value += item
    while counter.value < 10:
        break
    match jobs.get(timeout=0ms):
        case QueueReceive.Item(value):
            print(value)
        case QueueReceive.TimedOut:
            counter.value += 10
        case QueueReceive.Closed:
            pass
        case QueueReceive.Cancelled:
            pass
    jobs.close()
    with Resource() as resource:
        print(resource.closed)
    print(consume(value=User(label="aura")))
    with TaskGroup() as group:
        task = group.start(worker, counter.value)
        print(task.result())
    print("a" in seen)
    print(counts.get("a"))
    mut boxed = Boxed.Filled(3)
    counter.value += match mut boxed:
        case Filled(v): v + 1
        case Empty: 0
    counter.value += first_mut([4, 5])
    return counter.value
"#;

    let module = crate::lower_source_to_mir(source).expect("source should lower");
    assert!(function_names(&module).contains("main"));
    assert!(module
        .trait_impls
        .iter()
        .any(|impl_info| impl_info.trait_name == "Named"));
    let mut saw_task_start = false;
    let mut saw_vec_literal = false;
    let mut saw_set_literal = false;
    let mut saw_map_literal = false;
    for function in module.functions.iter().chain(module.top_level.iter()) {
        for block in &function.blocks {
            for instruction in &block.instructions {
                if let Instruction::Assign { value, .. } = instruction {
                    match value {
                        Rvalue::StartTask { .. } => saw_task_start = true,
                        Rvalue::VecLiteral { .. } => saw_vec_literal = true,
                        Rvalue::SetLiteral { .. } => saw_set_literal = true,
                        Rvalue::MapLiteral { .. } => saw_map_literal = true,
                        _ => {}
                    }
                }
            }
        }
    }
    assert!(saw_task_start);
    assert!(saw_vec_literal);
    assert!(saw_set_literal);
    assert!(saw_map_literal);
}

#[test]
fn indexed_compound_assignment_results_keep_the_collection_element_type() {
    let source = r#"
def main() -> int32:
    mut values: list[int32] = [-7]
    values[0] //= 3
    values[0] %= -3
    mut counts: dict[str, int32] = {"left": -7}
    counts["left"] //= 3
    counts["left"] %= -3
    return 0
"#;

    let module = crate::lower_source_to_mir(source).expect("source should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should be present in MIR");
    let local_types = main
        .local_types
        .iter()
        .map(|local| (local.name.as_str(), &local.ty))
        .collect::<BTreeMap<_, _>>();
    let result_types = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::Binary {
                        op: BinaryOp::FloorDiv | BinaryOp::Mod,
                        ..
                    },
            } => local_types.get(target.as_str()).copied(),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        result_types,
        vec![
            &Type::named("int32"),
            &Type::named("int32"),
            &Type::named("int32"),
            &Type::named("int32"),
        ],
        "Vec and Map compound results must retain their indexed int32 element type",
    );

    let negative_rhs_types = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Binary {
                        op: BinaryOp::Mod,
                        right: Operand::Place(place),
                        ..
                    },
                ..
            } => local_types.get(place.as_str()).copied(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        negative_rhs_types,
        vec![&Type::named("int32"), &Type::named("int32")],
        "contextual negative RHS values must retain the indexed int32 element type",
    );
}

#[test]
fn lowerer_constructor_inference_and_for_fallback_cover_unchecked_edges() {
    let program = Box::leak(Box::new(checked_program(
        "\
class Pair[A, B]:
    first: A
    second: B

def main() -> int32:
    return 0
",
    )));
    let mut lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::new(),
    );

    assert_eq!(
        lowerer.infer_class_constructor_type(
            "Pair",
            &[
                arg(expr(ExprKind::String("left".to_string()))),
                arg(expr(ExprKind::Int(1)))
            ],
            None,
        ),
        Some(Type::Named(
            "Pair".to_string(),
            vec![Type::named("str"), Type::named("int64")]
        ))
    );
    assert_eq!(
        lowerer.infer_class_constructor_type(
            "Pair",
            &[
                named_arg("first", expr(ExprKind::String("left".to_string()))),
                arg(expr(ExprKind::Int(1))),
            ],
            None,
        ),
        Some(Type::named("Pair"))
    );
    assert_eq!(
        lowerer.infer_class_constructor_type(
            "Pair",
            &[],
            Some(&[type_ref("str"), type_ref("int32")])
        ),
        Some(Type::Named(
            "Pair".to_string(),
            vec![Type::named("str"), Type::named("int32")]
        ))
    );
    assert_eq!(
        lowerer.infer_class_constructor_type("Pair", &[], None),
        Some(Type::named("Pair"))
    );

    lowerer.lower_for(&ForStmt {
        target: BindingTarget::Name {
            name: "item".to_string(),
            span: Span::new(1, 1),
        },
        iterable: expr(ExprKind::Bool(true)),
        borrow_mode: None,
        body: vec![Stmt::Pass(PassStmt {
            span: Span::new(1, 1),
        })],
        span: Span::new(1, 1),
    });
    let fallback_binding = lowerer
        .blocks
        .iter()
        .find_map(|block| match &block.terminator {
            Some(Terminator::ForRange { binding, .. }) => Some(binding),
            _ => None,
        })
        .expect("unchecked fallback iteration should still lower");
    assert!(fallback_binding.starts_with("%t"));
    assert_eq!(
        lowerer.local_types.get(fallback_binding),
        Some(&Type::named("int64"))
    );

    let mut return_lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::new(),
    );
    return_lowerer.local_types.insert(
        "items".to_string(),
        Type::Named("list".to_string(), vec![Type::named("int32")]),
    );
    let parent_return_block = return_lowerer.new_block("parent_return");
    let parent_return_label = return_lowerer.label(parent_return_block);
    let parent_return_place = return_lowerer.new_typed_temp(Type::named("int32"));
    return_lowerer.return_redirects.push(ReturnRedirect {
        label: parent_return_label.clone(),
        return_place: parent_return_place.clone(),
        cleanup_depth: 0,
    });
    return_lowerer.lower_for(&ForStmt {
        target: BindingTarget::Name {
            name: "item".to_string(),
            span: Span::new(1, 1),
        },
        iterable: name_expr("items"),
        borrow_mode: Some(ReceiverKind::BorrowMut),
        body: vec![Stmt::Return(crate::ast::ReturnStmt {
            value: Some(name_expr("item")),
            view: None,
            span: Span::new(1, 1),
        })],
        span: Span::new(1, 1),
    });
    return_lowerer.return_redirects.pop();
    assert!(return_lowerer.blocks.iter().any(|block| matches!(
        block.terminator,
        Some(Terminator::Goto(ref label)) if label == &parent_return_label
    )));
    assert!(return_lowerer.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    target,
                    value: Rvalue::Use(Operand::Place(_)),
                } if target == &parent_return_place
            )
        })
    }));

    let indexed_target = AssignTarget::Index {
        object: Box::new(name_expr("items")),
        index: Box::new(expr(ExprKind::Int(0))),
    };
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let indexed_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lowerer.render_assign_target(&indexed_target);
    }));
    std::panic::set_hook(previous_hook);
    assert!(
        indexed_panic.is_err(),
        "indexed assignments should lower through helper calls before rendering"
    );
}

#[test]
fn lowerer_direct_collection_literals_cover_uninferred_set_and_map_exprs() {
    let program = Box::leak(Box::new(checked_program(
        "def main() -> int32:\n    return 0\n",
    )));
    let mut lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::new(),
    );

    let set_operand = lowerer.lower_expr(&expr(ExprKind::Set(vec![
        expr(ExprKind::String("a".to_string())),
        expr(ExprKind::String("b".to_string())),
    ])));
    let map_operand = lowerer.lower_expr(&expr(ExprKind::Map(vec![MapEntryExpr {
        key: expr(ExprKind::String("a".to_string())),
        value: expr(ExprKind::Int(1)),
    }])));
    let empty_list_operand = lowerer.lower_expr(&expr(ExprKind::List(Vec::new())));
    let empty_set_operand = lowerer.lower_expr(&expr(ExprKind::Set(Vec::new())));
    let empty_map_operand = lowerer.lower_expr(&expr(ExprKind::Map(Vec::new())));
    let malformed_vec_constructor = lowerer.lower_expr(&expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Specialize {
            expr: Box::new(name_expr("list")),
            type_args: Vec::new(),
        })),
        args: Vec::new(),
    }));
    let malformed_map_constructor = lowerer.lower_expr(&expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Specialize {
            expr: Box::new(name_expr("dict")),
            type_args: Vec::new(),
        })),
        args: Vec::new(),
    }));

    assert!(matches!(set_operand, Operand::Place(_)));
    assert!(matches!(map_operand, Operand::Place(_)));
    assert!(matches!(empty_list_operand, Operand::Place(_)));
    assert!(matches!(empty_set_operand, Operand::Place(_)));
    assert!(matches!(empty_map_operand, Operand::Place(_)));
    assert!(matches!(malformed_vec_constructor, Operand::Place(_)));
    assert!(matches!(malformed_map_constructor, Operand::Place(_)));
    let instructions = lowerer.blocks[lowerer.current_block]
        .instructions
        .iter()
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::SetLiteral {
                element_type,
                elements,
            },
            ..
        } if element_type == &Type::named("str") && elements.len() == 2
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::MapLiteral {
                key_type,
                value_type,
                entries,
            },
            ..
        } if key_type == &Type::named("str")
            && value_type == &Type::named("int64")
            && entries.len() == 1
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::VecLiteral {
                element_type,
                elements,
            },
            ..
        } if element_type == &Type::named("Unknown") && elements.is_empty()
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::SetLiteral {
                element_type,
                elements,
            },
            ..
        } if element_type == &Type::named("Unknown") && elements.is_empty()
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::MapLiteral {
                key_type,
                value_type,
                entries,
            },
            ..
        } if key_type == &Type::named("Unknown")
            && value_type == &Type::named("Unknown")
            && entries.is_empty()
    )));
}

#[test]
fn lowerer_direct_pattern_helpers_cover_defensive_variant_and_literal_edges() {
    let program = Box::leak(Box::new(checked_program(
        "\
enum Maybe:
    Some(int32)
    Empty

def main() -> int32:
    return 0
",
    )));
    let mut lowerer = Lowerer::new(
        program,
        "main",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::new(),
    );
    lowerer.scoped_names.push(std::collections::HashMap::new());

    let binding_success = lowerer.new_block("binding_success");
    let binding_failure = lowerer.new_block("binding_failure");
    let binding_writeback = lowerer.lower_pattern(
        &binding_pattern("item"),
        Operand::Int(7),
        None,
        binding_success,
        binding_failure,
        PatternLoweringOptions {
            collect_writeback: true,
            consume_payloads: false,
        },
    );
    let PatternWriteback::Use(Operand::Place(binding_place)) =
        binding_writeback.expect("binding patterns should produce writeback")
    else {
        panic!("binding pattern should write back the generated place");
    };
    assert_eq!(
        lowerer
            .scoped_names
            .last()
            .and_then(|scope| scope.get("item")),
        Some(&binding_place)
    );
    assert!(
        !lowerer.local_types.contains_key(&binding_place),
        "untyped defensive pattern lowering should allocate an untyped temp"
    );

    let mismatch_entry = lowerer.new_block("mismatch_entry");
    let mismatch_success = lowerer.new_block("mismatch_success");
    let mismatch_failure = lowerer.new_block("mismatch_failure");
    lowerer.switch_to(mismatch_entry);
    let mismatched = lowerer.lower_pattern(
        &variant_pattern(Some("Maybe"), "Some", Vec::new()),
        Operand::Place("candidate".to_string()),
        Some(&Type::named("Maybe")),
        mismatch_success,
        mismatch_failure,
        PatternLoweringOptions {
            collect_writeback: true,
            consume_payloads: false,
        },
    );
    assert!(mismatched.is_none());
    assert!(matches!(
        lowerer.blocks[lowerer.current_block].terminator,
        Some(Terminator::Goto(ref label)) if label == &lowerer.label(mismatch_failure)
    ));

    let unknown_entry = lowerer.new_block("unknown_entry");
    let unknown_success = lowerer.new_block("unknown_success");
    let unknown_failure = lowerer.new_block("unknown_failure");
    lowerer.switch_to(unknown_entry);
    let unknown_writeback = lowerer.lower_pattern(
        &variant_pattern(None, "Some", vec![Pattern::Wildcard(Span::new(1, 1))]),
        Operand::Place("unknown".to_string()),
        None,
        unknown_success,
        unknown_failure,
        PatternLoweringOptions {
            collect_writeback: true,
            consume_payloads: false,
        },
    );
    let PatternWriteback::Variant { ty, payloads, .. } =
        unknown_writeback.expect("unknown variant lowering should produce a writeback")
    else {
        panic!("variant pattern should write back a reconstructed variant");
    };
    assert_eq!(ty, Type::named("Unknown"));
    assert!(matches!(
        payloads.as_slice(),
        [PatternWriteback::Use(Operand::Place(_))]
    ));

    let unit_variant_entry = lowerer.new_block("unit_variant_entry");
    let unit_variant_success = lowerer.new_block("unit_variant_success");
    let unit_variant_failure = lowerer.new_block("unit_variant_failure");
    lowerer.switch_to(unit_variant_entry);
    let unit_variant_writeback = lowerer.lower_pattern(
        &variant_pattern(Some("Maybe"), "Empty", Vec::new()),
        Operand::Place("unknown".to_string()),
        None,
        unit_variant_success,
        unit_variant_failure,
        PatternLoweringOptions {
            collect_writeback: true,
            consume_payloads: false,
        },
    );
    let PatternWriteback::Variant { ty, payloads, .. } =
        unit_variant_writeback.expect("unit variant pattern should produce a writeback")
    else {
        panic!("unit variant pattern should write back a reconstructed variant");
    };
    assert_eq!(ty, Type::named("Unknown"));
    assert!(payloads.is_empty());

    let positive = lowerer.lower_literal_pattern_operand(
        None,
        &LiteralPatternKind::Int(IntegerValue::from_signed(5)),
        Span::new(1, 1),
    );
    assert_eq!(positive, Operand::Int(5));

    let negative = lowerer.lower_literal_pattern_operand(
        Some(&Type::named("int32")),
        &LiteralPatternKind::Int(IntegerValue::from_signed(-5)),
        Span::new(1, 1),
    );
    assert!(matches!(negative, Operand::Place(_)));
    let negative_unknown = lowerer.lower_literal_pattern_operand(
        None,
        &LiteralPatternKind::Int(IntegerValue::from_signed(-7)),
        Span::new(1, 1),
    );
    assert!(matches!(negative_unknown, Operand::Place(_)));

    let literal_entry = lowerer.new_block("literal_entry");
    let literal_success = lowerer.new_block("literal_success");
    let literal_failure = lowerer.new_block("literal_failure");
    lowerer.switch_to(literal_entry);
    let literal_writeback = lowerer.lower_pattern(
        &Pattern::Literal(LiteralPattern {
            kind: LiteralPatternKind::Int(IntegerValue::from_signed(2)),
            span: Span::new(1, 1),
        }),
        Operand::Int(2),
        Some(&Type::named("int32")),
        literal_success,
        literal_failure,
        PatternLoweringOptions {
            collect_writeback: true,
            consume_payloads: false,
        },
    );
    assert!(matches!(
        literal_writeback,
        Some(PatternWriteback::Use(Operand::Int(2)))
    ));
}

#[test]
fn lower_path_to_mir_covers_imported_module_surface() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate should live under repo root")
        .parent()
        .expect("compiler crate should live under repo root")
        .to_path_buf();
    let path = repo_root.join("examples/modules/trait_impl_imports.au");
    let module = crate::lower_path_to_mir(&path).expect("example should lower");

    assert!(module
        .functions
        .iter()
        .any(|function| function.name == "show"));
    assert!(module
        .classes
        .iter()
        .any(|class| class.name == "pkg.user.User"));
    assert!(module
        .trait_impls
        .iter()
        .any(|impl_info| impl_info.trait_name == "Named"));
    assert!(module.top_level.is_none());
}

#[test]
fn mir_functions_preserve_defining_paths_with_source_only_fallback_remaining_absent() {
    let source_only = crate::lower_source_to_mir(
        "def helper() -> int32:\n    return 1\n\ndef main() -> int32:\n    return helper()\n",
    )
    .expect("source-only program should lower");
    assert!(
        source_only
            .functions
            .iter()
            .all(|function| function.source_path.is_none()),
        "source-only lowering should leave frame paths for the runtime caller to supply"
    );

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate should live under repo root")
        .parent()
        .expect("compiler crate should live under repo root")
        .to_path_buf();
    let entry_path = repo_root.join("examples/modules/trait_impl_imports.au");
    let imported_path = repo_root.join("examples/modules/pkg/user.au");
    let module = crate::lower_path_to_mir(&entry_path).expect("module program should lower");

    for name in ["show", "main"] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("entry function should lower");
        assert_eq!(
            function.source_path.as_deref(),
            Some(entry_path.to_string_lossy().as_ref())
        );
    }
    let imported_methods = module
        .functions
        .iter()
        .filter(|function| {
            function.source_path.as_deref() == Some(imported_path.to_string_lossy().as_ref())
        })
        .collect::<Vec<_>>();
    assert!(
        imported_methods
            .iter()
            .any(|function| function.name.ends_with("name")),
        "an imported User.name method should retain `{}`; lowered functions were {:?}",
        imported_path.display(),
        module
            .functions
            .iter()
            .map(|function| (&function.name, &function.module_name, &function.source_path))
            .collect::<Vec<_>>()
    );
}

#[test]
fn imported_generic_rng_holders_keep_distinct_canonical_mir_identities() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/run-pass/imported_same_leaf_class_identity.au");
    let module = crate::lower_path_to_mir(&path)
        .expect("same-leaf generic Rng holders should lower with module provenance");

    let rng = Type::named("random.Rng");
    let local_holder = Type::Named("Holder".to_string(), vec![rng.clone()]);
    let remote_holder = Type::Named(
        "same_leaf_support.remote.Holder".to_string(),
        vec![rng.clone()],
    );
    let local_envelope = Type::Named("Envelope".to_string(), vec![Type::named("random.Rng")]);
    let remote_envelope = Type::Named(
        "same_leaf_support.remote.Envelope".to_string(),
        vec![Type::named("random.Rng")],
    );
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("fixture main should lower");

    assert!(
        main.local_types
            .iter()
            .any(|local| local.ty == local_holder),
        "local types: {:#?}",
        main.local_types
    );
    assert!(
        main.local_types
            .iter()
            .any(|local| local.ty == remote_holder),
        "local types: {:#?}",
        main.local_types
    );
    assert!(main
        .local_types
        .iter()
        .any(|local| local.ty == local_envelope));
    assert!(main
        .local_types
        .iter()
        .any(|local| local.ty == remote_envelope));
    assert_eq!(
        main.local_types
            .iter()
            .find(|local| local.name == "bridge_holder")
            .map(|local| &local.ty),
        Some(&remote_holder)
    );
    assert_eq!(
        main.local_types
            .iter()
            .find(|local| local.name == "bridge_envelope")
            .map(|local| &local.ty),
        Some(&remote_envelope)
    );
    assert_eq!(
        main.local_types
            .iter()
            .find(|local| local.name == "bridge_empty_envelope")
            .map(|local| &local.ty),
        Some(&remote_envelope)
    );
    assert!(module.classes.iter().any(|class| class.name == "Holder"));
    assert!(module
        .classes
        .iter()
        .any(|class| class.name == "same_leaf_support.remote.Holder"));
    assert!(module
        .trait_impls
        .iter()
        .any(|trait_impl| trait_impl.for_type
            == Type::Named(
                "same_leaf_support.remote.Holder".to_string(),
                vec![Type::TypeParam("T".to_string())],
            )));
    assert!(module
        .functions
        .iter()
        .any(|function| function.name == "same_leaf_support.remote::Holder.source"));

    let bridge_holder = module
        .functions
        .iter()
        .find(|function| function.name == "same_leaf_support.bridge::make_holder")
        .expect("transitive holder factory should lower");
    assert_eq!(bridge_holder.return_type, remote_holder);
    assert!(bridge_holder.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Construct { class_name, .. },
                    ..
                } if class_name == "same_leaf_support.remote.Holder"
            )
        })
    }));

    for function_name in ["make_envelope", "make_empty_envelope"] {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == format!("same_leaf_support.bridge::{function_name}"))
            .expect("transitive enum factory should lower");
        assert_eq!(function.return_type, remote_envelope);
        assert!(function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::EnumVariant { enum_name, .. },
                        ..
                    } if enum_name == "same_leaf_support.remote.Envelope"
                )
            })
        }));
    }

    let enum_names = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::EnumVariant { enum_name, .. },
                ..
            } => Some(enum_name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        enum_names.contains(&"Envelope"),
        "enum names: {enum_names:?}"
    );
    assert!(
        enum_names.contains(&"same_leaf_support.remote.Envelope"),
        "enum names: {enum_names:?}"
    );
}

#[test]
fn contextual_none_equality_lowers_none_as_option_variants() {
    let source = include_str!("../tests/fixtures/run-pass/contextual_none_equality.au");
    let module = crate::lower_source_to_mir(source).expect("contextual None source should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should be lowered");
    let contextual_none_count = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::EnumVariant {
                        enum_name,
                        variant_name,
                        payloads,
                    },
                    ..
                } if enum_name == "Option" && variant_name == "None" && payloads.is_empty()
            )
        })
        .count();

    assert_eq!(contextual_none_count, 12);
}

#[test]
fn integer_call_equality_keeps_each_call_temporary_at_its_declared_type() {
    let source = include_str!("../tests/fixtures/run-pass/integer_call_equality.au");
    let module = crate::lower_source_to_mir(source).expect("integer equality source should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should be lowered");
    let local_types = main
        .local_types
        .iter()
        .map(|local| (local.name.as_str(), &local.ty))
        .collect::<std::collections::HashMap<_, _>>();

    let call_temporaries = main
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::Call {
                        callee: CallTarget::Name(name),
                        ..
                    },
            } if matches!(name.as_str(), "signed_value" | "unsigned_value") => {
                Some((target.as_str(), name.as_str()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(!call_temporaries.is_empty());
    for (temporary, function) in call_temporaries {
        let expected = Type::named(if function == "signed_value" {
            "int32"
        } else {
            "uint64"
        });
        assert_eq!(
            local_types.get(temporary).copied(),
            Some(&expected),
            "{function} call temporary {temporary} must keep its declared return type"
        );
    }
}

#[test]
fn assertions_lower_to_lazy_failure_blocks_with_keyword_spans() {
    let source = r#"def main() -> int32:
    assert true
    assert false, "  exact message  "
    return 0
"#;
    let module = crate::lower_source_to_mir(source).expect("assertions should lower to MIR");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should be lowered");

    let branches = main
        .blocks
        .iter()
        .filter(|block| matches!(block.terminator, Terminator::Branch { .. }))
        .count();
    assert_eq!(
        branches, 2,
        "each assertion must branch before its lazy failure message"
    );

    let failures = main
        .blocks
        .iter()
        .filter_map(|block| match &block.terminator {
            Terminator::AssertFail { message, span, .. } => Some((message, span)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0], (&None, &Span::new(2, 5)));
    assert_eq!(
        failures[1],
        (
            &Some(Operand::String("  exact message  ".to_string())),
            &Span::new(3, 5)
        )
    );
}

#[test]
fn conditional_expressions_lower_to_a_typed_branch_and_join() {
    let source = r#"
def condition() -> bool:
    return true

def left() -> int32:
    return 10

def right() -> int32:
    return 20

def choose() -> int32:
    return left() if condition() else right()

def main() -> int32:
    return choose()
"#;
    let module =
        crate::lower_source_to_mir(source).expect("conditional expression should lower to MIR");
    let choose = module
        .functions
        .iter()
        .find(|function| function.name == "choose")
        .expect("choose should be lowered");

    let branch = choose
        .blocks
        .iter()
        .find_map(|block| match &block.terminator {
            Terminator::Branch {
                then_label,
                else_label,
                ..
            } => Some((then_label, else_label)),
            _ => None,
        })
        .expect("conditional should lower to one branch");
    assert!(branch.0.contains("conditional_then"));
    assert!(branch.1.contains("conditional_else"));
    assert!(choose
        .blocks
        .iter()
        .any(|block| block.label.contains("conditional_join")));
    assert!(choose
        .local_types
        .iter()
        .any(|local| { local.ty == Type::named("int32") && local.name.starts_with("%t") }));
}

#[test]
fn owned_conditional_values_move_only_the_selected_arm_into_the_join() {
    let source = r#"
def choose(flag: bool, left: own str, right: own str) -> str:
    selected = left if flag else right
    return selected
"#;
    let module =
        crate::lower_source_to_mir(source).expect("owned conditional expression should lower");
    let choose = module
        .functions
        .iter()
        .find(|function| function.name == "choose")
        .expect("choose should be lowered");

    let moved_places = choose
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Use(Operand::MovePlace(place)),
                ..
            } => Some(place.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        moved_places
            .iter()
            .filter(|place| **place == "left" || **place == "right")
            .copied()
            .collect::<Vec<_>>(),
        vec!["left", "right"],
        "each branch must transfer its own source into the shared result"
    );
    assert!(
        choose.blocks.iter().any(|block| {
            matches!(
                &block.terminator,
                Terminator::Return(Operand::MovePlace(place)) if place == "selected"
            )
        }),
        "returning the selected str must transfer the joined value"
    );
}

#[test]
fn conditional_result_inference_uses_the_concrete_arm_in_either_position() {
    let source = r#"
def make_values() -> list[int32]:
    return [7]

def choose(flag: bool, exact: float32):
    integer_left = 1 if flag else exact
    integer_right = exact if flag else 1
    float_left = 1.5 if flag else exact
    float_right = exact if flag else 1.5
    empty_left = [] if flag else make_values()
    empty_right = make_values() if flag else []
    none_left = None if flag else Option.Some(7)
    none_right = Option.Some(7) if flag else None
    nested_empty = ([], 1) if flag else (make_values(), 2)
"#;
    let module = crate::lower_source_to_mir(source)
        .expect("conditional arms should inherit the concrete peer type");
    let choose = module
        .functions
        .iter()
        .find(|function| function.name == "choose")
        .expect("choose should be lowered");

    let expected = [
        ("integer_left", Type::named("float32")),
        ("integer_right", Type::named("float32")),
        ("float_left", Type::named("float32")),
        ("float_right", Type::named("float32")),
        (
            "empty_left",
            Type::Named("list".to_string(), vec![Type::named("int32")]),
        ),
        (
            "empty_right",
            Type::Named("list".to_string(), vec![Type::named("int32")]),
        ),
        (
            "none_left",
            Type::Named("Option".to_string(), vec![Type::named("int64")]),
        ),
        (
            "none_right",
            Type::Named("Option".to_string(), vec![Type::named("int64")]),
        ),
        (
            "nested_empty",
            Type::Tuple(vec![
                Type::Named("list".to_string(), vec![Type::named("int32")]),
                Type::named("int64"),
            ]),
        ),
    ];
    for (name, ty) in expected {
        assert_eq!(
            choose
                .local_types
                .iter()
                .find(|local| local.name == name)
                .map(|local| &local.ty),
            Some(&ty),
            "{name} should keep the concrete conditional result type"
        );
    }
}

#[test]
fn mir_function_value_lowering_preserves_defaults_and_capabilities() {
    let module = crate::lower_source_to_mir(
        r#"
class Counter:
    value: int32

def increment(counter: mut Counter) -> None:
    counter.value += 1

def consume(value: own str) -> str:
    return value

def mark(label: str, value: int32) -> int32:
    print(label)
    return value

def with_default(value: int32 = mark("fresh-default", 40)) -> int32:
    return value + 2

def choose(use_default: bool) -> def(int32) -> int32:
    return with_default if use_default else with_default

def main():
    mut counter = Counter(value=0)
    text = "owned"
    mutator = increment
    consumer = consume
    selected = with_default
    dynamic = choose(true)
    mutator(counter)
    print(consumer(text))
    print(selected())
    print(dynamic(1))
"#,
    )
    .expect("function-value capabilities and defaults should lower");

    let with_default = module
        .functions
        .iter()
        .find(|function| function.name == "with_default")
        .expect("the selected function should lower");
    assert_eq!(
        with_default.params[0].default_function.as_deref(),
        Some("with_default::__default_0_value"),
        "the declaration must retain the helper that evaluates its default freshly"
    );
    let default = module
        .functions
        .iter()
        .find(|function| function.name == "with_default::__default_0_value")
        .expect("a default expression should become a callable MIR helper");
    assert!(
        default.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Assign {
                        value: Rvalue::Call {
                            callee: CallTarget::Name(name),
                            ..
                        },
                        ..
                    } if name == "mark"
                )
            })
        }),
        "the helper must preserve the default expression rather than embedding one shared value"
    );

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    let literal_contract = |target: &str| {
        instructions
            .iter()
            .find_map(|instruction| match instruction {
                Instruction::Assign {
                    value:
                        Rvalue::Use(Operand::Function {
                            name, signature, ..
                        }),
                    ..
                } if name == target => Some(signature.as_ref()),
                _ => None,
            })
    };
    let Type::Function {
        params,
        return_type,
    } = literal_contract("increment").expect("increment should materialize as a function value")
    else {
        panic!("increment should carry a function signature");
    };
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "counter");
    assert_eq!(params[0].passing, ReceiverKind::BorrowMut);
    assert_eq!(params[0].ty, Type::named("Counter"));
    assert_eq!(return_type.as_ref(), &Type::Unit);

    let Type::Function { params, .. } =
        literal_contract("with_default").expect("with_default should materialize as a value")
    else {
        panic!("with_default should carry a function signature");
    };
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "value");
    assert!(params[0].has_default);
    assert!(!params[0].default_erased);

    let indirect_args = instructions
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Call {
                        callee: CallTarget::Value(_),
                        args,
                    },
                ..
            } => Some(args),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        indirect_args.iter().any(|args| {
            args.len() == 1
                && args[0].value == Operand::Place("counter".to_string())
                && args[0].writeback_place.as_deref() == Some("counter")
        }),
        "a mut function value must borrow the source and retain its writeback place"
    );
    assert!(
        indirect_args.iter().any(|args| {
            args.len() == 1
                && args[0].value == Operand::MovePlace("text".to_string())
                && args[0].writeback_place.is_none()
        }),
        "an own function value must consume its source without inventing a writeback"
    );
    assert!(
        indirect_args.iter().any(|args| args.is_empty()),
        "an omitted dynamic argument must remain omitted for target-owned default binding"
    );
    let dynamic_capture = instructions
        .iter()
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value: Rvalue::Use(Operand::Place(place)),
            } if place == "dynamic" => Some(target.as_str()),
            _ => None,
        })
        .expect("the runtime-selected local should be captured at a sequence point");
    assert!(
        instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Value(Operand::Place(place)),
                        args,
                    },
                    ..
                } if place == dynamic_capture
                    && args.len() == 1
                    && args[0].writeback_place.is_none()
            )
        }),
        "the captured runtime-selected local must become the indirect callee"
    );
}

#[test]
fn mir_function_value_helpers_preserve_nested_types_and_imported_specialization() {
    let contract = |ty: Type, passing: ReceiverKind| crate::sema::FunctionParamContract {
        name: String::new(),
        ty,
        passing,
        has_default: false,
        default_erased: true,
    };
    let nested = Type::Function {
        params: vec![contract(
            Type::TypeParam("T".to_string()),
            ReceiverKind::BorrowMut,
        )],
        return_type: Box::new(Type::Function {
            params: vec![contract(Type::named("str"), ReceiverKind::Borrow)],
            return_type: Box::new(Type::TypeParam("U".to_string())),
        }),
    };
    let mut type_params = BTreeSet::new();
    collect_type_params_from_type(&nested, &mut type_params);
    assert_eq!(
        type_params,
        BTreeSet::from(["T".to_string(), "U".to_string()]),
        "type-parameter discovery must descend through function parameters and returns"
    );

    assert!(!type_contains_unknown(&nested));
    assert!(type_contains_unknown(&Type::Function {
        params: vec![contract(Type::named("Unknown"), ReceiverKind::Borrow)],
        return_type: Box::new(Type::named("bool")),
    }));
    assert!(type_contains_unknown(&Type::Function {
        params: Vec::new(),
        return_type: Box::new(Type::named("Unknown")),
    }));
    let closure_with_unresolved_capture = Type::Closure {
        params: Box::new(vec![contract(
            Type::TypeParam("T".to_string()),
            ReceiverKind::Borrow,
        )]),
        return_type: Box::new(Type::TypeParam("U".to_string())),
        captures: Box::new(vec![crate::sema::ClosureCapture {
            name: "environment".to_string(),
            ty: Type::Named("Option".to_string(), vec![Type::named("Unknown")]),
            mode: crate::sema::ClosureCaptureMode::Copy,
            span: Span::new(1, 1),
        }]),
        call_kind: crate::sema::ClosureCallKind::Repeatable,
    };
    let mut closure_type_params = BTreeSet::new();
    collect_type_params_from_type(&closure_with_unresolved_capture, &mut closure_type_params);
    assert_eq!(
        closure_type_params,
        BTreeSet::from(["T".to_string(), "U".to_string()]),
        "closure type-parameter discovery must traverse parameters, captures, and returns"
    );
    assert!(
        type_contains_unknown(&closure_with_unresolved_capture),
        "conditional-result inference must not hide unresolved types inside closure environments"
    );

    let source_type = TypeRef::function_with_params(
        vec![
            crate::ast::FunctionTypeParam::new(
                crate::ast::ParamMode::BorrowMut,
                type_ref("int32"),
                Span::new(1, 1),
            ),
            crate::ast::FunctionTypeParam::new(
                crate::ast::ParamMode::Own,
                type_ref("str"),
                Span::new(1, 1),
            ),
        ],
        type_ref("bool"),
        Span::new(1, 1),
    );
    assert_eq!(
        lower_type_ref(&source_type),
        Type::Function {
            params: vec![
                contract(Type::named("int32"), ReceiverKind::BorrowMut),
                contract(Type::named("str"), ReceiverKind::Value),
            ],
            return_type: Box::new(Type::named("bool")),
        },
        "written function types must retain parameter capabilities while erasing names/defaults"
    );

    let lowerer = lowerer_with_imported_modules();
    let specialized = expr(ExprKind::Specialize {
        expr: Box::new(expr(ExprKind::Group(Box::new(name_expr("generic_helper"))))),
        type_args: vec![type_ref("str")],
    });
    let operand = lowerer
        .lower_function_value(&specialized)
        .expect("an imported generic function should specialize as a value");
    let expected = Type::Function {
        params: vec![crate::sema::FunctionParamContract {
            name: "value".to_string(),
            ty: Type::named("str"),
            passing: ReceiverKind::Value,
            has_default: false,
            default_erased: false,
        }],
        return_type: Box::new(Type::named("str")),
    };
    assert_eq!(
        operand,
        Operand::Function {
            name: "pkg.helpers::generic_helper".to_string(),
            signature: Box::new(expected.clone()),
        },
        "a bare imported sibling must keep its defining-module runtime identity"
    );
    assert_eq!(
        lowerer.infer_operand_type(&operand),
        Some(expected),
        "function operands must expose their concrete specialized signature"
    );
}

#[test]
fn list_algorithms_lower_callbacks_to_ordinary_indirect_calls_before_mutation() {
    let module = crate::lower_source_to_mir(
        r#"
def key(value: int32) -> int32:
    return 10 - value

def double(value: int32) -> int32:
    return value * 2

def even(value: int32) -> bool:
    return value % 2 == 0

def main():
    mut values: list[int32] = [3, 1, 2]
    values.sort(key = key)
    mapped = values.map(double)
    filtered = values.filter(even)
    print(mapped)
    print(filtered)
"#,
    )
    .expect("list algorithms should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    let callback_calls = instructions
        .iter()
        .filter(|instruction| {
            matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Value(_),
                        args,
                    },
                    ..
                } if args.len() == 1
            )
        })
        .count();
    assert_eq!(
        callback_calls, 3,
        "sort, map, and filter should each have one ordinary dynamic call site"
    );
    assert!(
        instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Member { field, .. },
                        ..
                    },
                    ..
                } if matches!(field.as_str(), "sort" | "map" | "filter")
            )
        }),
        "list algorithms should not introduce a second host-only callback ABI"
    );

    let callback_blocks = main
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            block
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        Instruction::Assign {
                            value: Rvalue::Call {
                                callee: CallTarget::Value(_),
                                ..
                            },
                            ..
                        }
                    )
                })
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let first_source_swap = main
        .blocks
        .iter()
        .enumerate()
        .find_map(|(index, block)| {
            block
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        Instruction::Assign {
                            value: Rvalue::Call {
                                callee:
                                    CallTarget::Member {
                                        field,
                                        receiver_place: Some(place),
                                        ..
                                    },
                                ..
                            },
                            ..
                        } if field == "swap" && place == "values"
                    )
                })
                .then_some(index)
        })
        .expect("sort should mutate its source only in the sorting phase");
    assert!(
        callback_blocks
            .first()
            .is_some_and(|callback| *callback < first_source_swap),
        "sort key extraction must precede the first source mutation"
    );
}

#[test]
fn list_algorithms_execute_stably_in_order_and_preserve_their_shared_source() {
    let module = crate::lower_source_to_mir(
        r#"
class Box:
    value: int32

def descending_key(value: int32) -> int32:
    print(value)
    return 10 - value

def box_double(value: int32) -> Box:
    return Box(value=value * 2)

def even(value: int32) -> bool:
    print(value)
    return value % 2 == 0

def main():
    mut plain: list[int32] = [3, 1, 2, 2]
    plain.sort()
    print(plain)

    mut keyed: list[int32] = [3, 1, 2, 2]
    keyed.sort(key = descending_key)
    print(keyed)

    boxes = plain.map(box_double)
    for box in own boxes:
        print(box.value)

    filtered = plain.filter(even)
    print(plain)
    print(filtered)
"#,
    )
    .expect("Vec algorithms should lower");
    let output = crate::run_mir(&module).expect("Vec algorithms should execute through MIR");
    assert_eq!(
        output.stdout,
        concat!(
            "[1, 2, 2, 3]\n",
            "3\n1\n2\n2\n",
            "[3, 2, 2, 1]\n",
            "2\n4\n4\n6\n",
            "1\n2\n2\n3\n",
            "[1, 2, 2, 3]\n",
            "[2, 2]\n",
        )
    );
}

#[test]
fn canonical_list_calls_preserve_value_types_and_widen_only_index_arguments() {
    let module = crate::lower_source_to_mir(
        r#"
limit: int64 = 4

def constant_key[T](value: T) -> int64:
    return 0

def main() -> int32:
    mut values: list[int8] = list[int8].with_capacity(limit)
    values.append(3)
    values.append(1)
    values.append(2)
    needle: int8 = 1
    values.remove(needle)
    index: uint32 = 0
    first: int8 = values.pop(index)
    values.append(first)
    values.reserve(limit)
    values.sort(key=constant_key[int8], reverse=true)
    position: int64 = values.index(2)
    copies: int64 = values.count(2)
    print(f"{first}|{position}|{copies}|{values}")
    return 0
"#,
    )
    .expect("canonical list calls should lower from valid Aura source");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    let member_call = |field: &str| {
        instructions
            .iter()
            .find_map(|instruction| match instruction {
                Instruction::Assign {
                    value:
                        Rvalue::Call {
                            callee:
                                CallTarget::Member {
                                    field: actual,
                                    receiver_place,
                                    ..
                                },
                            args,
                        },
                    ..
                } if actual == field => Some((receiver_place.as_deref(), args.as_slice())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected `{field}` member call in MIR"))
    };
    let local_type = |place: &str| {
        main.local_types
            .iter()
            .find(|local| local.name == place)
            .map(|local| &local.ty)
            .unwrap_or_else(|| panic!("expected local type for `{place}`"))
    };

    let (remove_receiver, remove_args) = member_call("remove");
    assert_eq!(remove_receiver, Some("values"));
    assert_eq!(remove_args.len(), 1);
    let Operand::Place(remove_value) = &remove_args[0].value else {
        panic!("list.remove should receive one typed place operand");
    };
    assert_eq!(local_type(remove_value), &Type::named("int8"));

    let (pop_receiver, pop_args) = member_call("pop");
    assert_eq!(pop_receiver, Some("values"));
    assert_eq!(pop_args.len(), 1);
    let Operand::Place(pop_index) = &pop_args[0].value else {
        panic!("list.pop should receive one widened index place");
    };
    assert_eq!(local_type(pop_index), &Type::named("int64"));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            target,
            value: Rvalue::Cast {
                ty,
                ..
            },
        } if target == pop_index && *ty == Type::named("int64")
    )));

    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Call {
                callee: CallTarget::Name(name),
                args,
            },
            ..
        } if name == "list.with_capacity" && args.len() == 1
    )));
    for field in ["append", "reserve", "index", "count"] {
        assert_eq!(member_call(field).0, Some("values"), "{field}");
    }
    assert!(instructions.iter().all(|instruction| !matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Call {
                callee: CallTarget::Member { field, .. },
                ..
            },
            ..
        } if field == "sort"
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::Call {
                callee: CallTarget::Value(Operand::Function { name, .. }),
                args,
            },
            ..
        } if name == "constant_key" && args.len() == 1
    )));
    assert!(main.blocks.iter().any(|block| {
        block.label.contains("vec_sort_inner_compare")
            && matches!(block.terminator, Terminator::Branch { .. })
    }));

    let output = crate::run_mir(&module).expect("canonical list MIR should execute");
    assert_eq!(output.stdout, "3|0|1|[2, 3]\n");
}

#[test]
fn canonical_numeric_constants_formats_and_patterns_preserve_exact_mir_types() {
    let module = crate::lower_source_to_mir(
        r#"
base: int8 = 12

def classify(value: int8) -> str:
    match value:
        case 0 | 15 if value > (10 as int8):
            return f"{value:04x}"
        case _:
            return "other"

def main() -> int32:
    right: int8 = 3
    quotient: int8 = base // right
    remainder: int8 = base % right
    powered: int8 = (2 as int8) ** right
    masked: int8 = base & right
    combined: int8 = base | right
    toggled: int8 = base ^ right
    left: int8 = base << right
    shifted: int8 = base >> right
    pair: (int8, int8) = divmod(base, right)
    rounded: int64 = round(2.5 as float32)
    assert base > right
    ratio: float32 = 1.25
    print(f"{quotient}|{remainder}|{powered}|{masked}|{combined}|{toggled}|{left}|{shifted}|{pair}|{rounded}|{base:04x}|{ratio:.2f}|{classify(15 as int8)}")
    return 0
"#,
    )
    .expect("canonical numeric and pattern source should lower");

    assert_eq!(module.constants.len(), 1);
    assert_eq!(module.constants[0].key, "<main>::base");
    assert_eq!(
        module.constants[0].initializer,
        "__aura_const_init::<main>::base"
    );
    assert_eq!(module.constants[0].ty, Type::named("int8"));
    let initializer = module
        .functions
        .iter()
        .find(|function| function.name == "__aura_const_init::<main>::base")
        .expect("module constant initializer should lower as an ordinary MIR function");
    assert_eq!(initializer.return_type, Type::named("int8"));
    assert!(matches!(
        initializer.blocks[0].instructions.as_slice(),
        [Instruction::Assign {
            target,
            value: Rvalue::Use(Operand::Int(12)),
        }] if target == "%t0"
    ));
    assert!(matches!(
        initializer.blocks[0].terminator,
        Terminator::Return(Operand::Place(ref place)) if place == "%t0"
    ));

    let classify = module
        .functions
        .iter()
        .find(|function| function.name == "classify")
        .expect("classify should lower");
    for label in [
        "match_or_next",
        "match_or_selected",
        "match_guard_true",
        "match_guard_false",
    ] {
        assert!(
            classify
                .blocks
                .iter()
                .any(|block| block.label.contains(label)),
            "guarded or-pattern should expose `{label}` control flow"
        );
    }
    assert!(classify.blocks.iter().any(|block| matches!(
        block.terminator,
        Terminator::Branch {
            ref then_label,
            ref else_label,
            ..
        } if then_label.contains("match_guard_true")
            && else_label.contains("match_guard_false")
    )));

    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    for op in [
        BinaryOp::FloorDiv,
        BinaryOp::Mod,
        BinaryOp::Pow,
        BinaryOp::BitAnd,
        BinaryOp::BitOr,
        BinaryOp::BitXor,
        BinaryOp::Shl,
        BinaryOp::Shr,
    ] {
        assert!(
            instructions.iter().any(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Binary { op: actual, .. },
                    ..
                } if *actual == op
            )),
            "expected `{op:?}` MIR operation"
        );
    }
    for name in [
        "quotient",
        "remainder",
        "powered",
        "masked",
        "combined",
        "toggled",
        "left",
        "shifted",
    ] {
        assert!(
            main.local_types
                .iter()
                .any(|local| { local.name == name && local.ty == Type::named("int8") }),
            "`{name}` should retain its narrow MIR type"
        );
    }
    assert!(main.local_types.iter().any(|local| {
        local.name == "pair"
            && local.ty == Type::Tuple(vec![Type::named("int8"), Type::named("int8")])
    }));
    assert!(main
        .local_types
        .iter()
        .any(|local| { local.name == "rounded" && local.ty == Type::named("int64") }));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            target,
            value: Rvalue::Call {
                callee: CallTarget::Name(name),
                args,
            },
        } if name == "divmod"
            && main.local_types.iter().any(|local| {
                local.name == *target
                    && local.ty
                        == Type::Tuple(vec![Type::named("int8"), Type::named("int8")])
            })
            && args.len() == 2
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            target,
            value: Rvalue::Call {
                callee: CallTarget::Name(name),
                args,
            },
        } if name == "round"
            && main.local_types.iter().any(|local| {
                local.name == *target && local.ty == Type::named("int64")
            })
            && args.len() == 1
    )));

    let formatted = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .flat_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::FormatString { parts },
                ..
            } => parts.as_slice(),
            _ => &[],
        })
        .filter_map(|part| match part {
            MirFormatPart::Formatted {
                spec, value_type, ..
            } => Some((spec.as_str(), value_type)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        formatted,
        vec![
            ("04x", &Type::named("int8")),
            ("04x", &Type::named("int8")),
            (".2f", &Type::named("float32")),
        ]
    );

    let failures = assertion_failures(&module);
    assert_eq!(failures.len(), 1);
    let Terminator::AssertFail { captures, .. } = &failures[0].1.terminator else {
        unreachable!();
    };
    assert_eq!(
        captures
            .iter()
            .map(|capture| (capture.label.as_str(), &capture.ty))
            .collect::<Vec<_>>(),
        vec![
            ("left", &Type::named("int8")),
            ("right", &Type::named("int8"))
        ]
    );
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            value: Rvalue::ModuleConstant { key, initializer },
            ..
        } if key == "<main>::base" && initializer == "__aura_const_init::<main>::base"
    )));

    let output = crate::run_mir(&module).expect("canonical numeric MIR should execute");
    assert_eq!(
        output.stdout,
        "4|0|8|0|15|15|96|1|(4, 0)|2|000c|1.25|000f\n"
    );
}

#[test]
fn mutable_guarded_or_match_expression_writes_back_before_the_next_arm() {
    let module = crate::lower_source_to_mir(
        r#"
enum Reading:
    Exact(int32)
    Approx(int32)

def mutate_and_reject(value: mut int32) -> bool:
    value += 5
    return false

def main() -> int32:
    mut reading = Reading.Approx(1)
    result: int32 = match mut reading:
        case Exact(value) | Approx(value) if mutate_and_reject(value): 0
        case Approx(value): value
        case Exact(value): -value
    print(result)
    return 0
"#,
    )
    .expect("mutable guarded or-pattern expression should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let guard_rejection = main
        .blocks
        .iter()
        .find(|block| block.label.contains("match_expr_guard_false"))
        .expect("the guard rejection edge should be explicit");
    assert!(matches!(
        guard_rejection.terminator,
        Terminator::Branch {
            ref then_label,
            ref else_label,
            ..
        } if then_label.contains("match_or_writeback_apply")
            && else_label.contains("match_or_writeback_next")
    ));
    let writeback_done = main
        .blocks
        .iter()
        .find(|block| block.label.contains("match_or_writeback_done"))
        .expect("the rejected guarded alternative should reassemble its enum");
    assert!(matches!(
        writeback_done.instructions.as_slice(),
        [
            Instruction::Assign {
                target,
                value: Rvalue::Use(Operand::Place(_)),
            },
            Instruction::Assign {
                value: Rvalue::Use(Operand::Bool(true)),
                ..
            }
        ] if target == "reading"
    ));
    assert!(matches!(
        writeback_done.terminator,
        Terminator::Goto(ref label) if label.contains("match_expr_next")
    ));

    let output = crate::run_mir(&module).expect("match expression MIR should execute");
    assert_eq!(output.stdout, "6\n");
}

#[test]
fn control_retry_lowers_to_shared_indirect_calls_and_runtime_policy_adapters() {
    let module = crate::lower_source_to_mir(
        r#"
import control

def worker() -> Result[list[str], str]:
    return Result.Ok(["ready"])

def main():
    print(control.retry[list[str], str](
        initial_backoff=0ms,
        worker=worker,
        max_attempts=2
    ))
"#,
    )
    .expect("control.retry should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let calls = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Call { callee, .. },
                ..
            } => Some(callee),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(calls.iter().any(|callee| {
        matches!(
            callee,
            CallTarget::Value(Operand::Function { signature, .. })
                if matches!(
                    &**signature,
                    Type::Function { params, return_type }
                        if params.is_empty()
                            && **return_type
                                == Type::Named(
                                    "Result".to_string(),
                                    vec![
                                        Type::Named(
                                            "list".to_string(),
                                            vec![Type::named("str")],
                                        ),
                                        Type::named("str"),
                                    ],
                                )
                )
        )
    }));
    for internal in [
        "control::__retry_validate",
        "control::__retry_cancel_if_requested",
        "control::__retry_next_backoff",
        "sleep",
    ] {
        assert!(
            calls
                .iter()
                .any(|callee| matches!(callee, CallTarget::Name(name) if name == internal)),
            "retry MIR should contain `{internal}`"
        );
    }
    assert!(!calls
        .iter()
        .any(|callee| matches!(callee, CallTarget::Name(name) if name == "control::retry")));
}

#[test]
fn control_retry_is_materialized_only_when_imported_and_has_a_real_callable_body() {
    let plain = crate::lower_source_to_mir(
        r#"
def main() -> int32:
    return 0
"#,
    )
    .expect("plain source should lower");
    assert!(
        plain
            .functions
            .iter()
            .all(|function| function.name != "control::retry"),
        "the checker-wide builtin registry must not inject retry into unrelated MIR"
    );

    let imported = crate::lower_source_to_mir(
        r#"
import control

def main() -> int32:
    return 0
"#,
    )
    .expect("control import should lower");
    let retry = imported
        .functions
        .iter()
        .find(|function| function.name == "control::retry")
        .expect("an imported control module should materialize its callable retry target");
    let calls = retry
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value: Rvalue::Call { callee, .. },
                ..
            } => Some(callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        calls
            .iter()
            .any(|callee| matches!(callee, CallTarget::Value(Operand::Place(worker)) if worker == "worker")),
        "the named retry target must invoke its worker through ordinary indirect-call MIR"
    );
    for adapter in [
        "control::__retry_validate",
        "control::__retry_cancel_if_requested",
        "control::__retry_next_backoff",
        "sleep",
    ] {
        assert!(
            calls
                .iter()
                .any(|callee| matches!(callee, CallTarget::Name(name) if name == adapter)),
            "the named retry target should contain `{adapter}`"
        );
    }
}

#[test]
fn mir_closure_conversion_preserves_capture_metadata_and_uses_value_calls() {
    let module = crate::lower_source_to_mir(
        r#"
def main():
    offset: int64 = 10
    factor: int64 = 3
    add_snapshot: def(int64) -> int64 = lambda value: value * factor + offset
    print(add_snapshot(1))
    identity: def(int64) -> int64 = lambda value: value
    print(identity(2))
    payload = "single-use"
    take_payload: def() -> str = lambda: payload
    print(take_payload())
"#,
    )
    .expect("capturing and capture-free lambdas should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");

    let closures = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::Closure {
                        function,
                        signature,
                        captures,
                        consuming,
                    },
                ..
            } => Some((function, signature, captures, consuming)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        closures.len(),
        2,
        "only capturing lambdas need environments"
    );

    let &(repeatable_name, repeatable_signature, repeatable_captures, _) = closures
        .iter()
        .find(|entry| !*entry.3)
        .expect("the Copy capture should produce a repeatable closure");
    let Type::Closure {
        captures,
        call_kind: crate::sema::ClosureCallKind::Repeatable,
        ..
    } = repeatable_signature
    else {
        panic!("the Copy captures should retain a repeatable closure signature");
    };
    assert_eq!(
        captures
            .iter()
            .map(|capture| { (capture.name.as_str(), capture.mode, capture.ty.clone(),) })
            .collect::<Vec<_>>(),
        vec![
            (
                "factor",
                crate::sema::ClosureCaptureMode::Copy,
                Type::named("int64"),
            ),
            (
                "offset",
                crate::sema::ClosureCaptureMode::Copy,
                Type::named("int64"),
            ),
        ],
        "closure metadata must preserve deterministic lexical-first-use order"
    );
    assert_eq!(
        repeatable_captures
            .iter()
            .map(|capture| {
                (
                    capture.name.as_str(),
                    capture.value.clone(),
                    capture.ty.clone(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "factor",
                Operand::Place("factor".to_string()),
                Type::named("int64"),
            ),
            (
                "offset",
                Operand::Place("offset".to_string()),
                Type::named("int64"),
            ),
        ],
        "the environment payload order must exactly match the closure type metadata"
    );
    let repeatable_body = module
        .functions
        .iter()
        .find(|function| function.name == *repeatable_name)
        .expect("the repeatable lambda body should be an ordinary lifted MIR function");
    assert_eq!(
        repeatable_body
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.passing))
            .collect::<Vec<_>>(),
        vec![
            ("factor", MirReceiverKind::Value),
            ("offset", MirReceiverKind::Value),
            ("value", MirReceiverKind::Borrow),
        ],
        "hidden captures must precede public lambda parameters"
    );

    let &(_, consuming_signature, consuming_captures, consuming) = closures
        .iter()
        .find(|entry| *entry.3)
        .expect("the moved str capture should produce a consuming closure");
    assert!(*consuming);
    assert!(matches!(
        consuming_signature,
        Type::Closure {
            call_kind: crate::sema::ClosureCallKind::Consuming,
            ..
        }
    ));
    assert!(matches!(
        consuming_captures.as_slice(),
        [MirClosureCapture {
            name,
            value: Operand::MovePlace(place),
            ..
        }] if name == "payload" && place == "payload"
    ));

    for binding in ["add_snapshot", "take_payload"] {
        assert!(
            matches!(
                main.local_types
                    .iter()
                    .find(|local| local.name == binding)
                    .map(|local| &local.ty),
                Some(Type::Closure { .. })
            ),
            "capturing binding `{binding}` must retain closure metadata in MIR"
        );
    }
    assert!(matches!(
        main.local_types
            .iter()
            .find(|local| local.name == "identity")
            .map(|local| &local.ty),
        Some(Type::Function { .. })
    ));
    assert!(
        main.blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Value(_),
                        ..
                    },
                    ..
                }
            ))
            .count()
            >= 3,
        "lambda invocation should reuse ordinary MIR function-value dispatch"
    );
}

#[test]
fn nested_closure_conversion_nests_environment_types_and_lifted_call_abis() {
    let module = crate::lower_source_to_mir(
        r#"
def main() -> int32:
    factor: int64 = 2
    inner: def(int64) -> int64 = lambda value: value * factor
    offset: int64 = 1
    outer: def(int64) -> int64 = lambda value: inner(value) + offset
    print(outer(3))
    return 0
"#,
    )
    .expect("nested capturing lambdas should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let closures = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::Assign {
                target,
                value:
                    Rvalue::Closure {
                        function,
                        signature,
                        captures,
                        consuming,
                    },
            } => Some((target, function, signature, captures, consuming)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(closures.len(), 2);

    let (_, inner_name, inner_signature, inner_captures, inner_consuming) = closures
        .iter()
        .find(|(_, _, _, captures, _)| {
            matches!(
                captures.as_slice(),
                [MirClosureCapture { name, .. }] if name == "factor"
            )
        })
        .expect("inner closure should be explicit in MIR");
    assert!(!**inner_consuming);
    assert!(matches!(
        inner_captures.as_slice(),
        [MirClosureCapture {
            name,
            value: Operand::Place(place),
            ty,
            ..
        }] if name == "factor" && place == "factor" && *ty == Type::named("int64")
    ));
    assert!(matches!(
        inner_signature,
        Type::Closure {
            captures,
            call_kind: crate::sema::ClosureCallKind::Repeatable,
            ..
        } if matches!(
            captures.as_slice(),
            [crate::sema::ClosureCapture {
                name,
                mode: crate::sema::ClosureCaptureMode::Copy,
                ty,
                ..
            }] if name == "factor" && *ty == Type::named("int64")
        )
    ));

    let (_, outer_name, outer_signature, outer_captures, outer_consuming) = closures
        .iter()
        .find(|(_, _, _, captures, _)| {
            matches!(
                captures.as_slice(),
                [
                    MirClosureCapture { name: first, .. },
                    MirClosureCapture { name: second, .. },
                ] if first == "inner" && second == "offset"
            )
        })
        .expect("outer closure should be explicit in MIR");
    assert!(!**outer_consuming);
    assert_eq!(
        outer_captures
            .iter()
            .map(|capture| {
                (
                    capture.name.as_str(),
                    capture.value.clone(),
                    capture.ty.clone(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "inner",
                Operand::MovePlace("inner".to_string()),
                (*inner_signature).clone(),
            ),
            (
                "offset",
                Operand::Place("offset".to_string()),
                Type::named("int64"),
            ),
        ],
        "the outer environment must embed the inner closure type before later lexical captures"
    );
    let Type::Closure {
        captures,
        call_kind: crate::sema::ClosureCallKind::Repeatable,
        ..
    } = outer_signature
    else {
        panic!("capturing a repeatable closure must keep the outer closure repeatable");
    };
    assert_eq!(
        captures
            .iter()
            .map(|capture| (capture.name.as_str(), capture.mode, capture.ty.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                "inner",
                crate::sema::ClosureCaptureMode::Move,
                (*inner_signature).clone(),
            ),
            (
                "offset",
                crate::sema::ClosureCaptureMode::Copy,
                Type::named("int64"),
            ),
        ]
    );

    let inner_lifted = module
        .functions
        .iter()
        .find(|function| function.name == **inner_name)
        .expect("inner lifted function should be emitted");
    assert_eq!(
        inner_lifted
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        vec!["factor", "value"]
    );
    let outer_lifted = module
        .functions
        .iter()
        .find(|function| function.name == **outer_name)
        .expect("outer lifted function should be emitted");
    assert_eq!(
        outer_lifted
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>(),
        vec!["inner", "offset", "value"],
        "nested environments must remain the leading hidden parameters"
    );
    assert!(
        outer_lifted
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Value(Operand::Place(place)),
                        ..
                    },
                    ..
                } if place == "inner"
            )),
        "the lifted outer body must invoke its captured closure through value-call MIR"
    );
}

#[test]
fn imported_lambdas_resolve_owner_qualified_metadata_and_lifted_function_ids() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/run-pass/lambda_imported_closure_ids.au");
    let module = crate::lower_path_to_mir(&path)
        .expect("same-span lambdas in distinct imported modules should lower");

    let mut lifted_names = Vec::new();
    for module_name in [
        "lambda_imported_closure_ids_support.left",
        "lambda_imported_closure_ids_support.right",
    ] {
        let compute_name = format!("{module_name}::compute");
        let compute = module
            .functions
            .iter()
            .find(|function| function.name == compute_name)
            .unwrap_or_else(|| panic!("imported function `{compute_name}` should lower"));
        let (lifted_name, captures) = compute
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                Instruction::Assign {
                    value:
                        Rvalue::Closure {
                            function, captures, ..
                        },
                    ..
                } => Some((function, captures)),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("imported function `{compute_name}` should construct its own closure")
            });
        assert!(
            lifted_name.starts_with(&format!("{compute_name}::__lambda_3_")),
            "lifted lambda identity must be qualified by its owning function: {lifted_name}"
        );
        assert!(matches!(
            captures.as_slice(),
            [MirClosureCapture {
                name,
                value: Operand::Place(place),
                ty,
                ..
            }] if name == "offset" && place == "offset" && *ty == Type::named("int64")
        ));

        let lifted = module
            .functions
            .iter()
            .find(|function| function.name == *lifted_name)
            .unwrap_or_else(|| panic!("lifted lambda `{lifted_name}` should be emitted"));
        assert_eq!(lifted.module_name, module_name);
        assert_eq!(
            lifted
                .params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>(),
            vec!["offset", "item"],
            "the hidden imported capture must precede the public lambda parameter"
        );
        lifted_names.push(lifted_name.clone());
    }

    assert_ne!(
        lifted_names[0], lifted_names[1],
        "equal source spans in different modules must never alias one lifted closure"
    );
}

#[test]
fn imported_comprehensions_resolve_owner_qualified_result_metadata() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/run-pass/comprehension_imported_metadata.au");
    let program =
        crate::check_path(&path).expect("the imported comprehension project should type-check");
    let namespace = program
        .module_registry
        .get("comprehension_imported_metadata_support.helpers")
        .expect("the imported helper namespace should be exported");
    let boxed_type = Type::named("comprehension_imported_metadata_support.helpers.Boxed");
    assert!(namespace.comprehensions.values().any(|info| {
        info.result_type == Type::Named("list".to_string(), vec![boxed_type.clone()])
    }));
    assert!(namespace.comprehensions.values().any(|info| {
        info.clauses
            .iter()
            .any(|clause| clause.binding_type == boxed_type)
    }));

    let module = crate::mir::lower(&program);
    let helper = module
        .functions
        .iter()
        .find(|function| function.name == "comprehension_imported_metadata_support.helpers::boxed")
        .expect("imported comprehension helper should lower");
    assert!(helper
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(
            instruction,
            Instruction::Assign {
                value: Rvalue::VecLiteral { element_type, .. },
                ..
            } if *element_type == boxed_type
        )));

    let output = crate::run_mir(&module)
        .expect("an imported function should retain its checked comprehension metadata");
    assert_eq!(output.stdout, "2\n6\n");
}

#[test]
fn closure_callbacks_and_task_targets_lower_through_shared_callable_contracts() {
    let module = crate::lower_source_to_mir(
        r#"
def main() -> int32:
    offset: int64 = 5
    mut values: list[int64] = [3, 1, 2]
    values.sort(key = lambda value: value + offset)
    mapped: list[int64] = values.map(lambda value: value + offset)
    worker: def(int64) -> int64 = lambda value: value + offset
    with TaskGroup() as group:
        task: Task[int64] = group.start((worker), 7)
    return 0
"#,
    )
    .expect("capturing list callbacks and a capturing task target should lower");
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main should lower");
    let instructions = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Closure { .. },
                    ..
                }
            ))
            .count(),
        3,
        "each capturing callback expression must construct one explicit environment"
    );
    assert!(
        instructions
            .iter()
            .filter(|instruction| matches!(
                instruction,
                Instruction::Assign {
                    value: Rvalue::Call {
                        callee: CallTarget::Value(_),
                        ..
                    },
                    ..
                }
            ))
            .count()
            >= 2,
        "list.sort and list.map callbacks must use ordinary indirect callable dispatch"
    );

    let (task_function, task_args, returns_handle) = instructions
        .iter()
        .find_map(|instruction| match instruction {
            Instruction::Assign {
                value:
                    Rvalue::StartTask {
                        function,
                        args,
                        returns_handle,
                        ..
                    },
                ..
            } => Some((function, args, returns_handle)),
            _ => None,
        })
        .expect("TaskGroup.start should lower to StartTask");
    assert!(
        matches!(task_function, Operand::Place(place) if place == "worker"),
        "Task lowering must retain the checked closure value as its callable target: {task_function:?}"
    );
    let [MirArg {
        name: None,
        value: Operand::Place(task_arg_place),
        writeback_place: None,
    }] = task_args.as_slice()
    else {
        panic!(
            "task arguments must retain their declaration slot and owned capture contract: {task_args:?}"
        );
    };
    assert!(main
        .local_types
        .iter()
        .any(|local| { local.name == *task_arg_place && local.ty == Type::named("int64") }));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        Instruction::Assign {
            target,
            value: Rvalue::Use(Operand::Int(7)),
        } if target == task_arg_place
    )));
    assert!(*returns_handle);
    assert!(matches!(
        main.local_types
            .iter()
            .find(|local| local.name == "task")
            .map(|local| &local.ty),
        Some(Type::Named(name, args))
            if name == "Task" && args.as_slice() == [Type::named("int64")]
    ));
}
