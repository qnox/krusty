use super::*;
use crate::jvm::method_node::{Category, Constant, LabelId, LocalVariable, TryCatchBlock};

const ACC_STATIC: u16 = 0x0008;

fn op(op: u8) -> Node {
    Node::Insn(Insn::Op(op))
}

fn var(op: u8, slot: u16) -> Node {
    Node::Insn(Insn::Var { op, slot })
}

fn temporaries(categories: &[Category]) -> Parameters {
    Parameters {
        parameters: categories
            .iter()
            .map(|&category| Parameter {
                category,
                binding: Binding::Temporary,
            })
            .collect(),
    }
}

fn local(name: &str, desc: &str, start: LabelId, end: LabelId, slot: u16) -> LocalVariable {
    LocalVariable {
        name: name.to_string(),
        desc: desc.to_string(),
        start,
        end,
        slot,
    }
}

/// `inline fun f(x: Int): Int { val y = x + 1; return y }` as kotlinc compiles it, with its
/// `$i$f$f` marker in slot 1 and `y` in slot 2.
fn plus_one() -> MethodNode {
    let mut node = MethodNode::new(ACC_STATIC, "f", "(I)I");
    let (start, body, end) = (node.new_label(), node.new_label(), node.new_label());
    node.nodes = vec![
        Node::Label(start),
        Node::Line { line: 3, start },
        op(0x03),
        var(0x36, 1),
        Node::Label(body),
        Node::Line {
            line: 4,
            start: body,
        },
        var(0x15, 0),
        op(0x04),
        op(0x60),
        var(0x36, 2),
        var(0x15, 2),
        op(0xac),
        Node::Label(end),
    ];
    node.local_variables = vec![
        local("x", "I", start, end, 0),
        local("$i$f$f", "I", body, end, 1),
        local("y", "I", body, end, 2),
    ];
    node.max_locals = 3;
    node
}

#[test]
fn a_body_moves_above_its_temporaries_and_renames_its_locals() {
    let inlined = inline(
        &plus_one(),
        &temporaries(&[Category::Int]),
        false,
        5,
        &Default::default(),
    )
    .expect("inlines");
    let instructions: Vec<&Insn> = inlined.instructions().collect();
    assert_eq!(
        instructions,
        vec![
            &Insn::Op(0x00),
            &Insn::Op(0x03),
            &Insn::Var { op: 0x36, slot: 6 },
            &Insn::Var { op: 0x15, slot: 5 },
            &Insn::Op(0x04),
            &Insn::Op(0x60),
            &Insn::Var { op: 0x36, slot: 7 },
            &Insn::Var { op: 0x15, slot: 7 },
            &Insn::Op(0x00),
            &Insn::Jump {
                op: 0xa7,
                target: match inlined.nodes.last() {
                    Some(Node::Label(end)) => *end,
                    other => panic!("the inlined code ends with its end label, not {other:?}"),
                },
            },
        ]
    );
    let locals: Vec<(&str, u16)> = inlined
        .local_variables
        .iter()
        .map(|local| (local.name.as_str(), local.slot))
        .collect();
    assert_eq!(locals, vec![("x$iv", 5), ("$i$f$f", 6), ("y$iv", 7)]);
    let lines: Vec<u16> = inlined
        .nodes
        .iter()
        .filter_map(|node| match node {
            Node::Line { line, .. } => Some(*line),
            _ => None,
        })
        .collect();
    assert_eq!(lines, vec![3, 4]);
}

#[test]
fn an_inline_only_body_loses_its_debug_information_and_its_marker() {
    let inlined = inline(
        &plus_one(),
        &temporaries(&[Category::Int]),
        true,
        5,
        &Default::default(),
    )
    .expect("inlines");
    assert!(inlined.local_variables.is_empty());
    assert!(!inlined
        .nodes
        .iter()
        .any(|node| matches!(node, Node::Line { .. })));
    let instructions: Vec<&Insn> = inlined.instructions().take(5).collect();
    // The marker store went with the variable table, and `y` took the first free slot.
    assert_eq!(
        instructions,
        vec![
            &Insn::Op(0x00),
            &Insn::Var { op: 0x15, slot: 5 },
            &Insn::Op(0x04),
            &Insn::Op(0x60),
            &Insn::Var { op: 0x36, slot: 6 },
        ]
    );
}

#[test]
fn a_parameter_bound_to_a_caller_local_reads_it_and_keeps_no_entry() {
    let parameters = Parameters {
        parameters: vec![Parameter {
            category: Category::Int,
            binding: Binding::CallerLocal {
                slot: 2,
                category: Category::Int,
                checkcast: None,
            },
        }],
    };
    let inlined = inline(&plus_one(), &parameters, false, 5, &Default::default()).expect("inlines");
    let instructions: Vec<&Insn> = inlined.instructions().take(4).collect();
    assert_eq!(
        instructions,
        vec![
            &Insn::Op(0x00),
            &Insn::Op(0x03),
            &Insn::Var { op: 0x36, slot: 5 },
            &Insn::Var { op: 0x15, slot: 2 },
        ]
    );
    let names: Vec<&str> = inlined
        .local_variables
        .iter()
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(names, vec!["$i$f$f", "y$iv"]);
}

