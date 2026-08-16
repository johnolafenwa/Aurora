use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::ast::{
    Argument, ClassDecl, ConstantDecl, EnumDecl, EnumPayloadFieldDecl, EnumVariantDecl, Expr,
    ExprKind, FunctionDecl, Param, ParamMode, ReceiverKind, TypeRef,
};
use crate::diag::{Diagnostic, Result, Span};
use crate::sema::{
    resolve_param_passing, ClassInfo, EnumInfo, EnumPayloadFieldInfo, EnumVariantInfo,
    FunctionInfo, FunctionParamContract, FunctionSignature, ImportedBinding, ModuleNamespace, Type,
};

fn builtin_span() -> Span {
    Span::new(1, 1)
}

fn type_ref(name: &str, args: Vec<TypeRef>) -> TypeRef {
    TypeRef::named(name, args, false, builtin_span())
}

fn lower_type_ref(type_ref: &TypeRef) -> Type {
    lower_type_ref_with_type_params(type_ref, None)
}

fn lower_generic_type_ref(type_ref: &TypeRef, type_params: &BTreeSet<String>) -> Type {
    lower_type_ref_with_type_params(type_ref, Some(type_params))
}

fn lower_type_ref_with_type_params(
    type_ref: &TypeRef,
    type_params: Option<&BTreeSet<String>>,
) -> Type {
    match &type_ref.kind {
        crate::ast::TypeRefKind::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| lower_type_ref_with_type_params(element, type_params))
                .collect(),
        ),
        crate::ast::TypeRefKind::Named { name, args } if name == "None" => Type::Unit,
        crate::ast::TypeRefKind::Named { name, args }
            if args.is_empty()
                && matches!(type_params, Some(type_params) if type_params.contains(name)) =>
        {
            Type::TypeParam(name.clone())
        }
        crate::ast::TypeRefKind::Named { name, args } => Type::Named(
            name.clone(),
            args.iter()
                .map(|arg| lower_type_ref_with_type_params(arg, type_params))
                .collect(),
        ),
        crate::ast::TypeRefKind::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|param| FunctionParamContract {
                    name: String::new(),
                    ty: lower_type_ref_with_type_params(&param.ty, type_params),
                    passing: resolve_param_passing(param.mode),
                    has_default: false,
                    default_erased: true,
                })
                .collect(),
            return_type: Box::new(lower_type_ref_with_type_params(return_type, type_params)),
        },
    }
}

fn value_param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.to_string(),
        mode: ParamMode::Default,
        ty,
        default: None,
        span: builtin_span(),
    }
}

fn value_param_with_default(name: &str, ty: TypeRef, default: Expr) -> Param {
    Param {
        name: name.to_string(),
        mode: ParamMode::Default,
        ty,
        default: Some(default),
        span: builtin_span(),
    }
}

fn borrow_param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.to_string(),
        mode: ParamMode::Default,
        ty,
        default: None,
        span: builtin_span(),
    }
}

fn own_param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.to_string(),
        mode: ParamMode::Own,
        ty,
        default: None,
        span: builtin_span(),
    }
}

fn name_expr(name: &str) -> Expr {
    Expr {
        kind: ExprKind::Name(name.to_string()),
        span: builtin_span(),
    }
}

fn builtin_omitted_expr() -> Expr {
    Expr {
        kind: ExprKind::BuiltinOmitted,
        span: builtin_span(),
    }
}

fn bool_expr(value: bool) -> Expr {
    Expr {
        kind: ExprKind::Bool(value),
        span: builtin_span(),
    }
}

fn int_expr(value: u128) -> Expr {
    Expr {
        kind: ExprKind::Int(value),
        span: builtin_span(),
    }
}

fn duration_expr(value: i128) -> Expr {
    Expr {
        kind: ExprKind::DurationNanos(value),
        span: builtin_span(),
    }
}

fn float_expr_from_bits(bits: u64) -> Expr {
    Expr {
        kind: ExprKind::Float(f64::from_bits(bits)),
        span: builtin_span(),
    }
}

fn builtin_float_constant(module_name: &str, name: &str, bits: u64) -> crate::sema::ConstantInfo {
    crate::sema::ConstantInfo {
        module_name: module_name.to_string(),
        decl: ConstantDecl {
            public: true,
            name: name.to_string(),
            annotation: Some(type_ref("float64", Vec::new())),
            value: float_expr_from_bits(bits),
            span: builtin_span(),
        },
        ty: Type::named("float64"),
    }
}

fn empty_map_expr() -> Expr {
    Expr {
        kind: ExprKind::Map(Vec::new()),
        span: builtin_span(),
    }
}

fn qualified_zero_arg_call_expr(module_name: &str, name: &str) -> Expr {
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Member {
                    object: Box::new(name_expr(module_name)),
                    field: name.to_string(),
                },
                span: builtin_span(),
            }),
            args: Vec::<Argument>::new(),
        },
        span: builtin_span(),
    }
}

fn function_info(
    module_name: &str,
    name: &str,
    params: Vec<Param>,
    return_type: TypeRef,
) -> FunctionInfo {
    let lowered_params = params
        .iter()
        .map(|param| lower_type_ref(&param.ty))
        .collect::<Vec<_>>();
    let param_passings = params
        .iter()
        .map(|param| resolve_param_passing(param.mode))
        .collect();
    FunctionInfo {
        module_name: module_name.to_string(),
        decl: FunctionDecl {
            public: true,
            name: name.to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            receiver: None,
            params: params.clone(),
            return_type: return_type.clone(),
            view_return: None,
            body: Vec::new(),
            span: builtin_span(),
        },
        signature: FunctionSignature {
            params: lowered_params,
            param_passings,
            return_type: lower_type_ref(&return_type),
            rng_clone_safe_type_params: BTreeSet::new(),
            array_equality_safe_type_params: BTreeSet::new(),
        },
        type_param_bounds: BTreeMap::new(),
    }
}

