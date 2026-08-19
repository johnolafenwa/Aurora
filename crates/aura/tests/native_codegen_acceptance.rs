use aura_compiler::diag::Span;
use aura_compiler::emit_host_native_object;
use aura_compiler::mir::{
    BasicBlock, CallTarget, Instruction, MirArg, MirFunction, MirLocalType, MirModule, Operand,
    Rvalue, Terminator,
};
use aura_compiler::sema::Type;

fn object_mentions(object: &[u8], symbol: &str) -> bool {
    object
        .windows(symbol.len())
        .any(|window| window == symbol.as_bytes())
}

#[test]
fn public_object_codegen_infers_host_and_wait_result_types() {
    let task_type = Type::Named("Task".to_string(), vec![Type::named("int32")]);
    let tasks_type = Type::Named("Vec".to_string(), vec![task_type.clone()]);
    let module = MirModule {
        constants: Vec::new(),
        functions: vec![MirFunction {
            name: "main".to_string(),
            module_name: "<native-codegen-acceptance>".to_string(),
            source_path: Some("<native-codegen-acceptance>".to_string()),
            span: Span::new(1, 1),
            receiver: None,
            params: Vec::new(),
            local_types: vec![
                MirLocalType {
                    name: "tasks".to_string(),
                    ty: tasks_type,
                },
                MirLocalType {
                    name: "host_result".to_string(),
                    ty: Type::named("Unknown"),
                },
                MirLocalType {
                    name: "wait_result".to_string(),
                    ty: Type::named("Unknown"),
                },
            ],
            return_type: Type::named("int32"),
            entry: "entry".to_string(),
            blocks: vec![BasicBlock {
                label: "entry".to_string(),
                instructions: vec![
                    Instruction::Assign {
                        target: "tasks".to_string(),
                        value: Rvalue::VecLiteral {
                            elements: Vec::new(),
                            element_type: task_type,
                        },
                    },
                    Instruction::Assign {
                        target: "host_result".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Name("sys::args".to_string()),
                            args: Vec::new(),
                        },
                    },
                    Instruction::Assign {
                        target: "wait_result".to_string(),
                        value: Rvalue::Call {
                            callee: CallTarget::Name("wait_any".to_string()),
                            args: vec![MirArg {
                                name: None,
                                value: Operand::Place("tasks".to_string()),
                                writeback_place: None,
                            }],
                        },
                    },
                ],
                terminator: Terminator::Return(Operand::Int(0)),
            }],
        }],
        classes: Vec::new(),
        trait_impls: Vec::new(),
        top_level: None,
    };

    let object =
        emit_host_native_object(&module).expect("valid inferred calls should emit an object");
    assert!(
        object_mentions(&object, "aura_direct_host_builtin"),
        "the host builtin call must lower through the runtime adapter"
    );
    assert!(
        object_mentions(&object, "aura_direct_wait_any"),
        "the typed wait call must lower through the direct wait adapter"
    );
}
