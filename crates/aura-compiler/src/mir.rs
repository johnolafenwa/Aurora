use crate::ast::{
    Argument, AssignStmt, AssignTarget, BinaryOp, BindingTarget, CompareLink, CompareOp,
    ComprehensionClause, ComprehensionOutput, DestructureStmt, Expr, ExprKind, IfStmt,
    LiteralPatternKind, MatchStmt, Param, Pattern, ReceiverKind, Stmt, TypeRefKind, UnaryOp,
    WhileStmt,
};
use crate::call::{
    bind_call_arguments, callable_params_from_decl, BuiltinAssociatedFunction,
    BuiltinClassConstructor, BuiltinFunction, BuiltinMember, CallConvention, CallableParam,
};
use crate::diag::Span;
use crate::integer::{minimal_signed_type_for_negative_literal, IntegerValue};
use crate::sema::{
    binary_operator_trait, resolve_param_passing, substitute_trait_bound, substitute_type,
    substitutions_from_decl_type_args, type_is_copy_in_program, unary_operator_trait,
    ClosureCallKind, ClosureCaptureMode, ClosureInfo, ClosureOwner, ComprehensionClauseInfo,
    ComprehensionInfo, FunctionParamContract, ModuleNamespace, Program, TraitBound, Type,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn is_known_enum_name(program: &Program, name: &str) -> bool {
    program.enums.contains_key(name)
        || matches!(
            name,
            "Result"
                | "Option"
                | "SendError"
                | "QueueReceive"
                | "TaskResult"
                | "WaitAny"
                | "WaitAll"
        )
}

const INTERNAL_VEC_INDEX_FIELD: &str = "__index";
const INTERNAL_SLICE_FIELD: &str = "__slice";
const INTERNAL_VEC_INDEX_OPTION_FIELD: &str = "__index_option";
const INTERNAL_COLLECTION_TAKE_INDEX_OPTION_FIELD: &str = "__take_index_option";
const INTERNAL_VEC_SET_INDEX_FIELD: &str = "__set_index";
const INTERNAL_MAP_INDEX_FIELD: &str = "__index";
const INTERNAL_MAP_SET_INDEX_FIELD: &str = "__set_index";
const INTERNAL_QUEUE_GET_IN_TASK_GROUP_FIELD: &str = "__get_in_task_group";
const INTERNAL_QUEUE_GET_WITH_REGISTERED_PRODUCERS_FIELD: &str = "__get_with_registered_producers";

pub(crate) fn has_runtime_named_function(name: &str) -> bool {
    BuiltinFunction::from_name(name).is_some()
        || crate::builtin_modules::host_builtin_metadata(name).is_some()
        || matches!(
            name,
            "random::secure_int"
                | "random::secure_bytes"
                | "io::write"
                | "io::flush"
                | "io::read_line"
                | "fs::exists"
                | "fs::read_to_string"
                | "fs::read_bytes"
                | "fs::write_string"
                | "fs::write_bytes"
                | "fs::append_string"
                | "fs::append_bytes"
                | "fs::create_dir"
                | "fs::read_dir"
                | "fs::remove_file"
                | "fs::open"
                | "fs::create"
                | "fs::append"
                | "net::connect"
                | "net::connect_timeout"
                | "net::listen"
                | "net::udp_bind"
                | "net::unix_listen"
                | "net::unix_connect"
                | "net::unix_connect_timeout"
                | "net::tls_listen"
                | "net::tls_connect"
                | "net::tls_connect_timeout"
                | "net::http_listen"
                | "net::http_request_text"
                | "net::http_request_text_timeout"
                | "net::http_request_bytes"
                | "net::http_request_bytes_timeout"
                | "net::websocket_listen"
                | "net::websocket_connect"
                | "net::websocket_connect_timeout"
                | "process::inherit"
                | "process::null"
                | "process::pipe"
                | "process::supervisor"
                | "process::start"
                | "process::run"
                | "bytes::hex_encode"
                | "bytes::base64_encode"
                | "bytes::sha256"
                | "bytes::hex_decode"
                | "bytes::base64_decode"
                | "bytes::sha256_string"
                | "json::parse"
                | "json::dumps"
                | "json::is_null"
                | "json::as_bool"
                | "json::as_int"
                | "json::as_float"
                | "json::into_string"
                | "json::into_array"
                | "json::into_object"
        )
}

fn is_builtin_unary_operator(op: UnaryOp, ty: &Type) -> bool {
    match op {
        UnaryOp::Not => *ty == Type::named("bool"),
        UnaryOp::BitNot => crate::sema::integer_type_bounds(ty).is_some(),
        UnaryOp::Neg => {
            crate::sema::integer_type_bounds(ty).is_some()
                || matches!(ty, Type::Named(name, _) if name == "float32" || name == "float64")
        }
    }
}

fn is_builtin_binary_operator(op: BinaryOp, left_ty: &Type, right_ty: &Type) -> bool {
    fn array_element(ty: &Type) -> Option<&Type> {
        match ty {
            Type::Named(name, arguments) if name == "Array" && arguments.len() == 1 => {
                arguments.first()
            }
            _ => None,
        }
    }
    let left_array_element = array_element(left_ty);
    let right_array_element = array_element(right_ty);
    if matches!(
        op,
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
    ) {
        let compatible = match (left_array_element, right_array_element) {
            (Some(left), Some(right)) => left_ty == right_ty && left == right,
            (Some(element), None) => element == right_ty,
            (None, Some(element)) => left_ty == element,
            (None, None) => false,
        };
        if compatible {
            return op != BinaryOp::Div
                || left_array_element
                    .or(right_array_element)
                    .is_some_and(is_float_type);
        }
    }
    let duration = Type::named("Duration");
    let int64 = Type::named("int64");
    if match op {
        BinaryOp::Add | BinaryOp::Sub => left_ty == &duration && right_ty == &duration,
        BinaryOp::Mul => {
            (left_ty == &duration && right_ty == &int64)
                || (left_ty == &int64 && right_ty == &duration)
        }
        BinaryOp::FloorDiv => left_ty == &duration && right_ty == &int64,
        BinaryOp::Eq
        | BinaryOp::NotEq
        | BinaryOp::Less
        | BinaryOp::LessEq
        | BinaryOp::Greater
        | BinaryOp::GreaterEq => left_ty == &duration && right_ty == &duration,
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Div
        | BinaryOp::Mod
        | BinaryOp::Pow
        | BinaryOp::BitAnd
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::Shl
        | BinaryOp::Shr => false,
    } {
        return true;
    }
    if left_ty != right_ty {
        return false;
    }
    match op {
        BinaryOp::And | BinaryOp::Or => *left_ty == Type::named("bool"),
        BinaryOp::Add => {
            crate::sema::integer_type_bounds(left_ty).is_some()
                || matches!(left_ty, Type::Named(name, _) if name == "float32" || name == "float64" || name == "str")
        }
        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::FloorDiv | BinaryOp::Mod => {
            crate::sema::integer_type_bounds(left_ty).is_some()
                || matches!(left_ty, Type::Named(name, _) if name == "float32" || name == "float64")
        }
        BinaryOp::Pow => {
            crate::sema::integer_type_bounds(left_ty).is_some()
                || matches!(left_ty, Type::Named(name, _) if name == "float32" || name == "float64")
        }
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr => {
            crate::sema::integer_type_bounds(left_ty).is_some()
        }
        BinaryOp::Eq | BinaryOp::NotEq => true,
        BinaryOp::Less | BinaryOp::LessEq | BinaryOp::Greater | BinaryOp::GreaterEq => {
            crate::sema::integer_type_bounds(left_ty).is_some()
                || matches!(left_ty, Type::Named(name, _) if name == "float32" || name == "float64")
        }
    }
}

fn is_float_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, args) if args.is_empty() && (name == "float32" || name == "float64"))
}

fn is_integer_literal_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::Group(inner) => is_integer_literal_expr(inner),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr: inner,
        } => matches!(inner.kind, ExprKind::Int(_)),
        _ => false,
    }
}

fn is_float_literal_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Float(_) => true,
        ExprKind::Group(inner) => is_float_literal_expr(inner),
        _ => false,
    }
}

fn type_contains_unknown(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) => name == "Unknown" || args.iter().any(type_contains_unknown),
        Type::Tuple(elements) => elements.iter().any(type_contains_unknown),
        Type::Function {
            params,
            return_type,
            ..
        } => {
            params.iter().any(|param| type_contains_unknown(&param.ty))
                || type_contains_unknown(return_type.as_ref())
        }
        Type::Closure {
            params,
            return_type,
            captures,
            ..
        } => {
            params.iter().any(|param| type_contains_unknown(&param.ty))
                || captures
                    .iter()
                    .any(|capture| type_contains_unknown(&capture.ty))
                || type_contains_unknown(return_type.as_ref())
        }
        Type::TypeParam(_) | Type::Unit | Type::Module(_) => false,
    }
}

fn contextual_float_literal_operand(
    value: u128,
    negative: bool,
    expected: &Type,
) -> Option<Operand> {
    let integer = IntegerValue::from_literal(value);
    let value = match expected {
        Type::Named(name, args) if args.is_empty() && name == "float32" => integer
            .to_exact_f32()
            .map(f64::from)
            .expect("checked float32-context integer literal should be exact"),
        Type::Named(name, args) if args.is_empty() && name == "float64" => integer
            .to_exact_f64()
            .expect("checked float64-context integer literal should be exact"),
        _ => return None,
    };
    Some(Operand::Float(if negative { -value } else { value }))
}

fn collect_type_params_from_type(ty: &Type, collected: &mut BTreeSet<String>) {
    match ty {
        Type::TypeParam(name) => {
            collected.insert(name.clone());
        }
        Type::Named(_, args) => {
            for arg in args {
                collect_type_params_from_type(arg, collected);
            }
        }
        Type::Tuple(elements) => {
            for element in elements {
                collect_type_params_from_type(element, collected);
            }
        }
        Type::Function {
            params,
            return_type,
            ..
        } => {
            for param in params.iter() {
                collect_type_params_from_type(&param.ty, collected);
            }
            collect_type_params_from_type(return_type, collected);
        }
        Type::Closure {
            params,
            return_type,
            captures,
            ..
        } => {
            for param in params.iter() {
                collect_type_params_from_type(&param.ty, collected);
            }
            for capture in captures.iter() {
                collect_type_params_from_type(&capture.ty, collected);
            }
            collect_type_params_from_type(return_type, collected);
        }
        Type::Unit | Type::Module(_) => {}
    }
}

fn adjusted_binary_operand_types(
    left_expr: &Expr,
    mut left_ty: Type,
    right_expr: &Expr,
    mut right_ty: Type,
) -> (Type, Type) {
    let duration = Type::named("Duration");
    if left_ty == duration || right_ty == duration {
        // Duration's scalar operators are deliberately heterogeneous. An
        // integer literal beside a Duration remains the default `int64`;
        // contextualizing it as Duration would incorrectly route `*` and
        // `//` through trait dispatch.
        return (left_ty, right_ty);
    }
    if left_ty != right_ty {
        if is_integer_literal_expr(left_expr) && is_float_type(&right_ty) {
            left_ty = right_ty.clone();
        } else if is_integer_literal_expr(right_expr) && is_float_type(&left_ty) {
            right_ty = left_ty.clone();
        } else if is_integer_literal_expr(left_expr) || matches!(left_expr.kind, ExprKind::Float(_))
        {
            left_ty = right_ty.clone();
        } else if is_integer_literal_expr(right_expr)
            || matches!(right_expr.kind, ExprKind::Float(_))
        {
            right_ty = left_ty.clone();
        }
    }
    (left_ty, right_ty)
}

fn default_return_operand(ty: &Type) -> Operand {
    match ty {
        Type::Unit => Operand::Unit,
        Type::Named(name, args) if args.is_empty() => match name.as_str() {
            "bool" => Operand::Bool(false),
            "float32" | "float64" => Operand::Float(0.0),
            "str" => Operand::String(String::new()),
            "Duration" => Operand::Duration(0),
            _ if matches!(
                name.as_str(),
                "int8"
                    | "int16"
                    | "int32"
                    | "int64"
                    | "int128"
                    | "intsize"
                    | "uint8"
                    | "uint16"
                    | "uint32"
                    | "uint64"
                    | "uint128"
                    | "uintsize"
            ) =>
            {
                Operand::Int(0)
            }
            _ => Operand::Unit,
        },
        _ => Operand::Unit,
    }
}

fn place_paths_overlap(left: &str, right: &str) -> bool {
    let left_segments = left.split('.').collect::<Vec<_>>();
    let right_segments = right.split('.').collect::<Vec<_>>();
    if left_segments.first() != right_segments.first() {
        return false;
    }
    let shared = left_segments
        .iter()
        .zip(right_segments.iter())
        .take_while(|(lhs, rhs)| lhs == rhs)
        .count();
    shared == left_segments.len() || shared == right_segments.len()
}

fn return_view_projection_expr(
    expr: &Expr,
    origin: &str,
    aliases: &BTreeMap<String, String>,
) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) if name == origin => Some(String::new()),
        ExprKind::Name(name) => aliases.get(name).cloned(),
        ExprKind::Group(inner) => return_view_projection_expr(inner, origin, aliases),
        ExprKind::Member { object, field } => {
            let parent = return_view_projection_expr(object, origin, aliases)?;
            Some(if parent.is_empty() {
                field.clone()
            } else {
                format!("{parent}.{field}")
            })
        }
        ExprKind::Index { object, index } => {
            let ExprKind::Int(index) = index.kind else {
                return None;
            };
            let index = usize::try_from(index).ok()?;
            let parent = return_view_projection_expr(object, origin, aliases)?;
            Some(if parent.is_empty() {
                index.to_string()
            } else {
                format!("{parent}.{index}")
            })
        }
        _ => None,
    }
}

fn collect_return_view_projections(
    body: &[Stmt],
    origin: &str,
    aliases: &mut BTreeMap<String, String>,
    projections: &mut Vec<String>,
) {
    for stmt in body {
        match stmt {
            Stmt::View(view) => {
                if let Some(projection) = return_view_projection_expr(&view.source, origin, aliases)
                {
                    aliases.insert(view.name.clone(), projection);
                }
            }
            Stmt::Return(return_stmt) if return_stmt.view.is_some() => {
                if let Some(projection) = return_stmt
                    .value
                    .as_ref()
                    .and_then(|value| return_view_projection_expr(value, origin, aliases))
                {
                    projections.push(projection);
                }
            }
            Stmt::If(if_stmt) => {
                for branch in &if_stmt.branches {
                    collect_return_view_projections(
                        &branch.body,
                        origin,
                        &mut aliases.clone(),
                        projections,
                    );
                }
                if let Some(body) = &if_stmt.else_body {
                    collect_return_view_projections(
                        body,
                        origin,
                        &mut aliases.clone(),
                        projections,
                    );
                }
            }
            Stmt::Match(match_stmt) => {
                for arm in &match_stmt.arms {
                    collect_return_view_projections(
                        &arm.body,
                        origin,
                        &mut aliases.clone(),
                        projections,
                    );
                }
            }
            Stmt::For(for_stmt) => collect_return_view_projections(
                &for_stmt.body,
                origin,
                &mut aliases.clone(),
                projections,
            ),
            Stmt::With(with_stmt) => collect_return_view_projections(
                &with_stmt.body,
                origin,
                &mut aliases.clone(),
                projections,
            ),
            Stmt::While(while_stmt) => collect_return_view_projections(
                &while_stmt.body,
                origin,
                &mut aliases.clone(),
                projections,
            ),
            _ => {}
        }
    }
}

fn return_view_projections(function: &crate::ast::FunctionDecl) -> Vec<String> {
    let Some(contract) = function.view_return.as_ref() else {
        return Vec::new();
    };
    let mut projections = Vec::new();
    collect_return_view_projections(
        &function.body,
        &contract.origin,
        &mut BTreeMap::new(),
        &mut projections,
    );
    projections.sort();
    projections.dedup();
    projections
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirModule {
    pub functions: Vec<MirFunction>,
    pub classes: Vec<MirClass>,
    pub trait_impls: Vec<MirTraitImpl>,
    #[serde(default)]
    pub constants: Vec<MirConstant>,
    pub top_level: Option<MirFunction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirConstant {
    pub key: String,
    pub initializer: String,
    pub ty: Type,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirFunction {
    pub name: String,
    pub module_name: String,
    #[serde(default)]
    pub source_path: Option<String>,
    pub span: crate::diag::Span,
    pub receiver: Option<MirReceiverKind>,
    pub params: Vec<MirParam>,
    pub local_types: Vec<MirLocalType>,
    pub return_type: Type,
    pub entry: String,
    pub blocks: Vec<BasicBlock>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirLocalType {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirClass {
    pub name: String,
    pub type_params: Vec<String>,
    pub fields: Vec<MirClassField>,
    pub methods: Vec<MirMethod>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirClassField {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirMethod {
    pub name: String,
    pub function_name: String,
    pub receiver: Option<MirReceiverKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirTraitImpl {
    pub trait_name: String,
    pub trait_args: Vec<Type>,
    pub for_type: Type,
    pub methods: Vec<MirMethod>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MirReceiverKind {
    Value,
    Borrow,
    BorrowMut,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirParam {
    pub name: String,
    pub passing: MirReceiverKind,
    pub ty: Type,
    /// Hidden zero-argument MIR function that freshly evaluates this
    /// parameter's declared default for runtime-selected function calls.
    #[serde(default)]
    pub default_function: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BasicBlock {
    pub label: String,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

/// Loop backedges consume one unit of function-local scheduling fuel. The
/// interpreter uses a shorter quantum because each interpreted iteration is
/// substantially more expensive; native code uses a longer quantum to keep
/// the amortized scheduler cost below the loop-performance budget.
pub const MIR_LOOP_SAFEPOINT_INTERVAL: u64 = 8;
pub const NATIVE_LOOP_SAFEPOINT_INTERVAL: u64 = 4_096;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Instruction {
    /// A compiler-inserted cooperative scheduling check. Loop lowering places
    /// one on each semantic backedge; runtimes amortize the actual yield with
    /// a per-function fuel counter.
    Safepoint,
    BeginLoan {
        loan: String,
        source: String,
        mutable: bool,
    },
    BeginReturnedLoan {
        loan: String,
        origin: String,
        projections: Vec<String>,
        mutable: bool,
    },
    Reborrow {
        loan: String,
        parent: String,
        projection: String,
        mutable: bool,
    },
    ReadLoan {
        target: String,
        loan: String,
    },
    WriteLoan {
        loan: String,
        value: Rvalue,
    },
    EndLoan {
        loan: String,
    },
    ReturnLoan {
        loan: String,
        origin: String,
    },
    Assign {
        target: String,
        value: Rvalue,
    },
    Eval {
        value: Operand,
    },
    PushCleanup {
        place: String,
    },
    PopCleanup {
        place: String,
        cancel_before_cleanup: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Rvalue {
    Use(Operand),
    /// Read a once-initialized immutable module value. The initializer thunk
    /// is shared by MIR and direct execution and guarded against re-entry.
    ModuleConstant {
        key: String,
        initializer: String,
    },
    /// Constructs a first-class closure around an ordinary synthesized MIR
    /// function. Captures are evaluated exactly once, in source order, and
    /// become hidden leading arguments whenever the function is invoked.
    Closure {
        function: String,
        signature: Type,
        captures: Vec<MirClosureCapture>,
        consuming: bool,
    },
    FormatString {
        parts: Vec<MirFormatPart>,
    },
    Unary {
        op: UnaryOp,
        value: Operand,
        span: crate::diag::Span,
    },
    Cast {
        value: Operand,
        ty: Type,
        span: crate::diag::Span,
    },
    Try {
        value: Operand,
    },
    StartTask {
        returns_handle: bool,
        result_is_copy: bool,
        stack_size: Option<Operand>,
        task_group: Operand,
        function: Operand,
        args: Vec<MirArg>,
        span: crate::diag::Span,
    },
    Binary {
        op: BinaryOp,
        left: Operand,
        right: Operand,
        span: crate::diag::Span,
    },
    Call {
        callee: CallTarget,
        args: Vec<MirArg>,
    },
    VecLiteral {
        elements: Vec<Operand>,
        element_type: Type,
    },
    TupleLiteral {
        elements: Vec<Operand>,
        element_types: Vec<Type>,
    },
    TupleElement {
        tuple: Operand,
        index: usize,
        element_type: Type,
    },
    TupleTakeElement {
        place: String,
        index: usize,
        element_type: Type,
    },
    SetLiteral {
        elements: Vec<Operand>,
        element_type: Type,
    },
    MapLiteral {
        entries: Vec<MirMapEntry>,
        key_type: Type,
        value_type: Type,
    },
    Construct {
        class_name: String,
        fields: Vec<MirFieldInit>,
    },
    EnumVariant {
        enum_name: String,
        variant_name: String,
        payloads: Vec<Operand>,
    },
    VariantPayload {
        scrutinee: Operand,
        variant_name: String,
        index: usize,
    },
    Member {
        object: Operand,
        field: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirClosureCapture {
    pub name: String,
    pub value: Operand,
    pub ty: Type,
    pub passing: MirReceiverKind,
    #[serde(default)]
    pub source_place: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallTarget {
    Name(String),
    Value(Operand),
    /// A direct, synchronous call through Aura's deliberately small C ABI.
    ///
    /// Extern declarations do not acquire ordinary [`MirFunction`] bodies.
    /// Instead, every call retains the complete checked source contract needed
    /// by either backend to marshal the arguments and result.
    Extern(MirExternCall),
    Member {
        object: Operand,
        field: String,
        receiver_place: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirExternCall {
    pub symbol: String,
    pub abi: String,
    pub params: Vec<MirExternParam>,
    pub return_type: Type,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirExternParam {
    pub name: String,
    pub passing: MirReceiverKind,
    pub ty: Type,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirArg {
    pub name: Option<String>,
    pub value: Operand,
    pub writeback_place: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirFieldInit {
    pub name: String,
    pub value: Operand,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirMapEntry {
    pub key: Operand,
    pub value: Operand,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MirFormatPart {
    Literal(String),
    Value(Operand),
    Formatted {
        value: Operand,
        spec: String,
        value_type: Type,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirMatchArm {
    pub enum_name: Option<String>,
    pub variant_name: Option<String>,
    pub wildcard: bool,
    pub label: String,
}

/// One assertion operand retained by an introspected comparison.
///
/// `value` is the already-rendered `str` value produced on the assertion's
/// failure edge. Keeping rendering out of the terminator lets the optional
/// source message remain lazy while giving both execution backends one stable
/// diagnostic payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssertionCapture {
    pub label: String,
    pub ty: Type,
    pub value: Operand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operand {
    Place(String),
    MovePlace(String),
    Function { name: String, signature: Box<Type> },
    Int(u128),
    Duration(i128),
    Float(f64),
    Bool(bool),
    String(String),
    Unit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Terminator {
    Return(Operand),
    Goto(String),
    Branch {
        condition: Operand,
        then_label: String,
        else_label: String,
    },
    ForRange {
        binding: String,
        iterable: Operand,
        body_label: String,
        exit_label: String,
    },
    Match {
        scrutinee: Operand,
        arms: Vec<MirMatchArm>,
        otherwise: String,
    },
    AssertFail {
        message: Option<Operand>,
        captures: Vec<AssertionCapture>,
        span: crate::diag::Span,
    },
    Unreachable,
}

pub fn lower(program: &Program) -> MirModule {
    let mut functions = program
        .functions
        .values()
        .filter(|function| function.module_name == program.module_name)
        .flat_map(|function| {
            lower_function(
                program,
                &function.decl.name,
                &function.module_name,
                ClosureOwner::Function(function.decl.name.clone()),
                function.decl.receiver,
                None,
                &function.decl,
                &function.signature.params,
                &function.signature.param_passings,
                &function.signature.return_type,
                function.type_param_bounds.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut seen_function_names = functions
        .iter()
        .map(|function| function.name.clone())
        .collect::<BTreeSet<_>>();

    push_imported_module_functions(program, &mut functions, &mut seen_function_names);

    let mut classes = Vec::new();
    for class in program.classes.values() {
        let class_name = mir_runtime_class_name(program, class);
        let fields = class
            .decl
            .fields
            .iter()
            .map(|field| MirClassField {
                name: field.name.clone(),
                ty: class
                    .fields
                    .get(&field.name)
                    .expect("class field type should be available during MIR lowering")
                    .ty
                    .clone(),
            })
            .collect::<Vec<_>>();
        let mut methods = Vec::new();
        for method in class.methods.values() {
            let qualified_name = mir_class_method_name(program, class, &method.decl.name);
            functions.extend(lower_function(
                program,
                &qualified_name,
                &class.module_name,
                ClosureOwner::ClassMethod {
                    class_name: class.decl.name.clone(),
                    method_name: method.decl.name.clone(),
                },
                method.decl.receiver,
                Some(Type::Named(
                    class_name.clone(),
                    class
                        .decl
                        .type_params
                        .iter()
                        .cloned()
                        .map(Type::TypeParam)
                        .collect(),
                )),
                &method.decl,
                &method.signature.params,
                &method.signature.param_passings,
                &method.signature.return_type,
                method.type_param_bounds.clone(),
            ));
            methods.push(MirMethod {
                name: method.decl.name.clone(),
                function_name: qualified_name,
                receiver: method.decl.receiver.map(lower_receiver_kind),
            });
        }
        classes.push(MirClass {
            name: class_name,
            type_params: class.decl.type_params.clone(),
            fields,
            methods,
        });
    }
    let mut seen_class_names = classes
        .iter()
        .map(|class| class.name.clone())
        .collect::<BTreeSet<_>>();
    push_imported_module_classes(
        program,
        &mut classes,
        &mut functions,
        &mut seen_function_names,
        &mut seen_class_names,
    );

    let mut trait_impls = Vec::new();
    let mut seen_trait_impls = BTreeSet::new();
    for trait_impl in &program.trait_impls {
        seen_trait_impls.insert(format!(
            "{}{} for {}",
            trait_impl.trait_name,
            format_trait_args(&trait_impl.trait_args),
            trait_impl.for_type
        ));
        trait_impls.push(lower_trait_impl(
            program,
            &program.module_name,
            trait_impl,
            &mut functions,
            &mut seen_function_names,
        ));
    }
    push_imported_module_trait_impls(
        program,
        &mut functions,
        &mut trait_impls,
        &mut seen_function_names,
        &mut seen_trait_impls,
    );

    let mut constants = Vec::new();
    let mut seen_constants = BTreeSet::new();
    for constant in &program.constant_init_plan {
        let key = format!("{}::{}", constant.module_name, constant.decl.name);
        if !seen_constants.insert(key.clone()) {
            continue;
        }
        let initializer = format!("__aura_const_init::{key}");
        functions.extend(lower_constant_initializer(program, &initializer, constant));
        constants.push(MirConstant {
            key,
            initializer,
            ty: constant.ty.clone(),
        });
    }

    let top_level = if program.top_level_stmts.is_empty() {
        None
    } else {
        let mut lowered = lower_top_level(program);
        let top_level = lowered.remove(0);
        functions.extend(lowered);
        Some(top_level)
    };

    MirModule {
        constants,
        functions,
        classes,
        trait_impls,
        top_level,
    }
}

fn lower_constant_initializer(
    program: &Program,
    name: &str,
    constant: &crate::sema::ConstantInfo,
) -> Vec<MirFunction> {
    let mut lowerer = Lowerer::new(
        program,
        name,
        &constant.module_name,
        constant.ty.clone(),
        BTreeMap::new(),
    )
    .with_metadata_owner(ClosureOwner::TopLevel);
    let value = lowerer.lower_expr_for_owned_value(&constant.decl.value, Some(&constant.ty));
    lowerer.terminate(Terminator::Return(value));
    lowerer.finish_with_generated(MirFunctionSpec {
        name: name.to_string(),
        span: constant.decl.span,
        receiver: None,
        params: Vec::new(),
        return_type: constant.ty.clone(),
        default_return: default_return_operand(&constant.ty),
    })
}

fn push_imported_module_functions(
    program: &Program,
    functions: &mut Vec<MirFunction>,
    seen: &mut BTreeSet<String>,
) {
    for namespace in program.module_registry.values() {
        push_imported_module_functions_from_namespace(program, namespace, functions, seen);
    }
}

fn namespace_imports_control_retry(namespace: &ModuleNamespace) -> bool {
    namespace.path == "control"
        || namespace
            .all_functions
            .values()
            .any(|function| function.module_name == "control" && function.decl.name == "retry")
        || namespace
            .modules
            .values()
            .any(namespace_imports_control_retry)
        || namespace
            .imported_modules
            .values()
            .any(namespace_imports_control_retry)
}

fn program_imports_control_retry(program: &Program) -> bool {
    program
        .functions
        .values()
        .any(|function| function.module_name == "control" && function.decl.name == "retry")
        || program
            .imported_modules
            .values()
            .any(namespace_imports_control_retry)
        || program
            .module_registry
            .values()
            .filter(|namespace| namespace.path != "control")
            .any(namespace_imports_control_retry)
}

fn push_imported_module_classes(
    program: &Program,
    classes: &mut Vec<MirClass>,
    functions: &mut Vec<MirFunction>,
    seen_function_names: &mut BTreeSet<String>,
    seen_class_names: &mut BTreeSet<String>,
) {
    for namespace in program.module_registry.values() {
        push_imported_module_classes_from_namespace(
            program,
            namespace,
            classes,
            functions,
            seen_function_names,
            seen_class_names,
        );
    }
}

fn push_imported_module_classes_from_namespace(
    program: &Program,
    namespace: &ModuleNamespace,
    classes: &mut Vec<MirClass>,
    functions: &mut Vec<MirFunction>,
    seen_function_names: &mut BTreeSet<String>,
    seen_class_names: &mut BTreeSet<String>,
) {
    for class in namespace.classes.values() {
        let class_name = mir_runtime_class_name(program, class);
        if !seen_class_names.insert(class_name.clone()) {
            continue;
        }
        let fields = class
            .decl
            .fields
            .iter()
            .map(|field| MirClassField {
                name: field.name.clone(),
                ty: class
                    .fields
                    .get(&field.name)
                    .expect("imported class field type should exist during MIR lowering")
                    .ty
                    .clone(),
            })
            .collect::<Vec<_>>();
        let mut methods = Vec::new();
        for method in class.methods.values() {
            let qualified_name = format!(
                "{}::{}.{}",
                namespace.path, class.decl.name, method.decl.name
            );
            if seen_function_names.insert(qualified_name.clone()) {
                functions.extend(lower_function(
                    program,
                    &qualified_name,
                    &class.module_name,
                    ClosureOwner::ClassMethod {
                        class_name: class.decl.name.clone(),
                        method_name: method.decl.name.clone(),
                    },
                    method.decl.receiver,
                    Some(Type::Named(
                        class_name.clone(),
                        class
                            .decl
                            .type_params
                            .iter()
                            .cloned()
                            .map(Type::TypeParam)
                            .collect(),
                    )),
                    &method.decl,
                    &method.signature.params,
                    &method.signature.param_passings,
                    &method.signature.return_type,
                    method.type_param_bounds.clone(),
                ));
            }
            methods.push(MirMethod {
                name: method.decl.name.clone(),
                function_name: qualified_name,
                receiver: method.decl.receiver.map(lower_receiver_kind),
            });
        }
        classes.push(MirClass {
            name: class_name,
            type_params: class.decl.type_params.clone(),
            fields,
            methods,
        });
    }
    for child in namespace.modules.values() {
        push_imported_module_classes_from_namespace(
            program,
            child,
            classes,
            functions,
            seen_function_names,
            seen_class_names,
        );
    }
}

fn push_imported_module_trait_impls(
    program: &Program,
    functions: &mut Vec<MirFunction>,
    trait_impls: &mut Vec<MirTraitImpl>,
    seen_function_names: &mut BTreeSet<String>,
    seen_trait_impls: &mut BTreeSet<String>,
) {
    for namespace in program.module_registry.values() {
        for trait_impl in &namespace.trait_impls {
            let impl_key = format!(
                "{}{} for {}",
                trait_impl.trait_name,
                format_trait_args(&trait_impl.trait_args),
                trait_impl.for_type
            );
            if !seen_trait_impls.insert(impl_key) {
                continue;
            }
            trait_impls.push(lower_trait_impl(
                program,
                &namespace.path,
                trait_impl,
                functions,
                seen_function_names,
            ));
        }
    }
}

fn lower_trait_impl(
    program: &Program,
    module_name: &str,
    trait_impl: &crate::sema::TraitImplInfo,
    functions: &mut Vec<MirFunction>,
    seen_function_names: &mut BTreeSet<String>,
) -> MirTraitImpl {
    let mut methods = Vec::new();
    for method in trait_impl.methods.values() {
        let qualified_name = format!(
            "{}{} for {}.{}",
            trait_impl.trait_name,
            format_trait_args(&trait_impl.trait_args),
            trait_impl.for_type,
            method.decl.name
        );
        if seen_function_names.insert(qualified_name.clone()) {
            functions.extend(lower_function(
                program,
                &qualified_name,
                module_name,
                ClosureOwner::TraitImplMethod {
                    trait_name: trait_impl.trait_name.clone(),
                    for_type: trait_impl.for_type.to_string(),
                    method_name: method.decl.name.clone(),
                },
                method.decl.receiver,
                Some(trait_impl.for_type.clone()),
                &method.decl,
                &method.signature.params,
                &method.signature.param_passings,
                &method.signature.return_type,
                crate::sema::merge_trait_bounds(
                    &trait_impl.type_param_bounds,
                    &method.type_param_bounds,
                ),
            ));
        }
        methods.push(MirMethod {
            name: method.decl.name.clone(),
            function_name: qualified_name,
            receiver: method.decl.receiver.map(lower_receiver_kind),
        });
    }
    MirTraitImpl {
        trait_name: trait_impl.trait_name.clone(),
        trait_args: trait_impl.trait_args.clone(),
        for_type: trait_impl.for_type.clone(),
        methods,
    }
}

fn format_trait_args(args: &[Type]) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            args.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn push_imported_module_functions_from_namespace(
    program: &Program,
    namespace: &ModuleNamespace,
    functions: &mut Vec<MirFunction>,
    seen: &mut BTreeSet<String>,
) {
    for (name, function) in &namespace.all_functions {
        let qualified_name = imported_module_function_name(&function.module_name, name);
        if qualified_name == "control::retry" && !program_imports_control_retry(program) {
            // The builtin registry is checker-wide, but `control.retry` owns a
            // generated Aura state machine rather than a zero-cost host
            // declaration. Do not inject that implementation into programs
            // which cannot name it; besides keeping the MIR honest, this
            // prevents its runtime adapters from leaking into unrelated
            // native objects.
            continue;
        }
        if seen.insert(qualified_name.clone()) {
            functions.extend(lower_function(
                program,
                &qualified_name,
                &function.module_name,
                ClosureOwner::Function(function.decl.name.clone()),
                function.decl.receiver,
                None,
                &function.decl,
                &function.signature.params,
                &function.signature.param_passings,
                &function.signature.return_type,
                function.type_param_bounds.clone(),
            ));
        }
    }
    for child in namespace.modules.values() {
        push_imported_module_functions_from_namespace(program, child, functions, seen);
    }
}

fn imported_module_function_name(module_path: &str, name: &str) -> String {
    format!("{}::{}", module_path, name)
}

fn lower_receiver_kind(receiver: ReceiverKind) -> MirReceiverKind {
    match receiver {
        ReceiverKind::Value => MirReceiverKind::Value,
        ReceiverKind::Borrow => MirReceiverKind::Borrow,
        ReceiverKind::BorrowMut => MirReceiverKind::BorrowMut,
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_function(
    program: &Program,
    name: &str,
    module_name: &str,
    metadata_owner: ClosureOwner,
    receiver: Option<ReceiverKind>,
    receiver_type: Option<Type>,
    function: &crate::ast::FunctionDecl,
    param_types: &[Type],
    param_passings: &[ReceiverKind],
    return_type: &Type,
    type_param_bounds: BTreeMap<String, Vec<crate::sema::TraitBound>>,
) -> Vec<MirFunction> {
    let params = function
        .params
        .iter()
        .zip(param_types.iter())
        .zip(param_passings.iter().copied())
        .enumerate()
        .map(|(index, ((param, ty), passing))| MirParam {
            name: param.name.clone(),
            passing: lower_receiver_kind(passing),
            ty: ty.clone(),
            default_function: param
                .default
                .as_ref()
                .map(|_| format!("{name}::__default_{index}_{}", param.name)),
        })
        .collect::<Vec<_>>();

    let default_functions = function
        .params
        .iter()
        .zip(param_types.iter())
        .enumerate()
        .flat_map(|(index, (param, ty))| {
            param.default.as_ref().map_or_else(Vec::new, |default| {
                lower_default_function(
                    program,
                    &format!("{name}::__default_{index}_{}", param.name),
                    module_name,
                    metadata_owner.clone(),
                    default,
                    ty,
                    type_param_bounds.clone(),
                )
            })
        })
        .collect::<Vec<_>>();

    let mut lowerer = Lowerer::new(
        program,
        name,
        module_name,
        return_type.clone(),
        type_param_bounds,
    )
    .with_metadata_owner(metadata_owner)
    .with_view_return_origin(
        function
            .view_return
            .as_ref()
            .map(|contract| contract.origin.clone()),
    );
    if let Some(receiver_type) = receiver_type {
        lowerer
            .local_types
            .insert("self".to_string(), receiver_type);
        if receiver != Some(ReceiverKind::Value) {
            lowerer.non_owning_roots.insert("self".to_string());
        }
    }
    for ((param, ty), passing) in function
        .params
        .iter()
        .zip(param_types.iter())
        .zip(param_passings.iter())
    {
        lowerer.local_types.insert(param.name.clone(), ty.clone());
        if *passing != ReceiverKind::Value {
            lowerer.non_owning_roots.insert(param.name.clone());
        }
    }
    if function.body.is_empty() && name == "control::retry" {
        // `control.retry[T, E]` is a concrete first-class function value after
        // specialization, so its named MIR target must implement the same
        // state machine as a direct source call. Keeping that implementation
        // here also means both direct calls and indirect calls share ordinary
        // MIR control flow and callback dispatch.
        let result = lowerer.new_typed_temp(return_type.clone());
        lowerer.lower_control_retry_state_machine(
            Operand::Place("worker".to_string()),
            Operand::Place("max_attempts".to_string()),
            Operand::Place("initial_backoff".to_string()),
            return_type.clone(),
            function.span,
            &result,
        );
        lowerer.terminate(Terminator::Return(Operand::MovePlace(result)));
    } else if function.body.is_empty() && has_runtime_named_function(name) {
        // Builtin declarations have signatures but no Aura body. Materialize
        // a tiny ordinary MIR wrapper so their first-class thunk follows the
        // same named-call implementation as a direct source call instead of
        // returning the empty body's fallback value.
        let result = lowerer.new_typed_temp(return_type.clone());
        let args = params
            .iter()
            .map(|param| MirArg {
                name: Some(param.name.clone()),
                value: if param.passing == MirReceiverKind::Value {
                    Operand::MovePlace(param.name.clone())
                } else {
                    Operand::Place(param.name.clone())
                },
                writeback_place: (param.passing == MirReceiverKind::BorrowMut)
                    .then(|| param.name.clone()),
            })
            .collect();
        lowerer.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Name(name.to_string()),
                args,
            },
        });
        lowerer.terminate(Terminator::Return(Operand::Place(result)));
    } else {
        lowerer.lower_stmts(&function.body);
    }
    let functions = lowerer.finish_with_generated(MirFunctionSpec {
        name: name.to_string(),
        span: function.span,
        receiver: receiver.map(lower_receiver_kind),
        params,
        return_type: return_type.clone(),
        default_return: default_return_operand(return_type),
    });
    functions.into_iter().chain(default_functions).collect()
}

fn lower_default_function(
    program: &Program,
    name: &str,
    module_name: &str,
    metadata_owner: ClosureOwner,
    default: &Expr,
    return_type: &Type,
    type_param_bounds: BTreeMap<String, Vec<crate::sema::TraitBound>>,
) -> Vec<MirFunction> {
    let mut lowerer = Lowerer::new(
        program,
        name,
        module_name,
        return_type.clone(),
        type_param_bounds,
    )
    .with_metadata_owner(metadata_owner);
    let value = lowerer.lower_expr_for_owned_value(default, Some(return_type));
    lowerer.terminate(Terminator::Return(value));
    lowerer.finish_with_generated(MirFunctionSpec {
        name: name.to_string(),
        span: default.span,
        receiver: None,
        params: Vec::new(),
        return_type: return_type.clone(),
        default_return: default_return_operand(return_type),
    })
}

fn lower_top_level(program: &Program) -> Vec<MirFunction> {
    let mut lowerer = Lowerer::new(
        program,
        "__script",
        &program.module_name,
        Type::named("int32"),
        BTreeMap::new(),
    )
    .with_metadata_owner(ClosureOwner::TopLevel);
    lowerer.lower_stmts(&program.top_level_stmts);
    lowerer.finish_with_generated(MirFunctionSpec {
        name: "__script".to_string(),
        span: crate::diag::Span::new(1, 1),
        receiver: None,
        params: Vec::new(),
        return_type: Type::named("int32"),
        default_return: Operand::Int(0),
    })
}

struct MirFunctionSpec {
    name: String,
    span: Span,
    receiver: Option<MirReceiverKind>,
    params: Vec<MirParam>,
    return_type: Type,
    default_return: Operand,
}

struct VecTransformOutput<'a> {
    place: &'a str,
    element_type: &'a Type,
}

struct PendingAssertionCapture {
    label: &'static str,
    ty: Type,
    value: Operand,
}

struct Lowerer<'a> {
    program: &'a Program,
    function_name: &'a str,
    module_name: &'a str,
    metadata_module_name: Option<String>,
    metadata_owner: Option<ClosureOwner>,
    return_type: Type,
    type_param_bounds: BTreeMap<String, Vec<crate::sema::TraitBound>>,
    blocks: Vec<BasicBlockBuilder>,
    current_block: usize,
    temp_counter: usize,
    block_counter: usize,
    loop_stack: Vec<LoopLabels>,
    return_redirects: Vec<ReturnRedirect>,
    with_stack: Vec<String>,
    match_writeback_stack: Vec<MatchWritebackState>,
    local_types: std::collections::BTreeMap<String, Type>,
    non_owning_roots: BTreeSet<String>,
    scoped_names: Vec<std::collections::HashMap<String, String>>,
    generated_functions: Vec<MirFunction>,
    view_sources: BTreeMap<String, String>,
    loan_scopes: Vec<Vec<String>>,
    view_return_origin: Option<String>,
}

#[derive(Clone, Debug)]
enum PatternWriteback {
    Use(Operand),
    Or {
        ty: Type,
        selected: Vec<String>,
        alternatives: Vec<PatternWriteback>,
    },
    Variant {
        ty: Type,
        enum_name: String,
        variant_name: String,
        payloads: Vec<PatternWriteback>,
    },
}

#[derive(Clone, Copy)]
struct PatternLoweringOptions {
    collect_writeback: bool,
    consume_payloads: bool,
}

struct TaskStartTarget {
    function_name: Option<String>,
    params: Vec<Param>,
    param_types: Vec<Type>,
    param_passings: Vec<ReceiverKind>,
    /// The callable contract carried by a first-class function value.
    ///
    /// Unlike `params`, this is also populated for dynamically selected
    /// targets, where parameter names and the default mask remain observable
    /// to argument binding even though no declaration is statically known.
    param_contracts: Vec<FunctionParamContract>,
    return_type: Type,
    type_params: Vec<String>,
    substitutions: std::collections::HashMap<String, Type>,
    display_name: String,
}

impl TaskStartTarget {
    fn function_type(&self) -> Type {
        Type::Function {
            params: self.param_contracts.clone(),
            return_type: Box::new(self.return_type.clone()),
        }
    }
}

fn task_param_contracts(
    params: &[Param],
    param_types: &[Type],
    param_passings: &[ReceiverKind],
) -> Vec<FunctionParamContract> {
    param_types
        .iter()
        .enumerate()
        .map(|(index, ty)| FunctionParamContract {
            name: params
                .get(index)
                .map(|param| param.name.clone())
                .unwrap_or_default(),
            ty: ty.clone(),
            passing: param_passings
                .get(index)
                .copied()
                .unwrap_or(ReceiverKind::Borrow),
            has_default: params
                .get(index)
                .is_some_and(|param| param.default.is_some()),
            default_erased: false,
        })
        .collect()
}

#[derive(Clone)]
struct MatchWritebackState {
    root: String,
    skip_place: String,
    /// How to rebuild the scrutinee from the arm's bindings. ADR-0022 Q3
    /// requires this on every exit path, not just normal arm fall-through, so
    /// `return`, `break`, and `continue` need it too.
    writeback: Option<PatternWriteback>,
}

struct LoopLabels {
    break_label: String,
    continue_label: String,
    cleanup_depth: usize,
    loan_depth: usize,
}

struct ReturnRedirect {
    label: String,
    return_place: String,
    cleanup_depth: usize,
}

struct BasicBlockBuilder {
    label: String,
    instructions: Vec<Instruction>,
    terminator: Option<Terminator>,
}

impl<'a> Lowerer<'a> {
    fn trait_info_in_scope(&self, name: &str) -> Option<&crate::sema::TraitInfo> {
        self.program.traits.get(name).or_else(|| {
            self.program
                .module_registry
                .values()
                .find_map(|namespace| namespace.all_traits.get(name))
        })
    }

    fn find_namespace_in_modules<'b>(
        modules: &'b BTreeMap<String, ModuleNamespace>,
        path: &str,
    ) -> Option<&'b ModuleNamespace> {
        for namespace in modules.values() {
            if namespace.path == path {
                return Some(namespace);
            }
            if let Some(found) = Self::find_namespace_in_modules(&namespace.modules, path) {
                return Some(found);
            }
            if let Some(found) = Self::find_namespace_in_modules(&namespace.imported_modules, path)
            {
                return Some(found);
            }
        }
        None
    }

    fn new(
        program: &'a Program,
        function_name: &'a str,
        module_name: &'a str,
        return_type: Type,
        type_param_bounds: BTreeMap<String, Vec<crate::sema::TraitBound>>,
    ) -> Self {
        Self {
            program,
            function_name,
            module_name,
            metadata_module_name: None,
            metadata_owner: None,
            return_type,
            type_param_bounds,
            blocks: vec![BasicBlockBuilder {
                label: "entry".to_string(),
                instructions: Vec::new(),
                terminator: None,
            }],
            current_block: 0,
            temp_counter: 0,
            block_counter: 0,
            loop_stack: Vec::new(),
            return_redirects: Vec::new(),
            with_stack: Vec::new(),
            match_writeback_stack: Vec::new(),
            local_types: std::collections::BTreeMap::new(),
            non_owning_roots: BTreeSet::new(),
            scoped_names: Vec::new(),
            generated_functions: Vec::new(),
            view_sources: BTreeMap::new(),
            loan_scopes: Vec::new(),
            view_return_origin: None,
        }
    }

    fn with_metadata_owner(mut self, owner: ClosureOwner) -> Self {
        self.metadata_owner = Some(owner);
        self
    }

    fn with_view_return_origin(mut self, origin: Option<String>) -> Self {
        self.view_return_origin = origin;
        self
    }

    fn closure_info_at(&self, span: Span) -> Option<&ClosureInfo> {
        let closures = if self.module_name == self.program.module_name {
            &self.program.closures
        } else {
            &self.module_namespace(self.module_name)?.closures
        };
        let mut matching = closures.values().filter(|info| {
            info.id.module_name == self.module_name
                && info.span.line == span.line
                && info.span.column == span.column
        });
        let found = matching.next()?;
        matching.next().is_none().then_some(found)
    }

    fn comprehension_info_at(&self, span: Span) -> Option<&ComprehensionInfo> {
        let module_name = self
            .metadata_module_name
            .as_deref()
            .unwrap_or(self.module_name);
        let comprehensions = if module_name == self.program.module_name {
            &self.program.comprehensions
        } else {
            &self.module_namespace(module_name)?.comprehensions
        };
        let matching = comprehensions.values().filter(|info| {
            info.id.module_name == module_name
                && info.id.line == span.line
                && info.id.column == span.column
        });
        if let Some(owner) = &self.metadata_owner {
            if let Some(found) = matching.clone().find(|info| &info.id.owner == owner) {
                return Some(found);
            }
        }
        let mut matching = matching;
        let found = matching.next()?;
        matching.next().is_none().then_some(found)
    }

    fn lower_field_default(
        &mut self,
        default: &Expr,
        field_type: &Type,
        defining_module: &str,
    ) -> Operand {
        let previous_module = self
            .metadata_module_name
            .replace(defining_module.to_string());
        let previous_owner = self.metadata_owner.replace(ClosureOwner::TopLevel);
        let value = self.lower_expr_for_owned_value(default, Some(field_type));
        self.metadata_module_name = previous_module;
        self.metadata_owner = previous_owner;
        value
    }

    fn module_namespace(&self, path: &str) -> Option<&ModuleNamespace> {
        if let Some(namespace) = self.program.module_registry.get(path) {
            return Some(namespace);
        }
        self.current_module_namespace()
            .and_then(|current| Self::find_namespace_in_modules(&current.imported_modules, path))
            .or_else(|| Self::find_namespace_in_modules(&self.program.imported_modules, path))
    }

    fn current_module_namespace(&self) -> Option<&ModuleNamespace> {
        if self.module_name == self.program.module_name {
            None
        } else {
            self.program.module_registry.get(self.module_name)
        }
    }

    fn resolve_function_info(&self, name: &str) -> Option<&crate::sema::FunctionInfo> {
        self.current_module_namespace()
            .and_then(|namespace| namespace.all_functions.get(name))
            .or_else(|| self.program.functions.get(name))
    }

    fn resolve_extern_function_info(&self, name: &str) -> Option<&crate::sema::ExternFunctionInfo> {
        self.current_module_namespace()
            .and_then(|namespace| namespace.all_extern_functions.get(name))
            .or_else(|| self.program.extern_functions.get(name))
    }

    fn extern_call_target(function: &crate::sema::ExternFunctionInfo) -> MirExternCall {
        MirExternCall {
            symbol: function.decl.name.clone(),
            abi: function.decl.abi.clone(),
            params: function
                .decl
                .params
                .iter()
                .zip(
                    function
                        .signature
                        .params
                        .iter()
                        .zip(&function.signature.param_passings),
                )
                .map(|(param, (ty, passing))| MirExternParam {
                    name: param.name.clone(),
                    passing: lower_receiver_kind(*passing),
                    ty: ty.clone(),
                })
                .collect(),
            return_type: function.signature.return_type.clone(),
        }
    }

    fn function_type(
        &self,
        function: &crate::sema::FunctionInfo,
        substitutions: &std::collections::HashMap<String, Type>,
    ) -> Type {
        Type::Function {
            params: function
                .signature
                .params
                .iter()
                .enumerate()
                .map(|(index, param)| FunctionParamContract {
                    name: function
                        .decl
                        .params
                        .get(index)
                        .map(|param| param.name.clone())
                        .unwrap_or_default(),
                    ty: substitute_type(param, substitutions),
                    passing: function
                        .signature
                        .param_passings
                        .get(index)
                        .copied()
                        .unwrap_or(ReceiverKind::Borrow),
                    has_default: function
                        .decl
                        .params
                        .get(index)
                        .is_some_and(|param| param.default.is_some()),
                    default_erased: false,
                })
                .collect(),
            return_type: Box::new(substitute_type(
                &function.signature.return_type,
                substitutions,
            )),
        }
    }

    fn function_runtime_name(
        &self,
        source_name: &str,
        function: &crate::sema::FunctionInfo,
    ) -> String {
        if function.module_name == self.program.module_name {
            source_name.to_string()
        } else {
            imported_module_function_name(&function.module_name, &function.decl.name)
        }
    }

    fn resolve_function_value_target(
        &self,
        expr: &Expr,
    ) -> Option<(String, &crate::sema::FunctionInfo)> {
        match &expr.kind {
            ExprKind::Group(inner) => self.resolve_function_value_target(inner),
            ExprKind::Name(name) => {
                if self.local_types.contains_key(&self.render_local_name(name)) {
                    return None;
                }
                let function = self.resolve_function_info(name)?;
                Some((self.function_runtime_name(name, function), function))
            }
            ExprKind::Member { object, field } => {
                let module_path = self.infer_module_path(object)?;
                let namespace = self.module_namespace(&module_path)?;
                let function = namespace
                    .functions
                    .get(field)
                    .or_else(|| namespace.all_functions.get(field))?;
                Some((
                    imported_module_function_name(&function.module_name, &function.decl.name),
                    function,
                ))
            }
            _ => None,
        }
    }

    fn lower_function_value(&self, expr: &Expr) -> Option<Operand> {
        match &expr.kind {
            ExprKind::Group(inner) => self.lower_function_value(inner),
            ExprKind::Specialize { expr, type_args } => {
                let (runtime_name, function) = self.resolve_function_value_target(expr)?;
                let substitutions = substitutions_from_decl_type_args(
                    &function.decl.type_params,
                    &type_args
                        .iter()
                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                        .collect::<Vec<_>>(),
                );
                Some(Operand::Function {
                    name: runtime_name,
                    signature: Box::new(self.function_type(function, &substitutions)),
                })
            }
            ExprKind::Index { object, index } => {
                let (runtime_name, function) = self.resolve_function_value_target(object)?;
                let type_args = self.task_type_args_from_index_expr(index)?;
                if function.decl.type_params.is_empty()
                    || function.decl.type_params.len() != type_args.len()
                {
                    return None;
                }
                let substitutions =
                    substitutions_from_decl_type_args(&function.decl.type_params, &type_args);
                Some(Operand::Function {
                    name: runtime_name,
                    signature: Box::new(self.function_type(function, &substitutions)),
                })
            }
            ExprKind::Name(_) | ExprKind::Member { .. } => {
                let (runtime_name, function) = self.resolve_function_value_target(expr)?;
                Some(Operand::Function {
                    name: runtime_name,
                    signature: Box::new(
                        self.function_type(function, &std::collections::HashMap::new()),
                    ),
                })
            }
            _ => None,
        }
    }

    fn lower_lambda(
        &mut self,
        expr: &Expr,
        params: &[crate::ast::LambdaParam],
        body: &Expr,
    ) -> Operand {
        let info = self
            .closure_info_at(expr.span)
            .cloned()
            .expect("checked lambda should retain owner-qualified closure metadata");
        debug_assert_eq!(params.len(), info.params.len());
        let name = format!(
            "{}::__lambda_{}_{}",
            self.function_name, expr.span.line, expr.span.column
        );
        let mut mir_params = info
            .captures
            .iter()
            .map(|capture| MirParam {
                name: capture.name.clone(),
                passing: match capture.mode {
                    ClosureCaptureMode::SharedView => MirReceiverKind::Borrow,
                    ClosureCaptureMode::MutableView => MirReceiverKind::BorrowMut,
                    ClosureCaptureMode::Copy | ClosureCaptureMode::Move => MirReceiverKind::Value,
                },
                ty: capture.ty.clone(),
                default_function: None,
            })
            .collect::<Vec<_>>();
        mir_params.extend(info.params.iter().map(|param| MirParam {
            name: param.name.clone(),
            passing: lower_receiver_kind(param.passing),
            ty: param.ty.clone(),
            default_function: None,
        }));

        let mut lowerer = Lowerer::new(
            self.program,
            &name,
            self.module_name,
            info.return_type.clone(),
            self.type_param_bounds.clone(),
        );
        lowerer.metadata_module_name = self.metadata_module_name.clone();
        lowerer.metadata_owner = self.metadata_owner.clone();
        for capture in &info.captures {
            lowerer
                .local_types
                .insert(capture.name.clone(), capture.ty.clone());
            if info.call_kind != ClosureCallKind::Consuming {
                lowerer.non_owning_roots.insert(capture.name.clone());
            }
        }
        for param in &info.params {
            lowerer
                .local_types
                .insert(param.name.clone(), param.ty.clone());
            if param.passing != ReceiverKind::Value {
                lowerer.non_owning_roots.insert(param.name.clone());
            }
        }
        let result = lowerer.lower_expr_for_owned_value(body, Some(&info.return_type));
        lowerer.terminate(Terminator::Return(result));
        self.generated_functions
            .extend(lowerer.finish_with_generated(MirFunctionSpec {
                name: name.clone(),
                span: expr.span,
                receiver: None,
                params: mir_params,
                return_type: info.return_type.clone(),
                default_return: default_return_operand(&info.return_type),
            }));

        let signature = info.ty();
        if info.captures.is_empty() {
            return Operand::Function {
                name,
                signature: Box::new(signature),
            };
        }
        let captures = info
            .captures
            .iter()
            .map(|capture| {
                let place = self.render_local_name(&capture.name);
                MirClosureCapture {
                    name: capture.name.clone(),
                    value: match capture.mode {
                        ClosureCaptureMode::Copy => Operand::Place(place.clone()),
                        ClosureCaptureMode::Move => Operand::MovePlace(place.clone()),
                        ClosureCaptureMode::SharedView | ClosureCaptureMode::MutableView => {
                            Operand::Place(place.clone())
                        }
                    },
                    ty: capture.ty.clone(),
                    passing: match capture.mode {
                        ClosureCaptureMode::SharedView => MirReceiverKind::Borrow,
                        ClosureCaptureMode::MutableView => MirReceiverKind::BorrowMut,
                        ClosureCaptureMode::Copy | ClosureCaptureMode::Move => {
                            MirReceiverKind::Value
                        }
                    },
                    source_place: matches!(
                        capture.mode,
                        ClosureCaptureMode::SharedView | ClosureCaptureMode::MutableView
                    )
                    .then_some(place),
                }
            })
            .collect::<Vec<_>>();
        let temp = self.new_typed_temp(signature.clone());
        self.emit(Instruction::Assign {
            target: temp.clone(),
            value: Rvalue::Closure {
                function: name,
                signature,
                captures,
                consuming: info.call_kind == ClosureCallKind::Consuming,
            },
        });
        Operand::Place(temp)
    }

    fn resolve_class_info(&self, name: &str) -> Option<&crate::sema::ClassInfo> {
        if let Some((module_path, item_name)) = name.rsplit_once('.') {
            if let Some(namespace) = self.module_namespace(module_path) {
                if let Some(class_info) = namespace
                    .classes
                    .get(item_name)
                    .or_else(|| namespace.all_classes.get(item_name))
                {
                    return Some(class_info);
                }
            }
        }
        self.current_module_namespace()
            .and_then(|namespace| namespace.all_classes.get(name))
            .or_else(|| self.program.classes.get(name))
            .or_else(|| self.find_imported_class_info(name))
    }

    fn resolve_enum_info(&self, name: &str) -> Option<&crate::sema::EnumInfo> {
        if let Some((module_path, item_name)) = name.rsplit_once('.') {
            if let Some(namespace) = self.module_namespace(module_path) {
                if let Some(enum_info) = namespace
                    .enums
                    .get(item_name)
                    .or_else(|| namespace.all_enums.get(item_name))
                {
                    return Some(enum_info);
                }
            }
        }
        self.current_module_namespace()
            .and_then(|namespace| namespace.all_enums.get(name))
            .or_else(|| self.program.enums.get(name))
            .or_else(|| self.find_imported_enum_info(name))
    }

    fn lower_type_ref_with_provenance(&self, type_ref: &crate::ast::TypeRef) -> Type {
        match &type_ref.kind {
            TypeRefKind::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|element| self.lower_type_ref_with_provenance(element))
                    .collect(),
            ),
            TypeRefKind::Function {
                params,
                return_type,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|param| FunctionParamContract {
                        name: String::new(),
                        ty: self.lower_type_ref_with_provenance(&param.ty),
                        passing: resolve_param_passing(param.mode),
                        has_default: false,
                        default_erased: true,
                    })
                    .collect(),
                return_type: Box::new(self.lower_type_ref_with_provenance(return_type)),
            },
            TypeRefKind::Named {
                name: source_name,
                args,
            } => {
                if source_name == "None" {
                    return Type::Unit;
                }
                let source_name = match source_name.as_str() {
                    "str" => "str",
                    "int" => "int64",
                    name => name,
                };
                let name = if let Some(class) = self.resolve_class_info(source_name) {
                    mir_class_type_name(self.program, class, source_name)
                } else if let Some(enum_info) = self.resolve_enum_info(source_name) {
                    mir_runtime_enum_name(self.program, enum_info)
                } else {
                    source_name.to_string()
                };
                Type::Named(
                    name,
                    args.iter()
                        .map(|arg| self.lower_type_ref_with_provenance(arg))
                        .collect(),
                )
            }
        }
    }

    fn infer_class_constructor_type(
        &self,
        class_name: &str,
        args: &[Argument],
        explicit_type_args: Option<&[crate::ast::TypeRef]>,
    ) -> Option<Type> {
        let class = self.resolve_class_info(class_name)?;
        let result_name = mir_class_type_name(self.program, class, class_name);
        if let Some(type_args) = explicit_type_args {
            return Some(Type::Named(
                result_name.clone(),
                type_args
                    .iter()
                    .map(|ty| self.lower_type_ref_with_provenance(ty))
                    .collect(),
            ));
        }
        if class.decl.type_params.is_empty() {
            return Some(Type::named(result_name));
        }

        let mut next_positional_field = 0usize;
        let mut saw_named = false;
        let type_params = class
            .decl
            .type_params
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut substitutions = std::collections::HashMap::new();

        for argument in args {
            let field_name = if let Some(field_name) = argument.name.as_ref() {
                saw_named = true;
                field_name.clone()
            } else {
                if saw_named {
                    return Some(Type::named(result_name.clone()));
                }
                let field = class.decl.fields.get(next_positional_field)?;
                next_positional_field += 1;
                field.name.clone()
            };
            let field_info = class.fields.get(&field_name)?;
            let actual_ty = self.infer_expr_type(&argument.value)?;
            if !crate::sema::type_pattern_matches(
                &field_info.ty,
                &actual_ty,
                &type_params,
                &mut substitutions,
            ) {
                return Some(Type::named(result_name.clone()));
            }
        }

        let resolved_args = class
            .decl
            .type_params
            .iter()
            .map(|type_param| substitutions.get(type_param).cloned())
            .collect::<Option<Vec<_>>>();
        resolved_args
            .map(|resolved_args| Type::Named(result_name.clone(), resolved_args))
            .or_else(|| Some(Type::named(result_name)))
    }

    fn infer_module_path(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(name) => self
                .current_module_namespace()
                .and_then(|namespace| namespace.imported_modules.get(name))
                .or_else(|| self.program.imported_modules.get(name))
                .map(|namespace| namespace.path.clone())
                .or_else(|| {
                    // Builtin declarations retain their public qualified
                    // spelling in default expressions (for example,
                    // `process.pipe()`). A hidden default function is lowered
                    // in that builtin's own namespace, where the module is not
                    // also present as an import of itself.
                    self.current_module_namespace()
                        .filter(|namespace| namespace.path == *name)
                        .map(|namespace| namespace.path.clone())
                }),
            ExprKind::Specialize { expr, .. } => self.infer_module_path(expr),
            ExprKind::Group(inner) => self.infer_module_path(inner),
            ExprKind::Member { object, field } => {
                let parent = self.infer_module_path(object)?;
                let namespace = self.module_namespace(&parent)?;
                namespace.modules.get(field).map(|child| child.path.clone())
            }
            _ => None,
        }
    }

    fn resolve_constant_info(&self, name: &str) -> Option<&crate::sema::ConstantInfo> {
        if self.local_types.contains_key(&self.render_local_name(name)) {
            return None;
        }
        if self.module_name == self.program.module_name {
            self.program.constants.get(name)
        } else {
            self.current_module_namespace()?.all_constants.get(name)
        }
    }

    fn lower_constant_read(&mut self, constant: &crate::sema::ConstantInfo) -> Operand {
        let temp = self.new_typed_temp(constant.ty.clone());
        let key = format!("{}::{}", constant.module_name, constant.decl.name);
        self.emit(Instruction::Assign {
            target: temp.clone(),
            value: Rvalue::ModuleConstant {
                initializer: format!("__aura_const_init::{key}"),
                key,
            },
        });
        Operand::Place(temp)
    }

    fn qualified_module_item(&self, expr: &Expr) -> Option<(String, String)> {
        match &expr.kind {
            ExprKind::Specialize { expr, .. } => self.qualified_module_item(expr),
            ExprKind::Member { object, field } => self
                .infer_module_path(object)
                .map(|path| (path, field.clone())),
            ExprKind::Group(inner) => self.qualified_module_item(inner),
            _ => None,
        }
    }

    fn find_class_in_modules<'b>(
        modules: &'b BTreeMap<String, ModuleNamespace>,
        name: &str,
        found: &mut Option<&'b crate::sema::ClassInfo>,
        ambiguous: &mut bool,
    ) {
        for namespace in modules.values() {
            if let Some(class_info) = namespace
                .classes
                .get(name)
                .or_else(|| namespace.all_classes.get(name))
            {
                if found.is_some() {
                    *ambiguous = true;
                } else {
                    *found = Some(class_info);
                }
            }
            Self::find_class_in_modules(&namespace.modules, name, found, ambiguous);
        }
    }

    fn find_enum_in_modules<'b>(
        modules: &'b BTreeMap<String, ModuleNamespace>,
        name: &str,
        found: &mut Option<&'b crate::sema::EnumInfo>,
        ambiguous: &mut bool,
    ) {
        for namespace in modules.values() {
            if let Some(enum_info) = namespace
                .enums
                .get(name)
                .or_else(|| namespace.all_enums.get(name))
            {
                if found.is_some() {
                    *ambiguous = true;
                } else {
                    *found = Some(enum_info);
                }
            }
            Self::find_enum_in_modules(&namespace.modules, name, found, ambiguous);
        }
    }

    fn find_imported_class_info(&self, name: &str) -> Option<&crate::sema::ClassInfo> {
        let modules = self
            .current_module_namespace()
            .map(|namespace| &namespace.imported_modules)
            .unwrap_or(&self.program.imported_modules);
        let mut found = None;
        let mut ambiguous = false;
        Self::find_class_in_modules(modules, name, &mut found, &mut ambiguous);
        if ambiguous {
            None
        } else {
            found
        }
    }

    fn find_imported_enum_info(&self, name: &str) -> Option<&crate::sema::EnumInfo> {
        let modules = self
            .current_module_namespace()
            .map(|namespace| &namespace.imported_modules)
            .unwrap_or(&self.program.imported_modules);
        let mut found = None;
        let mut ambiguous = false;
        Self::find_enum_in_modules(modules, name, &mut found, &mut ambiguous);
        if ambiguous {
            None
        } else {
            found
        }
    }

    fn finish(mut self, spec: MirFunctionSpec) -> MirFunction {
        if self.blocks[self.current_block].terminator.is_none() {
            self.blocks[self.current_block].terminator =
                Some(Terminator::Return(spec.default_return));
        }

        MirFunction {
            name: spec.name,
            module_name: self.module_name.to_string(),
            source_path: if self.module_name == self.program.module_name {
                self.program.source_path.clone()
            } else {
                self.program
                    .module_registry
                    .get(self.module_name)
                    .and_then(|namespace| namespace.source_path.clone())
                    .or_else(|| {
                        self.program
                            .imported_modules
                            .values()
                            .find(|namespace| namespace.path == self.module_name)
                            .and_then(|namespace| namespace.source_path.clone())
                    })
            },
            span: spec.span,
            receiver: spec.receiver,
            params: spec.params,
            local_types: self
                .local_types
                .into_iter()
                .map(|(name, ty)| MirLocalType { name, ty })
                .collect(),
            return_type: spec.return_type,
            entry: self.blocks[0].label.clone(),
            blocks: self
                .blocks
                .into_iter()
                .map(|block| BasicBlock {
                    label: block.label,
                    instructions: block.instructions,
                    terminator: block.terminator.unwrap_or(Terminator::Unreachable),
                })
                .collect(),
        }
    }

    fn finish_with_generated(mut self, spec: MirFunctionSpec) -> Vec<MirFunction> {
        let generated = std::mem::take(&mut self.generated_functions);
        std::iter::once(self.finish(spec))
            .chain(generated)
            .collect()
    }

    fn lower_stmts(&mut self, statements: &[Stmt]) {
        self.loan_scopes.push(Vec::new());
        for (index, stmt) in statements.iter().enumerate() {
            if !self.lower_stmt(stmt) {
                break;
            }
            let ending = self
                .loan_scopes
                .last()
                .into_iter()
                .flatten()
                .filter(|loan| {
                    !statements[index + 1..]
                        .iter()
                        .any(|later| crate::sema::stmt_references_name(later, loan))
                })
                .cloned()
                .collect::<Vec<_>>();
            for loan in ending {
                if self.view_sources.remove(&loan).is_some() {
                    self.emit(Instruction::EndLoan { loan: loan.clone() });
                }
                if let Some(scope) = self.loan_scopes.last_mut() {
                    scope.retain(|active| active != &loan);
                }
            }
        }
        if !self.current_terminated() {
            let scoped_views = self.loan_scopes.last().cloned().unwrap_or_default();
            for loan in scoped_views.into_iter().rev() {
                if self.view_sources.remove(&loan).is_some() {
                    self.emit(Instruction::EndLoan { loan });
                }
            }
        }
        self.loan_scopes.pop();
    }

    fn emit_loan_cleanup_from(&mut self, depth: usize, except: Option<&str>) {
        let loans = self
            .loan_scopes
            .iter()
            .skip(depth)
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .filter(|loan| except != Some(loan.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for loan in loans {
            if self.view_sources.remove(&loan).is_some() {
                self.emit(Instruction::EndLoan { loan });
            }
        }
    }

    fn lower_stmt(&mut self, stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Assign(assign) => {
                self.lower_assign(assign);
                true
            }
            Stmt::View(view) => {
                let ty = self.infer_expr_type(&view.source);
                if let Some(ty) = ty.clone() {
                    self.local_types.entry(view.name.clone()).or_insert(ty);
                }
                let returned_source = self.returned_view_source(&view.source);
                let source = if let Some(source) = self.render_place_expr_option(&view.source) {
                    source
                } else {
                    let (origin, projections) = returned_source
                        .clone()
                        .expect("checked returned-view calls retain an addressable origin");
                    let _ = self.lower_expr(&view.source);
                    self.emit(Instruction::BeginReturnedLoan {
                        loan: view.name.clone(),
                        origin: origin.clone(),
                        projections,
                        mutable: view.mutable,
                    });
                    origin
                };
                let root = source.split('.').next().unwrap_or(source.as_str());
                if returned_source.is_some() {
                    // The call above transfers the exact selected projection
                    // into the new caller-side descriptor.
                } else if self.view_sources.contains_key(root) {
                    let projection = source
                        .strip_prefix(root)
                        .unwrap_or_default()
                        .trim_start_matches('.')
                        .to_string();
                    self.emit(Instruction::Reborrow {
                        loan: view.name.clone(),
                        parent: root.to_string(),
                        projection,
                        mutable: view.mutable,
                    });
                } else {
                    self.emit(Instruction::BeginLoan {
                        loan: view.name.clone(),
                        source: source.clone(),
                        mutable: view.mutable,
                    });
                }
                self.view_sources.insert(view.name.clone(), source);
                if let Some(scope) = self.loan_scopes.last_mut() {
                    scope.push(view.name.clone());
                }
                true
            }
            Stmt::Destructure(destructure) => {
                self.lower_destructure(destructure);
                true
            }
            Stmt::Pass(_) => true,
            Stmt::Expr(expr_stmt) => {
                let value_type = self.infer_expr_type(&expr_stmt.expr);
                let value = self.lower_expr_for_owned_value(&expr_stmt.expr, value_type.as_ref());
                self.emit(Instruction::Eval { value });
                true
            }
            Stmt::Assert(assert_stmt) => {
                self.lower_assert(assert_stmt);
                true
            }
            Stmt::Return(return_stmt) => {
                let value = if let Some(value) = &return_stmt.value {
                    let return_type = self.return_type.clone();
                    self.lower_expr_for_owned_value(value, Some(&return_type))
                } else {
                    Operand::Unit
                };
                self.emit_active_match_writebacks();
                let mut returned_loan = None;
                if return_stmt.view.is_some() {
                    if let Some(value) = &return_stmt.value {
                        if let Some(loan) = self.render_place_expr_option(value) {
                            let root = loan.split('.').next().unwrap_or(loan.as_str());
                            if self.view_sources.contains_key(root) {
                                returned_loan = Some(root.to_string());
                            }
                            self.emit(Instruction::ReturnLoan {
                                loan,
                                origin: self
                                    .view_return_origin
                                    .clone()
                                    .expect("checked view returns declare an origin"),
                            });
                        }
                    }
                }
                self.emit_loan_cleanup_from(0, returned_loan.as_deref());
                if let Some(redirect) = self.return_redirects.last() {
                    let return_place = redirect.return_place.clone();
                    let cleanup_depth = redirect.cleanup_depth;
                    let label = redirect.label.clone();
                    self.emit(Instruction::Assign {
                        target: return_place,
                        value: Rvalue::Use(value),
                    });
                    self.emit_cleanup_range(cleanup_depth, true);
                    self.terminate(Terminator::Goto(label));
                } else {
                    let mut value = value;
                    if !self.with_stack.is_empty() {
                        let temp = self.new_temp();
                        self.emit(Instruction::Assign {
                            target: temp.clone(),
                            value: Rvalue::Use(value),
                        });
                        value = self.move_place_for_type(temp, &self.return_type);
                    }
                    self.emit_cleanup_range(0, true);
                    self.terminate(Terminator::Return(value));
                }
                false
            }
            Stmt::If(if_stmt) => {
                self.lower_if(if_stmt);
                true
            }
            Stmt::Match(match_stmt) => {
                self.lower_match(match_stmt);
                true
            }
            Stmt::For(for_stmt) => {
                self.lower_for(for_stmt);
                true
            }
            Stmt::With(with_stmt) => {
                if let Some(inferred) = self.infer_expr_type(&with_stmt.value) {
                    self.local_types
                        .entry(with_stmt.binding.clone())
                        .or_insert(inferred);
                }
                let expected = self.infer_expr_type(&with_stmt.value);
                let value = self.lower_expr_for_owned_value(&with_stmt.value, expected.as_ref());
                self.emit(Instruction::Assign {
                    target: with_stmt.binding.clone(),
                    value: Rvalue::Use(value),
                });
                self.emit(Instruction::PushCleanup {
                    place: with_stmt.binding.clone(),
                });
                self.with_stack.push(with_stmt.binding.clone());
                self.lower_stmts(&with_stmt.body);
                if !self.current_terminated() {
                    self.emit(Instruction::PopCleanup {
                        place: with_stmt.binding.clone(),
                        cancel_before_cleanup: false,
                    });
                }
                self.with_stack.pop();
                !self.current_terminated()
            }
            Stmt::While(while_stmt) => {
                self.lower_while(while_stmt);
                true
            }
            Stmt::Break(_) => {
                self.emit_active_match_writebacks();
                let loop_labels = self.loop_stack.last().expect("checked loop context");
                let cleanup_depth = loop_labels.cleanup_depth;
                let loan_depth = loop_labels.loan_depth;
                let break_label = loop_labels.break_label.clone();
                self.emit_loan_cleanup_from(loan_depth, None);
                self.emit_cleanup_range(cleanup_depth, true);
                self.terminate(Terminator::Goto(break_label));
                false
            }
            Stmt::Continue(_) => {
                self.emit_active_match_writebacks();
                let loop_labels = self.loop_stack.last().expect("checked loop context");
                let cleanup_depth = loop_labels.cleanup_depth;
                let loan_depth = loop_labels.loan_depth;
                let continue_label = loop_labels.continue_label.clone();
                self.emit_loan_cleanup_from(loan_depth, None);
                self.emit_cleanup_range(cleanup_depth, true);
                self.terminate(Terminator::Goto(continue_label));
                false
            }
        }
    }

    fn lower_assert(&mut self, assert_stmt: &crate::ast::AssertStmt) {
        let (condition, pending_captures) = self
            .lower_introspected_assertion_condition(&assert_stmt.condition)
            .unwrap_or_else(|| (self.lower_expr(&assert_stmt.condition), Vec::new()));
        let failure_block = self.new_block("assert_fail");
        let continuation_block = self.new_block("assert_pass");
        self.terminate(Terminator::Branch {
            condition,
            then_label: self.label(continuation_block),
            else_label: self.label(failure_block),
        });

        self.switch_to(failure_block);
        // Render retained operands only on failure, before evaluating the lazy
        // source message. The comparison itself has already consumed the raw
        // captures, so formatting cannot alter its result or evaluate either
        // source expression a second time.
        let captures = pending_captures
            .into_iter()
            .map(|capture| {
                let rendered = self.new_typed_temp(Type::named("str"));
                self.emit(Instruction::Assign {
                    target: rendered.clone(),
                    value: Rvalue::FormatString {
                        parts: vec![MirFormatPart::Value(capture.value)],
                    },
                });
                AssertionCapture {
                    label: capture.label.to_string(),
                    ty: capture.ty,
                    value: Operand::Place(rendered),
                }
            })
            .collect();
        let message = assert_stmt
            .message
            .as_ref()
            .map(|message| self.lower_expr(message));
        self.terminate(Terminator::AssertFail {
            message,
            captures,
            span: assert_stmt.span,
        });

        self.switch_to(continuation_block);
    }

    /// Lowers the deliberately narrow S3 assertion-introspection surface.
    /// Grouping around the complete condition is transparent; grouping or
    /// composition inside any other expression does not widen this boundary.
    fn lower_introspected_assertion_condition(
        &mut self,
        condition: &Expr,
    ) -> Option<(Operand, Vec<PendingAssertionCapture>)> {
        let mut condition = condition;
        while let ExprKind::Group(inner) = &condition.kind {
            condition = inner;
        }
        match &condition.kind {
            ExprKind::Binary { op, left, right }
                if matches!(
                    op,
                    BinaryOp::Eq
                        | BinaryOp::NotEq
                        | BinaryOp::Less
                        | BinaryOp::LessEq
                        | BinaryOp::Greater
                        | BinaryOp::GreaterEq
                ) && self.assertion_binary_dispatch_is_non_consuming(*op, left, right) =>
            {
                Some(self.lower_introspected_binary_condition(condition, *op, left, right))
            }
            ExprKind::Membership {
                value,
                container,
                negated: false,
                operator_span,
            } => {
                Some(self.lower_introspected_membership_condition(value, container, *operator_span))
            }
            _ => None,
        }
    }

    fn assertion_binary_dispatch_is_non_consuming(
        &self,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
    ) -> bool {
        let Some(left_ty) = self.infer_expr_type(left) else {
            return false;
        };
        let Some(right_ty) = self.infer_expr_type(right) else {
            return false;
        };
        let (left_ty, right_ty) = adjusted_binary_operand_types(left, left_ty, right, right_ty);
        if is_builtin_binary_operator(op, &left_ty, &right_ty) {
            return crate::sema::assertion_dispatch_is_non_consuming(None);
        }
        let Some((trait_name, method_name)) = binary_operator_trait(op) else {
            return false;
        };
        let Some(method) = self
            .trait_info_in_scope(trait_name)
            .and_then(|info| info.methods.get(method_name))
        else {
            return false;
        };
        let receiver = method.decl.receiver.unwrap_or(ReceiverKind::Value);
        let rhs = method
            .signature
            .param_passings
            .first()
            .copied()
            .unwrap_or(ReceiverKind::Value);
        crate::sema::assertion_dispatch_is_non_consuming(Some((receiver, rhs)))
    }

    fn lower_introspected_binary_condition(
        &mut self,
        condition: &Expr,
        op: BinaryOp,
        left: &Expr,
        right: &Expr,
    ) -> (Operand, Vec<PendingAssertionCapture>) {
        let inferred_left_ty = self
            .infer_expr_type(left)
            .unwrap_or_else(|| Type::named("Unknown"));
        let inferred_right_ty = self
            .infer_expr_type(right)
            .unwrap_or_else(|| Type::named("Unknown"));
        let shared_equality_expected = matches!(op, BinaryOp::Eq | BinaryOp::NotEq)
            .then(|| self.infer_equality_hint(left, right))
            .flatten();
        let left_expected = shared_equality_expected.as_ref().or_else(|| {
            if is_integer_literal_expr(left)
                && (is_float_type(&inferred_right_ty)
                    || crate::sema::integer_type_bounds(&inferred_right_ty).is_some())
            {
                Some(&inferred_right_ty)
            } else {
                None
            }
        });
        let right_expected = shared_equality_expected.as_ref().or_else(|| {
            if is_integer_literal_expr(right)
                && (is_float_type(&inferred_left_ty)
                    || crate::sema::integer_type_bounds(&inferred_left_ty).is_some())
            {
                Some(&inferred_left_ty)
            } else {
                None
            }
        });

        let left_value = self.lower_expr_at_sequence_point(left, left_expected);
        let right_value = self.lower_expr_at_sequence_point(right, right_expected);
        let left_ty = left_expected
            .cloned()
            .unwrap_or_else(|| inferred_left_ty.clone());
        let right_ty = right_expected
            .cloned()
            .unwrap_or_else(|| inferred_right_ty.clone());

        let result = self.new_typed_temp(Type::named("bool"));
        if let Some(field) = self.operator_field_for_binary(op, left, right) {
            self.emit(Instruction::Assign {
                target: result.clone(),
                value: Rvalue::Call {
                    callee: CallTarget::Member {
                        object: left_value.clone(),
                        field,
                        receiver_place: None,
                    },
                    args: vec![MirArg {
                        name: None,
                        value: right_value.clone(),
                        writeback_place: None,
                    }],
                },
            });
        } else {
            self.emit(Instruction::Assign {
                target: result.clone(),
                value: Rvalue::Binary {
                    op,
                    left: left_value.clone(),
                    right: right_value.clone(),
                    span: condition.span,
                },
            });
        }
        (
            Operand::Place(result),
            vec![
                PendingAssertionCapture {
                    label: "left",
                    ty: left_ty,
                    value: left_value,
                },
                PendingAssertionCapture {
                    label: "right",
                    ty: right_ty,
                    value: right_value,
                },
            ],
        )
    }

    fn lower_introspected_membership_condition(
        &mut self,
        value: &Expr,
        container: &Expr,
        operator_span: Span,
    ) -> (Operand, Vec<PendingAssertionCapture>) {
        let container_ty = self
            .infer_expr_type(container)
            .unwrap_or_else(|| Type::named("Unknown"));
        let item_ty = crate::sema::membership_needle_type(&container_ty)
            .or_else(|| self.infer_expr_type(value))
            .unwrap_or_else(|| Type::named("Unknown"));
        let item = self.lower_expr_at_sequence_point(value, Some(&item_ty));
        let collection = self.lower_expr_at_sequence_point(container, None);
        let condition = self.lower_membership_call(
            item.clone(),
            collection.clone(),
            None,
            &container_ty,
            false,
            operator_span,
        );
        (
            condition,
            vec![
                PendingAssertionCapture {
                    label: "item",
                    ty: item_ty,
                    value: item,
                },
                PendingAssertionCapture {
                    label: "collection",
                    ty: container_ty,
                    value: collection,
                },
            ],
        )
    }

    fn lower_assign(&mut self, assign: &AssignStmt) {
        let named_target_type = match (&assign.target, &assign.annotation) {
            (AssignTarget::Name(name), Some(annotation)) => {
                let annotation = self.lower_type_ref_with_provenance(annotation);
                let storage_type = self
                    .infer_expr_type(&assign.value)
                    .filter(|ty| matches!(ty, Type::Closure { .. }))
                    .unwrap_or(annotation);
                Some((name, storage_type))
            }
            (AssignTarget::Name(name), None) => {
                self.infer_expr_type(&assign.value).map(|ty| (name, ty))
            }
            _ => None,
        };
        if let Some((name, ty)) = named_target_type {
            // A mutable binding declared inside a scoped construct is a new
            // source binding, even when a sibling scope uses the same source
            // spelling. Give it its own MIR place so direct lowering cannot
            // collapse heterogeneous arm locals into one typed slot.
            if assign.mutable
                && self
                    .scoped_names
                    .last()
                    .is_some_and(|scope| !scope.contains_key(name))
            {
                let slot = self.new_typed_temp(ty.clone());
                self.scoped_names
                    .last_mut()
                    .expect("checked scoped local declaration")
                    .insert(name.clone(), slot);
            }
            let target = self.render_local_name(name);
            self.local_types.entry(target).or_insert(ty);
        }

        if let AssignTarget::Index { object, index } = &assign.target {
            let lowered_object = self.lower_expr(object);
            let object_ty = self.infer_expr_type(object);
            let (index_field, set_index_field, index_ty, target_ty) = match object_ty.as_ref() {
                Some(Type::Named(name, args)) if name == "dict" && args.len() == 2 => (
                    INTERNAL_MAP_INDEX_FIELD.to_string(),
                    INTERNAL_MAP_SET_INDEX_FIELD.to_string(),
                    Some(args[0].clone()),
                    Some(args[1].clone()),
                ),
                Some(Type::Named(name, args)) if name == "list" && args.len() == 1 => (
                    INTERNAL_VEC_INDEX_FIELD.to_string(),
                    INTERNAL_VEC_SET_INDEX_FIELD.to_string(),
                    Some(Type::named("int64")),
                    Some(args[0].clone()),
                ),
                Some(Type::Named(name, args)) if name == "Array" && args.len() == 1 => (
                    INTERNAL_VEC_INDEX_FIELD.to_string(),
                    INTERNAL_VEC_SET_INDEX_FIELD.to_string(),
                    Some(array_coordinate_type(index)),
                    Some(args[0].clone()),
                ),
                _ => (
                    INTERNAL_VEC_INDEX_FIELD.to_string(),
                    INTERNAL_VEC_SET_INDEX_FIELD.to_string(),
                    None,
                    None,
                ),
            };
            let lowered_index = match object_ty.as_ref() {
                Some(Type::Named(name, args)) if name == "list" && args.len() == 1 => {
                    self.lower_index_domain_expr(index)
                }
                Some(Type::Named(name, args)) if name == "Array" && args.len() == 1 => {
                    self.lower_array_coordinate_expr(index)
                }
                _ => self.lower_expr_for_owned_value(index, index_ty.as_ref()),
            };
            let (read_index, set_index) = if assign.op.is_some() {
                let captured = match index_ty.clone() {
                    Some(ty) => self.new_typed_temp(ty),
                    None => self.new_temp(),
                };
                self.emit(Instruction::Assign {
                    target: captured.clone(),
                    value: Rvalue::Use(lowered_index),
                });
                let read = Operand::Place(captured.clone());
                let set = match index_ty.as_ref() {
                    Some(ty) => self.move_place_for_type(captured.clone(), ty),
                    None => Operand::Place(captured),
                };
                (read, set)
            } else {
                (lowered_index.clone(), lowered_index)
            };
            let value = if let Some(op) = assign.op {
                let current = self.new_temp_for_expr(&Expr {
                    kind: ExprKind::Index {
                        object: object.clone(),
                        index: index.clone(),
                    },
                    span: assign.span,
                });
                self.emit(Instruction::Assign {
                    target: current.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: lowered_object.clone(),
                            field: index_field,
                            receiver_place: self.render_place_expr_option(object),
                        },
                        args: vec![
                            MirArg {
                                name: None,
                                value: read_index,
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(assign.span.line as u128),
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(assign.span.column as u128),
                                writeback_place: None,
                            },
                        ],
                    },
                });
                let indexed_value_type = self.local_types[&current].clone();
                let compound = Expr {
                    kind: ExprKind::Binary {
                        op,
                        left: Box::new(Expr {
                            kind: ExprKind::Name(current),
                            span: assign.span,
                        }),
                        right: Box::new(assign.value.clone()),
                    },
                    span: assign.span,
                };
                self.lower_expr_with_expected(&compound, Some(&indexed_value_type))
            } else {
                self.lower_expr_for_owned_value(&assign.value, target_ty.as_ref())
            };
            let temp = self.new_typed_temp(Type::Unit);
            self.emit(Instruction::Assign {
                target: temp,
                value: Rvalue::Call {
                    callee: CallTarget::Member {
                        object: lowered_object,
                        field: set_index_field,
                        receiver_place: self.render_place_expr_option(object),
                    },
                    args: vec![
                        MirArg {
                            name: None,
                            value: set_index,
                            writeback_place: None,
                        },
                        MirArg {
                            name: None,
                            value,
                            writeback_place: None,
                        },
                        MirArg {
                            name: None,
                            value: Operand::Int(assign.span.line as u128),
                            writeback_place: None,
                        },
                        MirArg {
                            name: None,
                            value: Operand::Int(assign.span.column as u128),
                            writeback_place: None,
                        },
                    ],
                },
            });
            return;
        }

        let target = self.render_assign_target(&assign.target);
        let target_ty = match &assign.target {
            AssignTarget::Name(name) => {
                self.local_types.get(&self.render_local_name(name)).cloned()
            }
            AssignTarget::Member { object, field } => self.infer_expr_type(&Expr {
                kind: ExprKind::Member {
                    object: object.clone(),
                    field: field.clone(),
                },
                span: assign.span,
            }),
            AssignTarget::Index { .. } => None,
        };
        if let Some(op) = assign.op {
            let left = match &assign.target {
                AssignTarget::Name(name) => Expr {
                    kind: ExprKind::Name(name.clone()),
                    span: assign.span,
                },
                AssignTarget::Member { object, field } => Expr {
                    kind: ExprKind::Member {
                        object: object.clone(),
                        field: field.clone(),
                    },
                    span: assign.span,
                },
                AssignTarget::Index { .. } => unreachable!("handled above"),
            };
            let compound = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(assign.value.clone()),
                },
                span: assign.span,
            };
            let value = self.lower_expr_for_owned_value(&compound, target_ty.as_ref());
            self.emit(Instruction::Assign {
                target,
                value: Rvalue::Use(value),
            });
            return;
        }

        if let Some(target_ty) = &target_ty {
            if let Some(value) = self.lower_collection_literal_with_type(&assign.value, target_ty) {
                self.emit(Instruction::Assign { target, value });
                return;
            }
        }

        let value = self.lower_expr_for_owned_value(&assign.value, target_ty.as_ref());
        if let (Some(target_ty), Operand::Place(place) | Operand::MovePlace(place)) =
            (target_ty, &value)
        {
            self.local_types.insert(place.clone(), target_ty);
        }
        self.emit(Instruction::Assign {
            target,
            value: Rvalue::Use(value),
        });
    }

    fn lower_destructure(&mut self, destructure: &DestructureStmt) {
        let tuple_ty = self
            .infer_expr_type(&destructure.value)
            .unwrap_or_else(|| Type::named("Unknown"));
        let source = self.lower_expr_for_owned_value(&destructure.value, Some(&tuple_ty));
        let captured = self.new_typed_temp(tuple_ty.clone());
        self.emit(Instruction::Assign {
            target: captured.clone(),
            value: Rvalue::Use(source),
        });
        self.lower_binding_target_from_place(&destructure.target, &captured, &tuple_ty, true);
    }

    fn lower_binding_target_from_place(
        &mut self,
        target: &BindingTarget,
        source_place: &str,
        source_ty: &Type,
        consume_non_copy: bool,
    ) {
        self.lower_binding_target_from_place_with_scope(
            target,
            source_place,
            source_ty,
            consume_non_copy,
            false,
        );
    }

    fn lower_scoped_binding_target_from_place(
        &mut self,
        target: &BindingTarget,
        source_place: &str,
        source_ty: &Type,
        consume_non_copy: bool,
    ) {
        self.lower_binding_target_from_place_with_scope(
            target,
            source_place,
            source_ty,
            consume_non_copy,
            true,
        );
    }

    fn lower_binding_target_from_place_with_scope(
        &mut self,
        target: &BindingTarget,
        source_place: &str,
        source_ty: &Type,
        consume_non_copy: bool,
        use_scoped_targets: bool,
    ) {
        match target {
            BindingTarget::Name { name, .. } => {
                let target = if use_scoped_targets {
                    self.scoped_local_name(name)
                        .expect("scoped binding target should have a registered slot")
                        .to_string()
                } else {
                    self.local_types.insert(name.clone(), source_ty.clone());
                    name.clone()
                };
                let source =
                    if consume_non_copy && !type_is_copy_in_program(source_ty, self.program) {
                        Operand::MovePlace(source_place.to_string())
                    } else {
                        Operand::Place(source_place.to_string())
                    };
                self.emit(Instruction::Assign {
                    target,
                    value: Rvalue::Use(source),
                });
            }
            BindingTarget::Tuple { elements, .. } => {
                if let Type::Tuple(element_types) = source_ty {
                    for (index, (element, element_ty)) in
                        elements.iter().zip(element_types).enumerate()
                    {
                        let captured = self.new_typed_temp(element_ty.clone());
                        let value = if consume_non_copy
                            && !type_is_copy_in_program(element_ty, self.program)
                        {
                            Rvalue::TupleTakeElement {
                                place: source_place.to_string(),
                                index,
                                element_type: element_ty.clone(),
                            }
                        } else {
                            Rvalue::TupleElement {
                                tuple: Operand::Place(source_place.to_string()),
                                index,
                                element_type: element_ty.clone(),
                            }
                        };
                        self.emit(Instruction::Assign {
                            target: captured.clone(),
                            value,
                        });
                        self.lower_binding_target_from_place_with_scope(
                            element,
                            &captured,
                            element_ty,
                            consume_non_copy,
                            use_scoped_targets,
                        );
                    }
                }
            }
        }
    }

    fn fresh_scoped_binding_target_slots(
        &mut self,
        target: &BindingTarget,
        target_ty: &Type,
    ) -> std::collections::HashMap<String, String> {
        let mut slots = std::collections::HashMap::new();
        self.register_scoped_binding_target(target, target_ty, &mut slots);
        slots
    }

    fn register_scoped_binding_target(
        &mut self,
        target: &BindingTarget,
        target_ty: &Type,
        slots: &mut std::collections::HashMap<String, String>,
    ) {
        match target {
            BindingTarget::Name { name, .. } => {
                let slot = self.new_typed_temp(target_ty.clone());
                slots.insert(name.clone(), slot);
            }
            BindingTarget::Tuple { elements, .. } => {
                if let Type::Tuple(element_types) = target_ty {
                    for (element, element_ty) in elements.iter().zip(element_types) {
                        self.register_scoped_binding_target(element, element_ty, slots);
                    }
                }
            }
        }
    }

    fn lower_collection_literal_with_type(&mut self, expr: &Expr, ty: &Type) -> Option<Rvalue> {
        match (&expr.kind, ty) {
            (ExprKind::List(elements), Type::Named(name, args))
                if name == "list" && args.len() == 1 =>
            {
                Some(Rvalue::VecLiteral {
                    elements: elements
                        .iter()
                        .map(|element| self.lower_expr_for_owned_value(element, Some(&args[0])))
                        .collect(),
                    element_type: args[0].clone(),
                })
            }
            (ExprKind::Set(elements), Type::Named(name, args))
                if name == "set" && args.len() == 1 =>
            {
                Some(Rvalue::SetLiteral {
                    elements: elements
                        .iter()
                        .map(|element| self.lower_expr_for_owned_value(element, Some(&args[0])))
                        .collect(),
                    element_type: args[0].clone(),
                })
            }
            (ExprKind::Map(entries), Type::Named(name, args))
                if entries.is_empty() && name == "set" && args.len() == 1 =>
            {
                Some(Rvalue::SetLiteral {
                    elements: Vec::new(),
                    element_type: args[0].clone(),
                })
            }
            (ExprKind::Map(entries), Type::Named(name, args))
                if name == "dict" && args.len() == 2 =>
            {
                Some(Rvalue::MapLiteral {
                    entries: entries
                        .iter()
                        .map(|entry| MirMapEntry {
                            key: self.lower_expr_for_owned_value(&entry.key, Some(&args[0])),
                            value: self.lower_expr_for_owned_value(&entry.value, Some(&args[1])),
                        })
                        .collect(),
                    key_type: args[0].clone(),
                    value_type: args[1].clone(),
                })
            }
            _ => None,
        }
    }

    fn render_assign_target(&self, target: &AssignTarget) -> String {
        match target {
            AssignTarget::Name(name) => self.render_local_name(name),
            AssignTarget::Member { object, field } => {
                format!("{}.{}", self.render_expr_place(object), field)
            }
            AssignTarget::Index { .. } => {
                panic!("indexed assignments must lower through runtime helper calls")
            }
        }
    }

    fn render_expr_place(&self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Name(name) => self.render_local_name(name),
            ExprKind::Group(inner) => self.render_expr_place(inner),
            ExprKind::Member { object, field } => {
                format!("{}.{}", self.render_expr_place(object), field)
            }
            _ => "<expr>".to_string(),
        }
    }

    fn returned_view_source(&self, expr: &Expr) -> Option<(String, Vec<String>)> {
        let ExprKind::Call { callee, args } = &expr.kind else {
            return None;
        };
        let callee = match &callee.kind {
            ExprKind::Specialize { expr, .. } => expr.as_ref(),
            _ => callee.as_ref(),
        };
        let (decl, receiver) = match &callee.kind {
            ExprKind::Name(name) => (&self.resolve_function_info(name)?.decl, None),
            ExprKind::Member { object, field } => {
                let Type::Named(class_name, _) = self.infer_expr_type(object)? else {
                    return None;
                };
                let method = self.resolve_class_info(&class_name)?.methods.get(field)?;
                (&method.decl, Some(&**object))
            }
            _ => return None,
        };
        let contract = decl.view_return.as_ref()?;
        let origin = if contract.origin == "self" {
            self.render_place_expr_option(receiver?)?
        } else {
            let origin_index = decl
                .params
                .iter()
                .position(|param| param.name == contract.origin)?;
            let ordered = bind_call_arguments(
                &format!("callable `{}`", decl.name),
                &callable_params_from_decl(&decl.params),
                args,
                callee.span,
                CallConvention::PositionalOrNamed,
            )
            .ok()?;
            let origin = ordered.get(origin_index).copied().flatten()?;
            self.render_place_expr_option(&origin.value)?
        };
        let projections = return_view_projections(decl);
        (!projections.is_empty()).then_some((origin, projections))
    }

    fn lowered_writeback_place(&self, expr: &Expr, value: &Operand) -> Option<String> {
        self.render_place_expr_option(expr).or_else(|| {
            self.returned_view_source(expr).and_then(|_| match value {
                Operand::Place(place) | Operand::MovePlace(place) => Some(place.clone()),
                _ => None,
            })
        })
    }

    fn scoped_local_name(&self, name: &str) -> Option<&str> {
        self.scoped_names
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(String::as_str))
    }

    fn render_local_name(&self, name: &str) -> String {
        self.scoped_local_name(name).unwrap_or(name).to_string()
    }

    fn lower_if(&mut self, if_stmt: &IfStmt) {
        let after_block = self.new_block("if_end");
        let mut next_condition_block = self.current_block;
        let mut else_block_to_lower = None;

        for (index, branch) in if_stmt.branches.iter().enumerate() {
            self.switch_to(next_condition_block);
            let condition = self.lower_expr(&branch.condition);
            let then_block = self.new_block("if_then");
            let is_last = index + 1 == if_stmt.branches.len();
            let else_block = if is_last {
                if if_stmt.else_body.is_some() {
                    let block = self.new_block("if_else");
                    else_block_to_lower = Some(block);
                    block
                } else {
                    after_block
                }
            } else {
                self.new_block("if_next")
            };

            self.terminate(Terminator::Branch {
                condition,
                then_label: self.label(then_block),
                else_label: self.label(else_block),
            });

            self.switch_to(then_block);
            self.lower_stmts(&branch.body);
            if !self.current_terminated() {
                self.terminate(Terminator::Goto(self.label(after_block)));
            }

            next_condition_block = else_block;
        }

        if let (Some(else_body), Some(else_block)) = (&if_stmt.else_body, else_block_to_lower) {
            self.switch_to(else_block);
            self.lower_stmts(else_body);
            if !self.current_terminated() {
                self.terminate(Terminator::Goto(self.label(after_block)));
            }
        }

        self.switch_to(after_block);
    }

    fn lower_match(&mut self, match_stmt: &MatchStmt) {
        let scrutinee_ty = self.infer_expr_type(&match_stmt.scrutinee);
        let consumes_scrutinee = match_stmt.capability == ReceiverKind::Value
            && scrutinee_ty
                .as_ref()
                .is_some_and(|ty| !type_is_copy_in_program(ty, self.program));
        let scrutinee = if consumes_scrutinee {
            let source =
                self.lower_expr_for_owned_value(&match_stmt.scrutinee, scrutinee_ty.as_ref());
            let captured = match scrutinee_ty.clone() {
                Some(ty) => self.new_typed_temp(ty),
                None => self.new_temp(),
            };
            self.emit(Instruction::Assign {
                target: captured.clone(),
                value: Rvalue::Use(source),
            });
            Operand::Place(captured)
        } else {
            self.lower_expr(&match_stmt.scrutinee)
        };
        let writeback_root = if match_stmt.capability == ReceiverKind::BorrowMut {
            self.render_place_expr_option(&match_stmt.scrutinee)
        } else {
            None
        };
        let after_block = self.new_block("match_end");
        let mut next_case_block = self.current_block;

        for (index, arm) in match_stmt.arms.iter().enumerate() {
            self.switch_to(next_case_block);
            let arm_block = self.new_block("match_arm");
            let next_block = if index + 1 == match_stmt.arms.len() {
                after_block
            } else {
                self.new_block("match_next")
            };
            self.scoped_names.push(std::collections::HashMap::new());
            let probes_candidates = arm.guard.is_some() || matches!(arm.pattern, Pattern::Or(_));
            let pattern_writeback = self.lower_pattern(
                &arm.pattern,
                scrutinee.clone(),
                scrutinee_ty.as_ref(),
                arm_block,
                next_block,
                PatternLoweringOptions {
                    collect_writeback: writeback_root.is_some(),
                    consume_payloads: consumes_scrutinee && !probes_candidates,
                },
            );
            self.switch_to(arm_block);
            if let Some(writeback_place) = writeback_root.as_ref() {
                let skip_place = self.new_typed_temp(Type::named("bool"));
                self.match_writeback_stack.push(MatchWritebackState {
                    root: writeback_place.clone(),
                    skip_place: skip_place.clone(),
                    writeback: pattern_writeback.clone(),
                });
                self.emit(Instruction::Assign {
                    target: skip_place,
                    value: Rvalue::Use(Operand::Bool(false)),
                });
            }
            if let Some(guard) = &arm.guard {
                let selected = self.new_block("match_guard_true");
                let rejected = self.new_block("match_guard_false");
                let condition = self.lower_expr(guard);
                self.terminate(Terminator::Branch {
                    condition,
                    then_label: self.label(selected),
                    else_label: self.label(rejected),
                });
                self.switch_to(rejected);
                if let (Some(writeback_place), Some(writeback)) =
                    (writeback_root.as_ref(), pattern_writeback.as_ref())
                {
                    let updated = self.materialize_pattern_writeback(writeback);
                    self.emit(Instruction::Assign {
                        target: writeback_place.clone(),
                        value: Rvalue::Use(updated),
                    });
                }
                self.terminate(Terminator::Goto(self.label(next_block)));
                self.switch_to(selected);
            }
            if consumes_scrutinee {
                self.lower_consuming_pattern_bindings(
                    &arm.pattern,
                    scrutinee.clone(),
                    scrutinee_ty.as_ref(),
                );
            }
            self.lower_stmts(&arm.body);
            let writeback_state = writeback_root
                .as_ref()
                .and_then(|_| self.match_writeback_stack.pop());
            if !self.current_terminated() {
                if let (Some(writeback_place), Some(writeback), Some(state)) = (
                    writeback_root.as_ref(),
                    pattern_writeback.as_ref(),
                    writeback_state.as_ref(),
                ) {
                    self.finish_match_arm_with_writeback(
                        after_block,
                        writeback_place,
                        writeback,
                        &state.skip_place,
                    );
                } else {
                    self.terminate(Terminator::Goto(self.label(after_block)));
                }
            }
            self.scoped_names.pop();
            next_case_block = next_block;
        }

        self.switch_to(after_block);
    }

    fn lower_pattern(
        &mut self,
        pattern: &Pattern,
        scrutinee: Operand,
        scrutinee_ty: Option<&Type>,
        success_block: usize,
        failure_block: usize,
        options: PatternLoweringOptions,
    ) -> Option<PatternWriteback> {
        match pattern {
            Pattern::Or(pattern) => {
                let mut next = self.current_block;
                let selected = (0..pattern.alternatives.len())
                    .map(|_| self.new_typed_temp(Type::named("bool")))
                    .collect::<Vec<_>>();
                for flag in &selected {
                    self.emit(Instruction::Assign {
                        target: flag.clone(),
                        value: Rvalue::Use(Operand::Bool(false)),
                    });
                }
                let mut alternatives = Vec::new();
                for (index, alternative) in pattern.alternatives.iter().enumerate() {
                    self.switch_to(next);
                    let failure = if index + 1 == pattern.alternatives.len() {
                        failure_block
                    } else {
                        self.new_block("match_or_next")
                    };
                    let alternative_success = self.new_block("match_or_selected");
                    let candidate = self.lower_pattern(
                        alternative,
                        scrutinee.clone(),
                        scrutinee_ty,
                        alternative_success,
                        failure,
                        options,
                    );
                    alternatives.push(
                        candidate.unwrap_or_else(|| PatternWriteback::Use(scrutinee.clone())),
                    );
                    self.switch_to(alternative_success);
                    self.emit(Instruction::Assign {
                        target: selected[index].clone(),
                        value: Rvalue::Use(Operand::Bool(true)),
                    });
                    self.terminate(Terminator::Goto(self.label(success_block)));
                    next = failure;
                }
                options.collect_writeback.then(|| PatternWriteback::Or {
                    ty: scrutinee_ty
                        .cloned()
                        .unwrap_or_else(|| Type::named("Unknown")),
                    selected,
                    alternatives,
                })
            }
            Pattern::Wildcard(_) => {
                self.terminate(Terminator::Goto(self.label(success_block)));
                options
                    .collect_writeback
                    .then_some(PatternWriteback::Use(scrutinee))
            }
            Pattern::Binding(binding) => {
                let target = self
                    .scoped_names
                    .last()
                    .and_then(|scope| scope.get(&binding.name).cloned())
                    .unwrap_or_else(|| {
                        let target = if let Some(ty) = scrutinee_ty.cloned() {
                            self.new_typed_temp(ty)
                        } else {
                            self.new_temp()
                        };
                        self.scoped_names
                            .last_mut()
                            .expect("match arm scope should exist")
                            .insert(binding.name.clone(), target.clone());
                        target
                    });
                if !options.consume_payloads {
                    self.emit(Instruction::Assign {
                        target: target.clone(),
                        value: Rvalue::Use(scrutinee),
                    });
                }
                self.terminate(Terminator::Goto(self.label(success_block)));
                options
                    .collect_writeback
                    .then_some(PatternWriteback::Use(Operand::Place(target)))
            }
            Pattern::Literal(pattern) => {
                let condition = self.lower_literal_pattern_condition(
                    scrutinee.clone(),
                    scrutinee_ty,
                    &pattern.kind,
                    pattern.span,
                );
                self.terminate(Terminator::Branch {
                    condition,
                    then_label: self.label(success_block),
                    else_label: self.label(failure_block),
                });
                options
                    .collect_writeback
                    .then_some(PatternWriteback::Use(scrutinee))
            }
            Pattern::Tuple(pattern) => {
                let Some(Type::Tuple(element_types)) = scrutinee_ty else {
                    unreachable!("checked tuple patterns always have tuple scrutinees");
                };
                debug_assert_eq!(element_types.len(), pattern.elements.len());
                debug_assert!(!pattern.elements.is_empty());

                let mut next_element_block = self.current_block;
                for (index, (element_pattern, element_ty)) in
                    pattern.elements.iter().zip(element_types).enumerate()
                {
                    self.switch_to(next_element_block);
                    let element_success = if index + 1 == pattern.elements.len() {
                        success_block
                    } else {
                        self.new_block("match_tuple_element")
                    };
                    if options.consume_payloads && !pattern_requires_runtime_test(element_pattern) {
                        self.register_consuming_pattern_bindings(element_pattern, element_ty);
                        self.terminate(Terminator::Goto(self.label(element_success)));
                    } else {
                        let element = self.new_typed_temp(element_ty.clone());
                        self.emit(Instruction::Assign {
                            target: element.clone(),
                            value: Rvalue::TupleElement {
                                tuple: scrutinee.clone(),
                                index,
                                element_type: element_ty.clone(),
                            },
                        });
                        self.lower_pattern(
                            element_pattern,
                            Operand::Place(element),
                            Some(element_ty),
                            element_success,
                            failure_block,
                            PatternLoweringOptions {
                                collect_writeback: false,
                                consume_payloads: options.consume_payloads,
                            },
                        );
                    }
                    next_element_block = element_success;
                }
                None
            }
            Pattern::Variant(pattern) => {
                let resolved_enum_name = self.resolve_pattern_enum_name(pattern, scrutinee_ty);
                let matched_block = self.new_block("match_variant");
                self.terminate(Terminator::Match {
                    scrutinee: scrutinee.clone(),
                    arms: vec![MirMatchArm {
                        enum_name: Some(resolved_enum_name.clone()),
                        variant_name: Some(pattern.variant_name.clone()),
                        wildcard: false,
                        label: self.label(matched_block),
                    }],
                    otherwise: self.label(failure_block),
                });
                self.switch_to(matched_block);
                let payload_types = match self.variant_payload_types(
                    scrutinee_ty,
                    &resolved_enum_name,
                    &pattern.variant_name,
                ) {
                    Some(payload_types) => payload_types,
                    None => vec![Type::named("Unknown"); pattern.subpatterns.len()],
                };
                if payload_types.len() != pattern.subpatterns.len() {
                    self.terminate(Terminator::Goto(self.label(failure_block)));
                    return None;
                }
                if pattern.subpatterns.is_empty() {
                    self.terminate(Terminator::Goto(self.label(success_block)));
                    return options
                        .collect_writeback
                        .then(|| PatternWriteback::Variant {
                            ty: scrutinee_ty
                                .cloned()
                                .unwrap_or_else(|| Type::named("Unknown")),
                            enum_name: resolved_enum_name,
                            variant_name: pattern.variant_name.clone(),
                            payloads: Vec::new(),
                        });
                }
                let mut next_block = matched_block;
                let mut payload_writebacks = Vec::new();
                for (index, subpattern) in pattern.subpatterns.iter().enumerate() {
                    self.switch_to(next_block);
                    let payload_ty = payload_types[index].clone();
                    let payload_target = self.new_typed_temp(payload_ty.clone());
                    self.emit(Instruction::Assign {
                        target: payload_target.clone(),
                        value: Rvalue::VariantPayload {
                            scrutinee: scrutinee.clone(),
                            variant_name: pattern.variant_name.clone(),
                            index,
                        },
                    });
                    let subpattern_success = if index + 1 == pattern.subpatterns.len() {
                        success_block
                    } else {
                        self.new_block("match_payload")
                    };
                    if let Some(writeback) = self.lower_pattern(
                        subpattern,
                        Operand::Place(payload_target),
                        Some(&payload_ty),
                        subpattern_success,
                        failure_block,
                        options,
                    ) {
                        payload_writebacks.push(writeback);
                    }
                    next_block = subpattern_success;
                }
                options
                    .collect_writeback
                    .then(|| PatternWriteback::Variant {
                        ty: scrutinee_ty
                            .cloned()
                            .unwrap_or_else(|| Type::named("Unknown")),
                        enum_name: resolved_enum_name,
                        variant_name: pattern.variant_name.clone(),
                        payloads: payload_writebacks,
                    })
            }
        }
    }

    fn register_consuming_pattern_bindings(&mut self, pattern: &Pattern, pattern_ty: &Type) {
        match pattern {
            Pattern::Or(pattern) => {
                if let Some(first) = pattern.alternatives.first() {
                    self.register_consuming_pattern_bindings(first, pattern_ty);
                }
            }
            Pattern::Binding(binding) => {
                if !self
                    .scoped_names
                    .last()
                    .is_some_and(|scope| scope.contains_key(&binding.name))
                {
                    let target = self.new_typed_temp(pattern_ty.clone());
                    self.scoped_names
                        .last_mut()
                        .expect("match arm scope should exist")
                        .insert(binding.name.clone(), target);
                }
            }
            Pattern::Tuple(pattern) => {
                let Type::Tuple(element_types) = pattern_ty else {
                    return;
                };
                for (element_pattern, element_ty) in pattern.elements.iter().zip(element_types) {
                    self.register_consuming_pattern_bindings(element_pattern, element_ty);
                }
            }
            Pattern::Variant(_) | Pattern::Wildcard(_) | Pattern::Literal(_) => {}
        }
    }

    fn lower_consuming_pattern_bindings(
        &mut self,
        pattern: &Pattern,
        scrutinee: Operand,
        scrutinee_ty: Option<&Type>,
    ) {
        match pattern {
            Pattern::Or(pattern) => {
                // Selection populated non-consuming candidate slots so a
                // guard could inspect them. Re-probe the private owner after
                // the guard succeeds, then destructively extract exactly the
                // selected alternative into those same shared binding slots.
                let done = self.new_block("match_or_commit_done");
                let mut next = self.current_block;
                for (index, alternative) in pattern.alternatives.iter().enumerate() {
                    self.switch_to(next);
                    let selected = self.new_block("match_or_commit_selected");
                    let rejected = if index + 1 == pattern.alternatives.len() {
                        done
                    } else {
                        self.new_block("match_or_commit_next")
                    };
                    self.lower_pattern(
                        alternative,
                        scrutinee.clone(),
                        scrutinee_ty,
                        selected,
                        rejected,
                        PatternLoweringOptions {
                            collect_writeback: false,
                            consume_payloads: true,
                        },
                    );
                    self.switch_to(selected);
                    self.lower_consuming_pattern_bindings(
                        alternative,
                        scrutinee.clone(),
                        scrutinee_ty,
                    );
                    if !self.current_terminated() {
                        self.terminate(Terminator::Goto(self.label(done)));
                    }
                    next = rejected;
                }
                self.switch_to(done);
            }
            Pattern::Wildcard(_) | Pattern::Literal(_) => {}
            Pattern::Binding(binding) => {
                let target = self.render_local_name(&binding.name);
                let value = match (scrutinee, scrutinee_ty) {
                    (Operand::Place(place), Some(ty))
                        if !type_is_copy_in_program(ty, self.program) =>
                    {
                        Operand::MovePlace(place)
                    }
                    (value, _) => value,
                };
                self.emit(Instruction::Assign {
                    target,
                    value: Rvalue::Use(value),
                });
            }
            Pattern::Variant(pattern) => {
                let resolved_enum_name = self.resolve_pattern_enum_name(pattern, scrutinee_ty);
                let payload_types = self
                    .variant_payload_types(scrutinee_ty, &resolved_enum_name, &pattern.variant_name)
                    .unwrap_or_else(|| vec![Type::named("Unknown"); pattern.subpatterns.len()]);
                for (index, (subpattern, payload_ty)) in
                    pattern.subpatterns.iter().zip(payload_types).enumerate()
                {
                    if !pattern_contains_binding(subpattern) {
                        continue;
                    }
                    let payload_target = self.new_typed_temp(payload_ty.clone());
                    let payload_scrutinee = match &scrutinee {
                        Operand::Place(place)
                            if !type_is_copy_in_program(&payload_ty, self.program) =>
                        {
                            Operand::MovePlace(place.clone())
                        }
                        other => other.clone(),
                    };
                    self.emit(Instruction::Assign {
                        target: payload_target.clone(),
                        value: Rvalue::VariantPayload {
                            scrutinee: payload_scrutinee,
                            variant_name: pattern.variant_name.clone(),
                            index,
                        },
                    });
                    self.lower_consuming_pattern_bindings(
                        subpattern,
                        Operand::Place(payload_target),
                        Some(&payload_ty),
                    );
                }
            }
            Pattern::Tuple(pattern) => {
                let Some(Type::Tuple(element_types)) = scrutinee_ty else {
                    return;
                };
                for (index, (element_pattern, element_ty)) in
                    pattern.elements.iter().zip(element_types).enumerate()
                {
                    if !pattern_contains_binding(element_pattern) {
                        continue;
                    }
                    let element_target = self.new_typed_temp(element_ty.clone());
                    let value = match &scrutinee {
                        Operand::Place(place)
                            if !type_is_copy_in_program(element_ty, self.program) =>
                        {
                            Rvalue::TupleTakeElement {
                                place: place.clone(),
                                index,
                                element_type: element_ty.clone(),
                            }
                        }
                        other => Rvalue::TupleElement {
                            tuple: other.clone(),
                            index,
                            element_type: element_ty.clone(),
                        },
                    };
                    self.emit(Instruction::Assign {
                        target: element_target.clone(),
                        value,
                    });
                    self.lower_consuming_pattern_bindings(
                        element_pattern,
                        Operand::Place(element_target),
                        Some(element_ty),
                    );
                }
            }
        }
    }

    fn lower_literal_pattern_condition(
        &mut self,
        scrutinee: Operand,
        scrutinee_ty: Option<&Type>,
        pattern: &LiteralPatternKind,
        span: Span,
    ) -> Operand {
        let right = self.lower_literal_pattern_operand(scrutinee_ty, pattern, span);
        let target = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: target.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Eq,
                left: scrutinee,
                right,
                span,
            },
        });
        Operand::Place(target)
    }

    fn lower_literal_pattern_operand(
        &mut self,
        scrutinee_ty: Option<&Type>,
        pattern: &LiteralPatternKind,
        span: Span,
    ) -> Operand {
        match pattern {
            LiteralPatternKind::Int(value) => match value.representation() {
                crate::integer::IntegerRepresentation::Unsigned(value) => Operand::Int(value),
                crate::integer::IntegerRepresentation::Signed(value) => {
                    if value >= 0 {
                        Operand::Int(value as u128)
                    } else {
                        let ty = scrutinee_ty.cloned().unwrap_or(Type::named("int64"));
                        let target = self.new_typed_temp(ty);
                        self.emit(Instruction::Assign {
                            target: target.clone(),
                            value: Rvalue::Unary {
                                op: UnaryOp::Neg,
                                value: Operand::Int(value.unsigned_abs()),
                                span,
                            },
                        });
                        Operand::Place(target)
                    }
                }
            },
            LiteralPatternKind::Float(value) => Operand::Float(*value),
            LiteralPatternKind::Bool(value) => Operand::Bool(*value),
            LiteralPatternKind::String(value) => Operand::String(value.clone()),
        }
    }

    /// Recognizes the checked `enumerate`/`zip` loop forms. The checker has
    /// already rejected every other shape, so this only has to classify.
    fn lockstep_loop_iterables<'e>(&self, iterable: &'e Expr) -> Option<(bool, Vec<&'e Expr>)> {
        let ExprKind::Call { callee, args } = &iterable.kind else {
            return None;
        };
        let ExprKind::Name(name) = &callee.kind else {
            return None;
        };
        let enumerated = match name.as_str() {
            "enumerate" => true,
            "zip" => false,
            _ => return None,
        };
        if self.program.functions.contains_key(name) {
            return None;
        }
        Some((
            enumerated,
            args.iter().map(|argument| &argument.value).collect(),
        ))
    }

    /// Lowers `for ... in enumerate(xs):` and `for ... in zip(xs, ys):`. Both
    /// read index-addressable collections in lockstep through the same
    /// position-indexed member the ordinary loop uses, and stop as soon as any
    /// one of them runs out.
    fn lower_lockstep_for_with_body(
        &mut self,
        for_stmt: &crate::ast::ForStmt,
        enumerated: bool,
        iterables: &[&Expr],
        checked_binding: Option<&ComprehensionClauseInfo>,
        lower_body: &mut dyn FnMut(&mut Self),
    ) {
        let dispatch_block = self.new_block("for_lockstep");
        let body_block = self.new_block("for_body");
        let safepoint_block = self.new_block("for_safepoint");
        let after_block = self.new_block("for_end");

        let checked_elements = checked_binding.and_then(|binding| match &binding.binding_type {
            Type::Tuple(elements) => Some(elements.as_slice()),
            _ => None,
        });
        if let Some(binding) = checked_binding {
            debug_assert!(
                !binding.receive_owned,
                "enumerate/zip comprehension targets are shared or copy values"
            );
        }
        let sources = iterables
            .iter()
            .enumerate()
            .map(|(index, iterable)| {
                let checked_index = index + usize::from(enumerated);
                let element_ty = checked_elements
                    .and_then(|elements| elements.get(checked_index))
                    .cloned()
                    .or_else(|| {
                        self.infer_expr_type(iterable)
                            .as_ref()
                            .and_then(crate::sema::lockstep_element_type)
                    })
                    .unwrap_or_else(|| Type::named("Unknown"));
                let object = self.lower_expr_at_sequence_point(iterable, None);
                let receiver_place = self.render_place_expr_option(iterable);
                (object, receiver_place, element_ty)
            })
            .collect::<Vec<_>>();

        let index = self.new_typed_temp(Type::named("int64"));
        self.emit(Instruction::Assign {
            target: index.clone(),
            value: Rvalue::Use(Operand::Int(0)),
        });
        self.terminate(Terminator::Goto(self.label(dispatch_block)));
        self.switch_to(dispatch_block);

        let mut element_operands = Vec::with_capacity(sources.len() + 1);
        let mut element_types = Vec::with_capacity(sources.len() + 1);
        if enumerated {
            let position = self.new_typed_temp(Type::named("int64"));
            self.emit(Instruction::Assign {
                target: position.clone(),
                value: Rvalue::Use(Operand::Place(index.clone())),
            });
            element_operands.push(Operand::Place(position));
            element_types.push(Type::named("int64"));
        }

        for (position, (object, receiver_place, element_ty)) in sources.iter().enumerate() {
            let next_value =
                self.new_typed_temp(Type::Named("Option".to_string(), vec![element_ty.clone()]));
            self.emit(Instruction::Assign {
                target: next_value.clone(),
                value: Rvalue::Call {
                    callee: CallTarget::Member {
                        object: object.clone(),
                        field: INTERNAL_VEC_INDEX_OPTION_FIELD.to_string(),
                        receiver_place: receiver_place.clone(),
                    },
                    args: vec![MirArg {
                        name: None,
                        value: Operand::Place(index.clone()),
                        writeback_place: None,
                    }],
                },
            });
            let present_block = if position + 1 == sources.len() {
                body_block
            } else {
                self.new_block("for_lockstep_next")
            };
            self.terminate(Terminator::Match {
                scrutinee: Operand::Place(next_value.clone()),
                arms: vec![
                    MirMatchArm {
                        enum_name: Some("Option".to_string()),
                        variant_name: Some("Some".to_string()),
                        wildcard: false,
                        label: self.label(present_block),
                    },
                    MirMatchArm {
                        enum_name: Some("Option".to_string()),
                        variant_name: Some("None".to_string()),
                        wildcard: false,
                        label: self.label(after_block),
                    },
                ],
                otherwise: self.label(after_block),
            });
            self.switch_to(present_block);
            let element = self.new_typed_temp(element_ty.clone());
            self.emit(Instruction::Assign {
                target: element.clone(),
                value: Rvalue::VariantPayload {
                    scrutinee: Operand::Place(next_value),
                    variant_name: "Some".to_string(),
                    index: 0,
                },
            });
            element_operands.push(Operand::Place(element));
            element_types.push(element_ty.clone());
        }

        // Advancing here rather than at the loop tail keeps `continue` correct:
        // every path back to the dispatch block has already moved past the
        // position it just read.
        self.emit(Instruction::Assign {
            target: index.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Add,
                left: Operand::Place(index.clone()),
                right: Operand::Int(1),
                span: for_stmt.span,
            },
        });

        let tuple_ty = Type::Tuple(element_types.clone());
        let tuple_place = self.new_typed_temp(tuple_ty.clone());
        self.emit(Instruction::Assign {
            target: tuple_place.clone(),
            value: Rvalue::TupleLiteral {
                elements: element_operands,
                element_types,
            },
        });

        self.loop_stack.push(LoopLabels {
            break_label: self.label(after_block),
            continue_label: self.label(safepoint_block),
            cleanup_depth: self.with_stack.len(),
            loan_depth: self.loan_scopes.len(),
        });
        let target_scope = self.fresh_scoped_binding_target_slots(&for_stmt.target, &tuple_ty);
        self.scoped_names.push(target_scope);
        self.lower_scoped_binding_target_from_place(
            &for_stmt.target,
            &tuple_place,
            &tuple_ty,
            false,
        );
        lower_body(self);
        if !self.current_terminated() {
            self.terminate(Terminator::Goto(self.label(safepoint_block)));
        }
        self.scoped_names.pop();
        self.loop_stack.pop();

        self.switch_to(safepoint_block);
        self.emit(Instruction::Safepoint);
        self.terminate(Terminator::Goto(self.label(dispatch_block)));

        self.switch_to(after_block);
    }

    fn lower_for(&mut self, for_stmt: &crate::ast::ForStmt) {
        let mut lower_body = |lowerer: &mut Self| lowerer.lower_stmts(&for_stmt.body);
        self.lower_for_with_body(for_stmt, None, &mut lower_body);
    }

    fn lower_for_with_body(
        &mut self,
        for_stmt: &crate::ast::ForStmt,
        checked_binding: Option<&ComprehensionClauseInfo>,
        lower_body: &mut dyn FnMut(&mut Self),
    ) {
        if let Some((enumerated, iterables)) = self.lockstep_loop_iterables(&for_stmt.iterable) {
            self.lower_lockstep_for_with_body(
                for_stmt,
                enumerated,
                &iterables,
                checked_binding,
                lower_body,
            );
            return;
        }
        let mut tuple_target_source: Option<(String, Type, bool)> = None;
        let iterable_ty = self.infer_expr_type(&for_stmt.iterable);
        let mut owned_iterable_place = None;
        let iterable = if for_stmt.borrow_mode == Some(ReceiverKind::Value) {
            let source = self.lower_expr_for_owned_value(&for_stmt.iterable, iterable_ty.as_ref());
            let captured = match iterable_ty.clone() {
                Some(ty) => self.new_typed_temp(ty),
                None => self.new_temp(),
            };
            self.emit(Instruction::Assign {
                target: captured.clone(),
                value: Rvalue::Use(source),
            });
            owned_iterable_place = Some(captured.clone());
            Operand::Place(captured)
        } else {
            self.lower_expr_at_sequence_point(&for_stmt.iterable, None)
        };
        let target_ty = checked_binding
            .map(|binding| binding.binding_type.clone())
            .unwrap_or_else(|| match iterable_ty.as_ref() {
                Some(Type::Named(name, _)) if name == "Range" => Type::named("int64"),
                Some(Type::Named(name, args))
                    if matches!(name.as_str(), "Queue" | "list" | "set") && args.len() == 1 =>
                {
                    args[0].clone()
                }
                _ => Type::named("int64"),
            });
        let target_scope = self.fresh_scoped_binding_target_slots(&for_stmt.target, &target_ty);
        let simple_binding = match &for_stmt.target {
            BindingTarget::Name { name, .. } => Some(
                target_scope
                    .get(name)
                    .expect("simple loop target should have a scoped slot")
                    .clone(),
            ),
            BindingTarget::Tuple { .. } => None,
        };
        let dispatch_block = self.new_block("for_iter");
        let body_block = self.new_block("for_body");
        let after_block = self.new_block("for_end");

        match iterable_ty {
            Some(Type::Named(name, _)) if name == "Range" => {
                let binding = simple_binding
                    .clone()
                    .expect("tuple targets are rejected for Range iteration by sema");
                self.terminate(Terminator::Goto(self.label(dispatch_block)));
                self.switch_to(dispatch_block);
                self.terminate(Terminator::ForRange {
                    binding,
                    iterable,
                    body_label: self.label(body_block),
                    exit_label: self.label(after_block),
                });
            }
            Some(Type::Named(name, args)) if name == "Queue" && args.len() == 1 => {
                let element_ty = args[0].clone();
                let binding = match simple_binding.clone() {
                    Some(binding) => binding,
                    None => {
                        let binding = self.new_typed_temp(element_ty.clone());
                        tuple_target_source = Some((
                            binding.clone(),
                            element_ty.clone(),
                            checked_binding
                                .map(|binding| binding.receive_owned)
                                .unwrap_or(true),
                        ));
                        binding
                    }
                };
                let next_value = self.new_typed_temp(Type::Named(
                    "QueueReceive".to_string(),
                    vec![element_ty.clone()],
                ));
                let (field, args) = if let Some(task_group_place) = self.active_task_group_place() {
                    (
                        INTERNAL_QUEUE_GET_IN_TASK_GROUP_FIELD.to_string(),
                        vec![MirArg {
                            name: None,
                            value: Operand::Place(task_group_place),
                            writeback_place: None,
                        }],
                    )
                } else {
                    (
                        INTERNAL_QUEUE_GET_WITH_REGISTERED_PRODUCERS_FIELD.to_string(),
                        Vec::new(),
                    )
                };
                self.terminate(Terminator::Goto(self.label(dispatch_block)));
                self.switch_to(dispatch_block);
                self.emit(Instruction::Assign {
                    target: next_value.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: iterable.clone(),
                            field,
                            receiver_place: self.render_place_expr_option(&for_stmt.iterable),
                        },
                        args,
                    },
                });
                self.terminate(Terminator::Match {
                    scrutinee: Operand::Place(next_value.clone()),
                    arms: vec![
                        MirMatchArm {
                            enum_name: Some("QueueReceive".to_string()),
                            variant_name: Some("Item".to_string()),
                            wildcard: false,
                            label: self.label(body_block),
                        },
                        MirMatchArm {
                            enum_name: Some("QueueReceive".to_string()),
                            variant_name: Some("Closed".to_string()),
                            wildcard: false,
                            label: self.label(after_block),
                        },
                        MirMatchArm {
                            enum_name: Some("QueueReceive".to_string()),
                            variant_name: Some("Cancelled".to_string()),
                            wildcard: false,
                            label: self.label(after_block),
                        },
                    ],
                    otherwise: self.label(after_block),
                });
                self.switch_to(body_block);
                self.emit(Instruction::Assign {
                    target: binding,
                    value: Rvalue::VariantPayload {
                        scrutinee: Operand::MovePlace(next_value),
                        variant_name: "Item".to_string(),
                        index: 0,
                    },
                });
            }
            Some(Type::Named(name, args)) if name == "list" && args.len() == 1 => {
                let element_ty = args[0].clone();
                let takes_owned = owned_iterable_place.is_some();
                let binding = match simple_binding.clone() {
                    Some(binding) => binding,
                    None => {
                        let binding = self.new_typed_temp(element_ty.clone());
                        tuple_target_source = Some((
                            binding.clone(),
                            element_ty.clone(),
                            checked_binding
                                .map(|binding| binding.receive_owned)
                                .unwrap_or(takes_owned),
                        ));
                        binding
                    }
                };
                let next_value = self
                    .new_typed_temp(Type::Named("Option".to_string(), vec![element_ty.clone()]));
                let index = self.new_typed_temp(Type::named("int64"));
                self.emit(Instruction::Assign {
                    target: index.clone(),
                    value: Rvalue::Use(Operand::Int(0)),
                });
                self.terminate(Terminator::Goto(self.label(dispatch_block)));
                self.switch_to(dispatch_block);
                let (iteration_object, iteration_field, iteration_receiver_place) =
                    if let Some(place) = owned_iterable_place.as_ref() {
                        (
                            Operand::MovePlace(place.clone()),
                            INTERNAL_COLLECTION_TAKE_INDEX_OPTION_FIELD.to_string(),
                            Some(place.clone()),
                        )
                    } else {
                        (
                            iterable.clone(),
                            INTERNAL_VEC_INDEX_OPTION_FIELD.to_string(),
                            self.render_place_expr_option(&for_stmt.iterable),
                        )
                    };
                self.emit(Instruction::Assign {
                    target: next_value.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: iteration_object,
                            field: iteration_field,
                            receiver_place: iteration_receiver_place,
                        },
                        args: vec![MirArg {
                            name: None,
                            value: Operand::Place(index.clone()),
                            writeback_place: None,
                        }],
                    },
                });
                self.terminate(Terminator::Match {
                    scrutinee: Operand::Place(next_value.clone()),
                    arms: vec![
                        MirMatchArm {
                            enum_name: Some("Option".to_string()),
                            variant_name: Some("Some".to_string()),
                            wildcard: false,
                            label: self.label(body_block),
                        },
                        MirMatchArm {
                            enum_name: Some("Option".to_string()),
                            variant_name: Some("None".to_string()),
                            wildcard: false,
                            label: self.label(after_block),
                        },
                    ],
                    otherwise: self.label(after_block),
                });
                self.switch_to(body_block);
                self.emit(Instruction::Assign {
                    target: binding.clone(),
                    value: Rvalue::VariantPayload {
                        scrutinee: if takes_owned {
                            Operand::MovePlace(next_value)
                        } else {
                            Operand::Place(next_value)
                        },
                        variant_name: "Some".to_string(),
                        index: 0,
                    },
                });
                if for_stmt.borrow_mode == Some(ReceiverKind::BorrowMut) {
                    let continue_block = self.new_block("for_vec_continue");
                    let break_block = self.new_block("for_vec_break");
                    let return_block = self.new_block("for_vec_return");
                    let cleanup_depth = self.with_stack.len();
                    let return_place = self
                        .return_redirects
                        .last()
                        .map(|redirect| redirect.return_place.clone())
                        .unwrap_or_else(|| self.new_typed_temp(self.return_type.clone()));
                    let parent_return_label = self
                        .return_redirects
                        .last()
                        .map(|redirect| redirect.label.clone());

                    self.loop_stack.push(LoopLabels {
                        break_label: self.label(break_block),
                        continue_label: self.label(continue_block),
                        cleanup_depth,
                        loan_depth: self.loan_scopes.len(),
                    });
                    self.return_redirects.push(ReturnRedirect {
                        label: self.label(return_block),
                        return_place: return_place.clone(),
                        cleanup_depth,
                    });
                    self.scoped_names.push(target_scope.clone());
                    lower_body(self);
                    if !self.current_terminated() {
                        self.terminate(Terminator::Goto(self.label(continue_block)));
                    }
                    self.scoped_names.pop();
                    self.return_redirects.pop();
                    self.loop_stack.pop();

                    self.switch_to(continue_block);
                    self.emit_vec_element_writeback(
                        iterable.clone(),
                        &for_stmt.iterable,
                        &index,
                        &binding,
                        for_stmt.span,
                    );
                    self.emit(Instruction::Assign {
                        target: index.clone(),
                        value: Rvalue::Binary {
                            op: BinaryOp::Add,
                            left: Operand::Place(index.clone()),
                            right: Operand::Int(1),
                            span: for_stmt.span,
                        },
                    });
                    self.emit(Instruction::Safepoint);
                    self.terminate(Terminator::Goto(self.label(dispatch_block)));

                    self.switch_to(break_block);
                    self.emit_vec_element_writeback(
                        iterable.clone(),
                        &for_stmt.iterable,
                        &index,
                        &binding,
                        for_stmt.span,
                    );
                    self.terminate(Terminator::Goto(self.label(after_block)));

                    self.switch_to(return_block);
                    self.emit_vec_element_writeback(
                        iterable,
                        &for_stmt.iterable,
                        &index,
                        &binding,
                        for_stmt.span,
                    );
                    if let Some(parent_label) = parent_return_label {
                        self.terminate(Terminator::Goto(parent_label));
                    } else {
                        self.emit_cleanup_range(0, true);
                        let return_value =
                            self.move_place_for_type(return_place, &self.return_type);
                        self.terminate(Terminator::Return(return_value));
                    }
                    self.switch_to(after_block);
                    return;
                }
                if !takes_owned {
                    self.emit(Instruction::Assign {
                        target: index.clone(),
                        value: Rvalue::Binary {
                            op: BinaryOp::Add,
                            left: Operand::Place(index),
                            right: Operand::Int(1),
                            span: for_stmt.span,
                        },
                    });
                }
            }
            Some(Type::Named(name, args)) if name == "set" && args.len() == 1 => {
                let element_ty = args[0].clone();
                let takes_owned = owned_iterable_place.is_some();
                let binding = match simple_binding.clone() {
                    Some(binding) => binding,
                    None => {
                        let binding = self.new_typed_temp(element_ty.clone());
                        tuple_target_source = Some((
                            binding.clone(),
                            element_ty.clone(),
                            checked_binding
                                .map(|binding| binding.receive_owned)
                                .unwrap_or(takes_owned),
                        ));
                        binding
                    }
                };
                let next_value = self
                    .new_typed_temp(Type::Named("Option".to_string(), vec![element_ty.clone()]));
                let index = self.new_typed_temp(Type::named("int64"));
                self.emit(Instruction::Assign {
                    target: index.clone(),
                    value: Rvalue::Use(Operand::Int(0)),
                });
                self.terminate(Terminator::Goto(self.label(dispatch_block)));
                self.switch_to(dispatch_block);
                let (iteration_object, iteration_field, iteration_receiver_place) =
                    if let Some(place) = owned_iterable_place.as_ref() {
                        (
                            Operand::MovePlace(place.clone()),
                            INTERNAL_COLLECTION_TAKE_INDEX_OPTION_FIELD.to_string(),
                            Some(place.clone()),
                        )
                    } else {
                        (
                            iterable.clone(),
                            INTERNAL_VEC_INDEX_OPTION_FIELD.to_string(),
                            self.render_place_expr_option(&for_stmt.iterable),
                        )
                    };
                self.emit(Instruction::Assign {
                    target: next_value.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: iteration_object,
                            field: iteration_field,
                            receiver_place: iteration_receiver_place,
                        },
                        args: vec![MirArg {
                            name: None,
                            value: Operand::Place(index.clone()),
                            writeback_place: None,
                        }],
                    },
                });
                self.terminate(Terminator::Match {
                    scrutinee: Operand::Place(next_value.clone()),
                    arms: vec![
                        MirMatchArm {
                            enum_name: Some("Option".to_string()),
                            variant_name: Some("Some".to_string()),
                            wildcard: false,
                            label: self.label(body_block),
                        },
                        MirMatchArm {
                            enum_name: Some("Option".to_string()),
                            variant_name: Some("None".to_string()),
                            wildcard: false,
                            label: self.label(after_block),
                        },
                    ],
                    otherwise: self.label(after_block),
                });
                self.switch_to(body_block);
                self.emit(Instruction::Assign {
                    target: binding,
                    value: Rvalue::VariantPayload {
                        scrutinee: if takes_owned {
                            Operand::MovePlace(next_value)
                        } else {
                            Operand::Place(next_value)
                        },
                        variant_name: "Some".to_string(),
                        index: 0,
                    },
                });
                if !takes_owned {
                    self.emit(Instruction::Assign {
                        target: index.clone(),
                        value: Rvalue::Binary {
                            op: BinaryOp::Add,
                            left: Operand::Place(index),
                            right: Operand::Int(1),
                            span: for_stmt.span,
                        },
                    });
                }
            }
            _ => {
                let binding = simple_binding
                    .clone()
                    .expect("tuple targets require a statically known iterable element type");
                self.terminate(Terminator::Goto(self.label(dispatch_block)));
                self.switch_to(dispatch_block);
                self.terminate(Terminator::ForRange {
                    binding,
                    iterable,
                    body_label: self.label(body_block),
                    exit_label: self.label(after_block),
                });
            }
        }

        let safepoint_block = self.new_block("for_safepoint");
        self.loop_stack.push(LoopLabels {
            break_label: self.label(after_block),
            continue_label: self.label(safepoint_block),
            cleanup_depth: self.with_stack.len(),
            loan_depth: self.loan_scopes.len(),
        });
        self.switch_to(body_block);
        self.scoped_names.push(target_scope);
        if let Some((source, source_ty, consume_non_copy)) = tuple_target_source {
            self.lower_scoped_binding_target_from_place(
                &for_stmt.target,
                &source,
                &source_ty,
                consume_non_copy,
            );
        }
        lower_body(self);
        if !self.current_terminated() {
            self.terminate(Terminator::Goto(self.label(safepoint_block)));
        }
        self.scoped_names.pop();
        self.loop_stack.pop();

        self.switch_to(safepoint_block);
        self.emit(Instruction::Safepoint);
        self.terminate(Terminator::Goto(self.label(dispatch_block)));

        self.switch_to(after_block);
    }

    fn emit_vec_element_writeback(
        &mut self,
        iterable: Operand,
        iterable_expr: &Expr,
        index: &str,
        binding: &str,
        span: Span,
    ) {
        let temp = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: temp,
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: iterable,
                    field: INTERNAL_VEC_SET_INDEX_FIELD.to_string(),
                    receiver_place: self.render_place_expr_option(iterable_expr),
                },
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Place(index.to_string()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Place(binding.to_string()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(span.line as u128),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(span.column as u128),
                        writeback_place: None,
                    },
                ],
            },
        });
    }

    fn lower_while(&mut self, while_stmt: &WhileStmt) {
        let condition_block = self.new_block("while_cond");
        let body_block = self.new_block("while_body");
        let safepoint_block = self.new_block("while_safepoint");
        let after_block = self.new_block("while_end");

        self.terminate(Terminator::Goto(self.label(condition_block)));

        self.switch_to(condition_block);
        let condition = self.lower_expr(&while_stmt.condition);
        self.terminate(Terminator::Branch {
            condition,
            then_label: self.label(body_block),
            else_label: self.label(after_block),
        });

        self.loop_stack.push(LoopLabels {
            break_label: self.label(after_block),
            continue_label: self.label(safepoint_block),
            cleanup_depth: self.with_stack.len(),
            loan_depth: self.loan_scopes.len(),
        });
        self.switch_to(body_block);
        self.lower_stmts(&while_stmt.body);
        if !self.current_terminated() {
            self.terminate(Terminator::Goto(self.label(safepoint_block)));
        }
        self.loop_stack.pop();

        self.switch_to(safepoint_block);
        self.emit(Instruction::Safepoint);
        self.terminate(Terminator::Goto(self.label(condition_block)));

        self.switch_to(after_block);
    }

    fn lower_comprehension(
        &mut self,
        expr: &Expr,
        output: &ComprehensionOutput,
        clauses: &[ComprehensionClause],
    ) -> Operand {
        let info = self
            .comprehension_info_at(expr.span)
            .cloned()
            .expect("checked comprehension should retain owner-qualified type metadata");
        let result_place = self.new_typed_temp(info.result_type.clone());
        let result_literal = match (&info.result_type, output) {
            (Type::Named(name, args), ComprehensionOutput::List(_))
                if name == "list" && args.len() == 1 =>
            {
                Rvalue::VecLiteral {
                    elements: Vec::new(),
                    element_type: args[0].clone(),
                }
            }
            (Type::Named(name, args), ComprehensionOutput::Set(_))
                if name == "set" && args.len() == 1 =>
            {
                Rvalue::SetLiteral {
                    elements: Vec::new(),
                    element_type: args[0].clone(),
                }
            }
            (Type::Named(name, args), ComprehensionOutput::Map { .. })
                if name == "dict" && args.len() == 2 =>
            {
                Rvalue::MapLiteral {
                    entries: Vec::new(),
                    key_type: args[0].clone(),
                    value_type: args[1].clone(),
                }
            }
            _ => panic!(
                "comprehension output and checked result type disagree: {:?}",
                info.result_type
            ),
        };
        self.emit(Instruction::Assign {
            target: result_place.clone(),
            value: result_literal,
        });
        self.lower_comprehension_clause(output, clauses, 0, &result_place, &info);
        Operand::Place(result_place)
    }

    fn lower_comprehension_clause(
        &mut self,
        output: &ComprehensionOutput,
        clauses: &[ComprehensionClause],
        clause_index: usize,
        result_place: &str,
        info: &ComprehensionInfo,
    ) {
        let clause = clauses
            .get(clause_index)
            .expect("checked comprehension should contain an iteration clause");
        let for_stmt = crate::ast::ForStmt {
            target: clause.target.clone(),
            iterable: clause.iterable.clone(),
            borrow_mode: None,
            body: Vec::new(),
            span: clause.span,
        };
        let mut lower_body = |lowerer: &mut Self| {
            lowerer.lower_comprehension_filters(
                output,
                clauses,
                clause_index,
                0,
                result_place,
                info,
            );
        };
        let checked_binding = info
            .clauses
            .get(clause_index)
            .expect("checked comprehension should retain every clause binding type");
        self.lower_for_with_body(&for_stmt, Some(checked_binding), &mut lower_body);
    }

    fn lower_comprehension_filters(
        &mut self,
        output: &ComprehensionOutput,
        clauses: &[ComprehensionClause],
        clause_index: usize,
        filter_index: usize,
        result_place: &str,
        info: &ComprehensionInfo,
    ) {
        let clause = &clauses[clause_index];
        if let Some(filter) = clause.filters.get(filter_index) {
            let pass_block = self.new_block("comprehension_filter");
            let continue_label = self
                .loop_stack
                .last()
                .expect("comprehension filter should be inside its clause loop")
                .continue_label
                .clone();
            let condition = self.lower_expr_with_expected(filter, Some(&Type::named("bool")));
            self.terminate(Terminator::Branch {
                condition,
                then_label: self.label(pass_block),
                else_label: continue_label,
            });
            self.switch_to(pass_block);
            self.lower_comprehension_filters(
                output,
                clauses,
                clause_index,
                filter_index + 1,
                result_place,
                info,
            );
            return;
        }
        if clause_index + 1 < clauses.len() {
            self.lower_comprehension_clause(output, clauses, clause_index + 1, result_place, info);
            return;
        }
        self.lower_comprehension_output(output, result_place, &info.result_type);
    }

    fn lower_comprehension_output(
        &mut self,
        output: &ComprehensionOutput,
        result_place: &str,
        result_type: &Type,
    ) {
        match (output, result_type) {
            (ComprehensionOutput::List(value), Type::Named(name, args))
                if name == "list" && args.len() == 1 =>
            {
                let value = self.lower_expr_for_owned_value(value, Some(&args[0]));
                self.emit_vec_push(
                    Operand::Place(result_place.to_string()),
                    result_place,
                    value,
                );
            }
            (ComprehensionOutput::Set(value), Type::Named(name, args))
                if name == "set" && args.len() == 1 =>
            {
                let value = self.lower_expr_for_owned_value(value, Some(&args[0]));
                let ignored = self.new_typed_temp(Type::Unit);
                self.emit(Instruction::Assign {
                    target: ignored,
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: Operand::Place(result_place.to_string()),
                            field: "add".to_string(),
                            receiver_place: Some(result_place.to_string()),
                        },
                        args: vec![MirArg {
                            name: None,
                            value,
                            writeback_place: None,
                        }],
                    },
                });
            }
            (
                ComprehensionOutput::Map {
                    key: key_expr,
                    value,
                },
                Type::Named(name, args),
            ) if name == "dict" && args.len() == 2 => {
                // Map output preserves source order: evaluate the key fully
                // before beginning the value, then replace any prior entry.
                let key = self.lower_expr_for_owned_value(key_expr, Some(&args[0]));
                let value = self.lower_expr_for_owned_value(value, Some(&args[1]));
                let ignored = self.new_typed_temp(Type::Unit);
                self.emit(Instruction::Assign {
                    target: ignored,
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: Operand::Place(result_place.to_string()),
                            field: INTERNAL_MAP_SET_INDEX_FIELD.to_string(),
                            receiver_place: Some(result_place.to_string()),
                        },
                        args: vec![
                            MirArg {
                                name: None,
                                value: key,
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value,
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(key_expr.span.line as u128),
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(key_expr.span.column as u128),
                                writeback_place: None,
                            },
                        ],
                    },
                });
            }
            _ => unreachable!("checked comprehension result shape should match its output"),
        }
    }

    fn lower_expr(&mut self, expr: &Expr) -> Operand {
        if let Some(function) = self.lower_function_value(expr) {
            return function;
        }
        match &expr.kind {
            ExprKind::Name(name) if name == "None" => Operand::Unit,
            ExprKind::BuiltinOmitted => Operand::Unit,
            ExprKind::Lambda { params, body, .. } => self.lower_lambda(expr, params, body),
            ExprKind::Name(name) => {
                if let Some(constant) = self.resolve_constant_info(name).cloned() {
                    self.lower_constant_read(&constant)
                } else if self
                    .view_sources
                    .contains_key(&self.render_local_name(name))
                {
                    let loan = self.render_local_name(name);
                    let ty = self
                        .local_types
                        .get(&loan)
                        .cloned()
                        .unwrap_or_else(|| Type::named("Unknown"));
                    let target = self.new_typed_temp(ty);
                    self.emit(Instruction::ReadLoan {
                        target: target.clone(),
                        loan,
                    });
                    Operand::Place(target)
                } else {
                    Operand::Place(self.render_local_name(name))
                }
            }
            ExprKind::Int(value) => Operand::Int(*value),
            ExprKind::DurationNanos(value) => Operand::Duration(*value),
            ExprKind::Float(value) => Operand::Float(*value),
            ExprKind::Bool(value) => Operand::Bool(*value),
            ExprKind::String(value) => Operand::String(value.clone()),
            ExprKind::Tuple(elements) => self.lower_tuple_literal(elements, None),
            ExprKind::List(elements) => {
                let element_type = elements
                    .first()
                    .and_then(|element| self.infer_expr_type(element))
                    .unwrap_or_else(|| Type::named("Unknown"));
                let temp = self.new_temp_for_expr(expr);
                let elements = elements
                    .iter()
                    .map(|element| self.lower_expr_for_owned_value(element, None))
                    .collect::<Vec<_>>();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::VecLiteral {
                        elements,
                        element_type,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Set(elements) => {
                let element_type = elements
                    .first()
                    .and_then(|element| self.infer_expr_type(element))
                    .unwrap_or_else(|| Type::named("Unknown"));
                let temp = self.new_temp_for_expr(expr);
                let elements = elements
                    .iter()
                    .map(|element| self.lower_expr_for_owned_value(element, None))
                    .collect::<Vec<_>>();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::SetLiteral {
                        elements,
                        element_type,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Map(entries) => {
                let (key_type, value_type) = entries
                    .first()
                    .and_then(|entry| {
                        Some((
                            self.infer_expr_type(&entry.key)?,
                            self.infer_expr_type(&entry.value)?,
                        ))
                    })
                    .unwrap_or_else(|| (Type::named("Unknown"), Type::named("Unknown")));
                let temp = self.new_temp_for_expr(expr);
                let entries = entries
                    .iter()
                    .map(|entry| MirMapEntry {
                        key: self.lower_expr_for_owned_value(&entry.key, None),
                        value: self.lower_expr_for_owned_value(&entry.value, None),
                    })
                    .collect::<Vec<_>>();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::MapLiteral {
                        entries,
                        key_type,
                        value_type,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Comprehension { output, clauses } => {
                self.lower_comprehension(expr, output, clauses)
            }
            ExprKind::FString(parts) => {
                let temp = self.new_typed_temp(Type::named("str"));
                let mut lowered_parts = Vec::with_capacity(parts.len());
                for part in parts {
                    let lowered = match part {
                        crate::ast::FormatPart::Literal(text) => {
                            MirFormatPart::Literal(text.clone())
                        }
                        crate::ast::FormatPart::Expr(expr) => {
                            let value = self.lower_expr_at_sequence_point(expr, None);
                            let rendered = self.new_typed_temp(Type::named("str"));
                            self.emit(Instruction::Assign {
                                target: rendered.clone(),
                                value: Rvalue::FormatString {
                                    parts: vec![MirFormatPart::Value(value)],
                                },
                            });
                            MirFormatPart::Value(Operand::Place(rendered))
                        }
                        crate::ast::FormatPart::Formatted { expr, spec, .. } => {
                            let value_type = self
                                .infer_expr_type(expr)
                                .unwrap_or_else(|| Type::named("Unknown"));
                            let value = self.lower_expr_at_sequence_point(expr, None);
                            MirFormatPart::Formatted {
                                value,
                                spec: spec.clone(),
                                value_type,
                            }
                        }
                    };
                    lowered_parts.push(lowered);
                }
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::FormatString {
                        parts: lowered_parts,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Specialize { expr, .. } => self.lower_expr(expr),
            ExprKind::Group(inner) => self.lower_expr(inner),
            ExprKind::Unary { op, expr: value } => {
                if let Some(field) = self.operator_field_for_unary(*op, value) {
                    let temp = self.new_temp_for_expr(expr);
                    let receiver_place = self.render_place_expr_option(value);
                    let receiver_passing = unary_operator_trait(*op)
                        .and_then(|(trait_name, method_name)| {
                            self.trait_info_in_scope(trait_name)
                                .and_then(|info| info.methods.get(method_name))
                                .and_then(|method| method.decl.receiver)
                        })
                        .unwrap_or(ReceiverKind::Borrow);
                    let object = self.lower_expr_for_passing(value, None, receiver_passing);
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Call {
                            callee: CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                            args: Vec::new(),
                        },
                    });
                    return Operand::Place(temp);
                }
                let value = self.lower_expr(value);
                let temp = self.new_temp_for_expr(expr);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Unary {
                        op: *op,
                        value,
                        span: expr.span,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Cast { expr: value, ty } => {
                let value = self.lower_expr(value);
                let temp = self.new_temp_for_expr(expr);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Cast {
                        value,
                        ty: self.lower_type_ref_with_provenance(ty),
                        span: expr.span,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Try(inner) => {
                let expected = self.infer_expr_type(inner);
                let value = self.lower_expr_for_owned_value(inner, expected.as_ref());
                // ADR-0022 Q3 counts error propagation as an exit path. `try`
                // returns from inside an rvalue rather than through a
                // terminator, so the writeback is applied before it. Applying
                // it early is safe: a successful `try` falls through to the
                // arm's own writeback, which stores the same or a newer value.
                self.emit_active_match_writebacks();
                let temp = self.new_temp_for_expr(expr);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Try { value },
                });
                Operand::Place(temp)
            }
            ExprKind::Binary { op, left, right } => {
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    return self.lower_logical_expr(*op, left, right);
                }
                if let Some(field) = self.operator_field_for_binary(*op, left, right) {
                    let temp = self.new_temp_for_expr(expr);
                    let receiver_place = self.render_place_expr_option(left);
                    let receiver_passing = binary_operator_trait(*op)
                        .and_then(|(trait_name, method_name)| {
                            self.trait_info_in_scope(trait_name)
                                .and_then(|info| info.methods.get(method_name))
                                .and_then(|method| method.decl.receiver)
                        })
                        .unwrap_or(ReceiverKind::Borrow);
                    let object = self.lower_expr_for_passing(left, None, receiver_passing);
                    let source_args = vec![Argument {
                        name: None,
                        value: (**right).clone(),
                        span: right.span,
                    }];
                    let args = self.lower_member_call_args(expr.span, left, &field, &source_args);
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Call {
                            callee: CallTarget::Member {
                                object,
                                field,
                                receiver_place,
                            },
                            args,
                        },
                    });
                    return Operand::Place(temp);
                }
                let left_ty = self.infer_expr_type(left);
                let right_ty = self.infer_expr_type(right);
                let shared_equality_expected = matches!(op, BinaryOp::Eq | BinaryOp::NotEq)
                    .then(|| self.infer_equality_hint(left, right))
                    .flatten();
                let left_expected = shared_equality_expected.as_ref().or_else(|| {
                    if is_integer_literal_expr(left)
                        && right_ty.as_ref().is_some_and(|ty| {
                            is_float_type(ty) || crate::sema::integer_type_bounds(ty).is_some()
                        })
                    {
                        right_ty.as_ref()
                    } else {
                        None
                    }
                });
                let right_expected = shared_equality_expected.as_ref().or_else(|| {
                    if is_integer_literal_expr(right)
                        && left_ty.as_ref().is_some_and(|ty| {
                            is_float_type(ty) || crate::sema::integer_type_bounds(ty).is_some()
                        })
                    {
                        left_ty.as_ref()
                    } else {
                        None
                    }
                });
                let left = self.lower_expr_at_sequence_point(left, left_expected);
                let right = self.lower_expr_with_expected(right, right_expected);
                let temp = self.new_temp_for_expr(expr);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Binary {
                        op: *op,
                        left,
                        right,
                        span: expr.span,
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Member { object, field } => {
                if let Some(module_path) = self.infer_module_path(object) {
                    if let Some(constant) = self
                        .module_namespace(&module_path)
                        .and_then(|namespace| namespace.constants.get(field))
                        .cloned()
                    {
                        return self.lower_constant_read(&constant);
                    }
                }
                if let Some((module_path, enum_name)) = self.qualified_module_item(object) {
                    if let Some(namespace) = self.module_namespace(&module_path) {
                        if let Some(enum_info) = namespace.enums.get(&enum_name).cloned() {
                            let temp = self.new_temp_for_expr(expr);
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::EnumVariant {
                                    enum_name: mir_runtime_enum_name(self.program, &enum_info),
                                    variant_name: field.clone(),
                                    payloads: Vec::new(),
                                },
                            });
                            return Operand::Place(temp);
                        }
                    }
                }
                let base_object = match &object.kind {
                    ExprKind::Specialize { expr, .. } => &**expr,
                    _ => object,
                };
                if let ExprKind::Name(enum_name) = &base_object.kind {
                    let runtime_enum_name = self
                        .resolve_enum_info(enum_name)
                        .map(|enum_info| mir_runtime_enum_name(self.program, enum_info))
                        .or_else(|| {
                            is_known_enum_name(self.program, enum_name).then(|| enum_name.clone())
                        });
                    if let Some(runtime_enum_name) = runtime_enum_name {
                        let temp = self.new_temp_for_expr(expr);
                        self.emit(Instruction::Assign {
                            target: temp.clone(),
                            value: Rvalue::EnumVariant {
                                enum_name: runtime_enum_name,
                                variant_name: field.clone(),
                                payloads: Vec::new(),
                            },
                        });
                        return Operand::Place(temp);
                    }
                }

                let object = self.lower_expr(object);
                let temp = self.new_temp_for_expr(expr);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Member {
                        object,
                        field: field.clone(),
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Index { object, index } => {
                if let Some(Type::Tuple(element_types)) = self.infer_expr_type(object) {
                    let tuple_index = tuple_constant_index(index)
                        .expect("tuple indices are validated as constant integers by sema");
                    let element_type = element_types
                        .get(tuple_index)
                        .cloned()
                        .expect("tuple index bounds are validated by sema");
                    debug_assert!(
                        type_is_copy_in_program(&element_type, self.program),
                        "non-Copy tuple indices are rejected by sema"
                    );
                    let tuple = self.lower_expr_at_sequence_point(object, None);
                    let temp = self.new_typed_temp(element_type.clone());
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::TupleElement {
                            tuple,
                            index: tuple_index,
                            element_type,
                        },
                    });
                    return Operand::Place(temp);
                }
                let temp = self.new_temp_for_expr(expr);
                let object_type = self.infer_expr_type(object);
                let lowered_object = self.lower_expr_at_sequence_point(object, None);
                let lowered_index = match object_type.as_ref() {
                    Some(Type::Named(name, args)) if name == "list" && args.len() == 1 => {
                        self.lower_index_domain_expr(index)
                    }
                    Some(Type::Named(name, args)) if name == "Array" && args.len() == 1 => {
                        self.lower_array_coordinate_expr(index)
                    }
                    _ => self.lower_expr_at_sequence_point(index, None),
                };
                let receiver_place = self.render_place_expr_option(object);
                let field = match object_type {
                    Some(Type::Named(name, args)) if name == "dict" && args.len() == 2 => {
                        INTERNAL_MAP_INDEX_FIELD.to_string()
                    }
                    _ => INTERNAL_VEC_INDEX_FIELD.to_string(),
                };
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: lowered_object,
                            field,
                            receiver_place,
                        },
                        args: vec![
                            MirArg {
                                name: None,
                                value: lowered_index,
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(index.span.line as u128),
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(index.span.column as u128),
                                writeback_place: None,
                            },
                        ],
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Slice {
                object,
                start,
                end,
                colon_span,
            } => {
                let temp = self.new_temp_for_expr(expr);
                let lowered_object = self.lower_expr_at_sequence_point(object, None);
                let lowered_start = start
                    .as_deref()
                    .map(|start| self.lower_index_domain_expr(start))
                    .unwrap_or(Operand::Int(0));
                let lowered_end = end
                    .as_deref()
                    .map(|end| self.lower_index_domain_expr(end))
                    .unwrap_or(Operand::Int(0));
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: lowered_object,
                            field: INTERNAL_SLICE_FIELD.to_string(),
                            receiver_place: self.render_place_expr_option(object),
                        },
                        args: vec![
                            MirArg {
                                name: None,
                                value: lowered_start,
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Bool(start.is_some()),
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: lowered_end,
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Bool(end.is_some()),
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(colon_span.line as u128),
                                writeback_place: None,
                            },
                            MirArg {
                                name: None,
                                value: Operand::Int(colon_span.column as u128),
                                writeback_place: None,
                            },
                        ],
                    },
                });
                Operand::Place(temp)
            }
            ExprKind::Call { callee, args } => self.lower_call(expr, callee, args, None),
            ExprKind::Conditional {
                then_expr,
                condition,
                else_expr,
            } => {
                let expected = self.infer_expr_type(expr);
                self.lower_conditional_expr(
                    then_expr,
                    condition,
                    else_expr,
                    expected.as_ref(),
                    false,
                )
            }
            ExprKind::Match {
                scrutinee,
                capability,
                arms,
            } => self.lower_match_expr(expr, scrutinee, *capability, arms),
            ExprKind::Membership {
                value,
                container,
                negated,
                operator_span,
            } => self.lower_membership_expr(value, container, *negated, *operator_span),
            ExprKind::CompareChain { first, links } => self.lower_compare_chain(first, links),
        }
    }

    /// Lowers the higher-order Vec surface to the ordinary MIR operations both
    /// execution backends already share. In particular, callbacks remain
    /// `CallTarget::Value` calls instead of acquiring a second host callback
    /// ABI. `sort(key=...)` materializes every key before entering the mutation
    /// phase, so a trapping key function cannot leave the source half-sorted.
    fn lower_vec_algorithm_call(
        &mut self,
        expr: &Expr,
        object: &Expr,
        field: &str,
        args: &[Argument],
        result: &str,
    ) -> bool {
        let Some(Type::Named(receiver_name, receiver_args)) = self.infer_expr_type(object) else {
            return false;
        };
        if receiver_name != "list" || receiver_args.len() != 1 {
            return false;
        }
        let element_ty = receiver_args[0].clone();
        let Some(member) = BuiltinMember::resolve("list", field) else {
            return false;
        };
        if !matches!(
            member,
            BuiltinMember::VecSort | BuiltinMember::VecMap | BuiltinMember::VecFilter
        ) {
            return false;
        }

        match member {
            BuiltinMember::VecSort => {
                let receiver_place = self
                    .render_place_expr_option(object)
                    .expect("checked list.sort receiver should be a mutable place");
                let ordered = member
                    .bind_args(args, expr.span)
                    .expect("checked list.sort arguments should bind during MIR lowering");
                let callback = ordered[0].map(|argument| {
                    let callback_ty = self
                        .infer_expr_type(&argument.value)
                        .expect("checked list.sort key should have a function type");
                    let callback =
                        self.lower_expr_at_sequence_point(&argument.value, Some(&callback_ty));
                    let (Type::Function {
                        return_type: key_ty,
                        ..
                    }
                    | Type::Closure {
                        return_type: key_ty,
                        ..
                    }) = callback_ty
                    else {
                        unreachable!("checked list.sort key should have a function type");
                    };
                    (callback, *key_ty)
                });
                let reverse = ordered[1]
                    .map(|argument| {
                        self.lower_expr_at_sequence_point(
                            &argument.value,
                            Some(&Type::named("bool")),
                        )
                    })
                    .unwrap_or(Operand::Bool(false));
                let source = Operand::Place(receiver_place.clone());
                if let Some((callback, key_ty)) = callback {
                    let keys =
                        self.new_typed_temp(Type::Named("list".to_string(), vec![key_ty.clone()]));
                    self.emit(Instruction::Assign {
                        target: keys.clone(),
                        value: Rvalue::VecLiteral {
                            elements: Vec::new(),
                            element_type: key_ty.clone(),
                        },
                    });
                    self.lower_vec_key_collection_loop(
                        source.clone(),
                        &element_ty,
                        callback,
                        &keys,
                        &key_ty,
                        expr.span,
                    );
                    self.lower_stable_vec_sort(
                        source,
                        &receiver_place,
                        &key_ty,
                        Some((&keys, &key_ty)),
                        reverse,
                        expr.span,
                        result,
                    );
                } else {
                    self.lower_stable_vec_sort(
                        source,
                        &receiver_place,
                        &element_ty,
                        None,
                        reverse,
                        expr.span,
                        result,
                    );
                }
            }
            BuiltinMember::VecMap | BuiltinMember::VecFilter => {
                let source = self.lower_shared_vec_source(object);
                let ordered = member
                    .bind_args(args, expr.span)
                    .expect("checked Vec callback arguments should bind during MIR lowering");
                let callback_arg =
                    ordered[0].expect("checked Vec callback call should provide a function");
                let callback_ty = self
                    .infer_expr_type(&callback_arg.value)
                    .expect("checked Vec callback should have a function type");
                let callback =
                    self.lower_expr_at_sequence_point(&callback_arg.value, Some(&callback_ty));
                let output_ty = if member == BuiltinMember::VecMap {
                    let Type::Named(name, output_args) = self
                        .infer_expr_type(expr)
                        .expect("checked Vec.map call should have a result type")
                    else {
                        unreachable!("checked Vec.map result should be Vec[U]");
                    };
                    assert_eq!(name, "list");
                    output_args
                        .into_iter()
                        .next()
                        .expect("checked Vec.map result should retain its element type")
                } else {
                    element_ty.clone()
                };
                self.emit(Instruction::Assign {
                    target: result.to_string(),
                    value: Rvalue::VecLiteral {
                        elements: Vec::new(),
                        element_type: output_ty.clone(),
                    },
                });
                self.lower_vec_transform_loop(
                    source,
                    &element_ty,
                    callback,
                    VecTransformOutput {
                        place: result,
                        element_type: &output_ty,
                    },
                    member == BuiltinMember::VecFilter,
                    expr.span,
                );
            }
            _ => unreachable!("Vec algorithm variants were filtered above"),
        }
        true
    }

    /// Lowers `control.retry` to the same ordinary indirect call, branching,
    /// and scheduler operations used by handwritten Aura code. The runtime
    /// adapters here validate only the host timer boundary and checked
    /// backoff doubling; callbacks never cross a second host ABI.
    fn lower_control_retry_call(
        &mut self,
        expr: &Expr,
        function: &crate::sema::FunctionInfo,
        args: &[Argument],
        explicit_type_args: Option<&[crate::ast::TypeRef]>,
        result: &str,
    ) {
        let mut substitutions = explicit_type_args
            .map(|type_args| {
                substitutions_from_decl_type_args(
                    &function.decl.type_params,
                    &type_args
                        .iter()
                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        if explicit_type_args.is_none() {
            let ordered = bind_call_arguments(
                "function `retry`",
                &callable_params_from_decl(&function.decl.params),
                args,
                expr.span,
                CallConvention::PositionalOrNamed,
            )
            .expect("checked control.retry arguments should bind during MIR lowering");
            let type_params = function
                .decl
                .type_params
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            for (argument, param) in ordered.iter().zip(&function.signature.params) {
                let Some(argument) = argument else {
                    continue;
                };
                let Some(actual) = self.infer_expr_type(&argument.value) else {
                    continue;
                };
                let _ = crate::sema::type_pattern_matches(
                    param,
                    &actual,
                    &type_params,
                    &mut substitutions,
                );
            }
        }
        let result_ty = substitute_type(&function.signature.return_type, &substitutions);
        let worker_ty = substitute_type(&function.signature.params[0], &substitutions);
        let expected_params = vec![
            worker_ty,
            substitute_type(&function.signature.params[1], &substitutions),
            substitute_type(&function.signature.params[2], &substitutions),
        ];
        debug_assert!(matches!(
            &result_ty,
            Type::Named(name, args) if name == "Result" && args.len() == 2
        ));
        let lowered = self.lower_user_args_with_types(
            "function `retry`",
            &function.decl.params,
            args,
            expr.span,
            Some(&expected_params),
            Some(&function.signature.param_passings),
        );
        let [worker, max_attempts, initial_backoff]: [MirArg; 3] = lowered
            .try_into()
            .expect("control.retry has exactly three maintained parameters");

        self.lower_control_retry_state_machine(
            worker.value,
            max_attempts.value,
            initial_backoff.value,
            result_ty,
            expr.span,
            result,
        );
    }

    fn lower_control_retry_state_machine(
        &mut self,
        worker: Operand,
        max_attempts: Operand,
        initial_backoff: Operand,
        result_ty: Type,
        span: Span,
        result: &str,
    ) {
        let max_attempts_place = self.new_typed_temp(Type::named("int32"));
        self.emit(Instruction::Assign {
            target: max_attempts_place.clone(),
            value: Rvalue::Use(max_attempts),
        });
        let backoff = self.new_typed_temp(Type::named("Duration"));
        self.emit(Instruction::Assign {
            target: backoff.clone(),
            value: Rvalue::Use(initial_backoff),
        });
        let validated = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: validated,
            value: Rvalue::Call {
                callee: CallTarget::Name("control::__retry_validate".to_string()),
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Place(max_attempts_place.clone()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Place(backoff.clone()),
                        writeback_place: None,
                    },
                ],
            },
        });

        let attempt = self.new_typed_temp(Type::named("int32"));
        self.emit(Instruction::Assign {
            target: attempt.clone(),
            value: Rvalue::Use(Operand::Int(1)),
        });
        let worker_result = self.new_typed_temp(result_ty.clone());
        let dispatch = self.new_block("retry_dispatch");
        let success = self.new_block("retry_success");
        let error = self.new_block("retry_error");
        let exhausted = self.new_block("retry_exhausted");
        let prepare_delay = self.new_block("retry_prepare_delay");
        let double_delay = self.new_block("retry_double_delay");
        let delay_ready = self.new_block("retry_delay_ready");
        let sleep = self.new_block("retry_sleep");
        let advance = self.new_block("retry_advance");
        let done = self.new_block("retry_done");
        self.terminate(Terminator::Goto(self.label(dispatch)));

        self.switch_to(dispatch);
        let cancellation_checked = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: cancellation_checked,
            value: Rvalue::Call {
                callee: CallTarget::Name("control::__retry_cancel_if_requested".to_string()),
                args: Vec::new(),
            },
        });
        self.emit(Instruction::Assign {
            target: worker_result.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Value(worker),
                args: Vec::new(),
            },
        });
        let cancellation_checked = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: cancellation_checked,
            value: Rvalue::Call {
                callee: CallTarget::Name("control::__retry_cancel_if_requested".to_string()),
                args: Vec::new(),
            },
        });
        self.terminate(Terminator::Match {
            scrutinee: Operand::Place(worker_result.clone()),
            arms: vec![
                MirMatchArm {
                    enum_name: Some("Result".to_string()),
                    variant_name: Some("Ok".to_string()),
                    wildcard: false,
                    label: self.label(success),
                },
                MirMatchArm {
                    enum_name: Some("Result".to_string()),
                    variant_name: Some("Err".to_string()),
                    wildcard: false,
                    label: self.label(error),
                },
            ],
            otherwise: self.label(error),
        });

        self.switch_to(success);
        self.emit(Instruction::Assign {
            target: result.to_string(),
            value: Rvalue::Use(Operand::MovePlace(worker_result.clone())),
        });
        self.terminate(Terminator::Goto(self.label(done)));

        self.switch_to(error);
        let is_exhausted = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: is_exhausted.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Eq,
                left: Operand::Place(attempt.clone()),
                right: Operand::Place(max_attempts_place),
                span,
            },
        });
        self.terminate(Terminator::Branch {
            condition: Operand::Place(is_exhausted),
            then_label: self.label(exhausted),
            else_label: self.label(prepare_delay),
        });

        self.switch_to(exhausted);
        self.emit(Instruction::Assign {
            target: result.to_string(),
            value: Rvalue::Use(Operand::MovePlace(worker_result)),
        });
        self.terminate(Terminator::Goto(self.label(done)));

        self.switch_to(prepare_delay);
        let first_retry = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: first_retry.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Eq,
                left: Operand::Place(attempt.clone()),
                right: Operand::Int(1),
                span,
            },
        });
        self.terminate(Terminator::Branch {
            condition: Operand::Place(first_retry),
            then_label: self.label(delay_ready),
            else_label: self.label(double_delay),
        });

        self.switch_to(double_delay);
        self.emit(Instruction::Assign {
            target: backoff.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Name("control::__retry_next_backoff".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(backoff.clone()),
                    writeback_place: None,
                }],
            },
        });
        self.terminate(Terminator::Goto(self.label(delay_ready)));

        self.switch_to(delay_ready);
        let delay_is_zero = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: delay_is_zero.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Eq,
                left: Operand::Place(backoff.clone()),
                right: Operand::Duration(0),
                span,
            },
        });
        self.terminate(Terminator::Branch {
            condition: Operand::Place(delay_is_zero),
            then_label: self.label(advance),
            else_label: self.label(sleep),
        });

        self.switch_to(sleep);
        let slept = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: slept,
            value: Rvalue::Call {
                callee: CallTarget::Name("sleep".to_string()),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(backoff),
                    writeback_place: None,
                }],
            },
        });
        self.terminate(Terminator::Goto(self.label(advance)));

        self.switch_to(advance);
        self.emit(Instruction::Assign {
            target: attempt.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Add,
                left: Operand::Place(attempt),
                right: Operand::Int(1),
                span,
            },
        });
        self.emit(Instruction::Safepoint);
        self.terminate(Terminator::Goto(self.label(dispatch)));
        self.switch_to(done);
    }

    fn lower_shared_vec_source(&mut self, object: &Expr) -> Operand {
        // A source-level name is not necessarily a frame-local MIR place.
        // Module constants are loaded through `Rvalue::ModuleConstant`, while
        // locals and parameters naturally lower back to their existing place.
        // Running every shared source through expression lowering preserves
        // that distinction for map/filter without changing ownership.
        let expected = self.infer_expr_type(object);
        self.lower_expr_at_sequence_point(object, expected.as_ref())
    }

    fn lower_vec_key_collection_loop(
        &mut self,
        source: Operand,
        element_ty: &Type,
        callback: Operand,
        keys: &str,
        key_ty: &Type,
        span: Span,
    ) {
        let index = self.new_typed_temp(Type::named("int64"));
        let next = self.new_typed_temp(Type::Named("Option".to_string(), vec![element_ty.clone()]));
        let element = self.new_typed_temp(element_ty.clone());
        self.emit(Instruction::Assign {
            target: index.clone(),
            value: Rvalue::Use(Operand::Int(0)),
        });

        let dispatch = self.new_block("vec_key_dispatch");
        let body = self.new_block("vec_key_body");
        let done = self.new_block("vec_key_done");
        self.terminate(Terminator::Goto(self.label(dispatch)));

        self.switch_to(dispatch);
        self.emit_vec_optional_read(&next, source.clone(), &index);
        self.terminate(Terminator::Match {
            scrutinee: Operand::Place(next.clone()),
            arms: vec![
                MirMatchArm {
                    enum_name: Some("Option".to_string()),
                    variant_name: Some("Some".to_string()),
                    wildcard: false,
                    label: self.label(body),
                },
                MirMatchArm {
                    enum_name: Some("Option".to_string()),
                    variant_name: Some("None".to_string()),
                    wildcard: false,
                    label: self.label(done),
                },
            ],
            otherwise: self.label(done),
        });

        self.switch_to(body);
        self.emit(Instruction::Assign {
            target: element.clone(),
            value: Rvalue::VariantPayload {
                scrutinee: Operand::MovePlace(next),
                variant_name: "Some".to_string(),
                index: 0,
            },
        });
        let key = self.new_typed_temp(key_ty.clone());
        self.emit(Instruction::Assign {
            target: key.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Value(callback),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(element),
                    writeback_place: None,
                }],
            },
        });
        self.emit_vec_push(
            Operand::Place(keys.to_string()),
            keys,
            self.move_place_for_type(key, key_ty),
        );
        self.emit_vec_index_increment(&index, span);
        self.terminate(Terminator::Goto(self.label(dispatch)));
        self.switch_to(done);
    }

    fn lower_vec_transform_loop(
        &mut self,
        source: Operand,
        element_ty: &Type,
        callback: Operand,
        output: VecTransformOutput<'_>,
        filter: bool,
        span: Span,
    ) {
        let index = self.new_typed_temp(Type::named("int64"));
        let next = self.new_typed_temp(Type::Named("Option".to_string(), vec![element_ty.clone()]));
        let element = self.new_typed_temp(element_ty.clone());
        self.emit(Instruction::Assign {
            target: index.clone(),
            value: Rvalue::Use(Operand::Int(0)),
        });

        let dispatch = self.new_block(if filter {
            "vec_filter_dispatch"
        } else {
            "vec_map_dispatch"
        });
        let body = self.new_block(if filter {
            "vec_filter_body"
        } else {
            "vec_map_body"
        });
        let advance = self.new_block(if filter {
            "vec_filter_advance"
        } else {
            "vec_map_advance"
        });
        let done = self.new_block(if filter {
            "vec_filter_done"
        } else {
            "vec_map_done"
        });
        self.terminate(Terminator::Goto(self.label(dispatch)));

        self.switch_to(dispatch);
        self.emit_vec_optional_read(&next, source.clone(), &index);
        self.terminate(Terminator::Match {
            scrutinee: Operand::Place(next.clone()),
            arms: vec![
                MirMatchArm {
                    enum_name: Some("Option".to_string()),
                    variant_name: Some("Some".to_string()),
                    wildcard: false,
                    label: self.label(body),
                },
                MirMatchArm {
                    enum_name: Some("Option".to_string()),
                    variant_name: Some("None".to_string()),
                    wildcard: false,
                    label: self.label(done),
                },
            ],
            otherwise: self.label(done),
        });

        self.switch_to(body);
        self.emit(Instruction::Assign {
            target: element.clone(),
            value: Rvalue::VariantPayload {
                scrutinee: Operand::MovePlace(next),
                variant_name: "Some".to_string(),
                index: 0,
            },
        });
        let callback_result = self.new_typed_temp(if filter {
            Type::named("bool")
        } else {
            output.element_type.clone()
        });
        self.emit(Instruction::Assign {
            target: callback_result.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Value(callback),
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(element.clone()),
                    writeback_place: None,
                }],
            },
        });
        if filter {
            let keep = self.new_block("vec_filter_keep");
            self.terminate(Terminator::Branch {
                condition: Operand::Place(callback_result),
                then_label: self.label(keep),
                else_label: self.label(advance),
            });
            self.switch_to(keep);
            self.emit_vec_push(
                Operand::Place(output.place.to_string()),
                output.place,
                self.move_place_for_type(element, element_ty),
            );
            self.terminate(Terminator::Goto(self.label(advance)));
        } else {
            self.emit_vec_push(
                Operand::Place(output.place.to_string()),
                output.place,
                self.move_place_for_type(callback_result, output.element_type),
            );
            self.terminate(Terminator::Goto(self.label(advance)));
        }

        self.switch_to(advance);
        self.emit_vec_index_increment(&index, span);
        self.terminate(Terminator::Goto(self.label(dispatch)));
        self.switch_to(done);
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_stable_vec_sort(
        &mut self,
        source: Operand,
        source_place: &str,
        ordering_ty: &Type,
        keys: Option<(&str, &Type)>,
        reverse: Operand,
        span: Span,
        result: &str,
    ) {
        let ordering_source = keys
            .map(|(keys, _)| Operand::Place(keys.to_string()))
            .unwrap_or_else(|| source.clone());
        let length = self.new_typed_temp(Type::named("int64"));
        self.emit(Instruction::Assign {
            target: length.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: ordering_source.clone(),
                    field: "len".to_string(),
                    receiver_place: None,
                },
                args: Vec::new(),
            },
        });
        let outer_index = self.new_typed_temp(Type::named("int64"));
        self.emit(Instruction::Assign {
            target: outer_index.clone(),
            value: Rvalue::Use(Operand::Int(1)),
        });

        let outer_condition = self.new_block("vec_sort_outer_condition");
        let outer_body = self.new_block("vec_sort_outer_body");
        let outer_advance = self.new_block("vec_sort_outer_advance");
        let inner_condition = self.new_block("vec_sort_inner_condition");
        let inner_compare = self.new_block("vec_sort_inner_compare");
        let swap = self.new_block("vec_sort_swap");
        let done = self.new_block("vec_sort_done");
        self.terminate(Terminator::Goto(self.label(outer_condition)));

        self.switch_to(outer_condition);
        let has_element = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: has_element.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Less,
                left: Operand::Place(outer_index.clone()),
                right: Operand::Place(length),
                span,
            },
        });
        self.terminate(Terminator::Branch {
            condition: Operand::Place(has_element),
            then_label: self.label(outer_body),
            else_label: self.label(done),
        });

        let inner_index = self.new_typed_temp(Type::named("int64"));
        self.switch_to(outer_body);
        self.emit(Instruction::Assign {
            target: inner_index.clone(),
            value: Rvalue::Use(Operand::Place(outer_index.clone())),
        });
        self.terminate(Terminator::Goto(self.label(inner_condition)));

        self.switch_to(inner_condition);
        let can_compare = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: can_compare.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Greater,
                left: Operand::Place(inner_index.clone()),
                right: Operand::Int(0),
                span,
            },
        });
        self.terminate(Terminator::Branch {
            condition: Operand::Place(can_compare),
            then_label: self.label(inner_compare),
            else_label: self.label(outer_advance),
        });

        self.switch_to(inner_compare);
        let previous_index = self.new_typed_temp(Type::named("int64"));
        self.emit(Instruction::Assign {
            target: previous_index.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Sub,
                left: Operand::Place(inner_index.clone()),
                right: Operand::Int(1),
                span,
            },
        });
        let current = self.emit_vec_read(ordering_source.clone(), &inner_index, ordering_ty, span);
        let previous =
            self.emit_vec_read(ordering_source.clone(), &previous_index, ordering_ty, span);
        let ascending = self.emit_vec_ordering_less(&current, &previous, ordering_ty, span);
        let descending = self.emit_vec_ordering_less(&previous, &current, ordering_ty, span);
        let not_reverse = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: not_reverse.clone(),
            value: Rvalue::Unary {
                op: UnaryOp::Not,
                value: reverse.clone(),
                span,
            },
        });
        let ascending_selected = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: ascending_selected.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::And,
                left: Operand::Place(not_reverse),
                right: Operand::Place(ascending),
                span,
            },
        });
        let descending_selected = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: descending_selected.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::And,
                left: reverse,
                right: Operand::Place(descending),
                span,
            },
        });
        let comes_before = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: comes_before.clone(),
            value: Rvalue::Binary {
                op: BinaryOp::Or,
                left: Operand::Place(ascending_selected),
                right: Operand::Place(descending_selected),
                span,
            },
        });
        self.terminate(Terminator::Branch {
            condition: Operand::Place(comes_before),
            then_label: self.label(swap),
            else_label: self.label(outer_advance),
        });

        self.switch_to(swap);
        self.emit_vec_swap(source.clone(), source_place, &inner_index, &previous_index);
        if let Some((keys, _)) = keys {
            self.emit_vec_swap(
                Operand::Place(keys.to_string()),
                keys,
                &inner_index,
                &previous_index,
            );
        }
        self.emit(Instruction::Assign {
            target: inner_index.clone(),
            value: Rvalue::Use(Operand::Place(previous_index)),
        });
        self.emit(Instruction::Safepoint);
        self.terminate(Terminator::Goto(self.label(inner_condition)));

        self.switch_to(outer_advance);
        self.emit_vec_index_increment(&outer_index, span);
        self.terminate(Terminator::Goto(self.label(outer_condition)));

        self.switch_to(done);
        self.emit(Instruction::Assign {
            target: result.to_string(),
            value: Rvalue::Use(Operand::Unit),
        });
    }

    fn emit_vec_optional_read(&mut self, target: &str, source: Operand, index: &str) {
        self.emit(Instruction::Assign {
            target: target.to_string(),
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: source,
                    field: INTERNAL_VEC_INDEX_OPTION_FIELD.to_string(),
                    receiver_place: None,
                },
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(index.to_string()),
                    writeback_place: None,
                }],
            },
        });
    }

    fn emit_vec_read(
        &mut self,
        source: Operand,
        index: &str,
        element_ty: &Type,
        span: Span,
    ) -> String {
        let target = self.new_typed_temp(element_ty.clone());
        self.emit(Instruction::Assign {
            target: target.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: source,
                    field: INTERNAL_VEC_INDEX_FIELD.to_string(),
                    receiver_place: None,
                },
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Place(index.to_string()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(span.line as u128),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Int(span.column as u128),
                        writeback_place: None,
                    },
                ],
            },
        });
        target
    }

    fn emit_vec_ordering_less(&mut self, left: &str, right: &str, ty: &Type, span: Span) -> String {
        let result = self.new_typed_temp(Type::named("bool"));
        let value = if is_builtin_binary_operator(BinaryOp::Less, ty, ty) {
            Rvalue::Binary {
                op: BinaryOp::Less,
                left: Operand::Place(left.to_string()),
                right: Operand::Place(right.to_string()),
                span,
            }
        } else {
            Rvalue::Call {
                callee: CallTarget::Member {
                    object: Operand::Place(left.to_string()),
                    field: "lt".to_string(),
                    receiver_place: None,
                },
                args: vec![MirArg {
                    name: None,
                    value: Operand::Place(right.to_string()),
                    writeback_place: None,
                }],
            }
        };
        self.emit(Instruction::Assign {
            target: result.clone(),
            value,
        });
        result
    }

    fn emit_vec_push(&mut self, vector: Operand, vector_place: &str, value: Operand) {
        let ignored = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: ignored,
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: vector,
                    field: "append".to_string(),
                    receiver_place: Some(vector_place.to_string()),
                },
                args: vec![MirArg {
                    name: None,
                    value,
                    writeback_place: None,
                }],
            },
        });
    }

    fn emit_vec_swap(&mut self, vector: Operand, vector_place: &str, first: &str, second: &str) {
        let ignored = self.new_typed_temp(Type::Unit);
        self.emit(Instruction::Assign {
            target: ignored,
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: vector,
                    field: "swap".to_string(),
                    receiver_place: Some(vector_place.to_string()),
                },
                args: vec![
                    MirArg {
                        name: None,
                        value: Operand::Place(first.to_string()),
                        writeback_place: None,
                    },
                    MirArg {
                        name: None,
                        value: Operand::Place(second.to_string()),
                        writeback_place: None,
                    },
                ],
            },
        });
    }

    fn emit_vec_index_increment(&mut self, index: &str, span: Span) {
        self.emit(Instruction::Assign {
            target: index.to_string(),
            value: Rvalue::Binary {
                op: BinaryOp::Add,
                left: Operand::Place(index.to_string()),
                right: Operand::Int(1),
                span,
            },
        });
        self.emit(Instruction::Safepoint);
    }

    /// Lowers `value in container` to the builtin membership member the
    /// container supplies, so both backends reuse one dispatch path.
    fn lower_membership_expr(
        &mut self,
        value: &Expr,
        container: &Expr,
        negated: bool,
        operator_span: crate::diag::Span,
    ) -> Operand {
        let container_ty = self
            .infer_expr_type(container)
            .unwrap_or_else(|| Type::named("Unknown"));
        let needle_ty = crate::sema::membership_needle_type(&container_ty);
        let value_operand = self.lower_expr_at_sequence_point(value, needle_ty.as_ref());
        let container_operand = self.lower_expr_at_sequence_point(container, None);
        let receiver_place = self.render_place_expr_option(container);
        self.lower_membership_call(
            value_operand,
            container_operand,
            receiver_place,
            &container_ty,
            negated,
            operator_span,
        )
    }

    fn lower_membership_call(
        &mut self,
        value: Operand,
        container: Operand,
        receiver_place: Option<String>,
        container_ty: &Type,
        negated: bool,
        operator_span: crate::diag::Span,
    ) -> Operand {
        let member = crate::sema::membership_member_name(container_ty).unwrap_or("contains");
        let contains = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: contains.clone(),
            value: Rvalue::Call {
                callee: CallTarget::Member {
                    object: container,
                    field: member.to_string(),
                    receiver_place,
                },
                args: vec![MirArg {
                    name: None,
                    value,
                    writeback_place: None,
                }],
            },
        });
        if !negated {
            return Operand::Place(contains);
        }
        let result = self.new_typed_temp(Type::named("bool"));
        self.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Unary {
                op: UnaryOp::Not,
                value: Operand::Place(contains),
                span: operator_span,
            },
        });
        Operand::Place(result)
    }

    /// Lowers `a < b <= c` so every operand is evaluated once, in source order,
    /// and a failing link short-circuits the operands that follow it.
    fn lower_compare_chain(&mut self, first: &Expr, links: &[CompareLink]) -> Operand {
        let result = self.new_typed_temp(Type::named("bool"));
        let join_block = self.new_block("compare_chain_join");
        let short_circuit_block = self.new_block("compare_chain_false");

        let first_expected = links.first().and_then(|link| {
            matches!(link.op.as_binary_op(), Some(BinaryOp::Eq | BinaryOp::NotEq))
                .then(|| self.infer_equality_hint(first, &link.operand))
                .flatten()
        });
        let mut left_ty = first_expected
            .clone()
            .or_else(|| self.infer_expr_type(first));
        let mut left_expr = first;
        let mut left = self.lower_expr_at_sequence_point(first, first_expected.as_ref());
        for (index, link) in links.iter().enumerate() {
            let link_value = match link.op.as_binary_op() {
                Some(op) => {
                    let shared_equality_expected = if matches!(op, BinaryOp::Eq | BinaryOp::NotEq) {
                        self.infer_equality_hint(left_expr, &link.operand)
                            .or_else(|| left_ty.clone())
                    } else {
                        None
                    };
                    let right = self.lower_expr_at_sequence_point(
                        &link.operand,
                        shared_equality_expected.as_ref(),
                    );
                    let compared = self.new_typed_temp(Type::named("bool"));
                    self.emit(Instruction::Assign {
                        target: compared.clone(),
                        value: Rvalue::Binary {
                            op,
                            left: left.clone(),
                            right: right.clone(),
                            span: link.op_span,
                        },
                    });
                    left = right;
                    left_ty =
                        shared_equality_expected.or_else(|| self.infer_expr_type(&link.operand));
                    Operand::Place(compared)
                }
                None => {
                    let container_ty = self
                        .infer_expr_type(&link.operand)
                        .unwrap_or_else(|| Type::named("Unknown"));
                    let container = self.lower_expr_at_sequence_point(&link.operand, None);
                    let receiver_place = self.render_place_expr_option(&link.operand);
                    let contains = self.lower_membership_call(
                        left,
                        container.clone(),
                        receiver_place,
                        &container_ty,
                        link.op == CompareOp::NotIn,
                        link.op_span,
                    );
                    left = container;
                    left_ty = Some(container_ty);
                    contains
                }
            };
            left_expr = &link.operand;
            if index + 1 == links.len() {
                self.emit(Instruction::Assign {
                    target: result.clone(),
                    value: Rvalue::Use(link_value),
                });
                self.terminate(Terminator::Goto(self.label(join_block)));
            } else {
                let next_block = self.new_block("compare_chain_next");
                self.terminate(Terminator::Branch {
                    condition: link_value,
                    then_label: self.label(next_block),
                    else_label: self.label(short_circuit_block),
                });
                self.switch_to(next_block);
            }
        }

        self.switch_to(short_circuit_block);
        self.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Use(Operand::Bool(false)),
        });
        self.terminate(Terminator::Goto(self.label(join_block)));

        self.switch_to(join_block);
        Operand::Place(result)
    }

    fn lower_tuple_literal(
        &mut self,
        elements: &[Expr],
        expected_element_types: Option<&[Type]>,
    ) -> Operand {
        let element_types = match expected_element_types {
            Some(expected) if expected.len() == elements.len() => expected.to_vec(),
            _ => elements
                .iter()
                .map(|element| {
                    self.infer_expr_type(element)
                        .unwrap_or_else(|| Type::named("Unknown"))
                })
                .collect::<Vec<_>>(),
        };
        let mut captured_elements = Vec::with_capacity(elements.len());
        for (element, element_ty) in elements.iter().zip(&element_types) {
            let value = self.lower_expr_for_owned_value(element, Some(element_ty));
            let captured = self.new_typed_temp(element_ty.clone());
            self.emit(Instruction::Assign {
                target: captured.clone(),
                value: Rvalue::Use(value),
            });
            captured_elements.push(self.move_place_for_type(captured, element_ty));
        }
        let temp = self.new_typed_temp(Type::Tuple(element_types.clone()));
        self.emit(Instruction::Assign {
            target: temp.clone(),
            value: Rvalue::TupleLiteral {
                elements: captured_elements,
                element_types,
            },
        });
        Operand::Place(temp)
    }

    fn lower_expr_with_expected(&mut self, expr: &Expr, expected: Option<&Type>) -> Operand {
        if let Some(expected @ Type::Function { .. }) = expected {
            if let Some(Operand::Function { name, .. }) = self.lower_function_value(expr) {
                // Contextual typing is what gives a generic named function
                // value its concrete callable type (for example, assigning
                // `empty` to `def() -> Option[String]`). Preserve that exact
                // type in MIR rather than leaving unresolved declaration type
                // parameters on the runtime value.
                return Operand::Function {
                    name,
                    signature: Box::new(expected.clone()),
                };
            }
        }
        if Self::is_contextual_none_expr(expr)
            && matches!(expected, Some(Type::Named(name, args)) if name == "Option" && args.len() == 1)
        {
            let expected = expected.expect("contextual Option type should be present");
            let temp = self.new_typed_temp(expected.clone());
            self.emit(Instruction::Assign {
                target: temp.clone(),
                value: Rvalue::EnumVariant {
                    enum_name: "Option".to_string(),
                    variant_name: "None".to_string(),
                    payloads: Vec::new(),
                },
            });
            return Operand::Place(temp);
        }
        if let Some(expected) = expected {
            if let Some(value) = self.lower_collection_literal_with_type(expr, expected) {
                let temp = self.new_typed_temp(expected.clone());
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value,
                });
                return Operand::Place(temp);
            }
        }
        match &expr.kind {
            ExprKind::Group(inner) => self.lower_expr_with_expected(inner, expected),
            ExprKind::Tuple(elements) => match expected {
                Some(Type::Tuple(element_types)) if element_types.len() == elements.len() => {
                    self.lower_tuple_literal(elements, Some(element_types))
                }
                _ => self.lower_tuple_literal(elements, None),
            },
            ExprKind::Conditional {
                then_expr,
                condition,
                else_expr,
            } => self.lower_conditional_expr(then_expr, condition, else_expr, expected, false),
            ExprKind::Int(value) => {
                if let Some(value) = expected
                    .and_then(|expected| contextual_float_literal_operand(*value, false, expected))
                {
                    return value;
                }
                if let Some(expected) =
                    expected.filter(|expected| crate::sema::integer_type_bounds(expected).is_some())
                {
                    let temp = self.new_typed_temp(expected.clone());
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Use(Operand::Int(*value)),
                    });
                    return Operand::Place(temp);
                }
                self.lower_expr(expr)
            }
            ExprKind::Unary {
                op: UnaryOp::Neg,
                expr: inner,
            } => match &inner.kind {
                ExprKind::Int(value) => {
                    if let Some(value) = expected.and_then(|expected| {
                        contextual_float_literal_operand(*value, true, expected)
                    }) {
                        return value;
                    }
                    if let Some(expected) = expected
                        .filter(|expected| crate::sema::integer_type_bounds(expected).is_some())
                    {
                        let temp = self.new_typed_temp(expected.clone());
                        self.emit(Instruction::Assign {
                            target: temp.clone(),
                            value: Rvalue::Unary {
                                op: UnaryOp::Neg,
                                value: Operand::Int(*value),
                                span: expr.span,
                            },
                        });
                        return Operand::Place(temp);
                    }
                    self.lower_expr(expr)
                }
                _ => self.lower_expr(expr),
            },
            ExprKind::Call { callee, args } => self.lower_call(expr, callee, args, expected),
            _ => self.lower_expr(expr),
        }
    }

    fn lower_expr_at_sequence_point(&mut self, expr: &Expr, expected: Option<&Type>) -> Operand {
        let value = self.lower_expr_with_expected(expr, expected);
        if self.render_place_expr_option(expr).is_none() {
            return value;
        }
        let Operand::Place(_) = value else {
            return value;
        };
        let inferred_type = self.infer_expr_type(expr);
        let value_type = if Self::is_contextual_none_expr(expr) {
            expected.cloned().or(inferred_type)
        } else {
            inferred_type.or_else(|| expected.cloned())
        };
        let Some(value_type) = value_type else {
            return value;
        };
        if !type_is_copy_in_program(&value_type, self.program) {
            return value;
        }
        let temp = self.new_typed_temp(value_type);
        self.emit(Instruction::Assign {
            target: temp.clone(),
            value: Rvalue::Use(value),
        });
        Operand::Place(temp)
    }

    /// Lowers one of the public index-domain positions. The checker admits
    /// only integer types whose complete domain fits in int64; MIR makes that
    /// narrowly scoped conversion explicit so both execution backends observe
    /// an int64 value without enabling general implicit numeric conversion.
    fn lower_index_domain_expr(&mut self, expr: &Expr) -> Operand {
        let target = Type::named("int64");
        let actual = self.infer_expr_type(expr).unwrap_or_else(|| target.clone());
        let source_expected = if actual == target { &target } else { &actual };
        let value = self.lower_expr_at_sequence_point(expr, Some(source_expected));
        if actual == target {
            return value;
        }
        let temp = self.new_typed_temp(target.clone());
        self.emit(Instruction::Assign {
            target: temp.clone(),
            value: Rvalue::Cast {
                value,
                ty: target,
                span: expr.span,
            },
        });
        Operand::Place(temp)
    }

    fn lower_array_coordinate_expr(&mut self, expr: &Expr) -> Operand {
        match &expr.kind {
            ExprKind::Group(inner) => self.lower_array_coordinate_expr(inner),
            ExprKind::Tuple(elements) => {
                let element_types = vec![Type::named("int64"); elements.len()];
                let elements = elements
                    .iter()
                    .map(|element| self.lower_index_domain_expr(element))
                    .collect();
                let temp = self.new_typed_temp(Type::Tuple(element_types.clone()));
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::TupleLiteral {
                        elements,
                        element_types,
                    },
                });
                Operand::Place(temp)
            }
            _ => self.lower_index_domain_expr(expr),
        }
    }

    fn lower_expr_for_passing(
        &mut self,
        expr: &Expr,
        expected: Option<&Type>,
        passing: ReceiverKind,
    ) -> Operand {
        if matches!(passing, ReceiverKind::Borrow | ReceiverKind::BorrowMut) {
            if let Some((origin, projections)) = self.returned_view_source(expr) {
                let _ = self.lower_expr_with_expected(expr, expected);
                let ty = expected
                    .cloned()
                    .or_else(|| self.infer_expr_type(expr))
                    .unwrap_or_else(|| Type::named("Unknown"));
                let loan = self.new_typed_temp(ty);
                self.emit(Instruction::BeginReturnedLoan {
                    loan: loan.clone(),
                    origin: origin.clone(),
                    projections,
                    mutable: passing == ReceiverKind::BorrowMut,
                });
                self.view_sources.insert(loan.clone(), origin);
                if let Some(scope) = self.loan_scopes.last_mut() {
                    scope.push(loan.clone());
                }
                return Operand::Place(loan);
            }
            return self.lower_expr_with_expected(expr, expected);
        }
        self.lower_expr_for_owned_value(expr, expected)
    }

    fn lower_expr_for_owned_value(&mut self, expr: &Expr, expected: Option<&Type>) -> Operand {
        if let ExprKind::Conditional {
            then_expr,
            condition,
            else_expr,
        } = &expr.kind
        {
            let value_type = match expected {
                Some(expected) => expected.clone(),
                None => match self.infer_conditional_result_type(then_expr, else_expr) {
                    Some(inferred) => inferred,
                    None => Type::named("Unknown"),
                },
            };
            let value = self.lower_conditional_expr(
                then_expr,
                condition,
                else_expr,
                Some(&value_type),
                true,
            );
            return match value {
                Operand::Place(place) if !type_is_copy_in_program(&value_type, self.program) => {
                    Operand::MovePlace(place)
                }
                other => other,
            };
        }
        let value_type = if Self::is_contextual_none_expr(expr) {
            match expected {
                Some(expected) => Some(expected.clone()),
                None => self.infer_expr_type(expr),
            }
        } else {
            self.infer_expr_type(expr).or_else(|| expected.cloned())
        };
        if self.is_non_owning_place_expr(expr) {
            return self.lower_expr_at_sequence_point(expr, expected);
        }
        if let (Some(place), Some(value_type)) = (
            self.render_addressable_place_expr_option(expr),
            value_type.as_ref(),
        ) {
            if !type_is_copy_in_program(value_type, self.program) {
                return Operand::MovePlace(place);
            }
        }
        let value = self.lower_expr_at_sequence_point(expr, expected);
        let Some(value_type) = value_type else {
            return value;
        };
        match value {
            Operand::Place(place) if !type_is_copy_in_program(&value_type, self.program) => {
                Operand::MovePlace(place)
            }
            other => other,
        }
    }

    fn move_place_for_type(&self, place: String, ty: &Type) -> Operand {
        if type_is_copy_in_program(ty, self.program) {
            Operand::Place(place)
        } else {
            Operand::MovePlace(place)
        }
    }

    fn is_contextual_none_expr(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Name(name) => name == "None",
            ExprKind::Group(inner) => Self::is_contextual_none_expr(inner),
            _ => false,
        }
    }

    fn lower_match_expr(
        &mut self,
        expr: &Expr,
        scrutinee_expr: &Expr,
        borrow_mode: ReceiverKind,
        arms: &[crate::ast::MatchExprArm],
    ) -> Operand {
        let scrutinee_ty = self.infer_expr_type(scrutinee_expr);
        let consumes_scrutinee = borrow_mode == ReceiverKind::Value
            && scrutinee_ty
                .as_ref()
                .is_some_and(|ty| !type_is_copy_in_program(ty, self.program));
        let scrutinee = if consumes_scrutinee {
            let source = self.lower_expr_for_owned_value(scrutinee_expr, scrutinee_ty.as_ref());
            let captured = match scrutinee_ty.clone() {
                Some(ty) => self.new_typed_temp(ty),
                None => self.new_temp(),
            };
            self.emit(Instruction::Assign {
                target: captured.clone(),
                value: Rvalue::Use(source),
            });
            Operand::Place(captured)
        } else {
            self.lower_expr(scrutinee_expr)
        };
        let writeback_root = if borrow_mode == ReceiverKind::BorrowMut {
            self.render_place_expr_option(scrutinee_expr)
        } else {
            None
        };
        let result = self.new_temp_for_expr(expr);
        let after_block = self.new_block("match_expr_end");
        let mut next_case_block = self.current_block;

        for (index, arm) in arms.iter().enumerate() {
            self.switch_to(next_case_block);
            let arm_block = self.new_block("match_expr_arm");
            let next_block = if index + 1 == arms.len() {
                after_block
            } else {
                self.new_block("match_expr_next")
            };
            self.scoped_names.push(std::collections::HashMap::new());
            let probes_candidates = arm.guard.is_some() || matches!(arm.pattern, Pattern::Or(_));
            let pattern_writeback = self.lower_pattern(
                &arm.pattern,
                scrutinee.clone(),
                scrutinee_ty.as_ref(),
                arm_block,
                next_block,
                PatternLoweringOptions {
                    collect_writeback: writeback_root.is_some(),
                    consume_payloads: consumes_scrutinee && !probes_candidates,
                },
            );
            self.switch_to(arm_block);
            if let Some(writeback_place) = writeback_root.as_ref() {
                let skip_place = self.new_typed_temp(Type::named("bool"));
                self.match_writeback_stack.push(MatchWritebackState {
                    root: writeback_place.clone(),
                    skip_place: skip_place.clone(),
                    writeback: pattern_writeback.clone(),
                });
                self.emit(Instruction::Assign {
                    target: skip_place,
                    value: Rvalue::Use(Operand::Bool(false)),
                });
            }
            if let Some(guard) = &arm.guard {
                let selected = self.new_block("match_expr_guard_true");
                let rejected = self.new_block("match_expr_guard_false");
                let condition = self.lower_expr(guard);
                self.terminate(Terminator::Branch {
                    condition,
                    then_label: self.label(selected),
                    else_label: self.label(rejected),
                });
                self.switch_to(rejected);
                if let (Some(writeback_place), Some(writeback)) =
                    (writeback_root.as_ref(), pattern_writeback.as_ref())
                {
                    let updated = self.materialize_pattern_writeback(writeback);
                    self.emit(Instruction::Assign {
                        target: writeback_place.clone(),
                        value: Rvalue::Use(updated),
                    });
                }
                self.terminate(Terminator::Goto(self.label(next_block)));
                self.switch_to(selected);
            }
            if consumes_scrutinee {
                self.lower_consuming_pattern_bindings(
                    &arm.pattern,
                    scrutinee.clone(),
                    scrutinee_ty.as_ref(),
                );
            }
            let arm_type = self.infer_expr_type(&arm.value);
            let value = self.lower_expr_for_owned_value(&arm.value, arm_type.as_ref());
            self.emit(Instruction::Assign {
                target: result.clone(),
                value: Rvalue::Use(value),
            });
            let writeback_state = writeback_root
                .as_ref()
                .and_then(|_| self.match_writeback_stack.pop());
            if !self.current_terminated() {
                if let (Some(writeback_place), Some(writeback), Some(state)) = (
                    writeback_root.as_ref(),
                    pattern_writeback.as_ref(),
                    writeback_state.as_ref(),
                ) {
                    self.finish_match_arm_with_writeback(
                        after_block,
                        writeback_place,
                        writeback,
                        &state.skip_place,
                    );
                } else {
                    self.terminate(Terminator::Goto(self.label(after_block)));
                }
            }
            self.scoped_names.pop();
            next_case_block = next_block;
        }

        self.switch_to(after_block);
        Operand::Place(result)
    }

    fn materialize_pattern_writeback(&mut self, writeback: &PatternWriteback) -> Operand {
        match writeback {
            PatternWriteback::Use(operand) => operand.clone(),
            PatternWriteback::Or {
                ty,
                selected,
                alternatives,
            } => {
                debug_assert_eq!(selected.len(), alternatives.len());
                let result = self.new_typed_temp(ty.clone());
                let done = self.new_block("match_or_writeback_done");
                let mut dispatch = self.current_block;
                for (index, (flag, alternative)) in selected.iter().zip(alternatives).enumerate() {
                    self.switch_to(dispatch);
                    let apply = self.new_block("match_or_writeback_apply");
                    let next = if index + 1 == alternatives.len() {
                        apply
                    } else {
                        self.new_block("match_or_writeback_next")
                    };
                    if index + 1 == alternatives.len() {
                        self.terminate(Terminator::Goto(self.label(apply)));
                    } else {
                        self.terminate(Terminator::Branch {
                            condition: Operand::Place(flag.clone()),
                            then_label: self.label(apply),
                            else_label: self.label(next),
                        });
                    }
                    self.switch_to(apply);
                    let value = self.materialize_pattern_writeback(alternative);
                    self.emit(Instruction::Assign {
                        target: result.clone(),
                        value: Rvalue::Use(value),
                    });
                    self.terminate(Terminator::Goto(self.label(done)));
                    dispatch = next;
                }
                self.switch_to(done);
                Operand::Place(result)
            }
            PatternWriteback::Variant {
                ty,
                enum_name,
                variant_name,
                payloads,
            } => {
                let payloads = payloads
                    .iter()
                    .map(|payload| self.materialize_pattern_writeback(payload))
                    .collect::<Vec<_>>();
                let temp = self.new_typed_temp(ty.clone());
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::EnumVariant {
                        enum_name: enum_name.clone(),
                        variant_name: variant_name.clone(),
                        payloads,
                    },
                });
                Operand::Place(temp)
            }
        }
    }

    fn lower_logical_expr(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> Operand {
        let result = self.new_typed_temp(Type::named("bool"));
        let rhs_block = self.new_block("logic_rhs");
        let short_block = self.new_block("logic_short");
        let join_block = self.new_block("logic_join");
        let left_value = self.lower_expr(left);
        // A mutable match guard may perform a successful mutation in the
        // left operand before the right operand traps. Publish that mutation
        // before control advances so runtime failure cleanup observes the
        // reconstructed scrutinee.
        if !self.match_writeback_stack.is_empty() {
            self.emit_active_match_writebacks();
        }

        let (then_label, else_label) = match op {
            BinaryOp::And => (self.label(rhs_block), self.label(short_block)),
            BinaryOp::Or => (self.label(short_block), self.label(rhs_block)),
            _ => unreachable!("logical lowering only handles `and` / `or`"),
        };

        self.terminate(Terminator::Branch {
            condition: left_value,
            then_label,
            else_label,
        });

        self.switch_to(short_block);
        self.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Use(Operand::Bool(matches!(op, BinaryOp::Or))),
        });
        self.terminate(Terminator::Goto(self.label(join_block)));

        self.switch_to(rhs_block);
        let right_value = self.lower_expr(right);
        self.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Use(right_value),
        });
        self.terminate(Terminator::Goto(self.label(join_block)));

        self.switch_to(join_block);
        Operand::Place(result)
    }

    fn lower_conditional_expr(
        &mut self,
        then_expr: &Expr,
        condition_expr: &Expr,
        else_expr: &Expr,
        expected: Option<&Type>,
        arms_owned: bool,
    ) -> Operand {
        let result_ty = expected
            .cloned()
            .or_else(|| self.infer_conditional_result_type(then_expr, else_expr))
            .unwrap_or_else(|| Type::named("Unknown"));
        let result = self.new_typed_temp(result_ty.clone());
        let then_block = self.new_block("conditional_then");
        let else_block = self.new_block("conditional_else");
        let join_block = self.new_block("conditional_join");

        let condition =
            self.lower_expr_at_sequence_point(condition_expr, Some(&Type::named("bool")));
        self.terminate(Terminator::Branch {
            condition,
            then_label: self.label(then_block),
            else_label: self.label(else_block),
        });

        self.switch_to(then_block);
        let then_value = if arms_owned {
            self.lower_expr_for_owned_value(then_expr, Some(&result_ty))
        } else {
            self.lower_expr_with_expected(then_expr, Some(&result_ty))
        };
        self.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Use(then_value),
        });
        self.terminate(Terminator::Goto(self.label(join_block)));

        self.switch_to(else_block);
        let else_value = if arms_owned {
            self.lower_expr_for_owned_value(else_expr, Some(&result_ty))
        } else {
            self.lower_expr_with_expected(else_expr, Some(&result_ty))
        };
        self.emit(Instruction::Assign {
            target: result.clone(),
            value: Rvalue::Use(else_value),
        });
        self.terminate(Terminator::Goto(self.label(join_block)));

        self.switch_to(join_block);
        Operand::Place(result)
    }

    fn resolve_task_start_target(&self, callee: &Expr) -> Option<TaskStartTarget> {
        let (base_callee, callable_type_args) = self.task_callable_specialization(callee);
        match &base_callee.kind {
            ExprKind::Name(function) => self
                .program
                .functions
                .get(function)
                .filter(|_| {
                    !self
                        .local_types
                        .contains_key(&self.render_local_name(function))
                })
                .map(|function_info| {
                    let substitutions = self.task_explicit_type_substitutions(
                        &function_info.decl.type_params,
                        callable_type_args.as_deref(),
                    );
                    let param_contracts = task_param_contracts(
                        &function_info.decl.params,
                        &function_info.signature.params,
                        &function_info.signature.param_passings,
                    );
                    TaskStartTarget {
                        function_name: Some(function.clone()),
                        params: function_info.decl.params.clone(),
                        param_types: function_info.signature.params.clone(),
                        param_passings: function_info.signature.param_passings.clone(),
                        param_contracts,
                        return_type: function_info.signature.return_type.clone(),
                        type_params: function_info.decl.type_params.clone(),
                        substitutions,
                        display_name: format!("function `{}`", function),
                    }
                })
                .or_else(|| {
                    self.infer_expr_type(callee).and_then(|ty| match ty {
                        Type::Function {
                            params,
                            return_type,
                        } => Some(TaskStartTarget {
                            function_name: None,
                            params: Vec::new(),
                            param_types: params.iter().map(|param| param.ty.clone()).collect(),
                            param_passings: params.iter().map(|param| param.passing).collect(),
                            param_contracts: params,
                            return_type: *return_type,
                            type_params: Vec::new(),
                            substitutions: std::collections::HashMap::new(),
                            display_name: "function value".to_string(),
                        }),
                        Type::Closure {
                            params,
                            return_type,
                            ..
                        } => Some(TaskStartTarget {
                            function_name: None,
                            params: Vec::new(),
                            param_types: params.iter().map(|param| param.ty.clone()).collect(),
                            param_passings: params.iter().map(|param| param.passing).collect(),
                            param_contracts: *params,
                            return_type: *return_type,
                            type_params: Vec::new(),
                            substitutions: std::collections::HashMap::new(),
                            display_name: "function value".to_string(),
                        }),
                        _ => None,
                    })
                }),
            ExprKind::Member { object, field } => {
                let (base_object, object_type_args) = self.task_callable_specialization(object);
                if let Some((module_path, item_name)) = self.qualified_module_item(object) {
                    if let Some(namespace) = self.module_namespace(&module_path) {
                        if let Some(class_info) = namespace.classes.get(&item_name) {
                            if let Some(method) = class_info.methods.get(field) {
                                if method.decl.receiver.is_none() {
                                    let mut substitutions = self.task_explicit_type_substitutions(
                                        &class_info.decl.type_params,
                                        object_type_args.as_deref(),
                                    );
                                    substitutions.extend(self.task_explicit_type_substitutions(
                                        &method.decl.type_params,
                                        callable_type_args.as_deref(),
                                    ));
                                    let mut type_params = class_info.decl.type_params.clone();
                                    type_params.extend(method.decl.type_params.iter().cloned());
                                    let param_contracts = task_param_contracts(
                                        &method.decl.params,
                                        &method.signature.params,
                                        &method.signature.param_passings,
                                    );
                                    return Some(TaskStartTarget {
                                        function_name: Some(format!(
                                            "{}::{}.{}",
                                            module_path, item_name, field
                                        )),
                                        params: method.decl.params.clone(),
                                        param_types: method.signature.params.clone(),
                                        param_passings: method.signature.param_passings.clone(),
                                        param_contracts,
                                        return_type: method.signature.return_type.clone(),
                                        type_params,
                                        substitutions,
                                        display_name: format!("method `{}.{}`", item_name, field),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some((module_path, function_name)) = self.qualified_module_item(base_callee)
                {
                    if let Some(namespace) = self.module_namespace(&module_path) {
                        if let Some(function) = namespace
                            .functions
                            .get(&function_name)
                            .or_else(|| namespace.all_functions.get(&function_name))
                        {
                            let substitutions = self.task_explicit_type_substitutions(
                                &function.decl.type_params,
                                callable_type_args.as_deref(),
                            );
                            let param_contracts = task_param_contracts(
                                &function.decl.params,
                                &function.signature.params,
                                &function.signature.param_passings,
                            );
                            return Some(TaskStartTarget {
                                function_name: Some(imported_module_function_name(
                                    &module_path,
                                    &function_name,
                                )),
                                params: function.decl.params.clone(),
                                param_types: function.signature.params.clone(),
                                param_passings: function.signature.param_passings.clone(),
                                param_contracts,
                                return_type: function.signature.return_type.clone(),
                                type_params: function.decl.type_params.clone(),
                                substitutions,
                                display_name: format!(
                                    "function `{}.{}`",
                                    module_path, function_name
                                ),
                            });
                        }
                    }
                }
                if let ExprKind::Name(class_name) = &base_object.kind {
                    if let Some(class_info) = self.resolve_class_info(class_name) {
                        if let Some(method) = class_info.methods.get(field) {
                            if method.decl.receiver.is_none() {
                                let mut substitutions = self.task_explicit_type_substitutions(
                                    &class_info.decl.type_params,
                                    object_type_args.as_deref(),
                                );
                                substitutions.extend(self.task_explicit_type_substitutions(
                                    &method.decl.type_params,
                                    callable_type_args.as_deref(),
                                ));
                                let mut type_params = class_info.decl.type_params.clone();
                                type_params.extend(method.decl.type_params.iter().cloned());
                                let param_contracts = task_param_contracts(
                                    &method.decl.params,
                                    &method.signature.params,
                                    &method.signature.param_passings,
                                );
                                return Some(TaskStartTarget {
                                    function_name: Some(mir_class_method_name(
                                        self.program,
                                        class_info,
                                        field,
                                    )),
                                    params: method.decl.params.clone(),
                                    param_types: method.signature.params.clone(),
                                    param_passings: method.signature.param_passings.clone(),
                                    param_contracts,
                                    return_type: method.signature.return_type.clone(),
                                    type_params,
                                    substitutions,
                                    display_name: format!("method `{}.{}`", class_name, field),
                                });
                            }
                        }
                    }
                }
                self.infer_expr_type(callee).and_then(|ty| match ty {
                    Type::Function {
                        params,
                        return_type,
                    } => Some(TaskStartTarget {
                        function_name: None,
                        params: Vec::new(),
                        param_types: params.iter().map(|param| param.ty.clone()).collect(),
                        param_passings: params.iter().map(|param| param.passing).collect(),
                        param_contracts: params,
                        return_type: *return_type,
                        type_params: Vec::new(),
                        substitutions: std::collections::HashMap::new(),
                        display_name: "function value".to_string(),
                    }),
                    Type::Closure {
                        params,
                        return_type,
                        ..
                    } => Some(TaskStartTarget {
                        function_name: None,
                        params: Vec::new(),
                        param_types: params.iter().map(|param| param.ty.clone()).collect(),
                        param_passings: params.iter().map(|param| param.passing).collect(),
                        param_contracts: *params,
                        return_type: *return_type,
                        type_params: Vec::new(),
                        substitutions: std::collections::HashMap::new(),
                        display_name: "function value".to_string(),
                    }),
                    _ => None,
                })
            }
            _ => self.infer_expr_type(callee).and_then(|ty| match ty {
                Type::Function {
                    params,
                    return_type,
                } => Some(TaskStartTarget {
                    function_name: None,
                    params: Vec::new(),
                    param_types: params.iter().map(|param| param.ty.clone()).collect(),
                    param_passings: params.iter().map(|param| param.passing).collect(),
                    param_contracts: params,
                    return_type: *return_type,
                    type_params: Vec::new(),
                    substitutions: std::collections::HashMap::new(),
                    display_name: "function value".to_string(),
                }),
                Type::Closure {
                    params,
                    return_type,
                    ..
                } => Some(TaskStartTarget {
                    function_name: None,
                    params: Vec::new(),
                    param_types: params.iter().map(|param| param.ty.clone()).collect(),
                    param_passings: params.iter().map(|param| param.passing).collect(),
                    param_contracts: *params,
                    return_type: *return_type,
                    type_params: Vec::new(),
                    substitutions: std::collections::HashMap::new(),
                    display_name: "function value".to_string(),
                }),
                _ => None,
            }),
        }
    }

    /// Task targets are expressions rather than ordinary calls, so the parser
    /// cannot use its call-suffix lookahead to distinguish `worker[T]` from an
    /// indexed read. Semantic checking has already established that this
    /// expression names a spawnable callable; reinterpret that one contextual
    /// shape here while leaving every ordinary index expression unchanged.
    fn task_callable_specialization<'b>(&self, callee: &'b Expr) -> (&'b Expr, Option<Vec<Type>>) {
        match &callee.kind {
            ExprKind::Specialize { expr, type_args } => (
                expr,
                Some(
                    type_args
                        .iter()
                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                        .collect(),
                ),
            ),
            ExprKind::Index { object, index } => {
                let type_args = self.task_type_args_from_index_expr(index);
                match type_args {
                    Some(type_args) => (object, Some(type_args)),
                    None => (callee, None),
                }
            }
            _ => (callee, None),
        }
    }

    fn task_type_args_from_index_expr(&self, expr: &Expr) -> Option<Vec<Type>> {
        match &expr.kind {
            ExprKind::Tuple(elements) => elements
                .iter()
                .map(|element| self.task_type_from_expr(element))
                .collect(),
            _ => self.task_type_from_expr(expr).map(|ty| vec![ty]),
        }
    }

    fn task_type_from_expr(&self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Name(name) => Some(self.lower_type_ref_with_provenance(
                &crate::ast::TypeRef::named(name, Vec::new(), false, expr.span),
            )),
            ExprKind::Member { .. } => {
                let name = self.render_expr_place(expr);
                Some(
                    self.lower_type_ref_with_provenance(&crate::ast::TypeRef::named(
                        name,
                        Vec::new(),
                        false,
                        expr.span,
                    )),
                )
            }
            ExprKind::Index { object, index } => {
                let name = self.render_expr_place(object);
                let args = self.task_type_args_from_index_expr(index)?;
                Some(Type::Named(name, args))
            }
            ExprKind::Tuple(elements) => elements
                .iter()
                .map(|element| self.task_type_from_expr(element))
                .collect::<Option<Vec<_>>>()
                .map(Type::Tuple),
            ExprKind::Group(inner) => self.task_type_from_expr(inner),
            _ => None,
        }
    }

    fn task_explicit_type_substitutions(
        &self,
        type_params: &[String],
        type_args: Option<&[Type]>,
    ) -> std::collections::HashMap<String, Type> {
        type_args
            .map(|type_args| substitutions_from_decl_type_args(type_params, type_args))
            .unwrap_or_default()
    }

    fn specialize_task_start_target(
        &self,
        mut target: TaskStartTarget,
        args: &[Argument],
        span: Span,
    ) -> TaskStartTarget {
        if target.function_name.is_none() {
            return target;
        }
        let ordered_args = bind_call_arguments(
            &target.display_name,
            &callable_params_from_decl(&target.params),
            args,
            span,
            CallConvention::PositionalOrNamed,
        )
        .expect("checked task-start arguments should bind during MIR lowering");
        let type_params = target.type_params.iter().cloned().collect::<BTreeSet<_>>();

        // Match semantic inference: concrete non-literal evidence wins before
        // integer literals get their standalone int64 default. Defaults also
        // participate, including explicit specializations whose only mention
        // of a type parameter is in a contextual default.
        for literal_pass in [false, true] {
            for ((argument, param), param_type) in ordered_args
                .iter()
                .zip(&target.params)
                .zip(&target.param_types)
            {
                if target
                    .type_params
                    .iter()
                    .all(|name| target.substitutions.contains_key(name))
                {
                    break;
                }
                let value = match argument {
                    Some(argument) => &argument.value,
                    None => match param.default.as_ref() {
                        Some(default) if !matches!(default.kind, ExprKind::BuiltinOmitted) => {
                            default
                        }
                        _ => continue,
                    },
                };
                if is_integer_literal_expr(value) != literal_pass {
                    continue;
                }
                let Some(actual) = self.infer_expr_type(value) else {
                    continue;
                };
                let _ = crate::sema::type_pattern_matches(
                    param_type,
                    &actual,
                    &type_params,
                    &mut target.substitutions,
                );
            }
        }

        target.param_types = target
            .param_types
            .iter()
            .map(|param| substitute_type(param, &target.substitutions))
            .collect();
        target.param_contracts =
            task_param_contracts(&target.params, &target.param_types, &target.param_passings);
        target.return_type = substitute_type(&target.return_type, &target.substitutions);
        target
    }

    fn lower_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Argument],
        expected: Option<&Type>,
    ) -> Operand {
        let temp = expected
            .cloned()
            .map(|ty| self.new_typed_temp(ty))
            .unwrap_or_else(|| self.new_temp_for_expr(expr));
        let (base_callee, explicit_type_args) = match &callee.kind {
            ExprKind::Specialize { expr, type_args } => (&**expr, Some(type_args.as_slice())),
            _ => (callee, None),
        };

        let direct_decl_callee = match &base_callee.kind {
            ExprKind::Name(name) => {
                !self.local_types.contains_key(&self.render_local_name(name))
                    && (self.resolve_function_info(name).is_some()
                        || self.resolve_extern_function_info(name).is_some())
            }
            ExprKind::Member { object, field } => self
                .infer_module_path(object)
                .and_then(|module_path| self.module_namespace(&module_path))
                .is_some_and(|namespace| {
                    namespace.functions.contains_key(field)
                        || namespace.all_functions.contains_key(field)
                        || namespace.extern_functions.contains_key(field)
                }),
            _ => false,
        };
        if !direct_decl_callee {
            if let Some(params) = self.infer_expr_type(callee).and_then(|ty| match ty {
                Type::Function { params, .. } => Some(params),
                Type::Closure { params, .. } => Some(*params),
                _ => None,
            }) {
                let function = self.lower_expr_at_sequence_point(callee, None);
                let args = self.lower_function_value_args(args, &params, callee.span);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Value(function),
                        args,
                    },
                });
                return Operand::Place(temp);
            }
        }

        match &base_callee.kind {
            // `len` and `str` are spelled as free calls but defined by
            // delegation, so they lower to the member call and the rendering
            // the language already has rather than to new runtime entry points.
            ExprKind::Name(name)
                if (name == "len" || name == "str")
                    && !self.program.functions.contains_key(name)
                    && self.resolve_class_info(name).is_none() =>
            {
                let argument = args
                    .first()
                    .expect("`len` and `str` bind exactly one argument before lowering");
                if name == "len" {
                    let object = self.lower_expr_at_sequence_point(&argument.value, None);
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Call {
                            callee: CallTarget::Member {
                                object,
                                field: "len".to_string(),
                                receiver_place: self.render_place_expr_option(&argument.value),
                            },
                            args: Vec::new(),
                        },
                    });
                } else {
                    let value = self.lower_expr_at_sequence_point(&argument.value, None);
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::FormatString {
                            parts: vec![MirFormatPart::Value(value)],
                        },
                    });
                }
            }
            ExprKind::Name(name) if self.resolve_class_info(name).is_some() => {
                let class = self
                    .resolve_class_info(name)
                    .expect("class should exist during MIR lowering")
                    .clone();
                if let Some(constructor) = class.builtin_constructor() {
                    let lowered_args =
                        self.lower_builtin_class_constructor_args(constructor, args, callee.span);
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Call {
                            callee: CallTarget::Name(imported_module_function_name(
                                &class.module_name,
                                &class.decl.name,
                            )),
                            args: lowered_args,
                        },
                    });
                    return Operand::Place(temp);
                }
                let constructor_type = expected
                    .filter(|expected| {
                        matches!(expected, Type::Named(expected_name, _) if expected_name == name)
                    })
                    .cloned()
                    .or_else(|| {
                        self.infer_class_constructor_type(name, args, explicit_type_args)
                    });
                let substitutions = match constructor_type {
                    Some(Type::Named(_, type_args)) => {
                        substitutions_from_decl_type_args(&class.decl.type_params, &type_args)
                    }
                    _ => std::collections::HashMap::new(),
                };
                let field_names = class
                    .decl
                    .fields
                    .iter()
                    .map(|field| field.name.clone())
                    .collect::<Vec<_>>();
                let mut next_positional_field = 0usize;
                let mut saw_named = false;
                let mut provided = std::collections::BTreeMap::<String, Operand>::new();
                for argument in args {
                    let field_name = if let Some(field_name) = argument.name.as_ref() {
                        saw_named = true;
                        field_name.clone()
                    } else {
                        assert!(
                            !saw_named,
                            "positional class constructor arguments must come before named arguments"
                        );
                        let field_name = field_names
                            .get(next_positional_field)
                            .expect("class constructor should have enough fields")
                            .clone();
                        next_positional_field += 1;
                        field_name
                    };
                    let field_type = class
                        .fields
                        .get(&field_name)
                        .map(|field| substitute_type(&field.ty, &substitutions));
                    let value =
                        self.lower_expr_for_owned_value(&argument.value, field_type.as_ref());
                    if let Some(field_decl) = class
                        .decl
                        .fields
                        .iter()
                        .find(|field| field.name == field_name)
                    {
                        let field_type = substitute_type(
                            &class
                                .fields
                                .get(&field_decl.name)
                                .expect("checked class field should have a semantic type")
                                .ty,
                            &substitutions,
                        );
                        self.retarget_operand_place(&value, &field_type);
                    }
                    provided.insert(field_name, value);
                }
                let fields = class
                    .decl
                    .fields
                    .iter()
                    .filter_map(|field| {
                        if let Some(value) = provided.get(&field.name) {
                            Some(MirFieldInit {
                                name: field.name.clone(),
                                value: value.clone(),
                            })
                        } else {
                            field.default.as_ref().map(|default| {
                                let field_type = substitute_type(
                                    &class
                                        .fields
                                        .get(&field.name)
                                        .expect("checked class field should have a semantic type")
                                        .ty,
                                    &substitutions,
                                );
                                let value = self.lower_field_default(
                                    default,
                                    &field_type,
                                    &class.module_name,
                                );
                                self.retarget_operand_place(&value, &field_type);
                                MirFieldInit {
                                    name: field.name.clone(),
                                    value,
                                }
                            })
                        }
                    })
                    .collect::<Vec<_>>();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Construct {
                        class_name: mir_runtime_class_name(self.program, &class),
                        fields,
                    },
                });
            }
            ExprKind::Name(name) if name == "Queue" => {
                let lowered_args = self.lower_args(args);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name("Queue".to_string()),
                        args: lowered_args,
                    },
                });
            }
            ExprKind::Name(name) if name == "TaskGroup" => {
                let lowered_args = self.lower_args(args);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name("TaskGroup".to_string()),
                        args: lowered_args,
                    },
                });
            }
            ExprKind::Name(name)
                if matches!(
                    name.as_str(),
                    "Some"
                        | "None"
                        | "Ok"
                        | "Err"
                        | "Closed"
                        | "Cancelled"
                        | "TimedOut"
                        | "Full"
                        | "Item"
                        | "Ready"
                ) =>
            {
                let inferred_type = self.infer_expr_type(expr);
                let enum_type = expected.or(inferred_type.as_ref());
                let enum_name = match enum_type {
                    Some(Type::Named(enum_name, _)) => enum_name.clone(),
                    _ => match name.as_str() {
                        "Some" | "None" => "Option".to_string(),
                        "Ok" | "Err" => "Result".to_string(),
                        "Closed" | "Cancelled" | "TimedOut" | "Full" => "SendError".to_string(),
                        "Item" => "QueueReceive".to_string(),
                        "Ready" => "TaskResult".to_string(),
                        _ => unreachable!(),
                    },
                };
                let payloads =
                    self.lower_enum_variant_payloads(expr, &enum_name, name, args, enum_type);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::EnumVariant {
                        enum_name,
                        variant_name: name.clone(),
                        payloads,
                    },
                });
            }
            ExprKind::Name(name)
                if name == "list"
                    && matches!(&callee.kind, ExprKind::Specialize { .. })
                    && args.is_empty() =>
            {
                let ExprKind::Specialize { type_args, .. } = &callee.kind else {
                    unreachable!();
                };
                let element_type = type_args
                    .first()
                    .map(|ty| self.lower_type_ref_with_provenance(ty))
                    .unwrap_or_else(|| Type::named("Unknown"));
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::VecLiteral {
                        elements: Vec::new(),
                        element_type,
                    },
                });
            }
            ExprKind::Name(name)
                if name == "dict"
                    && matches!(&callee.kind, ExprKind::Specialize { .. })
                    && args.is_empty() =>
            {
                let ExprKind::Specialize { type_args, .. } = &callee.kind else {
                    unreachable!();
                };
                let key_type = type_args
                    .first()
                    .map(|ty| self.lower_type_ref_with_provenance(ty))
                    .unwrap_or_else(|| Type::named("Unknown"));
                let value_type = type_args
                    .get(1)
                    .map(|ty| self.lower_type_ref_with_provenance(ty))
                    .unwrap_or_else(|| Type::named("Unknown"));
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::MapLiteral {
                        entries: Vec::new(),
                        key_type,
                        value_type,
                    },
                });
            }
            ExprKind::Member { object, field } => {
                let base_object = match &object.kind {
                    ExprKind::Specialize { expr, .. } => &**expr,
                    _ => object,
                };
                if let ExprKind::Name(type_name) = &base_object.kind {
                    if self.infer_expr_type(base_object).is_none() {
                        if let Some(associated) =
                            BuiltinAssociatedFunction::resolve(type_name, field)
                        {
                            let ordered_args = associated.bind_args(args, callee.span).expect(
                                "checked builtin associated call should bind during MIR lowering",
                            );
                            let array_element_type = self
                                .infer_expr_type(expr)
                                .or_else(|| expected.cloned())
                                .and_then(|ty| match ty {
                                    Type::Named(name, mut arguments)
                                        if name == "Array" && arguments.len() == 1 =>
                                    {
                                        arguments.pop()
                                    }
                                    _ => None,
                                })
                                .unwrap_or_else(|| Type::named("Unknown"));
                            let expected_types = match associated {
                                BuiltinAssociatedFunction::DurationMilliseconds
                                | BuiltinAssociatedFunction::DurationSeconds
                                | BuiltinAssociatedFunction::DurationMinutes => {
                                    vec![Type::named("int64")]
                                }
                                BuiltinAssociatedFunction::StringFromBytes => {
                                    vec![Type::Named(
                                        "list".to_string(),
                                        vec![Type::named("uint8")],
                                    )]
                                }
                                BuiltinAssociatedFunction::ArrayZeros => {
                                    vec![Type::Named(
                                        "list".to_string(),
                                        vec![Type::named("int64")],
                                    )]
                                }
                                BuiltinAssociatedFunction::ArrayFull => vec![
                                    Type::Named("list".to_string(), vec![Type::named("int64")]),
                                    array_element_type,
                                ],
                                BuiltinAssociatedFunction::ArrayFromVec => vec![
                                    Type::Named("list".to_string(), vec![array_element_type]),
                                    Type::Named("list".to_string(), vec![Type::named("int64")]),
                                ],
                                BuiltinAssociatedFunction::ListWithCapacity
                                | BuiltinAssociatedFunction::DictWithCapacity
                                | BuiltinAssociatedFunction::SetWithCapacity => {
                                    vec![Type::named("int64")]
                                }
                            };
                            let mut lowered_by_param = vec![None::<MirArg>; ordered_args.len()];
                            for argument in args {
                                let index = ordered_args
                                    .iter()
                                    .position(|bound| {
                                        matches!(
                                            bound,
                                            Some(bound) if std::ptr::eq(*bound, argument)
                                        )
                                    })
                                    .expect(
                                        "bound associated argument should retain its declaration slot",
                                    );
                                let passing = associated
                                    .argument_passing(index)
                                    .expect("builtin associated argument should declare passing");
                                let value = self.lower_expr_for_passing(
                                    &argument.value,
                                    expected_types.get(index),
                                    passing,
                                );
                                lowered_by_param[index] = Some(MirArg {
                                    name: argument.name.clone(),
                                    value,
                                    writeback_place: None,
                                });
                            }
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::Call {
                                    callee: CallTarget::Name(format!(
                                        "{}.{}",
                                        associated.owner_name(),
                                        associated.name()
                                    )),
                                    args: lowered_by_param
                                        .into_iter()
                                        .map(|argument| {
                                            argument.expect(
                                                "checked associated call fills every argument",
                                            )
                                        })
                                        .collect(),
                                },
                            });
                            return Operand::Place(temp);
                        }
                    }
                }
                if field == "to_bytes"
                    && self.infer_expr_type(object).as_ref() == Some(&Type::named("str"))
                {
                    let value = self.lower_expr_for_passing(
                        object,
                        Some(&Type::named("str")),
                        ReceiverKind::Borrow,
                    );
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Call {
                            callee: CallTarget::Name("str.to_bytes".to_string()),
                            args: vec![MirArg {
                                name: None,
                                value,
                                writeback_place: None,
                            }],
                        },
                    });
                    return Operand::Place(temp);
                }
                if let Some((module_path, item_name)) = self.qualified_module_item(object) {
                    if let Some(namespace) = self.module_namespace(&module_path) {
                        if let Some(class) = namespace.classes.get(&item_name).cloned() {
                            if let Some(method) = class
                                .methods
                                .get(field)
                                .filter(|method| method.decl.receiver.is_none())
                            {
                                let lowered_args = self.lower_user_args(
                                    &format!("method `{}`", field),
                                    &method.decl.params,
                                    args,
                                    callee.span,
                                    Some(&method.signature.param_passings),
                                );
                                self.emit(Instruction::Assign {
                                    target: temp.clone(),
                                    value: Rvalue::Call {
                                        callee: CallTarget::Name(format!(
                                            "{}::{}.{}",
                                            module_path, item_name, field
                                        )),
                                        args: lowered_args,
                                    },
                                });
                                return Operand::Place(temp);
                            }
                        }
                        if let Some(enum_info) = namespace.enums.get(&item_name).cloned() {
                            let enum_name = mir_runtime_enum_name(self.program, &enum_info);
                            let payloads = self.lower_enum_variant_payloads(
                                expr, &enum_name, field, args, expected,
                            );
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::EnumVariant {
                                    enum_name,
                                    variant_name: field.clone(),
                                    payloads,
                                },
                            });
                            return Operand::Place(temp);
                        }
                    }
                }
                if let Some(module_path) = self.infer_module_path(object) {
                    if let Some(namespace) = self.module_namespace(&module_path) {
                        if let Some(function) = namespace.extern_functions.get(field).cloned() {
                            let lowered_args = self.lower_user_args_with_types(
                                &format!("extern function `{}`", function.decl.name),
                                &function.decl.params,
                                args,
                                callee.span,
                                Some(&function.signature.params),
                                Some(&function.signature.param_passings),
                            );
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::Call {
                                    callee: CallTarget::Extern(Self::extern_call_target(&function)),
                                    args: lowered_args,
                                },
                            });
                            return Operand::Place(temp);
                        }
                        if let Some(constructor) = namespace
                            .classes
                            .get(field)
                            .and_then(crate::sema::ClassInfo::builtin_constructor)
                        {
                            let lowered_args = self.lower_builtin_class_constructor_args(
                                constructor,
                                args,
                                callee.span,
                            );
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::Call {
                                    callee: CallTarget::Name(imported_module_function_name(
                                        &module_path,
                                        field,
                                    )),
                                    args: lowered_args,
                                },
                            });
                            return Operand::Place(temp);
                        }
                        if let Some(function) = namespace.functions.get(field).cloned() {
                            if module_path == "control" && field == "retry" {
                                self.lower_control_retry_call(
                                    expr,
                                    &function,
                                    args,
                                    explicit_type_args,
                                    &temp,
                                );
                                return Operand::Place(temp);
                            }
                            let lowered_args = self.lower_user_args(
                                &format!("function `{}`", function.decl.name),
                                &function.decl.params,
                                args,
                                callee.span,
                                Some(&function.signature.param_passings),
                            );
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::Call {
                                    callee: CallTarget::Name(imported_module_function_name(
                                        &module_path,
                                        field,
                                    )),
                                    args: lowered_args,
                                },
                            });
                            return Operand::Place(temp);
                        }
                        if let Some(class) = namespace.classes.get(field).cloned() {
                            let runtime_class_name = mir_runtime_class_name(self.program, &class);
                            let qualified_class_name = format!("{}.{}", module_path, field);
                            let constructor_type = expected
                                .filter(|expected| {
                                    matches!(expected, Type::Named(expected_name, _) if expected_name == &runtime_class_name)
                                })
                                .cloned()
                                .or_else(|| {
                                    self.infer_class_constructor_type(
                                        &qualified_class_name,
                                        args,
                                        explicit_type_args,
                                    )
                                });
                            let substitutions = match constructor_type {
                                Some(Type::Named(_, type_args)) => {
                                    substitutions_from_decl_type_args(
                                        &class.decl.type_params,
                                        &type_args,
                                    )
                                }
                                _ => std::collections::HashMap::new(),
                            };
                            let field_names = class
                                .decl
                                .fields
                                .iter()
                                .map(|field_decl| field_decl.name.clone())
                                .collect::<Vec<_>>();
                            let mut next_positional_field = 0usize;
                            let mut saw_named = false;
                            let mut provided = std::collections::BTreeMap::<String, Operand>::new();
                            for argument in args {
                                let field_name = if let Some(field_name) = argument.name.as_ref() {
                                    saw_named = true;
                                    field_name.clone()
                                } else {
                                    assert!(
                                        !saw_named,
                                        "positional class constructor arguments must come before named arguments"
                                    );
                                    let field_name = field_names
                                        .get(next_positional_field)
                                        .expect("class constructor should have enough fields")
                                        .clone();
                                    next_positional_field += 1;
                                    field_name
                                };
                                let field_type = class
                                    .fields
                                    .get(&field_name)
                                    .map(|field| substitute_type(&field.ty, &substitutions));
                                let value = self.lower_expr_for_owned_value(
                                    &argument.value,
                                    field_type.as_ref(),
                                );
                                if let Some(field_decl) = class
                                    .decl
                                    .fields
                                    .iter()
                                    .find(|field_decl| field_decl.name == field_name)
                                {
                                    let field_type = substitute_type(
                                        &class
                                            .fields
                                            .get(&field_decl.name)
                                            .expect(
                                                "checked class field should have a semantic type",
                                            )
                                            .ty,
                                        &substitutions,
                                    );
                                    self.retarget_operand_place(&value, &field_type);
                                }
                                provided.insert(field_name, value);
                            }
                            let fields = class
                                .decl
                                .fields
                                .iter()
                                .filter_map(|field_decl| {
                                    if let Some(value) = provided.get(&field_decl.name) {
                                        Some(MirFieldInit {
                                            name: field_decl.name.clone(),
                                            value: value.clone(),
                                        })
                                    } else {
                                        field_decl.default.as_ref().map(|default| {
                                            let field_type = substitute_type(
                                                &class
                                                    .fields
                                                    .get(&field_decl.name)
                                                    .expect(
                                                        "checked class field should have a semantic type",
                                                    )
                                                    .ty,
                                                &substitutions,
                                            );
                                            let value = self.lower_field_default(
                                                default,
                                                &field_type,
                                                &class.module_name,
                                            );
                                            self.retarget_operand_place(&value, &field_type);
                                            MirFieldInit {
                                                name: field_decl.name.clone(),
                                                value,
                                            }
                                        })
                                    }
                                })
                                .collect::<Vec<_>>();
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::Construct {
                                    class_name: runtime_class_name,
                                    fields,
                                },
                            });
                            return Operand::Place(temp);
                        }
                    }
                }

                if matches!(
                    field.as_str(),
                    "start" | "start_soon" | "start_with_stack" | "start_soon_with_stack"
                ) && matches!(
                    self.infer_expr_type(object),
                    Some(Type::Named(ref name, ref args))
                        if name == "TaskGroup" && args.is_empty()
                ) {
                    let has_stack_override =
                        matches!(field.as_str(), "start_with_stack" | "start_soon_with_stack");
                    let target_index = usize::from(has_stack_override);
                    let first_target_arg = target_index + 1;
                    let target = self
                        .resolve_task_start_target(&args[target_index].value)
                        .expect("task-group start should lower from a supported callable target");
                    let target = self.specialize_task_start_target(
                        target,
                        &args[first_target_arg..],
                        callee.span,
                    );
                    let function_type = target.function_type();
                    let function = match target.function_name.as_ref() {
                        Some(name) => Operand::Function {
                            name: name.clone(),
                            signature: Box::new(function_type.clone()),
                        },
                        None => self.lower_expr_at_sequence_point(
                            &args[target_index].value,
                            Some(&function_type),
                        ),
                    };
                    let group = self.lower_expr_at_sequence_point(object, None);
                    let stack_size = has_stack_override.then(|| {
                        self.lower_expr_at_sequence_point(
                            &args[0].value,
                            Some(&Type::named("int64")),
                        )
                    });
                    // A task capture outlives this call expression. Copy values are
                    // snapshotted and non-copy values are transferred into the task,
                    // regardless of whether the eventual target parameter is a shared
                    // borrow or an owning parameter.
                    let capture_contracts = target
                        .param_contracts
                        .iter()
                        .cloned()
                        .map(|mut param| {
                            param.passing = ReceiverKind::Value;
                            param
                        })
                        .collect::<Vec<_>>();
                    let lowered_args = self.lower_function_value_args(
                        &args[first_target_arg..],
                        &capture_contracts,
                        callee.span,
                    );
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::StartTask {
                            returns_handle: matches!(field.as_str(), "start" | "start_with_stack"),
                            result_is_copy: type_is_copy_in_program(
                                &target.return_type,
                                self.program,
                            ),
                            stack_size,
                            task_group: group,
                            function,
                            args: lowered_args,
                            span: expr.span,
                        },
                    });
                    return Operand::Place(temp);
                }

                if let ExprKind::Name(class_name) = &base_object.kind {
                    if let Some(class) = self.resolve_class_info(class_name).cloned() {
                        if let Some(method) = class
                            .methods
                            .get(field)
                            .filter(|method| method.decl.receiver.is_none())
                        {
                            let lowered_args = self.lower_user_args(
                                &format!("method `{}`", field),
                                &method.decl.params,
                                args,
                                callee.span,
                                Some(&method.signature.param_passings),
                            );
                            self.emit(Instruction::Assign {
                                target: temp.clone(),
                                value: Rvalue::Call {
                                    callee: CallTarget::Name(mir_class_method_name(
                                        self.program,
                                        &class,
                                        field,
                                    )),
                                    args: lowered_args,
                                },
                            });
                            return Operand::Place(temp);
                        }
                    }
                    if let Some((function_name, params, param_passings)) = self
                        .trait_impl_method_for_class_name(class_name, field)
                        .filter(|(_, method)| method.decl.receiver.is_none())
                        .map(|(trait_impl, method)| {
                            (
                                format!(
                                    "{}{} for {}.{}",
                                    trait_impl.trait_name,
                                    format_trait_args(&trait_impl.trait_args),
                                    trait_impl.for_type,
                                    field
                                ),
                                method.decl.params.clone(),
                                method.signature.param_passings.clone(),
                            )
                        })
                    {
                        let lowered_args = self.lower_user_args(
                            &format!("method `{}`", field),
                            &params,
                            args,
                            callee.span,
                            Some(&param_passings),
                        );
                        self.emit(Instruction::Assign {
                            target: temp.clone(),
                            value: Rvalue::Call {
                                callee: CallTarget::Name(function_name),
                                args: lowered_args,
                            },
                        });
                        return Operand::Place(temp);
                    }
                }

                if let ExprKind::Name(enum_name) = &base_object.kind {
                    if is_known_enum_name(self.program, enum_name)
                        || self.resolve_enum_info(enum_name).is_some()
                    {
                        let runtime_enum_name = self
                            .resolve_enum_info(enum_name)
                            .map(|enum_info| mir_runtime_enum_name(self.program, enum_info))
                            .unwrap_or_else(|| enum_name.clone());
                        let inferred_type = self.infer_expr_type(expr);
                        let enum_type = expected.or(inferred_type.as_ref());
                        let payloads = self.lower_enum_variant_payloads(
                            expr,
                            &runtime_enum_name,
                            field,
                            args,
                            enum_type,
                        );
                        self.emit(Instruction::Assign {
                            target: temp.clone(),
                            value: Rvalue::EnumVariant {
                                enum_name: runtime_enum_name,
                                variant_name: field.clone(),
                                payloads,
                            },
                        });
                        return Operand::Place(temp);
                    }
                }

                if self.lower_vec_algorithm_call(expr, object, field, args, &temp) {
                    return Operand::Place(temp);
                }

                let receiver_place = self.render_place_expr_option(object);
                let receiver_type = self.infer_expr_type(object);
                let consumes_task_observation = matches!(
                    receiver_type.as_ref(),
                    Some(Type::Named(name, args))
                        if name == "Task"
                            && args.len() == 1
                            && matches!(
                                field.as_str(),
                                "result" | "result_or_none" | "result_or"
                            )
                            && !type_is_copy_in_program(
                                &Type::Named(name.clone(), args.clone()),
                                self.program,
                            )
                );
                let lowered_object = if consumes_task_observation {
                    self.lower_expr_for_owned_value(object, receiver_type.as_ref())
                } else if let Some(passing) = self.user_member_receiver_passing(object, field) {
                    self.lower_expr_for_passing(object, None, passing)
                } else {
                    self.lower_expr_at_sequence_point(object, None)
                };
                let lowered_args = self.lower_member_call_args(callee.span, object, field, args);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Member {
                            object: lowered_object,
                            field: field.clone(),
                            receiver_place,
                        },
                        args: lowered_args,
                    },
                });
            }
            ExprKind::Name(name) if name == "range" => {
                let lowered_args = args
                    .iter()
                    .map(|argument| MirArg {
                        name: argument.name.clone(),
                        value: self.lower_index_domain_expr(&argument.value),
                        writeback_place: None,
                    })
                    .collect();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name.clone()),
                        args: lowered_args,
                    },
                });
            }
            ExprKind::Name(name) if name == "select" => {
                let lowered_args = args
                    .iter()
                    .map(|argument| {
                        let source_type = self.infer_expr_type(&argument.value);
                        let consumes_task_observation = matches!(
                            source_type.as_ref(),
                            Some(Type::Named(task, result))
                                if task == "Task"
                                    && result.len() == 1
                                    && !type_is_copy_in_program(
                                        &Type::Named(task.clone(), result.clone()),
                                        self.program,
                                    )
                        );
                        MirArg {
                            name: argument.name.clone(),
                            value: if consumes_task_observation {
                                self.lower_expr_for_owned_value(
                                    &argument.value,
                                    source_type.as_ref(),
                                )
                            } else {
                                self.lower_expr_at_sequence_point(&argument.value, None)
                            },
                            writeback_place: None,
                        }
                    })
                    .collect();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name.clone()),
                        args: lowered_args,
                    },
                });
            }
            ExprKind::Name(name) if matches!(name.as_str(), "wait_any" | "wait_all") => {
                let builtin = BuiltinFunction::from_name(name)
                    .expect("wait builtins should have maintained call metadata");
                let ordered_args = builtin
                    .bind_args(args, callee.span)
                    .expect("checked wait arguments should bind during MIR lowering");
                let tasks_argument =
                    ordered_args[0].expect("checked wait call should provide tasks");
                let tasks_type = self.infer_expr_type(&tasks_argument.value);
                let consumes_tasks = matches!(
                    tasks_type.as_ref(),
                    Some(Type::Named(container, elements))
                        if container == "list"
                            && matches!(
                                elements.as_slice(),
                                [Type::Named(task, result)]
                                    if task == "Task"
                                        && result.len() == 1
                                        && !type_is_copy_in_program(
                                            &Type::Named(task.clone(), result.clone()),
                                            self.program,
                                        )
                            )
                );
                let lowered_args = args
                    .iter()
                    .map(|argument| {
                        let is_tasks = std::ptr::eq(argument, tasks_argument);
                        MirArg {
                            name: argument.name.clone(),
                            value: if is_tasks && consumes_tasks {
                                self.lower_expr_for_owned_value(
                                    &argument.value,
                                    tasks_type.as_ref(),
                                )
                            } else {
                                self.lower_expr_at_sequence_point(&argument.value, None)
                            },
                            writeback_place: None,
                        }
                    })
                    .collect();
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name(name.clone()),
                        args: lowered_args,
                    },
                });
            }
            ExprKind::Name(name) => {
                if let Some(function) = self.resolve_extern_function_info(name).cloned() {
                    let lowered_args = self.lower_user_args_with_types(
                        &format!("extern function `{}`", function.decl.name),
                        &function.decl.params,
                        args,
                        callee.span,
                        Some(&function.signature.params),
                        Some(&function.signature.param_passings),
                    );
                    self.emit(Instruction::Assign {
                        target: temp.clone(),
                        value: Rvalue::Call {
                            callee: CallTarget::Extern(Self::extern_call_target(&function)),
                            args: lowered_args,
                        },
                    });
                    return Operand::Place(temp);
                }
                let resolved_function = self.resolve_function_info(name).cloned();
                let contextual_param_types = resolved_function.as_ref().map(|function_info| {
                    let substitutions = match &callee.kind {
                        ExprKind::Specialize { type_args, .. } => {
                            let type_args = type_args
                                .iter()
                                .map(|ty| self.lower_type_ref_with_provenance(ty))
                                .collect::<Vec<_>>();
                            substitutions_from_decl_type_args(
                                &function_info.decl.type_params,
                                &type_args,
                            )
                        }
                        _ => {
                            let mut substitutions = std::collections::HashMap::new();
                            if let Some(expected) = expected {
                                let type_params = function_info
                                    .decl
                                    .type_params
                                    .iter()
                                    .cloned()
                                    .collect::<BTreeSet<_>>();
                                let _ = crate::sema::type_pattern_matches(
                                    &function_info.signature.return_type,
                                    expected,
                                    &type_params,
                                    &mut substitutions,
                                );
                            }
                            let ordered_args = bind_call_arguments(
                                name,
                                &callable_params_from_decl(&function_info.decl.params),
                                args,
                                callee.span,
                                CallConvention::PositionalOrNamed,
                            )
                            .expect("type-checked generic call should bind during MIR lowering");
                            let type_params = function_info
                                .decl
                                .type_params
                                .iter()
                                .cloned()
                                .collect::<BTreeSet<_>>();
                            // Infer from non-integer-literal arguments first so a
                            // literal can adopt a concrete float type established
                            // by another argument instead of prematurely fixing T
                            // to its standalone int64 default.
                            for literal_pass in [false, true] {
                                for (argument, param) in ordered_args
                                    .iter()
                                    .zip(function_info.signature.params.iter())
                                {
                                    let Some(argument) = argument else {
                                        continue;
                                    };
                                    if is_integer_literal_expr(&argument.value) != literal_pass {
                                        continue;
                                    }
                                    let Some(actual) = self.infer_expr_type(&argument.value) else {
                                        continue;
                                    };
                                    let _ = crate::sema::type_pattern_matches(
                                        param,
                                        &actual,
                                        &type_params,
                                        &mut substitutions,
                                    );
                                }
                            }
                            substitutions
                        }
                    };
                    function_info
                        .signature
                        .params
                        .iter()
                        .map(|param| substitute_type(param, &substitutions))
                        .collect::<Vec<_>>()
                });
                let lowered_args = if let Some(function_info) = resolved_function.as_ref() {
                    self.lower_user_args_with_types(
                        &format!("function `{}`", name),
                        &function_info.decl.params,
                        args,
                        callee.span,
                        contextual_param_types.as_deref(),
                        Some(&function_info.signature.param_passings),
                    )
                } else {
                    self.lower_args(args)
                };
                let callee_name = if self
                    .program
                    .functions
                    .get(name)
                    .is_some_and(|function| function.module_name == self.program.module_name)
                {
                    name.clone()
                } else if let Some(function_info) = resolved_function {
                    if function_info.module_name == self.program.module_name {
                        name.clone()
                    } else {
                        imported_module_function_name(
                            &function_info.module_name,
                            &function_info.decl.name,
                        )
                    }
                } else {
                    name.clone()
                };
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name(callee_name),
                        args: lowered_args,
                    },
                });
            }
            other => {
                let fallback = format!("unsupported<{:?}>", other);
                let lowered_args = self.lower_args(args);
                self.emit(Instruction::Assign {
                    target: temp.clone(),
                    value: Rvalue::Call {
                        callee: CallTarget::Name(fallback),
                        args: lowered_args,
                    },
                });
            }
        }

        Operand::Place(temp)
    }

    fn lower_enum_variant_payloads(
        &mut self,
        expr: &Expr,
        enum_name: &str,
        variant_name: &str,
        args: &[Argument],
        expected_enum_type: Option<&Type>,
    ) -> Vec<Operand> {
        let inferred_type = self.infer_expr_type(expr);
        let enum_type = expected_enum_type.or(inferred_type.as_ref());
        let payload_types = self.variant_payload_types(enum_type, enum_name, variant_name);
        let named_payloads = self
            .resolve_enum_info(enum_name)
            .and_then(|enum_info| enum_info.variants.get(variant_name))
            .filter(|variant| variant.named_payloads)
            .map(|variant| {
                variant
                    .payloads
                    .iter()
                    .map(|payload| {
                        payload
                            .name
                            .clone()
                            .expect("checked named enum payload should have a name")
                    })
                    .collect::<Vec<_>>()
            });
        let mut lowered = (0..args.len())
            .map(|_| None)
            .collect::<Vec<Option<Operand>>>();

        // Evaluate in call-site order, then bind named payloads back to their
        // declaration slots so positional pattern matching sees declaration
        // order rather than source argument order.
        for (source_index, argument) in args.iter().enumerate() {
            let slot = named_payloads
                .as_ref()
                .and_then(|names| {
                    argument
                        .name
                        .as_ref()
                        .and_then(|name| names.iter().position(|candidate| candidate == name))
                })
                .unwrap_or(source_index);
            let value = self.lower_expr_for_owned_value(
                &argument.value,
                payload_types.as_ref().and_then(|types| types.get(slot)),
            );
            lowered[slot] = Some(value);
        }

        lowered
            .into_iter()
            .map(|payload| payload.expect("checked enum constructor should fill every payload"))
            .collect()
    }

    fn lower_member_call_args(
        &mut self,
        span: Span,
        object_expr: &Expr,
        field: &str,
        args: &[Argument],
    ) -> Vec<MirArg> {
        let Some(receiver_type) = self.infer_expr_type(object_expr) else {
            return self.lower_args(args);
        };

        if let Type::Named(class_name, class_args) = &receiver_type {
            if let Some(class) = self.resolve_class_info(class_name).cloned() {
                if let Some(method) = class
                    .methods
                    .get(field)
                    .filter(|method| method.decl.receiver.is_some())
                {
                    let substitutions =
                        substitutions_from_decl_type_args(&class.decl.type_params, class_args);
                    let expected_param_types = method
                        .signature
                        .params
                        .iter()
                        .map(|param| substitute_type(param, &substitutions))
                        .collect::<Vec<_>>();
                    return self.lower_user_args_with_types(
                        &format!("method `{}`", field),
                        &method.decl.params,
                        args,
                        span,
                        Some(&expected_param_types),
                        Some(&method.signature.param_passings),
                    );
                }
            }

            if let Some(builtin_member) = BuiltinMember::resolve(class_name, field) {
                let ordered_args = builtin_member
                    .bind_args(args, span)
                    .expect("type-checked builtin member call should bind during MIR lowering");
                let mut lowered_by_param = (0..ordered_args.len())
                    .map(|_| None)
                    .collect::<Vec<Option<MirArg>>>();

                // Preserve source evaluation order, then place the evaluated
                // operands in their declaration slots. This is observable for
                // reversed named arguments and required for mutable writeback.
                for argument in args {
                    let index = ordered_args
                        .iter()
                        .position(
                            |bound| matches!(bound, Some(bound) if std::ptr::eq(*bound, argument)),
                        )
                        .expect("bound builtin argument should retain its declaration slot");
                    let passing = builtin_member
                        .argument_passing(index)
                        .expect("fixed builtin argument should declare a passing mode");
                    let expected = if class_name == "Array" && class_args.len() == 1 {
                        match (builtin_member, index) {
                            (BuiltinMember::ArrayGet | BuiltinMember::ArraySet, 0) => {
                                Some(Type::Named("list".to_string(), vec![Type::named("int64")]))
                            }
                            (BuiltinMember::ArraySet, 1) | (BuiltinMember::ArrayFill, 0) => {
                                Some(class_args[0].clone())
                            }
                            (
                                BuiltinMember::ArrayWrappingAdd
                                | BuiltinMember::ArrayWrappingSub
                                | BuiltinMember::ArrayWrappingMul
                                | BuiltinMember::ArraySaturatingAdd
                                | BuiltinMember::ArraySaturatingSub
                                | BuiltinMember::ArraySaturatingMul,
                                0,
                            ) => self
                                .infer_expr_type(&argument.value)
                                .filter(|ty| {
                                    matches!(
                                        ty,
                                        Type::Named(name, args)
                                            if name == "Array" && args.len() == 1
                                    )
                                })
                                .or_else(|| Some(class_args[0].clone())),
                            _ => self.infer_expr_type(&argument.value),
                        }
                    } else {
                        self.infer_expr_type(&argument.value)
                    };
                    let index_domain_argument = class_name == "list"
                        && matches!(
                            (builtin_member, index),
                            (BuiltinMember::VecGet, 0)
                                | (BuiltinMember::VecSet, 0)
                                | (BuiltinMember::VecPop, 0)
                                | (BuiltinMember::VecSwap, 0 | 1)
                                | (BuiltinMember::VecInsert, 0)
                        );
                    let value = if index_domain_argument {
                        self.lower_index_domain_expr(&argument.value)
                    } else {
                        self.lower_expr_for_passing(&argument.value, expected.as_ref(), passing)
                    };
                    let writeback_place = (passing == ReceiverKind::BorrowMut)
                        .then(|| self.lowered_writeback_place(&argument.value, &value))
                        .flatten();
                    lowered_by_param[index] = Some(MirArg {
                        name: argument.name.clone(),
                        value,
                        writeback_place,
                    });
                }

                return lowered_by_param.into_iter().flatten().collect();
            }
        }

        let trait_method =
            self.trait_method_for_receiver(&receiver_type, field)
                .map(|(method, substitutions)| {
                    (
                        method.decl.params.clone(),
                        method.signature.param_passings.clone(),
                        method
                            .signature
                            .params
                            .iter()
                            .map(|param| substitute_type(param, &substitutions))
                            .collect::<Vec<_>>(),
                    )
                });

        if let Some((params, param_passings, expected_param_types)) = trait_method {
            return self.lower_user_args_with_types(
                &format!("method `{}`", field),
                &params,
                args,
                span,
                Some(&expected_param_types),
                Some(&param_passings),
            );
        }

        self.lower_args(args)
    }

    fn lower_builtin_class_constructor_args(
        &mut self,
        constructor: BuiltinClassConstructor,
        args: &[Argument],
        span: Span,
    ) -> Vec<MirArg> {
        let ordered_args = constructor
            .bind_args(args, span)
            .expect("type-checked builtin class constructor should bind during MIR lowering");
        let mut lowered_by_param = (0..ordered_args.len())
            .map(|_| None)
            .collect::<Vec<Option<MirArg>>>();

        for argument in args {
            let index = ordered_args
                .iter()
                .position(|bound| matches!(bound, Some(bound) if std::ptr::eq(*bound, argument)))
                .expect("bound builtin constructor argument should retain its declaration slot");
            let expected = match constructor {
                BuiltinClassConstructor::RandomRng => Type::named("int64"),
            };
            let value = self.lower_expr_at_sequence_point(&argument.value, Some(&expected));
            lowered_by_param[index] = Some(MirArg {
                name: argument.name.clone(),
                value,
                writeback_place: None,
            });
        }

        lowered_by_param
            .into_iter()
            .map(|argument| {
                argument.expect("required builtin constructor parameter should be supplied")
            })
            .collect()
    }

    fn user_member_receiver_passing(
        &self,
        object_expr: &Expr,
        field: &str,
    ) -> Option<ReceiverKind> {
        let receiver_type = self.infer_expr_type(object_expr)?;
        if let Type::Named(class_name, _) = &receiver_type {
            if let Some(method) = self
                .resolve_class_info(class_name)
                .and_then(|class| class.methods.get(field))
                .filter(|method| method.decl.receiver.is_some())
            {
                return method.decl.receiver;
            }
        }
        self.trait_method_for_receiver(&receiver_type, field)
            .and_then(|(method, _)| method.decl.receiver)
    }

    fn lower_user_args(
        &mut self,
        callee_name: &str,
        params: &[crate::ast::Param],
        args: &[Argument],
        span: Span,
        param_passings: Option<&[ReceiverKind]>,
    ) -> Vec<MirArg> {
        self.lower_user_args_with_types(callee_name, params, args, span, None, param_passings)
    }

    fn lower_user_args_with_types(
        &mut self,
        callee_name: &str,
        params: &[crate::ast::Param],
        args: &[Argument],
        span: Span,
        expected_param_types: Option<&[Type]>,
        param_passings: Option<&[ReceiverKind]>,
    ) -> Vec<MirArg> {
        let ordered_args = bind_call_arguments(
            callee_name,
            &callable_params_from_decl(params),
            args,
            span,
            CallConvention::PositionalOrNamed,
        )
        .expect("type-checked user-defined call should bind during MIR lowering");

        let mut lowered_by_param = (0..params.len())
            .map(|_| None)
            .collect::<Vec<Option<MirArg>>>();

        // Supplied expressions retain call-site order even when named
        // arguments bind to declaration slots in another order.
        for argument in args {
            let index = ordered_args
                .iter()
                .position(|bound| matches!(bound, Some(bound) if std::ptr::eq(*bound, argument)))
                .expect("bound source argument should retain its declaration slot");
            let param = &params[index];
            let expected = expected_param_types
                .and_then(|types| types.get(index))
                .cloned()
                .unwrap_or_else(|| self.lower_type_ref_with_provenance(&param.ty));
            let passing = param_passings
                .and_then(|passings| passings.get(index))
                .copied();
            let value = if let Some(passing) = passing {
                self.lower_expr_for_passing(&argument.value, Some(&expected), passing)
            } else if is_float_type(&expected) {
                self.lower_expr_at_sequence_point(&argument.value, Some(&expected))
            } else {
                self.lower_expr_at_sequence_point(&argument.value, None)
            };
            let writeback_place = if passing == Some(ReceiverKind::BorrowMut)
                || param.mode == crate::ast::ParamMode::BorrowMut
            {
                self.lowered_writeback_place(&argument.value, &value)
            } else {
                None
            };
            lowered_by_param[index] = Some(MirArg {
                name: None,
                value,
                writeback_place,
            });
        }

        // Omitted defaults follow only after every supplied argument, in
        // declaration order.
        for (index, param) in params.iter().enumerate() {
            if lowered_by_param[index].is_some() {
                continue;
            }
            let default = param
                .default
                .as_ref()
                .expect("optional parameter should provide a default expression");
            let expected = expected_param_types
                .and_then(|types| types.get(index))
                .cloned()
                .unwrap_or_else(|| self.lower_type_ref_with_provenance(&param.ty));
            let passing = param_passings
                .and_then(|passings| passings.get(index))
                .copied();
            let value = if let Some(passing) = passing {
                self.lower_expr_for_passing(default, Some(&expected), passing)
            } else if is_float_type(&expected) {
                self.lower_expr_with_expected(default, Some(&expected))
            } else {
                self.lower_expr_at_sequence_point(default, None)
            };
            lowered_by_param[index] = Some(MirArg {
                name: None,
                value,
                writeback_place: None,
            });
        }

        lowered_by_param
            .into_iter()
            .map(|argument| argument.expect("every parameter should have a lowered argument"))
            .collect()
    }

    fn lower_args(&mut self, args: &[Argument]) -> Vec<MirArg> {
        args.iter()
            .map(|argument| MirArg {
                name: argument.name.clone(),
                value: self.lower_expr_at_sequence_point(&argument.value, None),
                writeback_place: None,
            })
            .collect()
    }

    fn lower_function_value_args(
        &mut self,
        args: &[Argument],
        params: &[FunctionParamContract],
        span: Span,
    ) -> Vec<MirArg> {
        let callable_params = params
            .iter()
            .map(|param| CallableParam {
                name: &param.name,
                required: !param.has_default,
            })
            .collect::<Vec<_>>();
        let ordered = bind_call_arguments(
            "function value",
            &callable_params,
            args,
            span,
            CallConvention::PositionalOrNamed,
        )
        .expect("checked indirect-call arguments should bind during MIR lowering");
        args.iter()
            .map(|argument| {
                let index = ordered
                    .iter()
                    .position(
                        |bound| matches!(bound, Some(bound) if std::ptr::eq(*bound, argument)),
                    )
                    .expect("bound indirect argument should retain its declaration slot");
                let param = &params[index];
                let value =
                    self.lower_expr_for_passing(&argument.value, Some(&param.ty), param.passing);
                MirArg {
                    name: argument.name.clone(),
                    value: value.clone(),
                    writeback_place: (param.passing == ReceiverKind::BorrowMut)
                        .then(|| self.lowered_writeback_place(&argument.value, &value))
                        .flatten(),
                }
            })
            .collect()
    }

    fn retarget_operand_place(&mut self, operand: &Operand, ty: &Type) {
        if let Operand::Place(place) | Operand::MovePlace(place) = operand {
            self.local_types.insert(place.clone(), ty.clone());
        }
    }

    fn builtin_enum_variant_type(&self, receiver_type: &Type, field: &str) -> Option<Type> {
        match receiver_type {
            Type::Named(name, args) if name == "Option" && args.len() == 1 => {
                matches!(field, "Some" | "None").then(|| receiver_type.clone())
            }
            Type::Named(name, args) if name == "Result" && args.len() == 2 => {
                matches!(field, "Ok" | "Err").then(|| receiver_type.clone())
            }
            Type::Named(name, args) if name == "SendError" && args.len() == 1 => {
                matches!(field, "Closed" | "Cancelled").then(|| receiver_type.clone())
            }
            _ => None,
        }
    }

    fn infer_option_some_call_type(&self, value: &Expr) -> Option<Type> {
        let inner = self.infer_expr_type(value)?;
        let payload = if inner == Type::Unit {
            Type::Named("Option".to_string(), vec![Type::named("Unknown")])
        } else {
            inner
        };
        Some(Type::Named("Option".to_string(), vec![payload]))
    }

    fn infer_expr_type(&self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Membership { .. } | ExprKind::CompareChain { .. } => {
                Some(Type::named("bool"))
            }
            ExprKind::Lambda { .. } => self.closure_info_at(expr.span).map(ClosureInfo::ty),
            ExprKind::Name(name) if name == "None" => Some(Type::Unit),
            ExprKind::Name(name) => {
                if let Some(mapped) = self.scoped_local_name(name) {
                    return self.local_types.get(mapped).cloned();
                }
                self.local_types
                    .get(name)
                    .cloned()
                    .or_else(|| {
                        self.resolve_constant_info(name)
                            .map(|constant| constant.ty.clone())
                    })
                    .or_else(|| {
                        self.program
                            .imported_modules
                            .get(name)
                            .map(|namespace| Type::Module(namespace.path.clone()))
                    })
                    .or_else(|| {
                        self.resolve_class_info(name).map(|class| {
                            Type::named(mir_class_type_name(self.program, class, name))
                        })
                    })
                    .or_else(|| {
                        self.resolve_enum_info(name).map(|enum_info| {
                            Type::named(mir_runtime_enum_name(self.program, enum_info))
                        })
                    })
                    .or_else(|| {
                        self.resolve_function_info(name).map(|function| {
                            self.function_type(function, &std::collections::HashMap::new())
                        })
                    })
            }
            ExprKind::Group(inner) => self.infer_expr_type(inner),
            ExprKind::Cast { ty, .. } => Some(self.lower_type_ref_with_provenance(ty)),
            ExprKind::Int(_) => Some(Type::named("int64")),
            ExprKind::Float(_) => Some(Type::named("float64")),
            ExprKind::Bool(_) => Some(Type::named("bool")),
            ExprKind::String(_) => Some(Type::named("str")),
            ExprKind::Tuple(elements) => Some(Type::Tuple(
                elements
                    .iter()
                    .map(|element| {
                        self.infer_expr_type(element)
                            .unwrap_or_else(|| Type::named("Unknown"))
                    })
                    .collect(),
            )),
            ExprKind::List(elements) => Some(Type::Named(
                "list".to_string(),
                vec![elements
                    .first()
                    .and_then(|element| self.infer_expr_type(element))
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ExprKind::Set(elements) => Some(Type::Named(
                "set".to_string(),
                vec![elements
                    .first()
                    .and_then(|element| self.infer_expr_type(element))
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ExprKind::Map(entries) => Some(Type::Named(
                "dict".to_string(),
                vec![
                    entries
                        .first()
                        .and_then(|entry| self.infer_expr_type(&entry.key))
                        .unwrap_or_else(|| Type::named("Unknown")),
                    entries
                        .first()
                        .and_then(|entry| self.infer_expr_type(&entry.value))
                        .unwrap_or_else(|| Type::named("Unknown")),
                ],
            )),
            ExprKind::Comprehension { .. } => self
                .comprehension_info_at(expr.span)
                .map(|info| info.result_type.clone()),
            ExprKind::FString(_) => Some(Type::named("str")),
            ExprKind::Specialize { expr, type_args } => {
                if let Some((_runtime_name, function)) = self.resolve_function_value_target(expr) {
                    let substitutions = substitutions_from_decl_type_args(
                        &function.decl.type_params,
                        &type_args
                            .iter()
                            .map(|ty| self.lower_type_ref_with_provenance(ty))
                            .collect::<Vec<_>>(),
                    );
                    return Some(self.function_type(function, &substitutions));
                }
                match &expr.kind {
                    ExprKind::Name(name)
                        if matches!(
                            name.as_str(),
                            "Option" | "Result" | "SendError" | "Queue" | "list" | "set" | "dict"
                        ) =>
                    {
                        Some(Type::Named(
                            name.clone(),
                            type_args
                                .iter()
                                .map(|ty| self.lower_type_ref_with_provenance(ty))
                                .collect(),
                        ))
                    }
                    _ => self.infer_expr_type(expr),
                }
            }
            ExprKind::DurationNanos(_) => Some(Type::named("Duration")),
            ExprKind::BuiltinOmitted => None,
            ExprKind::Unary { op, expr } => match op {
                UnaryOp::Not => Some(Type::named("bool")),
                UnaryOp::BitNot => {
                    let value_ty = self.infer_expr_type(expr)?;
                    is_builtin_unary_operator(*op, &value_ty).then_some(value_ty)
                }
                UnaryOp::Neg => match &expr.kind {
                    ExprKind::Int(value) => minimal_signed_type_for_negative_literal(*value),
                    _ => {
                        let value_ty = self.infer_expr_type(expr)?;
                        if is_builtin_unary_operator(*op, &value_ty) {
                            Some(value_ty)
                        } else {
                            self.operator_return_type_for_unary(&value_ty, *op)
                        }
                    }
                },
            },
            ExprKind::Try(inner) => match self.infer_expr_type(inner)? {
                Type::Named(name, mut args) if name == "Result" && args.len() == 2 => {
                    Some(args.remove(0))
                }
                _ => None,
            },
            ExprKind::Call { callee, args } => {
                let (base_callee, explicit_type_args) = match &callee.kind {
                    ExprKind::Specialize { expr, type_args } => {
                        (&**expr, Some(type_args.as_slice()))
                    }
                    _ => (&**callee, None),
                };
                let direct_decl_callee = match &base_callee.kind {
                    ExprKind::Name(name) => {
                        !self.local_types.contains_key(&self.render_local_name(name))
                            && self.resolve_function_info(name).is_some()
                    }
                    ExprKind::Member { object, field } => self
                        .infer_module_path(object)
                        .and_then(|module_path| self.module_namespace(&module_path))
                        .is_some_and(|namespace| {
                            namespace.functions.contains_key(field)
                                || namespace.all_functions.contains_key(field)
                        }),
                    _ => false,
                };
                if !direct_decl_callee {
                    if let Some(
                        Type::Function { return_type, .. } | Type::Closure { return_type, .. },
                    ) = self.infer_expr_type(callee)
                    {
                        return Some(*return_type);
                    }
                }
                match &base_callee.kind {
                    ExprKind::Name(name) => {
                        if name == "range" {
                            return Some(Type::named("Range"));
                        }
                        if name == "cancelled" {
                            return Some(Type::named("bool"));
                        }
                        if name == "yield_now" {
                            return Some(Type::Unit);
                        }
                        if name == "sleep" {
                            return Some(Type::Unit);
                        }
                        if name == "select" {
                            let mut queue_payload = None;
                            let mut task_result = None;
                            for argument in args {
                                match self.infer_expr_type(&argument.value) {
                                    Some(Type::Named(source, mut source_args))
                                        if source == "Queue" && source_args.len() == 1 =>
                                    {
                                        queue_payload = Some(source_args.remove(0));
                                    }
                                    Some(Type::Named(source, mut source_args))
                                        if source == "Task" && source_args.len() == 1 =>
                                    {
                                        task_result = Some(source_args.remove(0));
                                    }
                                    _ => {}
                                }
                            }
                            return Some(Type::Named(
                                "SelectOutcome".to_string(),
                                vec![
                                    queue_payload.unwrap_or(Type::Unit),
                                    task_result.unwrap_or(Type::Unit),
                                ],
                            ));
                        }
                        if name == "wait_any" {
                            return args.first().and_then(|argument| {
                                match self.infer_expr_type(&argument.value) {
                                    Some(Type::Named(container, container_args))
                                        if container == "list" && container_args.len() == 1 =>
                                    {
                                        match &container_args[0] {
                                            Type::Named(task, task_args)
                                                if task == "Task" && task_args.len() == 1 =>
                                            {
                                                Some(Type::Named(
                                                    "WaitAny".to_string(),
                                                    vec![task_args[0].clone()],
                                                ))
                                            }
                                            _ => None,
                                        }
                                    }
                                    _ => None,
                                }
                            });
                        }
                        if name == "wait_all" {
                            return args.first().and_then(|argument| {
                                match self.infer_expr_type(&argument.value) {
                                    Some(Type::Named(container, container_args))
                                        if container == "list" && container_args.len() == 1 =>
                                    {
                                        match &container_args[0] {
                                            Type::Named(task, task_args)
                                                if task == "Task" && task_args.len() == 1 =>
                                            {
                                                Some(Type::Named(
                                                    "WaitAll".to_string(),
                                                    vec![task_args[0].clone()],
                                                ))
                                            }
                                            _ => None,
                                        }
                                    }
                                    _ => None,
                                }
                            });
                        }
                        if name == "len" {
                            return Some(Type::named("int64"));
                        }
                        if name == "str" {
                            return Some(Type::named("str"));
                        }
                        if matches!(name.as_str(), "abs" | "min" | "max" | "sqrt") {
                            return args
                                .first()
                                .and_then(|argument| self.infer_expr_type(&argument.value));
                        }
                        if name == "round" {
                            let ty = args
                                .first()
                                .and_then(|argument| self.infer_expr_type(&argument.value))?;
                            return Some(
                                if matches!(
                                    ty,
                                    Type::Named(ref type_name, ref type_args)
                                        if type_args.is_empty()
                                            && matches!(type_name.as_str(), "float32" | "float64")
                                ) {
                                    Type::named("int64")
                                } else {
                                    ty
                                },
                            );
                        }
                        if name == "divmod" {
                            let ty = args
                                .first()
                                .and_then(|argument| self.infer_expr_type(&argument.value))?;
                            return Some(Type::Tuple(vec![ty.clone(), ty]));
                        }
                        if name == "parse_int32" {
                            return Some(Type::Named(
                                "Result".to_string(),
                                vec![Type::named("int32"), Type::named("str")],
                            ));
                        }
                        if name == "parse_int64" {
                            return Some(Type::Named(
                                "Result".to_string(),
                                vec![Type::named("int64"), Type::named("str")],
                            ));
                        }
                        if name == "parse_float64" {
                            return Some(Type::Named(
                                "Result".to_string(),
                                vec![Type::named("float64"), Type::named("str")],
                            ));
                        }
                        if name == "Queue" {
                            return explicit_type_args.map(|type_args| {
                                Type::Named(
                                    "Queue".to_string(),
                                    type_args
                                        .iter()
                                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                                        .collect(),
                                )
                            });
                        }
                        if name == "TaskGroup" {
                            return Some(Type::named("TaskGroup"));
                        }
                        if name == "list" {
                            return explicit_type_args.map(|type_args| {
                                Type::Named(
                                    "list".to_string(),
                                    type_args
                                        .iter()
                                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                                        .collect(),
                                )
                            });
                        }
                        if name == "set" {
                            return explicit_type_args.map(|type_args| {
                                Type::Named(
                                    "set".to_string(),
                                    type_args
                                        .iter()
                                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                                        .collect(),
                                )
                            });
                        }
                        if name == "dict" {
                            return explicit_type_args.map(|type_args| {
                                Type::Named(
                                    "dict".to_string(),
                                    type_args
                                        .iter()
                                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                                        .collect(),
                                )
                            });
                        }
                        if self.resolve_class_info(name).is_some() {
                            return self.infer_class_constructor_type(
                                name,
                                args,
                                explicit_type_args,
                            );
                        }
                        if name == "Some" && args.len() == 1 {
                            return self.infer_option_some_call_type(&args[0].value);
                        }
                        self.resolve_function_info(name).map(|function| {
                            if let Some(type_args) = explicit_type_args {
                                let substitutions = function
                                    .decl
                                    .type_params
                                    .iter()
                                    .cloned()
                                    .zip(
                                        type_args
                                            .iter()
                                            .map(|ty| self.lower_type_ref_with_provenance(ty)),
                                    )
                                    .collect();
                                substitute_type(&function.signature.return_type, &substitutions)
                            } else {
                                let ordered_args = bind_call_arguments(
                                    name,
                                    &callable_params_from_decl(&function.decl.params),
                                    args,
                                    callee.span,
                                    CallConvention::PositionalOrNamed,
                                )
                                .ok();
                                let type_params = function
                                    .decl
                                    .type_params
                                    .iter()
                                    .cloned()
                                    .collect::<BTreeSet<_>>();
                                let mut substitutions = std::collections::HashMap::new();
                                if let Some(ordered_args) = ordered_args {
                                    for (bound_arg, expected) in ordered_args
                                        .into_iter()
                                        .zip(function.signature.params.iter())
                                    {
                                        let Some(argument) = bound_arg else {
                                            continue;
                                        };
                                        let Some(actual_ty) = self.infer_expr_type(&argument.value)
                                        else {
                                            continue;
                                        };
                                        let _ = crate::sema::type_pattern_matches(
                                            expected,
                                            &actual_ty,
                                            &type_params,
                                            &mut substitutions,
                                        );
                                    }
                                }
                                substitute_type(&function.signature.return_type, &substitutions)
                            }
                        })
                    }
                    ExprKind::Member { object, field } => {
                        let receiver_type = match &object.kind {
                            ExprKind::Specialize { expr, type_args }
                                if matches!(&expr.kind, ExprKind::Name(_)) =>
                            {
                                let inner_name = match &expr.kind {
                                    ExprKind::Name(name) => name,
                                    _ => unreachable!(),
                                };
                                Some(Type::Named(
                                    inner_name.clone(),
                                    type_args
                                        .iter()
                                        .map(|ty| self.lower_type_ref_with_provenance(ty))
                                        .collect(),
                                ))
                            }
                            _ => self.infer_expr_type(object),
                        };
                        if let Some((module_path, item_name)) = self.qualified_module_item(object) {
                            if let Some(namespace) = self.module_namespace(&module_path) {
                                if let Some(class) = namespace.classes.get(&item_name) {
                                    if let Some(method) = class.methods.get(field) {
                                        return Some(method.signature.return_type.clone());
                                    }
                                }
                                if let Some(enum_info) = namespace.enums.get(&item_name) {
                                    if enum_info.variants.contains_key(field) {
                                        return Some(Type::named(mir_runtime_enum_name(
                                            self.program,
                                            enum_info,
                                        )));
                                    }
                                }
                            }
                        }
                        if let Some(module_path) = self.infer_module_path(object) {
                            if let Some(namespace) = self.module_namespace(&module_path) {
                                if let Some(child) = namespace.modules.get(field) {
                                    return Some(Type::Module(child.path.clone()));
                                }
                                if let Some(function) = namespace.functions.get(field) {
                                    let substitutions = if let Some(type_args) = explicit_type_args
                                    {
                                        substitutions_from_decl_type_args(
                                            &function.decl.type_params,
                                            &type_args
                                                .iter()
                                                .map(|ty| self.lower_type_ref_with_provenance(ty))
                                                .collect::<Vec<_>>(),
                                        )
                                    } else {
                                        let ordered = bind_call_arguments(
                                            &format!("function `{field}`"),
                                            &callable_params_from_decl(&function.decl.params),
                                            args,
                                            callee.span,
                                            CallConvention::PositionalOrNamed,
                                        )
                                        .ok();
                                        let type_params = function
                                            .decl
                                            .type_params
                                            .iter()
                                            .cloned()
                                            .collect::<BTreeSet<_>>();
                                        let mut substitutions = std::collections::HashMap::new();
                                        if let Some(ordered) = ordered {
                                            for (argument, param) in
                                                ordered.iter().zip(&function.signature.params)
                                            {
                                                let Some(argument) = argument else {
                                                    continue;
                                                };
                                                let Some(actual) =
                                                    self.infer_expr_type(&argument.value)
                                                else {
                                                    continue;
                                                };
                                                let _ = crate::sema::type_pattern_matches(
                                                    param,
                                                    &actual,
                                                    &type_params,
                                                    &mut substitutions,
                                                );
                                            }
                                        }
                                        substitutions
                                    };
                                    return Some(substitute_type(
                                        &function.signature.return_type,
                                        &substitutions,
                                    ));
                                }
                                if namespace.classes.contains_key(field) {
                                    return self.infer_class_constructor_type(
                                        &format!("{}.{}", module_path, field),
                                        args,
                                        explicit_type_args,
                                    );
                                }
                                if let Some(enum_info) = namespace.enums.get(field) {
                                    return Some(Type::named(mir_runtime_enum_name(
                                        self.program,
                                        enum_info,
                                    )));
                                }
                            }
                        }
                        if let ExprKind::Name(enum_name) = &object.kind {
                            if enum_name == "Option" && field == "Some" && args.len() == 1 {
                                return self.infer_option_some_call_type(&args[0].value);
                            }
                        }
                        let associated_owner = match &object.kind {
                            ExprKind::Specialize { expr, .. } => expr.as_ref(),
                            _ => object.as_ref(),
                        };
                        if let ExprKind::Name(type_name) = &associated_owner.kind {
                            if self.infer_expr_type(associated_owner).is_none() {
                                if let Some(associated) =
                                    BuiltinAssociatedFunction::resolve(type_name, field)
                                {
                                    return match associated {
                                        BuiltinAssociatedFunction::ArrayZeros
                                        | BuiltinAssociatedFunction::ArrayFull
                                        | BuiltinAssociatedFunction::ArrayFromVec => receiver_type
                                            .filter(|ty| {
                                                matches!(
                                                    ty,
                                                    Type::Named(name, arguments)
                                                        if name == "Array" && arguments.len() == 1
                                                )
                                            }),
                                        BuiltinAssociatedFunction::DurationMilliseconds
                                        | BuiltinAssociatedFunction::DurationSeconds
                                        | BuiltinAssociatedFunction::DurationMinutes => {
                                            Some(Type::named("Duration"))
                                        }
                                        BuiltinAssociatedFunction::StringFromBytes => {
                                            Some(Type::Named(
                                                "Result".to_string(),
                                                vec![
                                                    Type::named("str"),
                                                    Type::named("bytes.Error"),
                                                ],
                                            ))
                                        }
                                        BuiltinAssociatedFunction::ListWithCapacity
                                        | BuiltinAssociatedFunction::DictWithCapacity
                                        | BuiltinAssociatedFunction::SetWithCapacity => {
                                            receiver_type
                                        }
                                    };
                                }
                            }
                        }
                        let receiver_type = receiver_type?;
                        if let Some(enum_ty) = self.builtin_enum_variant_type(&receiver_type, field)
                        {
                            return Some(enum_ty);
                        }
                        if let Type::Named(class_name, class_args) = &receiver_type {
                            if let Some(enum_info) = self.resolve_enum_info(class_name) {
                                if enum_info.variants.contains_key(field) {
                                    return Some(Type::Named(
                                        mir_runtime_enum_name(self.program, enum_info),
                                        class_args.clone(),
                                    ));
                                }
                            }
                            if let Some(class) = self.resolve_class_info(class_name) {
                                if let Some(method) = class.methods.get(field) {
                                    let substitutions = substitutions_from_decl_type_args(
                                        &class.decl.type_params,
                                        class_args,
                                    );
                                    return Some(substitute_type(
                                        &method.signature.return_type,
                                        &substitutions,
                                    ));
                                }
                            }
                        }
                        if matches!(&receiver_type, Type::Named(name, _) if name == "TaskGroup")
                            && matches!(
                                field.as_str(),
                                "start"
                                    | "start_soon"
                                    | "start_with_stack"
                                    | "start_soon_with_stack"
                            )
                        {
                            let has_stack_override = matches!(
                                field.as_str(),
                                "start_with_stack" | "start_soon_with_stack"
                            );
                            let target_index = usize::from(has_stack_override);
                            return if matches!(
                                field.as_str(),
                                "start_soon" | "start_soon_with_stack"
                            ) {
                                Some(Type::Unit)
                            } else {
                                args.get(target_index).and_then(|argument| {
                                    self.resolve_task_start_target(&argument.value)
                                        .map(|target| {
                                            let target = self.specialize_task_start_target(
                                                target,
                                                &args[target_index + 1..],
                                                callee.span,
                                            );
                                            Type::Named(
                                                "Task".to_string(),
                                                vec![target.return_type],
                                            )
                                        })
                                })
                            };
                        }
                        if let Type::Named(name, receiver_args) = &receiver_type {
                            if name == "list" && receiver_args.len() == 1 {
                                match field.as_str() {
                                    "sort" => return Some(Type::Unit),
                                    "filter" => return Some(receiver_type.clone()),
                                    "map" => {
                                        let callback = BuiltinMember::VecMap
                                            .bind_args(args, callee.span)
                                            .ok()
                                            .and_then(|ordered| ordered[0]);
                                        if let Some(
                                            Type::Function { return_type, .. }
                                            | Type::Closure { return_type, .. },
                                        ) = callback.and_then(|argument| {
                                            self.infer_expr_type(&argument.value)
                                        }) {
                                            return Some(Type::Named(
                                                "list".to_string(),
                                                vec![*return_type],
                                            ));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if name == "Array" && receiver_args.len() == 1 && field == "map" {
                                let callback = BuiltinMember::ArrayMap
                                    .bind_args(args, callee.span)
                                    .ok()
                                    .and_then(|ordered| ordered[0]);
                                if let Some(
                                    Type::Function { return_type, .. }
                                    | Type::Closure { return_type, .. },
                                ) = callback
                                    .and_then(|argument| self.infer_expr_type(&argument.value))
                                {
                                    return Some(Type::Named(
                                        "Array".to_string(),
                                        vec![*return_type],
                                    ));
                                }
                            }
                        }
                        if let Some(runtime_ty) =
                            self.builtin_runtime_member_return_type(&receiver_type, field)
                        {
                            return Some(runtime_ty);
                        }
                        self.trait_method_for_receiver(&receiver_type, field).map(
                            |(method, substitutions)| {
                                substitute_type(&method.signature.return_type, &substitutions)
                            },
                        )
                    }
                    _ => None,
                }
            }
            ExprKind::Member { object, field } => {
                if let Some(module_path) = self.infer_module_path(object) {
                    if let Some(constant) = self
                        .module_namespace(&module_path)
                        .and_then(|namespace| namespace.constants.get(field))
                    {
                        return Some(constant.ty.clone());
                    }
                    if let Some(function) =
                        self.module_namespace(&module_path).and_then(|namespace| {
                            namespace
                                .functions
                                .get(field)
                                .or_else(|| namespace.all_functions.get(field))
                        })
                    {
                        return Some(
                            self.function_type(function, &std::collections::HashMap::new()),
                        );
                    }
                }
                if let Some((module_path, item_name)) = self.qualified_module_item(object) {
                    if let Some(namespace) = self.module_namespace(&module_path) {
                        if let Some(enum_info) = namespace.enums.get(&item_name) {
                            if enum_info.variants.contains_key(field) {
                                return Some(Type::named(mir_runtime_enum_name(
                                    self.program,
                                    enum_info,
                                )));
                            }
                        }
                    }
                }
                let receiver_type = match &object.kind {
                    ExprKind::Specialize { expr, type_args }
                        if matches!(&expr.kind, ExprKind::Name(_)) =>
                    {
                        let inner_name = match &expr.kind {
                            ExprKind::Name(name) => name,
                            _ => unreachable!(),
                        };
                        Type::Named(
                            inner_name.clone(),
                            type_args
                                .iter()
                                .map(|ty| self.lower_type_ref_with_provenance(ty))
                                .collect(),
                        )
                    }
                    _ => self.infer_expr_type(object)?,
                };
                if let Some(enum_ty) = self.builtin_enum_variant_type(&receiver_type, field) {
                    return Some(enum_ty);
                }
                let Type::Named(class_name, class_args) = receiver_type else {
                    return None;
                };
                if let Some(enum_info) = self.resolve_enum_info(&class_name) {
                    if enum_info.variants.contains_key(field) {
                        return Some(Type::Named(
                            mir_runtime_enum_name(self.program, enum_info),
                            class_args,
                        ));
                    }
                }
                let class = self.resolve_class_info(&class_name)?;
                let substitutions =
                    substitutions_from_decl_type_args(&class.decl.type_params, &class_args);
                class
                    .fields
                    .get(field)
                    .map(|field| substitute_type(&field.ty, &substitutions))
            }
            ExprKind::Index { object, index } => {
                if let Some((_runtime_name, function)) = self.resolve_function_value_target(object)
                {
                    if let Some(type_args) = self.task_type_args_from_index_expr(index) {
                        if !function.decl.type_params.is_empty()
                            && function.decl.type_params.len() == type_args.len()
                        {
                            let substitutions = substitutions_from_decl_type_args(
                                &function.decl.type_params,
                                &type_args,
                            );
                            return Some(self.function_type(function, &substitutions));
                        }
                    }
                }
                match self.infer_expr_type(object)? {
                    Type::Tuple(elements) => elements.get(tuple_constant_index(index)?).cloned(),
                    Type::Named(name, args) if name == "list" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    Type::Named(name, args) if name == "dict" && args.len() == 2 => {
                        Some(args[1].clone())
                    }
                    Type::Named(name, args) if name == "Array" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                }
            }
            ExprKind::Slice { object, .. } => self.infer_expr_type(object),
            ExprKind::Binary { op, left, right } => {
                if matches!(
                    op,
                    BinaryOp::Eq
                        | BinaryOp::NotEq
                        | BinaryOp::Less
                        | BinaryOp::LessEq
                        | BinaryOp::Greater
                        | BinaryOp::GreaterEq
                        | BinaryOp::And
                        | BinaryOp::Or
                ) {
                    return Some(Type::named("bool"));
                }
                let left_ty = self.infer_expr_type(left)?;
                let right_ty = self.infer_expr_type(right)?;
                let (left_ty, right_ty) =
                    adjusted_binary_operand_types(left, left_ty, right, right_ty);
                if is_builtin_binary_operator(*op, &left_ty, &right_ty) {
                    let duration = Type::named("Duration");
                    if matches!(
                        &left_ty,
                        Type::Named(name, arguments) if name == "Array" && arguments.len() == 1
                    ) {
                        Some(left_ty)
                    } else if matches!(
                        &right_ty,
                        Type::Named(name, arguments) if name == "Array" && arguments.len() == 1
                    ) {
                        Some(right_ty)
                    } else if left_ty == duration || right_ty == duration {
                        Some(duration)
                    } else {
                        Some(left_ty)
                    }
                } else {
                    self.operator_return_type_for_binary(&left_ty, &right_ty, *op)
                }
            }
            ExprKind::Conditional {
                then_expr,
                else_expr,
                ..
            } => self.infer_conditional_result_type(then_expr, else_expr),
            ExprKind::Match { arms, .. } => arms
                .first()
                .and_then(|arm| self.infer_expr_type(&arm.value)),
        }
    }

    fn infer_conditional_result_type(&self, then_expr: &Expr, else_expr: &Expr) -> Option<Type> {
        let then_ty = self.infer_expr_type(then_expr);
        let else_ty = self.infer_expr_type(else_expr);
        match (then_ty, else_ty) {
            (None, other) | (other, None) => other,
            (Some(then_ty), Some(else_ty)) if then_ty == else_ty => Some(then_ty),
            (Some(Type::Unit), Some(else_ty)) => Some(else_ty),
            (Some(then_ty), Some(Type::Unit)) => Some(then_ty),
            (Some(then_ty), Some(else_ty))
                if type_contains_unknown(&then_ty) && !type_contains_unknown(&else_ty) =>
            {
                Some(else_ty)
            }
            (Some(then_ty), Some(else_ty))
                if type_contains_unknown(&else_ty) && !type_contains_unknown(&then_ty) =>
            {
                Some(then_ty)
            }
            (Some(_), Some(else_ty))
                if is_integer_literal_expr(then_expr)
                    && (is_float_type(&else_ty)
                        || crate::sema::integer_type_bounds(&else_ty).is_some()) =>
            {
                Some(else_ty)
            }
            (Some(then_ty), Some(_))
                if is_integer_literal_expr(else_expr)
                    && (is_float_type(&then_ty)
                        || crate::sema::integer_type_bounds(&then_ty).is_some()) =>
            {
                Some(then_ty)
            }
            (Some(_), Some(else_ty))
                if is_float_literal_expr(then_expr)
                    && !is_float_literal_expr(else_expr)
                    && is_float_type(&else_ty) =>
            {
                Some(else_ty)
            }
            (Some(then_ty), Some(_))
                if is_float_literal_expr(else_expr)
                    && !is_float_literal_expr(then_expr)
                    && is_float_type(&then_ty) =>
            {
                Some(then_ty)
            }
            (Some(then_ty), Some(_)) => Some(then_ty),
        }
    }

    fn infer_tuple_equality_hint(&self, left: &Expr, right: &Expr) -> Option<Type> {
        if let ExprKind::Group(inner) = &left.kind {
            return self.infer_tuple_equality_hint(inner, right);
        }
        if let ExprKind::Group(inner) = &right.kind {
            return self.infer_tuple_equality_hint(left, inner);
        }
        if let (ExprKind::Tuple(left_elements), ExprKind::Tuple(right_elements)) =
            (&left.kind, &right.kind)
        {
            if left_elements.len() == right_elements.len() {
                let element_types = left_elements
                    .iter()
                    .zip(right_elements)
                    .map(|(left_element, right_element)| {
                        self.infer_tuple_equality_hint(left_element, right_element)
                            .or_else(|| {
                                self.infer_conditional_result_type(left_element, right_element)
                            })
                    })
                    .collect::<Option<Vec<_>>>()?;
                return Some(Type::Tuple(element_types));
            }
        }

        let left_ty = self.infer_expr_type(left);
        let right_ty = self.infer_expr_type(right);
        match (&left.kind, &right.kind, left_ty, right_ty) {
            (ExprKind::Tuple(_), _, _, Some(right_ty @ Type::Tuple(_))) => Some(right_ty),
            (_, ExprKind::Tuple(_), Some(left_ty @ Type::Tuple(_)), _) => Some(left_ty),
            (_, _, Some(left_ty @ Type::Tuple(_)), Some(right_ty)) if left_ty == right_ty => {
                Some(left_ty)
            }
            _ => None,
        }
    }

    fn infer_equality_hint(&self, left: &Expr, right: &Expr) -> Option<Type> {
        self.infer_tuple_equality_hint(left, right)
            .or_else(|| self.infer_conditional_result_type(left, right))
    }

    fn operator_field_for_unary(&self, op: UnaryOp, value: &Expr) -> Option<String> {
        let value_ty = self.infer_expr_type(value)?;
        (!is_builtin_unary_operator(op, &value_ty))
            .then(|| unary_operator_trait(op).map(|(_, field)| field.to_string()))
            .flatten()
    }

    fn operator_field_for_binary(&self, op: BinaryOp, left: &Expr, right: &Expr) -> Option<String> {
        let left_ty = self.infer_expr_type(left)?;
        let right_ty = self.infer_expr_type(right)?;
        let (left_ty, right_ty) = adjusted_binary_operand_types(left, left_ty, right, right_ty);
        (!is_builtin_binary_operator(op, &left_ty, &right_ty))
            .then(|| binary_operator_trait(op).map(|(_, field)| field.to_string()))
            .flatten()
    }

    fn trait_impls_in_scope(&self) -> impl Iterator<Item = &crate::sema::TraitImplInfo> + '_ {
        self.program.trait_impls.iter().chain(
            self.program
                .module_registry
                .values()
                .flat_map(|namespace| namespace.trait_impls.iter()),
        )
    }

    fn trait_impl_substitutions(
        &self,
        trait_impl: &crate::sema::TraitImplInfo,
        actual: &Type,
    ) -> Option<std::collections::HashMap<String, Type>> {
        let type_params = trait_impl
            .type_params
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut substitutions = std::collections::HashMap::new();
        if !crate::sema::type_pattern_matches(
            &trait_impl.for_type,
            actual,
            &type_params,
            &mut substitutions,
        ) {
            return None;
        }
        for (type_param, bounds) in &trait_impl.type_param_bounds {
            let actual_ty = substitutions.get(type_param)?;
            for bound in bounds {
                let resolved_bound = substitute_trait_bound(bound, &substitutions);
                if !self.type_implements_trait_bound(actual_ty, &resolved_bound) {
                    return None;
                }
            }
        }
        Some(substitutions)
    }

    fn trait_impl_substitutions_for_bound(
        &self,
        trait_impl: &crate::sema::TraitImplInfo,
        actual: &Type,
        bound: &TraitBound,
    ) -> Option<std::collections::HashMap<String, Type>> {
        if trait_impl.trait_name != bound.trait_name
            || trait_impl.trait_args.len() != bound.trait_args.len()
        {
            return None;
        }
        let mut type_params = BTreeSet::new();
        collect_type_params_from_type(&trait_impl.for_type, &mut type_params);
        for trait_arg in &trait_impl.trait_args {
            collect_type_params_from_type(trait_arg, &mut type_params);
        }
        let mut substitutions = std::collections::HashMap::new();
        if !crate::sema::type_pattern_matches(
            &trait_impl.for_type,
            actual,
            &type_params,
            &mut substitutions,
        ) {
            return None;
        }
        for (pattern, actual_arg) in trait_impl.trait_args.iter().zip(&bound.trait_args) {
            if !crate::sema::type_pattern_matches(
                pattern,
                actual_arg,
                &type_params,
                &mut substitutions,
            ) {
                return None;
            }
        }
        for (type_param, bounds) in &trait_impl.type_param_bounds {
            let actual_ty = substitutions.get(type_param)?;
            for impl_bound in bounds {
                let resolved_bound = substitute_trait_bound(impl_bound, &substitutions);
                if !self.type_implements_trait_bound(actual_ty, &resolved_bound) {
                    return None;
                }
            }
        }
        Some(substitutions)
    }

    fn type_implements_trait_bound(&self, ty: &Type, bound: &TraitBound) -> bool {
        self.trait_impls_in_scope().any(|trait_impl| {
            self.trait_impl_substitutions_for_bound(trait_impl, ty, bound)
                .or_else(|| {
                    if bound.trait_args.is_empty() && trait_impl.trait_name == bound.trait_name {
                        self.trait_impl_substitutions(trait_impl, ty)
                    } else {
                        None
                    }
                })
                .is_some()
        })
    }

    fn operator_return_type_for_unary(&self, value_ty: &Type, op: UnaryOp) -> Option<Type> {
        let (trait_name, _field) = unary_operator_trait(op)?;
        match value_ty {
            Type::TypeParam(type_param) => self
                .type_param_bounds
                .get(type_param)
                .into_iter()
                .flatten()
                .find(|bound| bound.trait_name == trait_name && bound.trait_args.len() == 1)
                .map(|bound| bound.trait_args[0].clone()),
            _ => self
                .trait_impls_in_scope()
                .filter(|trait_impl| {
                    trait_impl.trait_name == trait_name && trait_impl.trait_args.len() == 1
                })
                .filter_map(|trait_impl| {
                    let substitutions = self.trait_impl_substitutions(trait_impl, value_ty)?;
                    Some((
                        crate::sema::trait_impl_specificity(trait_impl),
                        substitute_type(&trait_impl.trait_args[0], &substitutions),
                    ))
                })
                .max_by_key(|(specificity, _)| *specificity)
                .map(|(_, result_ty)| result_ty),
        }
    }

    fn operator_return_type_for_binary(
        &self,
        left_ty: &Type,
        right_ty: &Type,
        op: BinaryOp,
    ) -> Option<Type> {
        let (trait_name, field) = binary_operator_trait(op)?;
        let trait_info = self.trait_info_in_scope(trait_name)?;
        let method = trait_info.methods.get(field)?;
        match left_ty {
            Type::TypeParam(type_param) => self
                .type_param_bounds
                .get(type_param)
                .into_iter()
                .flatten()
                .find(|bound| {
                    bound.trait_name == trait_name
                        && !bound.trait_args.is_empty()
                        && bound.trait_args[0] == *right_ty
                })
                .map(|bound| {
                    let substitutions = crate::sema::self_type_substitutions(
                        &trait_info.decl,
                        &bound.trait_args,
                        Type::TypeParam(type_param.to_string()),
                    );
                    substitute_type(&method.signature.return_type, &substitutions)
                }),
            _ => self
                .trait_impls_in_scope()
                .filter(|trait_impl| trait_impl.trait_name == trait_name)
                .filter_map(|trait_impl| {
                    let trait_method = trait_impl.methods.get(field)?;
                    let mut type_params = BTreeSet::new();
                    collect_type_params_from_type(&trait_impl.for_type, &mut type_params);
                    for trait_arg in &trait_impl.trait_args {
                        collect_type_params_from_type(trait_arg, &mut type_params);
                    }
                    let mut substitutions = std::collections::HashMap::new();
                    if !crate::sema::type_pattern_matches(
                        &trait_impl.for_type,
                        left_ty,
                        &type_params,
                        &mut substitutions,
                    ) {
                        return None;
                    }
                    if trait_impl.trait_args.is_empty() {
                        return None;
                    }
                    if !crate::sema::type_pattern_matches(
                        &trait_impl.trait_args[0],
                        right_ty,
                        &type_params,
                        &mut substitutions,
                    ) {
                        return None;
                    }
                    let trait_substitutions = crate::sema::self_type_substitutions(
                        &trait_info.decl,
                        &trait_impl.trait_args,
                        left_ty.clone(),
                    );
                    Some((
                        crate::sema::trait_impl_specificity(trait_impl),
                        substitute_type(
                            &trait_method.signature.return_type,
                            &trait_substitutions
                                .into_iter()
                                .chain(substitutions)
                                .collect::<std::collections::HashMap<_, _>>(),
                        ),
                    ))
                })
                .max_by_key(|(specificity, _)| *specificity)
                .map(|(_, result_ty)| result_ty),
        }
    }

    fn trait_method_for_receiver(
        &self,
        receiver_type: &Type,
        field: &str,
    ) -> Option<(
        &crate::sema::TraitImplMethodInfo,
        std::collections::HashMap<String, Type>,
    )> {
        self.trait_impls_in_scope()
            .filter_map(|trait_impl| {
                let substitutions = self.trait_impl_substitutions(trait_impl, receiver_type)?;
                let method = trait_impl.methods.get(field)?;
                Some((
                    crate::sema::trait_impl_specificity(trait_impl),
                    method,
                    substitutions,
                ))
            })
            .max_by_key(|(specificity, _, _)| *specificity)
            .map(|(_, method, substitutions)| (method, substitutions))
    }

    fn trait_impl_method_for_class_name(
        &self,
        class_name: &str,
        field: &str,
    ) -> Option<(crate::sema::TraitImplInfo, crate::sema::TraitImplMethodInfo)> {
        self.trait_impls_in_scope()
            .filter_map(|trait_impl| match &trait_impl.for_type {
                Type::Named(name, _) if name == class_name => {
                    trait_impl.methods.get(field).cloned().map(|method| {
                        (
                            crate::sema::trait_impl_specificity(trait_impl),
                            trait_impl.clone(),
                            method,
                        )
                    })
                }
                _ => None,
            })
            .max_by_key(|(specificity, _, _)| *specificity)
            .map(|(_, trait_impl, method)| (trait_impl, method))
    }

    fn resolve_pattern_enum_name(
        &self,
        pattern: &crate::ast::VariantPattern,
        scrutinee_ty: Option<&Type>,
    ) -> String {
        if let Some(enum_name) = pattern.enum_name.as_deref() {
            if let Some((module_path, item_name)) = enum_name.rsplit_once('.') {
                if let Some(namespace) = self.module_namespace(module_path) {
                    if let Some(enum_info) = namespace
                        .enums
                        .get(item_name)
                        .or_else(|| namespace.all_enums.get(item_name))
                    {
                        return mir_runtime_enum_name(self.program, enum_info);
                    }
                }
            }
            return self
                .resolve_enum_info(enum_name)
                .map(|enum_info| mir_runtime_enum_name(self.program, enum_info))
                .unwrap_or_else(|| enum_name.to_string());
        }
        match scrutinee_ty {
            Some(Type::Named(name, _)) => name.clone(),
            _ => "Unknown".to_string(),
        }
    }

    fn variant_payload_types(
        &self,
        enum_ty: Option<&Type>,
        enum_name: &str,
        variant_name: &str,
    ) -> Option<Vec<Type>> {
        if let Some(enum_ty) = enum_ty {
            match enum_ty {
                Type::Named(name, args) if name == "Option" && args.len() == 1 => {
                    return Some(match variant_name {
                        "Some" => vec![args[0].clone()],
                        "None" => Vec::new(),
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "Result" && args.len() == 2 => {
                    return Some(match variant_name {
                        "Ok" => vec![args[0].clone()],
                        "Err" => vec![args[1].clone()],
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "SendError" && args.len() == 1 => {
                    return Some(match variant_name {
                        "Closed" | "Cancelled" | "TimedOut" | "Full" => vec![args[0].clone()],
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "QueueReceive" && args.len() == 1 => {
                    return Some(match variant_name {
                        "Item" => vec![args[0].clone()],
                        "Closed" | "TimedOut" | "Cancelled" => Vec::new(),
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "TaskResult" && args.len() == 1 => {
                    return Some(match variant_name {
                        "Ready" => vec![args[0].clone()],
                        "Error" => vec![Type::named("str")],
                        "TimedOut" | "Cancelled" => Vec::new(),
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "WaitAny" && args.len() == 1 => {
                    return Some(match variant_name {
                        "Ready" => vec![Type::named("int64"), args[0].clone()],
                        "Error" => vec![Type::named("int64"), Type::named("str")],
                        "TimedOut" | "Cancelled" => Vec::new(),
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "WaitAll" && args.len() == 1 => {
                    return Some(match variant_name {
                        "Ready" => vec![Type::Named("list".to_string(), vec![args[0].clone()])],
                        "Error" => vec![Type::named("int64"), Type::named("str")],
                        "TimedOut" | "Cancelled" => Vec::new(),
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == "SelectOutcome" && args.len() == 2 => {
                    return Some(match variant_name {
                        "Queue" => vec![
                            Type::named("int64"),
                            Type::Named("QueueReceive".to_string(), vec![args[0].clone()]),
                        ],
                        "Task" => vec![
                            Type::named("int64"),
                            Type::Named("TaskResult".to_string(), vec![args[1].clone()]),
                        ],
                        "Deadline" => vec![Type::named("int64")],
                        "Cancelled" => Vec::new(),
                        _ => return None,
                    });
                }
                Type::Named(name, args) if name == enum_name => {
                    let enum_info = self.resolve_enum_info(name)?;
                    let variant = enum_info.variants.get(variant_name)?;
                    let substitutions = enum_info
                        .decl
                        .type_params
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect::<std::collections::HashMap<_, _>>();
                    return Some(
                        variant
                            .payloads
                            .iter()
                            .map(|payload| substitute_type(&payload.ty, &substitutions))
                            .collect(),
                    );
                }
                _ => {}
            }
        }
        let enum_info = self.resolve_enum_info(enum_name)?;
        let variant = enum_info.variants.get(variant_name)?;
        Some(
            variant
                .payloads
                .iter()
                .map(|payload| payload.ty.clone())
                .collect(),
        )
    }

    fn builtin_runtime_member_return_type(
        &self,
        receiver_type: &Type,
        field: &str,
    ) -> Option<Type> {
        let Type::Named(name, args) = receiver_type else {
            return None;
        };
        let bytes_ty = Type::Named("list".to_string(), vec![Type::named("uint8")]);
        let io_error_ty = Type::Named("io.Error".to_string(), Vec::new());
        if args.is_empty()
            && field == "to_float"
            && matches!(
                name.as_str(),
                "int8"
                    | "int16"
                    | "int32"
                    | "int64"
                    | "int128"
                    | "intsize"
                    | "uint8"
                    | "uint16"
                    | "uint32"
                    | "uint64"
                    | "uint128"
                    | "uintsize"
            )
        {
            return Some(Type::named("float64"));
        }
        if args.is_empty()
            && matches!(
                BuiltinMember::resolve(name, field),
                Some(
                    BuiltinMember::IntegerWrappingAdd
                        | BuiltinMember::IntegerWrappingSub
                        | BuiltinMember::IntegerWrappingMul
                        | BuiltinMember::IntegerSaturatingAdd
                        | BuiltinMember::IntegerSaturatingSub
                        | BuiltinMember::IntegerSaturatingMul
                        | BuiltinMember::IntegerWrappingShl
                        | BuiltinMember::IntegerWrappingShr
                        | BuiltinMember::IntegerSaturatingShl
                        | BuiltinMember::IntegerSaturatingShr
                )
            )
        {
            return Some(receiver_type.clone());
        }
        if args.is_empty()
            && field == "to_string"
            && matches!(
                name.as_str(),
                "bool"
                    | "int8"
                    | "int16"
                    | "int32"
                    | "int64"
                    | "int128"
                    | "intsize"
                    | "uint8"
                    | "uint16"
                    | "uint32"
                    | "uint64"
                    | "uint128"
                    | "uintsize"
                    | "float32"
                    | "float64"
            )
        {
            return Some(Type::named("str"));
        }
        match (name.as_str(), field) {
            ("Array", "shape") => Some(Type::Named("list".to_string(), vec![Type::named("int64")])),
            ("Array", "len") => Some(Type::named("int64")),
            ("Array", "clone")
            | ("Array", "wrapping_add")
            | ("Array", "wrapping_sub")
            | ("Array", "wrapping_mul")
            | ("Array", "saturating_add")
            | ("Array", "saturating_sub")
            | ("Array", "saturating_mul") => Some(Type::Named("Array".to_string(), args.clone())),
            ("Array", "get") | ("Array", "set") => Some(Type::Named(
                "Option".to_string(),
                vec![args
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ("Array", "fill") => Some(Type::Unit),
            ("Array", "sum") | ("Array", "min") | ("Array", "max") => args.first().cloned(),
            ("Array", "mean") => Some(Type::named("float64")),
            ("str", "len" | "byte_len") => Some(Type::named("int64")),
            ("str", "contains") | ("str", "starts_with") | ("str", "ends_with") => {
                Some(Type::named("bool"))
            }
            ("str", "split") => Some(Type::Named("list".to_string(), vec![Type::named("str")])),
            ("str", "replace")
            | ("str", "to_lower")
            | ("str", "to_upper")
            | ("str", "trim")
            | ("str", "join")
            | ("str", "clone") => Some(Type::named("str")),
            ("str", "strip_prefix") | ("str", "strip_suffix") => {
                Some(Type::Named("Option".to_string(), vec![Type::named("str")]))
            }
            ("list", "len") => Some(Type::named("int64")),
            ("list", "is_empty") => Some(Type::named("bool")),
            ("list", "copy") => Some(Type::Named("list".to_string(), args.clone())),
            ("list", "append")
            | ("list", "extend")
            | ("list", "clear")
            | ("list", "reverse")
            | ("list", "sort")
            | ("list", "reserve")
            | ("list", "remove")
            | ("list", "swap")
            | ("list", "insert") => Some(Type::Unit),
            ("list", "filter") => Some(Type::Named("list".to_string(), args.clone())),
            ("list", "contains") => Some(Type::named("bool")),
            ("list", "index") | ("list", "count") => Some(Type::named("int64")),
            ("list", "pop") | ("list", "set") => args.first().cloned(),
            ("list", "get") => Some(Type::Named(
                "Option".to_string(),
                vec![args
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ("set", "len") => Some(Type::named("int64")),
            ("set", "is_empty") => Some(Type::named("bool")),
            ("set", "copy") => Some(Type::Named("set".to_string(), args.clone())),
            ("set", "contains") => Some(Type::named("bool")),
            ("set", "add" | "remove" | "discard" | "clear" | "reserve") => Some(Type::Unit),
            ("dict", "len") => Some(Type::named("int64")),
            ("dict", "is_empty") => Some(Type::named("bool")),
            ("dict", "copy") => Some(Type::Named("dict".to_string(), args.clone())),
            ("dict", "contains_key") => Some(Type::named("bool")),
            ("dict", "keys") => Some(Type::Named(
                "list".to_string(),
                vec![args
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ("dict", "values") => Some(Type::Named(
                "list".to_string(),
                vec![args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ("dict", "items") => Some(Type::Named(
                "list".to_string(),
                vec![Type::Tuple(vec![
                    args.first()
                        .cloned()
                        .unwrap_or_else(|| Type::named("Unknown")),
                    args.get(1)
                        .cloned()
                        .unwrap_or_else(|| Type::named("Unknown")),
                ])],
            )),
            ("dict", "clear") | ("dict", "update") | ("dict", "reserve") => Some(Type::Unit),
            ("dict", "get") | ("dict", "set") | ("dict", "remove") => Some(Type::Named(
                "Option".to_string(),
                vec![args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ("Queue", "get" | "__get_in_task_group" | "__get_with_registered_producers") => {
                Some(Type::Named(
                    "QueueReceive".to_string(),
                    vec![args
                        .first()
                        .cloned()
                        .unwrap_or_else(|| Type::named("Unknown"))],
                ))
            }
            ("Queue", "get_or_none") => Some(Type::Named(
                "Option".to_string(),
                vec![args
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Type::named("Unknown"))],
            )),
            ("Queue", "put") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named(
                        "SendError".to_string(),
                        vec![args
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Type::named("Unknown"))],
                    ),
                ],
            )),
            ("Queue", "try_put") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Unit,
                    Type::Named(
                        "SendError".to_string(),
                        vec![args
                            .first()
                            .cloned()
                            .unwrap_or_else(|| Type::named("Unknown"))],
                    ),
                ],
            )),
            ("Queue", "close") | ("TaskGroup", "cancel") | ("TaskGroup", "close") => {
                Some(Type::Unit)
            }
            ("Task", "result") => Some(Type::Named(
                "TaskResult".to_string(),
                vec![args.first().cloned().unwrap_or(Type::Unit)],
            )),
            ("Task", "result_or_none") => Some(Type::Named(
                "Option".to_string(),
                vec![args.first().cloned().unwrap_or(Type::Unit)],
            )),
            ("TaskGroup", "start") | ("TaskGroup", "start_with_stack") => Some(Type::Named(
                "Task".to_string(),
                vec![Type::named("Unknown")],
            )),
            ("TaskGroup", "start_soon") | ("TaskGroup", "start_soon_with_stack") => {
                Some(Type::Unit)
            }
            ("fs.File", "read_all") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("fs.File", "read_bytes") => Some(Type::Named(
                "Result".to_string(),
                vec![bytes_ty.clone(), io_error_ty.clone()],
            )),
            ("fs.File", "write_all") | ("fs.File", "write_bytes") | ("fs.File", "flush") => Some(
                Type::Named("Result".to_string(), vec![Type::Unit, io_error_ty.clone()]),
            ),
            ("fs.File", "close") => Some(Type::Unit),
            ("process.Completed", "status") => Some(Type::named("process.ExitStatus")),
            ("process.Completed", "success") => Some(Type::named("bool")),
            ("process.Completed", "stdout") | ("process.Completed", "stderr") => {
                Some(Type::named("str"))
            }
            ("process.Completed", "stdout_bytes") | ("process.Completed", "stderr_bytes") => {
                Some(bytes_ty.clone())
            }
            ("process.Completed", "check") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::Unit, Type::named("process.Error")],
            )),
            ("net.TcpListener", "accept") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("net.TcpStream"), io_error_ty.clone()],
            )),
            ("net.TcpListener", "local_addr") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.TcpListener", "close") => Some(Type::Unit),
            ("net.TcpStream", "read_all")
            | ("net.TcpStream", "local_addr")
            | ("net.TcpStream", "peer_addr") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.TcpStream", "read_line") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.TcpStream", "read_bytes") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![bytes_ty.clone()]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.TcpStream", "read_exact") => Some(Type::Named(
                "Result".to_string(),
                vec![bytes_ty.clone(), io_error_ty.clone()],
            )),
            ("net.TcpStream", "write_all")
            | ("net.TcpStream", "write_bytes")
            | ("net.TcpStream", "flush")
            | ("net.TcpStream", "shutdown_read")
            | ("net.TcpStream", "shutdown_write")
            | ("net.TcpStream", "shutdown_both") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::Unit, io_error_ty.clone()],
            )),
            ("net.TcpStream", "close") => Some(Type::Unit),
            ("net.UdpSocket", "send_text") | ("net.UdpSocket", "send_bytes") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::Unit, io_error_ty.clone()],
            )),
            ("net.UdpSocket", "recv") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![bytes_ty.clone()]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.UdpSocket", "recv_from") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("net.UdpDatagram")]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.UdpSocket", "local_addr") | ("net.UdpSocket", "peer_addr") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.UdpSocket", "close") => Some(Type::Unit),
            ("net.UdpDatagram", "address") => Some(Type::named("str")),
            ("net.UdpDatagram", "bytes") => Some(bytes_ty.clone()),
            ("net.UdpDatagram", "text") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.HttpListener", "accept") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("net.HttpExchange"), io_error_ty.clone()],
            )),
            ("net.HttpListener", "local_addr") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.HttpListener", "close") => Some(Type::Unit),
            ("net.HttpExchange", "method") | ("net.HttpExchange", "path") => {
                Some(Type::named("str"))
            }
            ("net.HttpExchange", "headers") | ("net.HttpResponse", "headers") => Some(Type::Named(
                "dict".to_string(),
                vec![Type::named("str"), Type::named("str")],
            )),
            ("net.HttpExchange", "body_text") | ("net.HttpResponse", "text") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.HttpExchange", "body_bytes") | ("net.HttpResponse", "bytes") => {
                Some(bytes_ty.clone())
            }
            ("net.HttpExchange", "respond_text") | ("net.HttpExchange", "respond_bytes") => Some(
                Type::Named("Result".to_string(), vec![Type::Unit, io_error_ty.clone()]),
            ),
            ("net.HttpExchange", "close") => Some(Type::Unit),
            ("net.HttpResponse", "status") => Some(Type::named("int32")),
            ("net.HttpResponse", "reason") => Some(Type::named("str")),
            ("net.HttpResponse", "close") => Some(Type::Unit),
            ("net.WebSocketListener", "accept") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("net.WebSocket"), io_error_ty.clone()],
            )),
            ("net.WebSocketListener", "local_addr") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.WebSocketListener", "close") => Some(Type::Unit),
            ("net.WebSocket", "send_text") | ("net.WebSocket", "send_bytes") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::Unit, io_error_ty.clone()],
            )),
            ("net.WebSocket", "recv_text") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.WebSocket", "recv_bytes") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![bytes_ty.clone()]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.WebSocket", "close") => Some(Type::Unit),
            ("net.UnixListener", "accept") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("net.UnixStream"), io_error_ty.clone()],
            )),
            ("net.UnixListener", "close") => Some(Type::Unit),
            ("net.UnixStream", "read_line") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.UnixStream", "read_exact") => Some(Type::Named(
                "Result".to_string(),
                vec![bytes_ty.clone(), io_error_ty.clone()],
            )),
            ("net.UnixStream", "write_all") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::Unit, io_error_ty.clone()],
            )),
            ("net.UnixStream", "close") => Some(Type::Unit),
            ("net.TlsListener", "accept") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("net.TlsStream"), io_error_ty.clone()],
            )),
            ("net.TlsListener", "local_addr") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), io_error_ty.clone()],
            )),
            ("net.TlsListener", "close") => Some(Type::Unit),
            ("net.TlsStream", "read_line") => Some(Type::Named(
                "Result".to_string(),
                vec![
                    Type::Named("Option".to_string(), vec![Type::named("str")]),
                    io_error_ty.clone(),
                ],
            )),
            ("net.TlsStream", "read_exact") => Some(Type::Named(
                "Result".to_string(),
                vec![bytes_ty, io_error_ty.clone()],
            )),
            ("net.TlsStream", "write_all") => Some(Type::Named(
                "Result".to_string(),
                vec![Type::Unit, io_error_ty],
            )),
            ("net.TlsStream", "close") => Some(Type::Unit),
            _ => None,
        }
    }

    fn render_place_expr_option(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(_) | ExprKind::Group(_) | ExprKind::Member { .. } => {
                let rendered = self.render_expr_place(expr);
                if rendered == "<expr>" {
                    None
                } else {
                    Some(rendered)
                }
            }
            ExprKind::Index { object, index } => {
                let ExprKind::Int(index) = index.kind else {
                    return None;
                };
                let index = usize::try_from(index).ok()?;
                self.render_place_expr_option(object)
                    .map(|object| format!("{object}.{index}"))
            }
            _ => None,
        }
    }

    fn render_addressable_place_expr_option(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(name)
                if (self.local_types.contains_key(name)
                    || self.scoped_local_name(name).is_some())
                    && !self.non_owning_roots.contains(name) =>
            {
                Some(self.render_local_name(name))
            }
            ExprKind::Group(inner) => self.render_addressable_place_expr_option(inner),
            ExprKind::Member { object, field } => self
                .render_addressable_place_expr_option(object)
                .map(|object| format!("{object}.{field}")),
            ExprKind::Index { object, index } => {
                let ExprKind::Int(index) = index.kind else {
                    return None;
                };
                let index = usize::try_from(index).ok()?;
                self.render_addressable_place_expr_option(object)
                    .map(|object| format!("{object}.{index}"))
            }
            _ => None,
        }
    }

    fn is_non_owning_place_expr(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Name(name) => self.non_owning_roots.contains(name),
            ExprKind::Group(inner) => self.is_non_owning_place_expr(inner),
            ExprKind::Member { object, .. } => self.is_non_owning_place_expr(object),
            ExprKind::Index { object, .. } => self.is_non_owning_place_expr(object),
            _ => false,
        }
    }

    fn emit(&mut self, instruction: Instruction) {
        let instruction = match instruction {
            Instruction::Assign { target, value }
                if self
                    .view_sources
                    .contains_key(target.split('.').next().unwrap_or_default()) =>
            {
                Instruction::WriteLoan {
                    loan: target,
                    value,
                }
            }
            instruction => instruction,
        };
        let match_writeback_flag = self.match_writeback_flag_for_instruction(&instruction);
        self.blocks[self.current_block]
            .instructions
            .push(instruction);
        if let Some(skip_place) = match_writeback_flag {
            self.blocks[self.current_block]
                .instructions
                .push(Instruction::Assign {
                    target: skip_place,
                    value: Rvalue::Use(Operand::Bool(true)),
                });
        }
    }

    fn match_writeback_flag_for_instruction(&self, instruction: &Instruction) -> Option<String> {
        let state = self.match_writeback_stack.last()?;
        let Instruction::Assign { target, value } = instruction else {
            return None;
        };
        if place_paths_overlap(target, &state.root) || self.rvalue_writes_place(value, &state.root)
        {
            Some(state.skip_place.clone())
        } else {
            None
        }
    }

    fn rvalue_writes_place(&self, value: &Rvalue, place: &str) -> bool {
        let Rvalue::Call { callee, args } = value else {
            return false;
        };
        let receiver_writes_place = match callee {
            CallTarget::Member {
                object,
                field,
                receiver_place: Some(receiver_place),
            } => {
                place_paths_overlap(receiver_place, place)
                    && self.member_call_mutates_receiver(object, field)
            }
            _ => false,
        };
        receiver_writes_place
            || args.iter().any(|arg| {
                arg.writeback_place
                    .as_ref()
                    .is_some_and(|writeback_place| place_paths_overlap(writeback_place, place))
            })
    }

    fn member_call_mutates_receiver(&self, object: &Operand, field: &str) -> bool {
        let Some(receiver_type) = self.infer_operand_type(object) else {
            return false;
        };
        if let Type::Named(receiver_name, _) = &receiver_type {
            if let Some(class) = self.resolve_class_info(receiver_name) {
                if let Some(method) = class.methods.get(field) {
                    return method.decl.receiver == Some(ReceiverKind::BorrowMut);
                }
            }
            if let Some(builtin_member) = BuiltinMember::resolve(receiver_name, field) {
                return builtin_member.receiver_passing() == ReceiverKind::BorrowMut;
            }
        }
        self.trait_method_for_receiver(&receiver_type, field)
            .is_some_and(|(method, _)| method.decl.receiver == Some(ReceiverKind::BorrowMut))
    }

    fn infer_operand_type(&self, operand: &Operand) -> Option<Type> {
        match operand {
            Operand::Place(place) | Operand::MovePlace(place) => {
                self.local_types.get(place).cloned()
            }
            Operand::Function { signature, .. } => Some(signature.as_ref().clone()),
            Operand::Int(_) => Some(Type::named("int64")),
            Operand::Duration(_) => Some(Type::named("Duration")),
            Operand::Float(_) => Some(Type::named("float64")),
            Operand::Bool(_) => Some(Type::named("bool")),
            Operand::String(_) => Some(Type::named("str")),
            Operand::Unit => Some(Type::Unit),
        }
    }

    /// Applies every active mutable-match writeback before control leaves the
    /// arm early.
    ///
    /// ADR-0022 Q3 makes writeback unconditional across exit kinds: a `return`,
    /// `break`, or `continue` inside `match mut` must reconstruct and store the
    /// scrutinee exactly as a normal arm exit does. Without this the mutation
    /// is silently lost, which is the one outcome the ADR forbids.
    fn emit_active_match_writebacks(&mut self) {
        for index in (0..self.match_writeback_stack.len()).rev() {
            let state = self.match_writeback_stack[index].clone();
            let Some(writeback) = state.writeback.clone() else {
                continue;
            };
            let apply_block = self.new_block("match_writeback_exit_apply");
            let resume_block = self.new_block("match_writeback_exit_resume");
            self.terminate(Terminator::Branch {
                condition: Operand::Place(state.skip_place.clone()),
                then_label: self.label(resume_block),
                else_label: self.label(apply_block),
            });
            self.switch_to(apply_block);
            let updated = self.materialize_pattern_writeback(&writeback);
            self.emit(Instruction::Assign {
                target: state.root.clone(),
                value: Rvalue::Use(updated),
            });
            self.terminate(Terminator::Goto(self.label(resume_block)));
            self.switch_to(resume_block);
        }
    }

    fn finish_match_arm_with_writeback(
        &mut self,
        after_block: usize,
        writeback_place: &str,
        writeback: &PatternWriteback,
        skip_place: &str,
    ) {
        let writeback_block = self.new_block("match_writeback_apply");
        let skip_block = self.new_block("match_writeback_skip");
        self.terminate(Terminator::Branch {
            condition: Operand::Place(skip_place.to_string()),
            then_label: self.label(skip_block),
            else_label: self.label(writeback_block),
        });

        self.switch_to(writeback_block);
        let updated = self.materialize_pattern_writeback(writeback);
        self.emit(Instruction::Assign {
            target: writeback_place.to_string(),
            value: Rvalue::Use(updated),
        });
        self.terminate(Terminator::Goto(self.label(after_block)));

        self.switch_to(skip_block);
        self.terminate(Terminator::Goto(self.label(after_block)));
    }

    fn emit_cleanup_range(&mut self, depth: usize, cancel_before_cleanup: bool) {
        let places = self.with_stack[depth..].to_vec();
        for place in places.into_iter().rev() {
            self.emit(Instruction::PopCleanup {
                place,
                cancel_before_cleanup,
            });
        }
    }

    fn active_task_group_place(&self) -> Option<String> {
        self.with_stack.iter().rev().find_map(|place| {
            matches!(
                self.local_types.get(place),
                Some(Type::Named(name, args)) if name == "TaskGroup" && args.is_empty()
            )
            .then(|| place.clone())
        })
    }

    fn terminate(&mut self, terminator: Terminator) {
        self.blocks[self.current_block].terminator = Some(terminator);
    }

    fn current_terminated(&self) -> bool {
        self.blocks[self.current_block].terminator.is_some()
    }

    fn new_temp(&mut self) -> String {
        let name = format!("%t{}", self.temp_counter);
        self.temp_counter += 1;
        name
    }

    fn new_typed_temp(&mut self, ty: Type) -> String {
        let name = self.new_temp();
        self.local_types.insert(name.clone(), ty);
        name
    }

    fn new_temp_for_expr(&mut self, expr: &Expr) -> String {
        if let Some(ty) = self.infer_expr_type(expr) {
            self.new_typed_temp(ty)
        } else {
            self.new_temp()
        }
    }

    fn new_block(&mut self, prefix: &str) -> usize {
        let suffix = self.block_counter;
        self.block_counter += 1;
        self.blocks.push(BasicBlockBuilder {
            label: format!("{}_{}_{}", self.function_name, prefix, suffix),
            instructions: Vec::new(),
            terminator: None,
        });
        self.blocks.len() - 1
    }

    fn label(&self, block_index: usize) -> String {
        self.blocks[block_index].label.clone()
    }

    fn switch_to(&mut self, block_index: usize) {
        self.current_block = block_index;
    }
}

fn tuple_constant_index(expr: &Expr) -> Option<usize> {
    match &expr.kind {
        ExprKind::Int(value) => usize::try_from(*value).ok(),
        ExprKind::Group(inner) => tuple_constant_index(inner),
        _ => None,
    }
}

fn array_coordinate_type(expr: &Expr) -> Type {
    match &expr.kind {
        ExprKind::Tuple(elements) => Type::Tuple(vec![Type::named("int64"); elements.len()]),
        ExprKind::Group(inner) => array_coordinate_type(inner),
        _ => Type::named("int64"),
    }
}

#[cfg(test)]
fn lower_type_ref(type_ref: &crate::ast::TypeRef) -> Type {
    match &type_ref.kind {
        TypeRefKind::Tuple(elements) => Type::Tuple(elements.iter().map(lower_type_ref).collect()),
        TypeRefKind::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|param| FunctionParamContract {
                    name: String::new(),
                    ty: lower_type_ref(&param.ty),
                    passing: resolve_param_passing(param.mode),
                    has_default: false,
                    default_erased: true,
                })
                .collect(),
            return_type: Box::new(lower_type_ref(return_type)),
        },
        TypeRefKind::Named { name, args } => {
            if name == "None" {
                return Type::Unit;
            }
            let name = match name.as_str() {
                "str" => "str",
                "int" => "int64",
                name => name,
            };
            Type::Named(name.to_string(), args.iter().map(lower_type_ref).collect())
        }
    }
}

fn pattern_contains_binding(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Or(pattern) => pattern.alternatives.iter().any(pattern_contains_binding),
        Pattern::Binding(_) => true,
        Pattern::Variant(pattern) => pattern.subpatterns.iter().any(pattern_contains_binding),
        Pattern::Tuple(pattern) => pattern.elements.iter().any(pattern_contains_binding),
        Pattern::Wildcard(_) | Pattern::Literal(_) => false,
    }
}

fn pattern_requires_runtime_test(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Or(pattern) => pattern
            .alternatives
            .iter()
            .any(pattern_requires_runtime_test),
        Pattern::Literal(_) | Pattern::Variant(_) => true,
        Pattern::Tuple(pattern) => pattern.elements.iter().any(pattern_requires_runtime_test),
        Pattern::Binding(_) | Pattern::Wildcard(_) => false,
    }
}

fn is_builtin_class(class: &crate::sema::ClassInfo) -> bool {
    class.is_builtin
}

fn mir_runtime_class_name(program: &Program, class: &crate::sema::ClassInfo) -> String {
    if class.module_name == program.module_name || is_builtin_class(class) {
        class.decl.name.clone()
    } else {
        format!("{}.{}", class.module_name, class.decl.name)
    }
}

fn mir_runtime_enum_name(program: &Program, enum_info: &crate::sema::EnumInfo) -> String {
    if enum_info.module_name == program.module_name {
        return enum_info.decl.name.clone();
    }

    let qualified_name = format!("{}.{}", enum_info.module_name, enum_info.decl.name);
    if matches!(
        qualified_name.as_str(),
        "io.Error" | "process.Error" | "bytes.Error" | "json.Value" | "json.Error"
    ) {
        return qualified_name;
    }

    let module_path = enum_info
        .module_name
        .split('.')
        .map(str::to_string)
        .collect::<Vec<_>>();
    if crate::builtin_modules::builtin_module_namespace(&module_path).is_some() {
        enum_info.decl.name.clone()
    } else {
        qualified_name
    }
}

fn mir_class_method_name(
    program: &Program,
    class: &crate::sema::ClassInfo,
    method_name: &str,
) -> String {
    if class.module_name == program.module_name || is_builtin_class(class) {
        format!("{}.{}", class.decl.name, method_name)
    } else {
        format!("{}::{}.{}", class.module_name, class.decl.name, method_name)
    }
}

fn mir_class_type_name(
    program: &Program,
    class: &crate::sema::ClassInfo,
    fallback: &str,
) -> String {
    class
        .builtin_constructor()
        .map(|constructor| constructor.qualified_name().to_string())
        .unwrap_or_else(|| {
            if class.module_name == program.module_name || is_builtin_class(class) {
                fallback.to_string()
            } else {
                format!("{}.{}", class.module_name, class.decl.name)
            }
        })
}

#[cfg(test)]
#[path = "mir_tests.rs"]
mod tests;