fn generic_function_info(
    module_name: &str,
    name: &str,
    type_params: Vec<&str>,
    params: Vec<Param>,
    return_type: TypeRef,
) -> FunctionInfo {
    let type_params = type_params
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let type_param_set = type_params.iter().cloned().collect::<BTreeSet<_>>();
    let lowered_params = params
        .iter()
        .map(|param| lower_generic_type_ref(&param.ty, &type_param_set))
        .collect::<Vec<_>>();
    let param_passings = params
        .iter()
        .map(|param| resolve_param_passing(param.mode))
        .collect();
    FunctionInfo {
        module_name: module_name.to_string(),
        decl: FunctionDecl {
            public: true,
            name: name.to_string(),
            type_params,
            type_param_bounds: BTreeMap::new(),
            receiver: None,
            params: params.clone(),
            return_type: return_type.clone(),
            view_return: None,
            body: Vec::new(),
            span: builtin_span(),
        },
        signature: FunctionSignature {
            params: lowered_params,
            param_passings,
            return_type: lower_generic_type_ref(&return_type, &type_param_set),
            rng_clone_safe_type_params: BTreeSet::new(),
            array_equality_safe_type_params: BTreeSet::new(),
        },
        type_param_bounds: BTreeMap::new(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostBuiltinParamMetadata {
    pub name: String,
    pub ty: Type,
    pub passing: ReceiverKind,
    pub required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostBuiltinMetadata {
    pub qualified_name: String,
    pub params: Vec<HostBuiltinParamMetadata>,
    pub return_type: Type,
}

impl HostBuiltinMetadata {
    fn from_function_info(function: &FunctionInfo) -> Self {
        assert_eq!(
            function.decl.params.len(),
            function.signature.params.len(),
            "builtin function parameter declarations and types must stay aligned"
        );
        assert_eq!(
            function.decl.params.len(),
            function.signature.param_passings.len(),
            "builtin function parameter declarations and passing modes must stay aligned"
        );
        Self {
            qualified_name: format!("{}::{}", function.module_name, function.decl.name),
            params: function
                .decl
                .params
                .iter()
                .zip(function.signature.params.iter())
                .zip(function.signature.param_passings.iter().copied())
                .map(|((param, ty), passing)| HostBuiltinParamMetadata {
                    name: param.name.clone(),
                    ty: ty.clone(),
                    passing,
                    required: param.default.is_none(),
                })
                .collect(),
            return_type: function.signature.return_type.clone(),
        }
    }
}

fn class_info(module_name: &str, name: &str) -> ClassInfo {
    ClassInfo {
        module_name: module_name.to_string(),
        is_builtin: true,
        decl: ClassDecl {
            public: true,
            copy: false,
            name: name.to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        fields: BTreeMap::new(),
        methods: BTreeMap::new(),
    }
}

fn error_enum_info() -> EnumInfo {
    let variants = vec![
        ("NotFound", Vec::new()),
        ("PermissionDenied", Vec::new()),
        ("AlreadyExists", Vec::new()),
        ("IsDirectory", Vec::new()),
        ("ConnectionRefused", Vec::new()),
        ("ConnectionReset", Vec::new()),
        ("ConnectionAborted", Vec::new()),
        ("NotConnected", Vec::new()),
        ("AddrInUse", Vec::new()),
        ("AddrNotAvailable", Vec::new()),
        ("BrokenPipe", Vec::new()),
        ("TimedOut", Vec::new()),
        ("WouldBlock", Vec::new()),
        ("UnexpectedEof", Vec::new()),
        ("InvalidInput", Vec::new()),
        ("InvalidData", Vec::new()),
        ("Closed", Vec::new()),
        ("Cancelled", Vec::new()),
        (
            "Other",
            vec![EnumPayloadFieldDecl {
                name: Some("message".to_string()),
                ty: type_ref("str", Vec::new()),
                span: builtin_span(),
            }],
        ),
    ];

    EnumInfo {
        module_name: "io".to_string(),
        decl: EnumDecl {
            public: true,
            name: "Error".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: !payloads.is_empty(),
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name.clone(),
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads: !payloads.is_empty(),
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn io_error_type_ref() -> TypeRef {
    type_ref("io.Error", Vec::new())
}

fn process_error_type_ref() -> TypeRef {
    type_ref("process.Error", Vec::new())
}

fn bytes_type_ref() -> TypeRef {
    type_ref("list", vec![type_ref("uint8", Vec::new())])
}

fn string_map_type_ref() -> TypeRef {
    type_ref(
        "dict",
        vec![type_ref("str", Vec::new()), type_ref("str", Vec::new())],
    )
}

fn result_type_ref(ok: TypeRef) -> TypeRef {
    type_ref("Result", vec![ok, io_error_type_ref()])
}

fn process_result_type_ref(ok: TypeRef) -> TypeRef {
    type_ref("Result", vec![ok, process_error_type_ref()])
}

fn builtin_io_error_type() -> Type {
    Type::Named("io.Error".to_string(), Vec::new())
}

fn process_error_enum_info() -> EnumInfo {
    let variants = vec![
        ("NoCommand", Vec::new()),
        ("TimedOut", Vec::new()),
        ("Cancelled", Vec::new()),
        (
            "Io",
            vec![EnumPayloadFieldDecl {
                name: Some("error".to_string()),
                ty: io_error_type_ref(),
                span: builtin_span(),
            }],
        ),
        (
            "Spawn",
            vec![EnumPayloadFieldDecl {
                name: Some("message".to_string()),
                ty: type_ref("str", Vec::new()),
                span: builtin_span(),
            }],
        ),
        (
            "Other",
            vec![EnumPayloadFieldDecl {
                name: Some("message".to_string()),
                ty: type_ref("str", Vec::new()),
                span: builtin_span(),
            }],
        ),
    ];

    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "Error".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: !payloads.is_empty(),
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name.clone(),
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads: !payloads.is_empty(),
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn process_stdio_enum_info() -> EnumInfo {
    let variants = vec![
        ("Inherit", Vec::new()),
        ("Null", Vec::new()),
        ("Pipe", Vec::new()),
    ];
    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "Stdio".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: false,
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, _)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: Vec::new(),
                        named_payloads: false,
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn process_exit_status_enum_info() -> EnumInfo {
    let variants = vec![
        (
            "Exited",
            vec![EnumPayloadFieldDecl {
                name: Some("code".to_string()),
                ty: type_ref("int32", Vec::new()),
                span: builtin_span(),
            }],
        ),
        (
            "Signaled",
            vec![EnumPayloadFieldDecl {
                name: Some("signal".to_string()),
                ty: type_ref("int32", Vec::new()),
                span: builtin_span(),
            }],
        ),
    ];
    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "ExitStatus".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: true,
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name.clone(),
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads: true,
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn process_wait_enum_info() -> EnumInfo {
    let variants = vec![
        (
            "Exited",
            vec![EnumPayloadFieldDecl {
                name: Some("status".to_string()),
                ty: type_ref("process.ExitStatus", Vec::new()),
                span: builtin_span(),
            }],
        ),
        ("TimedOut", Vec::new()),
        ("Cancelled", Vec::new()),
        (
            "Failed",
            vec![EnumPayloadFieldDecl {
                name: Some("error".to_string()),
                ty: process_error_type_ref(),
                span: builtin_span(),
            }],
        ),
    ];
    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "Wait".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: !payloads.is_empty(),
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name.clone(),
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads: !payloads.is_empty(),
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn process_restart_policy_enum_info() -> EnumInfo {
    let variants = vec![
        ("Never", Vec::new()),
        ("OnFailure", Vec::new()),
        ("Always", Vec::new()),
    ];
    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "RestartPolicy".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: false,
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, _)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: Vec::new(),
                        named_payloads: false,
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn process_supervisor_event_enum_info() -> EnumInfo {
    let variants = vec![
        (
            "Exited",
            vec![
                EnumPayloadFieldDecl {
                    name: Some("name".to_string()),
                    ty: type_ref("str", Vec::new()),
                    span: builtin_span(),
                },
                EnumPayloadFieldDecl {
                    name: Some("status".to_string()),
                    ty: type_ref("process.ExitStatus", Vec::new()),
                    span: builtin_span(),
                },
                EnumPayloadFieldDecl {
                    name: Some("restart_count".to_string()),
                    ty: type_ref("int32", Vec::new()),
                    span: builtin_span(),
                },
            ],
        ),
        (
            "Restarted",
            vec![
                EnumPayloadFieldDecl {
                    name: Some("name".to_string()),
                    ty: type_ref("str", Vec::new()),
                    span: builtin_span(),
                },
                EnumPayloadFieldDecl {
                    name: Some("status".to_string()),
                    ty: type_ref("process.ExitStatus", Vec::new()),
                    span: builtin_span(),
                },
                EnumPayloadFieldDecl {
                    name: Some("restart_count".to_string()),
                    ty: type_ref("int32", Vec::new()),
                    span: builtin_span(),
                },
            ],
        ),
        (
            "Failed",
            vec![
                EnumPayloadFieldDecl {
                    name: Some("name".to_string()),
                    ty: type_ref("str", Vec::new()),
                    span: builtin_span(),
                },
                EnumPayloadFieldDecl {
                    name: Some("error".to_string()),
                    ty: process_error_type_ref(),
                    span: builtin_span(),
                },
                EnumPayloadFieldDecl {
                    name: Some("restart_count".to_string()),
                    ty: type_ref("int32", Vec::new()),
                    span: builtin_span(),
                },
            ],
        ),
    ];
    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "SupervisorEvent".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: true,
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name.clone(),
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads: true,
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn process_supervisor_wait_enum_info() -> EnumInfo {
    let variants = vec![
        (
            "Event",
            vec![EnumPayloadFieldDecl {
                name: Some("event".to_string()),
                ty: type_ref("process.SupervisorEvent", Vec::new()),
                span: builtin_span(),
            }],
        ),
        ("TimedOut", Vec::new()),
        ("Cancelled", Vec::new()),
    ];
    EnumInfo {
        module_name: "process".to_string(),
        decl: EnumDecl {
            public: true,
            name: "SupervisorWait".to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: !payloads.is_empty(),
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name.clone(),
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads: !payloads.is_empty(),
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn io_namespace() -> ModuleNamespace {
    let mut functions = BTreeMap::new();
    for function in [
        function_info(
            "io",
            "write",
            vec![value_param("text", type_ref("str", Vec::new()))],
            type_ref(
                "Result",
                vec![type_ref("None", Vec::new()), io_error_type_ref()],
            ),
        ),
        function_info(
            "io",
            "flush",
            Vec::new(),
            type_ref(
                "Result",
                vec![type_ref("None", Vec::new()), io_error_type_ref()],
            ),
        ),
        function_info(
            "io",
            "read_line",
            Vec::new(),
            type_ref(
                "Result",
                vec![
                    type_ref("Option", vec![type_ref("str", Vec::new())]),
                    io_error_type_ref(),
                ],
            ),
        ),
    ] {
        functions.insert(function.decl.name.clone(), function);
    }

    let error = error_enum_info();
    let mut enums = BTreeMap::new();
    enums.insert(error.decl.name.clone(), error.clone());

    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "io".to_string(),
        path: "io".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: enums.clone(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: enums,
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn fs_namespace() -> ModuleNamespace {
    let file = class_info("fs", "File");
    let mut classes = BTreeMap::new();
    classes.insert(file.decl.name.clone(), file.clone());

    let result_none = type_ref(
        "Result",
        vec![type_ref("None", Vec::new()), io_error_type_ref()],
    );
    let result_string = type_ref(
        "Result",
        vec![type_ref("str", Vec::new()), io_error_type_ref()],
    );
    let result_file = type_ref(
        "Result",
        vec![type_ref("fs.File", Vec::new()), io_error_type_ref()],
    );
    let result_bytes = type_ref("Result", vec![bytes_type_ref(), io_error_type_ref()]);
    let result_vec_string = type_ref(
        "Result",
        vec![
            type_ref("list", vec![type_ref("str", Vec::new())]),
            io_error_type_ref(),
        ],
    );

    let mut functions = BTreeMap::new();
    for function in [
        function_info(
            "fs",
            "exists",
            vec![value_param("path", type_ref("str", Vec::new()))],
            type_ref("bool", Vec::new()),
        ),
        function_info(
            "fs",
            "read_to_string",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_string.clone(),
        ),
        function_info(
            "fs",
            "read_bytes",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_bytes.clone(),
        ),
        function_info(
            "fs",
            "write_string",
            vec![
                value_param("path", type_ref("str", Vec::new())),
                value_param("text", type_ref("str", Vec::new())),
            ],
            result_none.clone(),
        ),
        function_info(
            "fs",
            "write_bytes",
            vec![
                value_param("path", type_ref("str", Vec::new())),
                value_param("bytes", bytes_type_ref()),
            ],
            result_none.clone(),
        ),
        function_info(
            "fs",
            "append_string",
            vec![
                value_param("path", type_ref("str", Vec::new())),
                value_param("text", type_ref("str", Vec::new())),
            ],
            result_none.clone(),
        ),
        function_info(
            "fs",
            "append_bytes",
            vec![
                value_param("path", type_ref("str", Vec::new())),
                value_param("bytes", bytes_type_ref()),
            ],
            result_none.clone(),
        ),
        function_info(
            "fs",
            "create_dir",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_none.clone(),
        ),
        function_info(
            "fs",
            "read_dir",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_vec_string,
        ),
        function_info(
            "fs",
            "remove_file",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_none.clone(),
        ),
        function_info(
            "fs",
            "open",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_file.clone(),
        ),
        function_info(
            "fs",
            "create",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_file.clone(),
        ),
        function_info(
            "fs",
            "append",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_file,
        ),
    ] {
        functions.insert(function.decl.name.clone(), function);
    }

    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "fs".to_string(),
        path: "fs".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: classes.clone(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: classes,
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn net_namespace() -> ModuleNamespace {
    let stream = class_info("net", "TcpStream");
    let listener = class_info("net", "TcpListener");
    let udp_socket = class_info("net", "UdpSocket");
    let udp_datagram = class_info("net", "UdpDatagram");
    let http_listener = class_info("net", "HttpListener");
    let http_exchange = class_info("net", "HttpExchange");
    let http_response = class_info("net", "HttpResponse");
    let websocket_listener = class_info("net", "WebSocketListener");
    let websocket = class_info("net", "WebSocket");
    let unix_listener = class_info("net", "UnixListener");
    let unix_stream = class_info("net", "UnixStream");
    let tls_listener = class_info("net", "TlsListener");
    let tls_stream = class_info("net", "TlsStream");
    let mut classes = BTreeMap::new();
    classes.insert(stream.decl.name.clone(), stream.clone());
    classes.insert(listener.decl.name.clone(), listener.clone());
    classes.insert(udp_socket.decl.name.clone(), udp_socket.clone());
    classes.insert(udp_datagram.decl.name.clone(), udp_datagram.clone());
    classes.insert(http_listener.decl.name.clone(), http_listener.clone());
    classes.insert(http_exchange.decl.name.clone(), http_exchange.clone());
    classes.insert(http_response.decl.name.clone(), http_response.clone());
    classes.insert(
        websocket_listener.decl.name.clone(),
        websocket_listener.clone(),
    );
    classes.insert(websocket.decl.name.clone(), websocket.clone());
    classes.insert(unix_listener.decl.name.clone(), unix_listener.clone());
    classes.insert(unix_stream.decl.name.clone(), unix_stream.clone());
    classes.insert(tls_listener.decl.name.clone(), tls_listener.clone());
    classes.insert(tls_stream.decl.name.clone(), tls_stream.clone());

    let mut functions = BTreeMap::new();
    for function in [
        function_info(
            "net",
            "connect",
            vec![value_param("address", type_ref("str", Vec::new()))],
            type_ref(
                "Result",
                vec![type_ref("net.TcpStream", Vec::new()), io_error_type_ref()],
            ),
        ),
        function_info(
            "net",
            "connect_timeout",
            vec![
                value_param("address", type_ref("str", Vec::new())),
                value_param("timeout", type_ref("Duration", Vec::new())),
            ],
            type_ref(
                "Result",
                vec![type_ref("net.TcpStream", Vec::new()), io_error_type_ref()],
            ),
        ),
        function_info(
            "net",
            "listen",
            vec![value_param("address", type_ref("str", Vec::new()))],
            type_ref(
                "Result",
                vec![type_ref("net.TcpListener", Vec::new()), io_error_type_ref()],
            ),
        ),
        function_info(
            "net",
            "udp_bind",
            vec![value_param("address", type_ref("str", Vec::new()))],
            result_type_ref(type_ref("net.UdpSocket", Vec::new())),
        ),
        function_info(
            "net",
            "unix_listen",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_type_ref(type_ref("net.UnixListener", Vec::new())),
        ),
        function_info(
            "net",
            "unix_connect",
            vec![value_param("path", type_ref("str", Vec::new()))],
            result_type_ref(type_ref("net.UnixStream", Vec::new())),
        ),
        function_info(
            "net",
            "unix_connect_timeout",
            vec![
                value_param("path", type_ref("str", Vec::new())),
                value_param("timeout", type_ref("Duration", Vec::new())),
            ],
            result_type_ref(type_ref("net.UnixStream", Vec::new())),
        ),
        function_info(
            "net",
            "tls_listen",
            vec![
                value_param("address", type_ref("str", Vec::new())),
                value_param("cert_pem_path", type_ref("str", Vec::new())),
                value_param("key_pem_path", type_ref("str", Vec::new())),
            ],
            result_type_ref(type_ref("net.TlsListener", Vec::new())),
        ),
        function_info(
            "net",
            "tls_connect",
            vec![
                value_param("address", type_ref("str", Vec::new())),
                value_param("server_name", type_ref("str", Vec::new())),
                value_param("ca_pem_path", type_ref("str", Vec::new())),
            ],
            result_type_ref(type_ref("net.TlsStream", Vec::new())),
        ),
        function_info(
            "net",
            "tls_connect_timeout",
            vec![
                value_param("address", type_ref("str", Vec::new())),
                value_param("server_name", type_ref("str", Vec::new())),
                value_param("ca_pem_path", type_ref("str", Vec::new())),
                value_param("timeout", type_ref("Duration", Vec::new())),
            ],
            result_type_ref(type_ref("net.TlsStream", Vec::new())),
        ),
        function_info(
            "net",
            "http_listen",
            vec![value_param("address", type_ref("str", Vec::new()))],
            result_type_ref(type_ref("net.HttpListener", Vec::new())),
        ),
        function_info(
            "net",
            "http_request_text",
            vec![
                value_param("method", type_ref("str", Vec::new())),
                value_param("url", type_ref("str", Vec::new())),
                value_param("body", type_ref("str", Vec::new())),
                value_param("headers", string_map_type_ref()),
            ],
            result_type_ref(type_ref("net.HttpResponse", Vec::new())),
        ),
        function_info(
            "net",
            "http_request_text_timeout",
            vec![
                value_param("method", type_ref("str", Vec::new())),
                value_param("url", type_ref("str", Vec::new())),
                value_param("body", type_ref("str", Vec::new())),
                value_param("headers", string_map_type_ref()),
                value_param("timeout", type_ref("Duration", Vec::new())),
            ],
            result_type_ref(type_ref("net.HttpResponse", Vec::new())),
        ),
        function_info(
            "net",
            "http_request_bytes",
            vec![
                value_param("method", type_ref("str", Vec::new())),
                value_param("url", type_ref("str", Vec::new())),
                value_param("bytes", bytes_type_ref()),
                value_param("headers", string_map_type_ref()),
            ],
            result_type_ref(type_ref("net.HttpResponse", Vec::new())),
        ),
        function_info(
            "net",
            "http_request_bytes_timeout",
            vec![
                value_param("method", type_ref("str", Vec::new())),
                value_param("url", type_ref("str", Vec::new())),
                value_param("bytes", bytes_type_ref()),
                value_param("headers", string_map_type_ref()),
                value_param("timeout", type_ref("Duration", Vec::new())),
            ],
            result_type_ref(type_ref("net.HttpResponse", Vec::new())),
        ),
        function_info(
            "net",
            "websocket_listen",
            vec![value_param("address", type_ref("str", Vec::new()))],
            result_type_ref(type_ref("net.WebSocketListener", Vec::new())),
        ),
        function_info(
            "net",
            "websocket_connect",
            vec![value_param("url", type_ref("str", Vec::new()))],
            result_type_ref(type_ref("net.WebSocket", Vec::new())),
        ),
        function_info(
            "net",
            "websocket_connect_timeout",
            vec![
                value_param("url", type_ref("str", Vec::new())),
                value_param("timeout", type_ref("Duration", Vec::new())),
            ],
            result_type_ref(type_ref("net.WebSocket", Vec::new())),
        ),
    ] {
        functions.insert(function.decl.name.clone(), function);
    }

    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "net".to_string(),
        path: "net".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: classes.clone(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: classes,
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn process_namespace() -> ModuleNamespace {
    let child = class_info("process", "Child");
    let pipe = class_info("process", "Pipe");
    let completed = class_info("process", "Completed");
    let supervisor = class_info("process", "Supervisor");
    let mut classes = BTreeMap::new();
    classes.insert(child.decl.name.clone(), child.clone());
    classes.insert(pipe.decl.name.clone(), pipe.clone());
    classes.insert(completed.decl.name.clone(), completed.clone());
    classes.insert(supervisor.decl.name.clone(), supervisor.clone());

    let stdio = process_stdio_enum_info();
    let exit_status = process_exit_status_enum_info();
    let wait = process_wait_enum_info();
    let restart_policy = process_restart_policy_enum_info();
    let supervisor_event = process_supervisor_event_enum_info();
    let supervisor_wait = process_supervisor_wait_enum_info();
    let error = process_error_enum_info();
    let mut enums = BTreeMap::new();
    enums.insert(stdio.decl.name.clone(), stdio.clone());
    enums.insert(exit_status.decl.name.clone(), exit_status.clone());
    enums.insert(wait.decl.name.clone(), wait.clone());
    enums.insert(restart_policy.decl.name.clone(), restart_policy.clone());
    enums.insert(supervisor_event.decl.name.clone(), supervisor_event.clone());
    enums.insert(supervisor_wait.decl.name.clone(), supervisor_wait.clone());
    enums.insert(error.decl.name.clone(), error.clone());

    let mut functions = BTreeMap::new();
    for function in [
        function_info(
            "process",
            "inherit",
            Vec::new(),
            type_ref("process.Stdio", Vec::new()),
        ),
        function_info(
            "process",
            "null",
            Vec::new(),
            type_ref("process.Stdio", Vec::new()),
        ),
        function_info(
            "process",
            "pipe",
            Vec::new(),
            type_ref("process.Stdio", Vec::new()),
        ),
        function_info(
            "process",
            "supervisor",
            Vec::new(),
            type_ref("process.Supervisor", Vec::new()),
        ),
        function_info(
            "process",
            "start",
            vec![
                value_param(
                    "command",
                    type_ref("list", vec![type_ref("str", Vec::new())]),
                ),
                value_param_with_default(
                    "cwd",
                    type_ref("Option", vec![type_ref("str", Vec::new())]),
                    name_expr("None"),
                ),
                value_param_with_default("env", string_map_type_ref(), empty_map_expr()),
                value_param_with_default(
                    "stdin",
                    type_ref("process.Stdio", Vec::new()),
                    qualified_zero_arg_call_expr("process", "null"),
                ),
                value_param_with_default(
                    "stdout",
                    type_ref("process.Stdio", Vec::new()),
                    qualified_zero_arg_call_expr("process", "inherit"),
                ),
                value_param_with_default(
                    "stderr",
                    type_ref("process.Stdio", Vec::new()),
                    qualified_zero_arg_call_expr("process", "inherit"),
                ),
                value_param_with_default("group", type_ref("bool", Vec::new()), bool_expr(false)),
            ],
            process_result_type_ref(type_ref("process.Child", Vec::new())),
        ),
        function_info(
            "process",
            "run",
            vec![
                value_param(
                    "command",
                    type_ref("list", vec![type_ref("str", Vec::new())]),
                ),
                value_param_with_default(
                    "cwd",
                    type_ref("Option", vec![type_ref("str", Vec::new())]),
                    name_expr("None"),
                ),
                value_param_with_default("env", string_map_type_ref(), empty_map_expr()),
                value_param_with_default(
                    "stdin",
                    type_ref("process.Stdio", Vec::new()),
                    qualified_zero_arg_call_expr("process", "null"),
                ),
                value_param_with_default(
                    "stdout",
                    type_ref("process.Stdio", Vec::new()),
                    qualified_zero_arg_call_expr("process", "pipe"),
                ),
                value_param_with_default(
                    "stderr",
                    type_ref("process.Stdio", Vec::new()),
                    qualified_zero_arg_call_expr("process", "pipe"),
                ),
                value_param_with_default(
                    "timeout",
                    type_ref("Duration", Vec::new()),
                    builtin_omitted_expr(),
                ),
                value_param_with_default("group", type_ref("bool", Vec::new()), bool_expr(false)),
            ],
            process_result_type_ref(type_ref("process.Completed", Vec::new())),
        ),
    ] {
        functions.insert(function.decl.name.clone(), function);
    }

    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "process".to_string(),
        path: "process".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: classes.clone(),
        enums: enums.clone(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: classes,
        all_enums: enums,
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn random_namespace() -> ModuleNamespace {
    let rng = class_info("random", "Rng");
    let classes = BTreeMap::from([("Rng".to_string(), rng)]);
    let functions = [
        function_info(
            "random",
            "secure_int",
            vec![
                value_param("lo", type_ref("int64", Vec::new())),
                value_param("hi", type_ref("int64", Vec::new())),
            ],
            type_ref("int64", Vec::new()),
        ),
        function_info(
            "random",
            "secure_bytes",
            vec![value_param("n", type_ref("int64", Vec::new()))],
            type_ref("list", vec![type_ref("uint8", Vec::new())]),
        ),
    ]
    .into_iter()
    .map(|function| (function.decl.name.clone(), function))
    .collect::<BTreeMap<_, _>>();

    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "random".to_string(),
        path: "random".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: classes.clone(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: classes,
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn control_namespace() -> ModuleNamespace {
    let result_type = type_ref(
        "Result",
        vec![type_ref("T", Vec::new()), type_ref("E", Vec::new())],
    );
    function_only_namespace(
        "control",
        vec![generic_function_info(
            "control",
            "retry",
            vec!["T", "E"],
            vec![
                borrow_param(
                    "worker",
                    TypeRef::function(Vec::new(), result_type.clone(), builtin_span()),
                ),
                value_param_with_default(
                    "max_attempts",
                    type_ref("int32", Vec::new()),
                    int_expr(3),
                ),
                value_param_with_default(
                    "initial_backoff",
                    type_ref("Duration", Vec::new()),
                    duration_expr(0),
                ),
            ],
            result_type,
        )],
    )
}

fn function_only_namespace(name: &str, functions: Vec<FunctionInfo>) -> ModuleNamespace {
    let functions = functions
        .into_iter()
        .map(|function| (function.decl.name.clone(), function))
        .collect::<BTreeMap<_, _>>();
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: name.to_string(),
        path: name.to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: BTreeMap::new(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: BTreeMap::new(),
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn sys_namespace() -> ModuleNamespace {
    function_only_namespace(
        "sys",
        vec![
            function_info(
                "sys",
                "args",
                Vec::new(),
                type_ref("list", vec![type_ref("str", Vec::new())]),
            ),
            function_info(
                "sys",
                "env",
                vec![value_param("name", type_ref("str", Vec::new()))],
                type_ref("Option", vec![type_ref("str", Vec::new())]),
            ),
            function_info(
                "sys",
                "current_dir",
                Vec::new(),
                result_type_ref(type_ref("str", Vec::new())),
            ),
            function_info(
                "sys",
                "unix_time_ms",
                Vec::new(),
                type_ref("int64", Vec::new()),
            ),
            function_info(
                "sys",
                "monotonic_time_ms",
                Vec::new(),
                type_ref("int64", Vec::new()),
            ),
        ],
    )
}

fn path_namespace() -> ModuleNamespace {
    let string = || type_ref("str", Vec::new());
    let optional_string = || type_ref("Option", vec![string()]);
    function_only_namespace(
        "path",
        vec![
            function_info(
                "path",
                "join",
                vec![
                    value_param("base", string()),
                    value_param("child", string()),
                ],
                string(),
            ),
            function_info(
                "path",
                "parent",
                vec![value_param("path", string())],
                optional_string(),
            ),
            function_info(
                "path",
                "file_name",
                vec![value_param("path", string())],
                optional_string(),
            ),
            function_info(
                "path",
                "extension",
                vec![value_param("path", string())],
                optional_string(),
            ),
            function_info(
                "path",
                "is_absolute",
                vec![value_param("path", string())],
                type_ref("bool", Vec::new()),
            ),
        ],
    )
}

fn math_namespace() -> ModuleNamespace {
    let float = || type_ref("float64", Vec::new());
    let unary_float =
        |name| function_info("math", name, vec![value_param("value", float())], float());
    let unary_int = |name| {
        function_info(
            "math",
            name,
            vec![value_param("value", float())],
            type_ref("int64", Vec::new()),
        )
    };
    let mut namespace = function_only_namespace(
        "math",
        vec![
            unary_int("floor"),
            unary_int("ceil"),
            unary_int("trunc"),
            function_info(
                "math",
                "pow",
                vec![
                    value_param("base", float()),
                    value_param("exponent", float()),
                ],
                float(),
            ),
            unary_float("exp"),
            unary_float("log"),
            unary_float("log2"),
            unary_float("log10"),
            unary_float("sin"),
            unary_float("cos"),
            unary_float("tan"),
        ],
    );
    let constants = [
        ("pi", 0x4009_21fb_5444_2d18_u64),
        ("e", 0x4005_bf0a_8b14_5769_u64),
        ("inf", 0x7ff0_0000_0000_0000_u64),
        ("nan", 0x7ff8_0000_0000_0000_u64),
    ]
    .into_iter()
    .map(|(name, bits)| (name.to_string(), builtin_float_constant("math", name, bits)))
    .collect::<BTreeMap<_, _>>();
    namespace.constants = constants.clone();
    namespace.all_constants = constants;
    namespace
}

fn serialization_namespace(name: &str) -> ModuleNamespace {
    let result_string = || {
        type_ref(
            "Result",
            vec![type_ref("str", Vec::new()), type_ref("str", Vec::new())],
        )
    };
    let result_map = || {
        type_ref(
            "Result",
            vec![string_map_type_ref(), type_ref("str", Vec::new())],
        )
    };
    function_only_namespace(
        name,
        vec![
            function_info(
                name,
                "is_valid",
                vec![value_param("text", type_ref("str", Vec::new()))],
                type_ref("bool", Vec::new()),
            ),
            function_info(
                name,
                "stringify_map",
                vec![value_param("value", string_map_type_ref())],
                result_string(),
            ),
            function_info(
                name,
                "parse_string_map",
                vec![value_param("text", type_ref("str", Vec::new()))],
                result_map(),
            ),
        ],
    )
}

fn builtin_enum_info(
    module_name: &str,
    enum_name: &str,
    variants: Vec<(&str, Vec<EnumPayloadFieldDecl>, bool)>,
) -> EnumInfo {
    EnumInfo {
        module_name: module_name.to_string(),
        decl: EnumDecl {
            public: true,
            name: enum_name.to_string(),
            type_params: Vec::new(),
            type_param_bounds: BTreeMap::new(),
            variants: variants
                .iter()
                .map(|(name, payloads, named_payloads)| EnumVariantDecl {
                    name: (*name).to_string(),
                    payloads: payloads.clone(),
                    named_payloads: *named_payloads,
                    span: builtin_span(),
                })
                .collect(),
            span: builtin_span(),
        },
        type_param_bounds: BTreeMap::new(),
        variants: variants
            .into_iter()
            .map(|(name, payloads, named_payloads)| {
                (
                    name.to_string(),
                    EnumVariantInfo {
                        payloads: payloads
                            .into_iter()
                            .map(|payload| EnumPayloadFieldInfo {
                                name: payload.name,
                                ty: lower_type_ref(&payload.ty),
                                span: payload.span,
                            })
                            .collect(),
                        named_payloads,
                        span: builtin_span(),
                    },
                )
            })
            .collect(),
    }
}

fn json_value_type_ref() -> TypeRef {
    type_ref("json.Value", Vec::new())
}

fn json_value_enum_info() -> EnumInfo {
    let positional = |ty| {
        vec![EnumPayloadFieldDecl {
            name: None,
            ty,
            span: builtin_span(),
        }]
    };
    builtin_enum_info(
        "json",
        "Value",
        vec![
            ("Null", Vec::new(), false),
            ("Bool", positional(type_ref("bool", Vec::new())), false),
            ("Int", positional(type_ref("int64", Vec::new())), false),
            ("Float", positional(type_ref("float64", Vec::new())), false),
            ("String", positional(type_ref("str", Vec::new())), false),
            (
                "Array",
                positional(type_ref("list", vec![json_value_type_ref()])),
                false,
            ),
            (
                "Object",
                positional(type_ref(
                    "dict",
                    vec![type_ref("str", Vec::new()), json_value_type_ref()],
                )),
                false,
            ),
        ],
    )
}

fn json_error_enum_info() -> EnumInfo {
    let named = |name: &str, ty| EnumPayloadFieldDecl {
        name: Some(name.to_string()),
        ty,
        span: builtin_span(),
    };
    let location = || {
        vec![
            named("line", type_ref("int32", Vec::new())),
            named("column", type_ref("int32", Vec::new())),
        ]
    };
    let mut syntax = vec![named("message", type_ref("str", Vec::new()))];
    syntax.extend(location());
    let mut nesting = vec![named("limit", type_ref("int32", Vec::new()))];
    nesting.extend(location());
    builtin_enum_info(
        "json",
        "Error",
        vec![
            ("Syntax", syntax, true),
            ("NumberOutOfRange", location(), true),
            ("NestingTooDeep", nesting, true),
            (
                "InputTooLarge",
                vec![
                    named("actual_bytes", type_ref("int64", Vec::new())),
                    named("limit_bytes", type_ref("int64", Vec::new())),
                ],
                true,
            ),
        ],
    )
}

fn json_namespace() -> ModuleNamespace {
    let option = |inner| type_ref("Option", vec![inner]);
    let json_value = json_value_type_ref;
    let functions_to_add = vec![
        function_info(
            "json",
            "parse",
            vec![value_param("text", type_ref("str", Vec::new()))],
            type_ref(
                "Result",
                vec![json_value(), type_ref("json.Error", Vec::new())],
            ),
        ),
        function_info(
            "json",
            "dumps",
            vec![
                value_param("value", json_value()),
                value_param_with_default(
                    "indent",
                    option(type_ref("int64", Vec::new())),
                    name_expr("None"),
                ),
            ],
            type_ref("str", Vec::new()),
        ),
        function_info(
            "json",
            "is_null",
            vec![borrow_param("value", json_value())],
            type_ref("bool", Vec::new()),
        ),
        function_info(
            "json",
            "as_bool",
            vec![borrow_param("value", json_value())],
            option(type_ref("bool", Vec::new())),
        ),
        function_info(
            "json",
            "as_int",
            vec![borrow_param("value", json_value())],
            option(type_ref("int64", Vec::new())),
        ),
        function_info(
            "json",
            "as_float",
            vec![borrow_param("value", json_value())],
            option(type_ref("float64", Vec::new())),
        ),
        function_info(
            "json",
            "into_string",
            vec![own_param("value", json_value())],
            option(type_ref("str", Vec::new())),
        ),
        function_info(
            "json",
            "into_array",
            vec![own_param("value", json_value())],
            option(type_ref("list", vec![json_value()])),
        ),
        function_info(
            "json",
            "into_object",
            vec![own_param("value", json_value())],
            option(type_ref(
                "dict",
                vec![type_ref("str", Vec::new()), json_value()],
            )),
        ),
    ];

    let mut functions = serialization_namespace("json").functions;
    functions.extend(
        functions_to_add
            .into_iter()
            .map(|function| (function.decl.name.clone(), function)),
    );
    let value = json_value_enum_info();
    let error = json_error_enum_info();
    let enums = BTreeMap::from([
        (value.decl.name.clone(), value),
        (error.decl.name.clone(), error),
    ]);
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "json".to_string(),
        path: "json".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: enums.clone(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: enums,
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn bytes_vec_type_ref() -> TypeRef {
    type_ref("list", vec![type_ref("uint8", Vec::new())])
}

fn bytes_error_enum_info() -> EnumInfo {
    let named = |name: &str, ty| EnumPayloadFieldDecl {
        name: Some(name.to_string()),
        ty,
        span: builtin_span(),
    };
    builtin_enum_info(
        "bytes",
        "Error",
        vec![
            (
                "InvalidUtf8",
                vec![named("index", type_ref("int32", Vec::new()))],
                true,
            ),
            (
                "InvalidHexLength",
                vec![named("length", type_ref("int32", Vec::new()))],
                true,
            ),
            (
                "InvalidHexDigit",
                vec![
                    named("index", type_ref("int32", Vec::new())),
                    named("byte", type_ref("uint8", Vec::new())),
                ],
                true,
            ),
            (
                "InvalidBase64",
                vec![named("index", type_ref("int32", Vec::new()))],
                true,
            ),
        ],
    )
}

fn bytes_namespace() -> ModuleNamespace {
    let bytes_result = || {
        type_ref(
            "Result",
            vec![bytes_vec_type_ref(), type_ref("bytes.Error", Vec::new())],
        )
    };
    let functions = vec![
        function_info(
            "bytes",
            "hex_encode",
            vec![value_param("value", bytes_vec_type_ref())],
            type_ref("str", Vec::new()),
        ),
        function_info(
            "bytes",
            "hex_decode",
            vec![value_param("text", type_ref("str", Vec::new()))],
            bytes_result(),
        ),
        function_info(
            "bytes",
            "base64_encode",
            vec![value_param("value", bytes_vec_type_ref())],
            type_ref("str", Vec::new()),
        ),
        function_info(
            "bytes",
            "base64_decode",
            vec![value_param("text", type_ref("str", Vec::new()))],
            bytes_result(),
        ),
        function_info(
            "bytes",
            "sha256",
            vec![value_param("value", bytes_vec_type_ref())],
            bytes_vec_type_ref(),
        ),
        function_info(
            "bytes",
            "sha256_string",
            vec![value_param("text", type_ref("str", Vec::new()))],
            bytes_vec_type_ref(),
        ),
    ]
    .into_iter()
    .map(|function| (function.decl.name.clone(), function))
    .collect::<BTreeMap<_, _>>();
    let error = bytes_error_enum_info();
    let enums = BTreeMap::from([(error.decl.name.clone(), error)]);
    ModuleNamespace {
        constants: BTreeMap::new(),
        all_constants: BTreeMap::new(),
        name: "bytes".to_string(),
        path: "bytes".to_string(),
        source_path: None,
        modules: BTreeMap::new(),
        functions: functions.clone(),
        extern_functions: BTreeMap::new(),
        opaque_handles: BTreeMap::new(),
        classes: BTreeMap::new(),
        enums: enums.clone(),
        traits: BTreeMap::new(),
        trait_impls: Vec::new(),
        all_functions: functions,
        all_extern_functions: BTreeMap::new(),
        all_opaque_handles: BTreeMap::new(),
        all_classes: BTreeMap::new(),
        all_enums: enums,
        all_traits: BTreeMap::new(),
        imported_modules: BTreeMap::new(),
        closures: BTreeMap::new(),
        comprehensions: BTreeMap::new(),
    }
}

fn telemetry_namespace(name: &str) -> ModuleNamespace {
    let functions = match name {
        "metrics" => vec![
            function_info(
                name,
                "increment",
                vec![
                    value_param("name", type_ref("str", Vec::new())),
                    value_param("value", type_ref("int64", Vec::new())),
                ],
                type_ref("None", Vec::new()),
            ),
            function_info(
                name,
                "get",
                vec![value_param("name", type_ref("str", Vec::new()))],
                type_ref("int64", Vec::new()),
            ),
            function_info(name, "reset", Vec::new(), type_ref("None", Vec::new())),
        ],
        "log" => ["debug", "info", "warn", "error"]
            .into_iter()
            .map(|level| {
                function_info(
                    name,
                    level,
                    vec![
                        value_param("message", type_ref("str", Vec::new())),
                        value_param("fields", string_map_type_ref()),
                    ],
                    type_ref("None", Vec::new()),
                )
            })
            .collect(),
        "trace" => vec![function_info(
            name,
            "event",
            vec![
                value_param("name", type_ref("str", Vec::new())),
                value_param("fields", string_map_type_ref()),
            ],
            type_ref("None", Vec::new()),
        )],
        _ => unreachable!("unknown telemetry namespace"),
    };
    function_only_namespace(name, functions)
}

fn builtin_root_namespace(name: &str) -> Option<ModuleNamespace> {
    match name {
        "io" => Some(io_namespace()),
        "fs" => Some(fs_namespace()),
        "net" => Some(net_namespace()),
        "process" => Some(process_namespace()),
        "random" => Some(random_namespace()),
        "control" => Some(control_namespace()),
        "sys" => Some(sys_namespace()),
        "path" => Some(path_namespace()),
        "math" => Some(math_namespace()),
        "bytes" => Some(bytes_namespace()),
        "json" => Some(json_namespace()),
        "toml" => Some(serialization_namespace(name)),
        "log" | "metrics" | "trace" => Some(telemetry_namespace(name)),
        _ => None,
    }
}

const HOST_BUILTIN_MODULES: &[&str] = &[
    "sys", "path", "math", "bytes", "json", "toml", "metrics", "log", "trace", "random",
];

fn build_host_builtin_metadata() -> BTreeMap<String, HostBuiltinMetadata> {
    let mut metadata = HOST_BUILTIN_MODULES
        .iter()
        .flat_map(|module_name| {
            builtin_root_namespace(module_name)
                .expect("host builtin module must have a namespace")
                .functions
                .into_values()
        })
        .map(|function| {
            let metadata = HostBuiltinMetadata::from_function_info(&function);
            (metadata.qualified_name.clone(), metadata)
        })
        .collect::<BTreeMap<_, _>>();
    for associated in [
        HostBuiltinMetadata {
            qualified_name: "str.to_bytes".to_string(),
            params: vec![HostBuiltinParamMetadata {
                name: "value".to_string(),
                ty: Type::named("str"),
                passing: ReceiverKind::Borrow,
                required: true,
            }],
            return_type: Type::Named("list".to_string(), vec![Type::named("uint8")]),
        },
        HostBuiltinMetadata {
            qualified_name: "str.from_bytes".to_string(),
            params: vec![HostBuiltinParamMetadata {
                name: "bytes".to_string(),
                ty: Type::Named("list".to_string(), vec![Type::named("uint8")]),
                passing: ReceiverKind::Borrow,
                required: true,
            }],
            return_type: Type::Named(
                "Result".to_string(),
                vec![Type::named("str"), Type::named("bytes.Error")],
            ),
        },
    ] {
        metadata.insert(associated.qualified_name.clone(), associated);
    }
    for internal in [
        HostBuiltinMetadata {
            qualified_name: "control::__retry_validate".to_string(),
            params: vec![
                HostBuiltinParamMetadata {
                    name: "max_attempts".to_string(),
                    ty: Type::named("int32"),
                    passing: ReceiverKind::Borrow,
                    required: true,
                },
                HostBuiltinParamMetadata {
                    name: "initial_backoff".to_string(),
                    ty: Type::named("Duration"),
                    passing: ReceiverKind::Borrow,
                    required: true,
                },
            ],
            return_type: Type::Unit,
        },
        HostBuiltinMetadata {
            qualified_name: "control::__retry_next_backoff".to_string(),
            params: vec![HostBuiltinParamMetadata {
                name: "backoff".to_string(),
                ty: Type::named("Duration"),
                passing: ReceiverKind::Borrow,
                required: true,
            }],
            return_type: Type::named("Duration"),
        },
        HostBuiltinMetadata {
            qualified_name: "control::__retry_cancel_if_requested".to_string(),
            params: Vec::new(),
            return_type: Type::Unit,
        },
    ] {
        metadata.insert(internal.qualified_name.clone(), internal);
    }
    metadata
}

pub(crate) fn host_builtin_metadata(name: &str) -> Option<&'static HostBuiltinMetadata> {
    static METADATA: OnceLock<BTreeMap<String, HostBuiltinMetadata>> = OnceLock::new();
    METADATA.get_or_init(build_host_builtin_metadata).get(name)
}

pub(crate) fn builtin_module_namespace(path: &[String]) -> Option<ModuleNamespace> {
    match path {
        [name] => builtin_root_namespace(name),
        _ => None,
    }
}

pub(crate) fn builtin_module_registry() -> BTreeMap<String, ModuleNamespace> {
    [
        "io", "fs", "net", "process", "random", "control", "sys", "path", "math", "bytes", "json",
        "toml", "log", "metrics", "trace",
    ]
    .into_iter()
    .filter_map(|name| builtin_root_namespace(name).map(|namespace| (name.to_string(), namespace)))
    .collect()
}

pub(crate) fn builtin_imported_binding(
    module_path: &[String],
    name: &str,
    span: Span,
) -> Result<ImportedBinding> {
    let namespace = builtin_module_namespace(module_path).ok_or_else(|| {
        Diagnostic::at(
            span,
            format!("cannot resolve builtin module `{}`", module_path.join(".")),
        )
    })?;
    if let Some(function) = namespace.functions.get(name) {
        return Ok(ImportedBinding::Function(function.clone()));
    }
    if let Some(constant) = namespace.constants.get(name) {
        return Ok(ImportedBinding::Constant(constant.clone()));
    }
    if let Some(class_info) = namespace.classes.get(name) {
        return Ok(ImportedBinding::Class(class_info.clone()));
    }
    if let Some(enum_info) = namespace.enums.get(name) {
        return Ok(ImportedBinding::Enum(enum_info.clone()));
    }
    Err(Diagnostic::at(
        span,
        format!(
            "module `{}` has no export named `{}`",
            module_path.join("."),
            name
        ),
    ))
}

pub(crate) fn io_error_type() -> Type {
    builtin_io_error_type()
}

pub(crate) fn process_error_type() -> Type {
    Type::Named("process.Error".to_string(), Vec::new())
}

#[cfg(test)]
#[path = "builtin_modules_tests.rs"]
mod tests;