#[test]
fn a_return_under_other_values_stores_its_value_and_pops_the_rest() {
    // iconst_1; lconst_0; iconst_2; ireturn — a long and an int under the returned int.
    let mut node = MethodNode::new(ACC_STATIC, "g", "()I");
    node.nodes = vec![op(0x04), op(0x09), op(0x05), op(0xac)];
    node.max_locals = 0;
    let inlined =
        inline(&node, &Parameters::default(), true, 4, &Default::default()).expect("inlines");
    let instructions: Vec<&Insn> = inlined.instructions().collect();
    assert_eq!(
        &instructions[1..4],
        &[&Insn::Op(0x04), &Insn::Op(0x09), &Insn::Op(0x05)]
    );
    assert_eq!(
        &instructions[4..8],
        &[
            &Insn::Var { op: 0x36, slot: 4 },
            &Insn::Op(0x58),
            &Insn::Op(0x57),
            &Insn::Var { op: 0x15, slot: 4 },
        ]
    );
}

#[test]
fn dead_code_and_parameter_checks_are_removed() {
    // aload_0; ldc "s"; checkNotNullParameter; aload_0; areturn; <dead> aconst_null; areturn
    let mut node = MethodNode::new(ACC_STATIC, "h", "(Ljava/lang/String;)Ljava/lang/String;");
    let handler = node.new_label();
    let (start, end) = (node.new_label(), node.new_label());
    node.nodes = vec![
        var(0x19, 0),
        Node::Insn(Insn::Ldc(Constant::String("s".into()))),
        Node::Insn(Insn::Method {
            op: 0xb8,
            owner: "kotlin/jvm/internal/Intrinsics".into(),
            name: "checkNotNullParameter".into(),
            desc: "(Ljava/lang/Object;Ljava/lang/String;)V".into(),
            interface: false,
        }),
        var(0x19, 0),
        op(0xb0),
        Node::Label(start),
        op(0x01),
        op(0xb0),
        Node::Label(end),
        Node::Label(handler),
    ];
    node.try_catch_blocks = vec![TryCatchBlock {
        start,
        end,
        handler,
        catch_type: None,
    }];
    node.max_locals = 1;
    let inlined = inline(
        &node,
        &temporaries(&[Category::Reference]),
        true,
        0,
        &Default::default(),
    )
    .expect("inlines");
    let instructions: Vec<&Insn> = inlined.instructions().collect();
    assert_eq!(
        instructions[..2],
        [&Insn::Op(0x00), &Insn::Var { op: 0x19, slot: 0 }]
    );
    assert_eq!(instructions.len(), 4, "{instructions:?}");
    assert!(inlined.try_catch_blocks.is_empty());
}

#[test]
fn arguments_are_read_in_place_only_when_loaded_first_and_once() {
    let mut node = MethodNode::new(ACC_STATIC, "p", "(ILjava/lang/Object;)V");
    node.nodes = vec![
        var(0x15, 0),
        var(0x19, 1),
        var(0x19, 1),
        op(0x57),
        op(0x57),
        op(0xb1),
    ];
    assert!(can_inline_arguments_in_place(&node));
    node.nodes = vec![var(0x19, 1), var(0x15, 0), op(0x57), op(0x57), op(0xb1)];
    assert!(!can_inline_arguments_in_place(&node));
}

#[test]
fn a_loop_or_a_handler_needs_an_empty_stack() {
    let mut node = MethodNode::new(ACC_STATIC, "l", "()V");
    let head = node.new_label();
    node.nodes = vec![
        Node::Label(head),
        Node::Insn(Insn::Jump {
            op: 0xa7,
            target: head,
        }),
    ];
    assert!(requires_empty_stack_on_entry(&node));
    assert!(!requires_empty_stack_on_entry(&plus_one()));
}

#[test]
fn intrinsic_rewrites_require_the_exact_jvm_method_shape() {
    let method = |name: &str, desc: &str, interface: bool| {
        Node::Insn(Insn::Method {
            op: 0xb8,
            owner: "kotlin/jvm/internal/Intrinsics".into(),
            name: name.into(),
            desc: desc.into(),
            interface,
        })
    };

    let mut reified = MethodNode::new(ACC_STATIC, "r", "()V");
    reified.nodes = vec![method("needClassReification", "()V", false)];
    assert_eq!(
        unsupported_shape(&reified),
        Some(super::callee_shape::UnsupportedShape::ClassReification),
    );
    reified.nodes = vec![method("needClassReification", "(I)V", false)];
    assert_eq!(unsupported_shape(&reified), None);
    reified.nodes = vec![method("needClassReification", "()V", true)];
    assert_eq!(unsupported_shape(&reified), None);

    let mut null_check = MethodNode::new(ACC_STATIC, "n", "(Ljava/lang/Object;)V");
    null_check.nodes = vec![
        var(0x19, 0),
        Node::Insn(Insn::Ldc(Constant::String("value".into()))),
        method(
            "checkNotNullParameter",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
            false,
        ),
        var(0x19, 0),
        op(0x57),
        op(0xb1),
    ];
    assert!(can_inline_arguments_in_place(&null_check));
    null_check.nodes[2] = method("checkNotNullParameter", "()V", false);
    assert!(!can_inline_arguments_in_place(&null_check));
    null_check.nodes[2] = method(
        "checkNotNullParameter",
        "(Ljava/lang/Object;Ljava/lang/String;)V",
        true,
    );
    assert!(!can_inline_arguments_in_place(&null_check));
}
