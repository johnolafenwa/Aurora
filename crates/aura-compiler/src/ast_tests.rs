use super::{
    BinaryOp, BindingTarget, ClassDecl, CompareLink, CompareOp, EnumDecl, Expr, ExprKind,
    ExternFunctionDecl, ExternOpaqueClassDecl, FieldDecl, ForStmt, FunctionDecl, FunctionTypeParam,
    ImplDecl, Item, ParamMode, ReceiverKind, TraitDecl, TypeRef,
};
use crate::diag::Span;
use serde_json::json;
use std::collections::BTreeMap;

fn dummy_type(name: &str) -> TypeRef {
    TypeRef::named(name, vec![], false, Span::new(1, 1))
}

fn dummy_function(name: &str) -> FunctionDecl {
    FunctionDecl {
        public: false,
        name: name.to_string(),
        type_params: vec![],
        type_param_bounds: BTreeMap::new(),
        receiver: Some(ReceiverKind::Borrow),
        params: vec![],
        return_type: dummy_type("None"),
        view_return: None,
        body: vec![],
        span: Span::new(1, 1),
    }
}

#[test]
fn item_name_matches_decl_name() {
    let class_item = Item::Class(ClassDecl {
        public: true,
        copy: false,
        name: "Point".to_string(),
        type_params: vec![],
        type_param_bounds: BTreeMap::new(),
        fields: vec![FieldDecl {
            public: true,
            name: "x".to_string(),
            ty: dummy_type("int32"),
            default: None,
            span: Span::new(1, 1),
        }],
        methods: vec![],
        span: Span::new(1, 1),
    });
    let enum_item = Item::Enum(EnumDecl {
        public: true,
        name: "Status".to_string(),
        type_params: vec![],
        type_param_bounds: BTreeMap::new(),
        variants: vec![],
        span: Span::new(1, 1),
    });
    let function_item = Item::Function(dummy_function("main"));
    let extern_function_item = Item::ExternFunction(ExternFunctionDecl {
        public: true,
        abi: "C".to_string(),
        name: "getpid".to_string(),
        name_span: Span::new(1, 16),
        params: vec![],
        return_type: dummy_type("int32"),
        span: Span::new(1, 1),
    });
    let extern_opaque_item = Item::ExternOpaqueClass(ExternOpaqueClassDecl {
        public: true,
        abi: "C".to_string(),
        name: "ProcessHandle".to_string(),
        name_span: Span::new(1, 25),
        span: Span::new(1, 1),
    });
    let trait_item = Item::Trait(TraitDecl {
        public: true,
        name: "Display".to_string(),
        type_params: vec![],
        supertraits: vec![],
        methods: vec![dummy_function("show")],
        span: Span::new(1, 1),
    });
    let impl_item = Item::Impl(ImplDecl {
        type_params: vec![],
        type_param_bounds: BTreeMap::new(),
        trait_name: "Display".to_string(),
        trait_args: vec![dummy_type("str")],
        for_type: dummy_type("Point"),
        methods: vec![dummy_function("show")],
        span: Span::new(1, 1),
    });

    assert_eq!(class_item.name(), "Point");
    assert_eq!(enum_item.name(), "Status");
    assert_eq!(function_item.name(), "main");
    assert_eq!(extern_function_item.name(), "getpid");
    assert_eq!(extern_opaque_item.name(), "ProcessHandle");
    assert_eq!(trait_item.name(), "Display");
    assert_eq!(impl_item.name(), "Display");
}

#[test]
fn dummy_helpers_cover_receiver_and_none_defaults() {
    let ty = dummy_type("str");
    assert!(matches!(ty.named_parts(), Some(("str", args)) if args.is_empty()));
    assert!(!ty.indirect);
    assert_eq!(ty.span, Span::new(1, 1));

    let function = dummy_function("render");
    assert_eq!(function.name, "render");
    assert_eq!(function.receiver, Some(ReceiverKind::Borrow));
    assert!(matches!(
        function.return_type.named_parts(),
        Some(("None", args)) if args.is_empty()
    ));
    assert!(function.type_params.is_empty());
    assert!(function.type_param_bounds.is_empty());
    assert!(function.params.is_empty());
    assert!(function.body.is_empty());
    assert_eq!(function.span, Span::new(1, 1));
}

#[test]
fn tuple_ast_nodes_are_structural_and_keep_binding_spans() {
    let span = Span::new(3, 5);
    let tuple_ty = TypeRef {
        kind: super::TypeRefKind::Tuple(vec![
            TypeRef::named("int32", vec![], false, span),
            TypeRef::named("str", vec![], false, Span::new(3, 12)),
        ]),
        indirect: false,
        span,
    };
    assert!(matches!(
        tuple_ty.kind,
        super::TypeRefKind::Tuple(ref elements) if elements.len() == 2
    ));
    assert!(tuple_ty.named_parts().is_none());
    assert_eq!(tuple_ty.elements().map(<[TypeRef]>::len), Some(2));

    let target = super::BindingTarget::Tuple {
        elements: vec![
            super::BindingTarget::Name {
                name: "left".to_string(),
                span,
            },
            super::BindingTarget::Tuple {
                elements: vec![super::BindingTarget::Name {
                    name: "right".to_string(),
                    span: Span::new(3, 15),
                }],
                span: Span::new(3, 14),
            },
        ],
        span,
    };
    assert_eq!(target.span(), span);
    assert!(target.name().is_none());

    let name_target = super::BindingTarget::Name {
        name: "value".to_string(),
        span,
    };
    assert_eq!(name_target.span(), span);
    assert_eq!(name_target.name(), Some("value"));

    let named_ty = TypeRef::named("int32", vec![], false, span);
    assert_eq!(named_ty.named_parts(), Some(("int32", &[][..])));
    assert!(named_ty.elements().is_none());
}

#[test]
fn type_ref_json_preserves_named_tuple_and_function_shapes() {
    let span = Span::new(3, 5);
    let named = TypeRef::named(
        "Option",
        vec![TypeRef::named("int32", vec![], false, span)],
        false,
        span,
    );
    assert_eq!(
        serde_json::to_value(&named).expect("named type reference should serialize"),
        serde_json::json!({
            "name": "Option",
            "args": [{
                "name": "int32",
                "args": [],
                "indirect": false,
                "span": {"line": 3, "column": 5}
            }],
            "indirect": false,
            "span": {"line": 3, "column": 5}
        })
    );

    let tuple = TypeRef::tuple(
        vec![TypeRef::named("str", vec![], false, span)],
        false,
        span,
    );
    assert_eq!(
        serde_json::to_value(&tuple).expect("tuple type reference should serialize"),
        serde_json::json!({
            "elements": [{
                "name": "str",
                "args": [],
                "indirect": false,
                "span": {"line": 3, "column": 5}
            }],
            "indirect": false,
            "span": {"line": 3, "column": 5}
        })
    );

    let function = TypeRef::function_with_params(
        vec![
            FunctionTypeParam::new(
                ParamMode::BorrowMut,
                TypeRef::named("str", vec![], false, span),
                span,
            ),
            FunctionTypeParam::new(
                ParamMode::Own,
                TypeRef::named("int32", vec![], false, span),
                span,
            ),
        ],
        TypeRef::named("bool", vec![], false, span),
        span,
    );
    let (params, return_type) = function
        .function_parts()
        .expect("function type should expose its signature");
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].mode, ParamMode::BorrowMut);
    assert_eq!(params[0].ty.named_parts(), Some(("str", &[][..])));
    assert_eq!(params[1].mode, ParamMode::Own);
    assert_eq!(params[1].ty.named_parts(), Some(("int32", &[][..])));
    assert_eq!(return_type.named_parts(), Some(("bool", &[][..])));
    assert!(function.named_parts().is_none());
    assert!(function.elements().is_none());
    assert_eq!(
        serde_json::to_value(&function).expect("function type reference should serialize"),
        serde_json::json!({
            "params": [{
                "mode": "BorrowMut",
                "ty": {
                    "name": "str",
                    "args": [],
                    "indirect": false,
                    "span": {"line": 3, "column": 5}
                },
                "span": {"line": 3, "column": 5}
            }, {
                "mode": "Own",
                "ty": {
                    "name": "int32",
                    "args": [],
                    "indirect": false,
                    "span": {"line": 3, "column": 5}
                },
                "span": {"line": 3, "column": 5}
            }],
            "return_type": {
                "name": "bool",
                "args": [],
                "indirect": false,
                "span": {"line": 3, "column": 5}
            },
            "indirect": false,
            "span": {"line": 3, "column": 5}
        })
    );
}

#[test]
fn function_type_convenience_constructor_preserves_parameter_spans_and_default_capability() {
    let signature_span = Span::new(4, 9);
    let first_span = Span::new(4, 13);
    let second_span = Span::new(4, 21);
    let return_span = Span::new(4, 32);
    let function = TypeRef::function(
        vec![
            TypeRef::named("str", vec![], false, first_span),
            TypeRef::tuple(
                vec![TypeRef::named("int32", vec![], false, second_span)],
                false,
                second_span,
            ),
        ],
        TypeRef::named("bool", vec![], false, return_span),
        signature_span,
    );

    let (params, return_type) = function
        .function_parts()
        .expect("the convenience constructor should create a structural function type");
    assert_eq!(
        params
            .iter()
            .map(|param| (param.mode, param.span, param.ty.span))
            .collect::<Vec<_>>(),
        vec![
            (ParamMode::Default, first_span, first_span),
            (ParamMode::Default, second_span, second_span),
        ]
    );
    assert_eq!(return_type.span, return_span);
    assert_eq!(function.span, signature_span);
    assert!(!function.indirect);

    let named = TypeRef::named("str", vec![], false, first_span);
    let tuple = TypeRef::tuple(vec![named.clone()], false, second_span);
    assert!(named.function_parts().is_none());
    assert!(tuple.function_parts().is_none());
}

#[test]
fn function_type_pretty_json_preserves_the_public_wire_shape() {
    let span = Span::new(8, 4);
    let function = TypeRef::function_with_params(
        vec![FunctionTypeParam::new(
            ParamMode::Own,
            TypeRef::tuple(
                vec![TypeRef::named("str", vec![], false, Span::new(8, 12))],
                false,
                Span::new(8, 11),
            ),
            Span::new(8, 7),
        )],
        TypeRef::named("None", vec![], false, Span::new(8, 25)),
        span,
    );

    let encoded =
        serde_json::to_string_pretty(&function).expect("function type should serialize as JSON");
    assert!(
        encoded.contains("\n  \"params\": ["),
        "pretty serialization should retain a human-readable parameter list: {encoded}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded)
            .expect("the public function-type JSON should round trip as a JSON value"),
        serde_json::json!({
            "params": [{
                "mode": "Own",
                "ty": {
                    "elements": [{
                        "name": "str",
                        "args": [],
                        "indirect": false,
                        "span": {"line": 8, "column": 12}
                    }],
                    "indirect": false,
                    "span": {"line": 8, "column": 11}
                },
                "span": {"line": 8, "column": 7}
            }],
            "return_type": {
                "name": "None",
                "args": [],
                "indirect": false,
                "span": {"line": 8, "column": 25}
            },
            "indirect": false,
            "span": {"line": 8, "column": 4}
        })
    );
}

#[test]
fn adding_conditional_expressions_preserves_existing_expression_json_shapes() {
    let span = Span::new(3, 5);
    let existing = Expr {
        kind: ExprKind::Binary {
            op: BinaryOp::Add,
            left: Box::new(Expr {
                kind: ExprKind::Name("value".to_string()),
                span,
            }),
            right: Box::new(Expr {
                kind: ExprKind::Int(1),
                span,
            }),
        },
        span,
    };

    assert_eq!(
        serde_json::to_value(existing).expect("serialize existing expression"),
        json!({
            "kind": {
                "Binary": {
                    "op": "Add",
                    "left": {
                        "kind": {"Name": "value"},
                        "span": {"line": 3, "column": 5}
                    },
                    "right": {
                        "kind": {"Int": 1},
                        "span": {"line": 3, "column": 5}
                    }
                }
            },
            "span": {"line": 3, "column": 5}
        })
    );
}

#[test]
fn owned_slice_json_keeps_omitted_endpoints_and_colon_span_explicit() {
    let slice = Expr {
        kind: ExprKind::Slice {
            object: Box::new(Expr {
                kind: ExprKind::Name("values".to_string()),
                span: Span::new(4, 5),
            }),
            start: None,
            end: None,
            colon_span: Span::new(4, 12),
        },
        span: Span::new(4, 5),
    };

    assert_eq!(
        serde_json::to_value(slice).expect("owned slice AST should serialize"),
        json!({
            "kind": {
                "Slice": {
                    "object": {
                        "kind": {"Name": "values"},
                        "span": {"line": 4, "column": 5}
                    },
                    "start": null,
                    "end": null,
                    "colon_span": {"line": 4, "column": 12}
                }
            },
            "span": {"line": 4, "column": 5}
        })
    );
}

#[test]
fn membership_and_comparison_chain_json_shapes_are_stable() {
    let span = Span::new(2, 11);
    let operator_span = Span::new(2, 13);
    let membership = Expr {
        kind: ExprKind::Membership {
            value: Box::new(Expr {
                kind: ExprKind::Int(1),
                span,
            }),
            container: Box::new(Expr {
                kind: ExprKind::Name("ports".to_string()),
                span: Span::new(2, 16),
            }),
            negated: true,
            operator_span,
        },
        span,
    };

    assert_eq!(
        serde_json::to_value(&membership).expect("membership should serialize"),
        json!({
            "kind": {
                "Membership": {
                    "value": {
                        "kind": {"Int": 1},
                        "span": {"line": 2, "column": 11}
                    },
                    "container": {
                        "kind": {"Name": "ports"},
                        "span": {"line": 2, "column": 16}
                    },
                    "negated": true,
                    "operator_span": {"line": 2, "column": 13}
                }
            },
            "span": {"line": 2, "column": 11}
        })
    );

    let chain = Expr {
        kind: ExprKind::CompareChain {
            first: Box::new(Expr {
                kind: ExprKind::Int(1),
                span,
            }),
            links: vec![
                CompareLink {
                    op: CompareOp::Less,
                    op_span: Span::new(2, 13),
                    operand: Expr {
                        kind: ExprKind::Int(2),
                        span: Span::new(2, 15),
                    },
                },
                CompareLink {
                    op: CompareOp::LessEq,
                    op_span: Span::new(2, 17),
                    operand: Expr {
                        kind: ExprKind::Int(3),
                        span: Span::new(2, 20),
                    },
                },
            ],
        },
        span,
    };

    assert_eq!(
        serde_json::to_value(&chain).expect("comparison chain should serialize"),
        json!({
            "kind": {
                "CompareChain": {
                    "first": {
                        "kind": {"Int": 1},
                        "span": {"line": 2, "column": 11}
                    },
                    "links": [
                        {
                            "op": "Less",
                            "op_span": {"line": 2, "column": 13},
                            "operand": {
                                "kind": {"Int": 2},
                                "span": {"line": 2, "column": 15}
                            }
                        },
                        {
                            "op": "LessEq",
                            "op_span": {"line": 2, "column": 17},
                            "operand": {
                                "kind": {"Int": 3},
                                "span": {"line": 2, "column": 20}
                            }
                        }
                    ]
                }
            },
            "span": {"line": 2, "column": 11}
        })
    );

    // Every comparison operator maps to its binary counterpart except the two
    // membership operators, which have no binary form.
    for (op, expected) in [
        (CompareOp::Eq, Some(BinaryOp::Eq)),
        (CompareOp::NotEq, Some(BinaryOp::NotEq)),
        (CompareOp::Less, Some(BinaryOp::Less)),
        (CompareOp::LessEq, Some(BinaryOp::LessEq)),
        (CompareOp::Greater, Some(BinaryOp::Greater)),
        (CompareOp::GreaterEq, Some(BinaryOp::GreaterEq)),
        (CompareOp::In, None),
        (CompareOp::NotIn, None),
    ] {
        assert_eq!(op.as_binary_op(), expected, "{op:?}");
    }
}

#[test]
fn for_stmt_json_preserves_simple_binding_shape_and_exposes_tuple_targets() {
    let span = Span::new(4, 5);
    let iterable = Expr {
        kind: ExprKind::Name("rows".to_string()),
        span: Span::new(4, 24),
    };
    let simple = ForStmt {
        target: BindingTarget::Name {
            name: "row".to_string(),
            span,
        },
        iterable: iterable.clone(),
        borrow_mode: None,
        body: vec![],
        span,
    };
    let simple_json = serde_json::to_value(&simple).expect("simple for statement should serialize");
    assert_eq!(simple_json.get("binding"), Some(&serde_json::json!("row")));
    assert!(simple_json.get("target").is_none());

    let tuple = ForStmt {
        target: BindingTarget::Tuple {
            elements: vec![
                BindingTarget::Name {
                    name: "left".to_string(),
                    span,
                },
                BindingTarget::Name {
                    name: "right".to_string(),
                    span: Span::new(4, 11),
                },
            ],
            span,
        },
        iterable,
        borrow_mode: None,
        body: vec![],
        span,
    };
    let tuple_json =
        serde_json::to_value(&tuple).expect("tuple-target for statement should serialize");
    assert!(tuple_json.get("binding").is_none());
    assert!(matches!(
        tuple_json.get("target"),
        Some(serde_json::Value::Object(target)) if target.contains_key("Tuple")
    ));
}

#[test]
fn conditional_expression_json_shape_names_all_three_operands() {
    let span = Span::new(8, 9);
    let conditional = Expr {
        kind: ExprKind::Conditional {
            then_expr: Box::new(Expr {
                kind: ExprKind::String("yes".to_string()),
                span,
            }),
            condition: Box::new(Expr {
                kind: ExprKind::Bool(true),
                span,
            }),
            else_expr: Box::new(Expr {
                kind: ExprKind::String("no".to_string()),
                span,
            }),
        },
        span,
    };

    assert_eq!(
        serde_json::to_value(conditional).expect("serialize conditional expression"),
        json!({
            "kind": {
                "Conditional": {
                    "then_expr": {
                        "kind": {"String": "yes"},
                        "span": {"line": 8, "column": 9}
                    },
                    "condition": {
                        "kind": {"Bool": true},
                        "span": {"line": 8, "column": 9}
                    },
                    "else_expr": {
                        "kind": {"String": "no"},
                        "span": {"line": 8, "column": 9}
                    }
                }
            },
            "span": {"line": 8, "column": 9}
        })
    );
}
