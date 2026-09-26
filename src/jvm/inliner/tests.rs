use super::anonymous_object::MalformedType;
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
        captured: Vec::new(),
    }
}

/// Objects are regenerated only by the call-site tests.
struct NoObjects;

impl AnonymousObjects for NoObjects {
    fn regenerate(&mut self, _class: &str, _desc: &str) -> Result<(String, String), InlineError> {
        panic!("a body without anonymous objects regenerates none")
    }
}

/// Lines left as the callee's own.
struct OwnLines;

impl SourceLines for OwnLines {
    fn map(&mut self, line: u16) -> Option<u16> {
        Some(line)
    }
    fn synthetic(&mut self) -> Option<u16> {
        None
    }
    fn call_site(&self) -> Option<u16> {
        None
    }
}

/// [`inline`] of a call without lambdas, its lines left as the callee's.
fn inline_plain(
    callee: &MethodNode,
    parameters: &Parameters,
    inline_only: bool,
    frame_base: u16,
    reified_arguments: &crate::jvm::inline::ReifiedArguments,
) -> Result<MethodNode, InlineError> {
    inline(
        callee,
        parameters,
        &[],
        inline_only,
        frame_base,
        reified_arguments,
        InliningContext {
            lines: &mut OwnLines,
            objects: &mut NoObjects,
        },
    )
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
    let inlined = inline_plain(
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
    let inlined = inline_plain(
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
        captured: Vec::new(),
    };
    let inlined =
        inline_plain(&plus_one(), &parameters, false, 5, &Default::default()).expect("inlines");
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
        inline_plain(&node, &Parameters::default(), true, 4, &Default::default()).expect("inlines");
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
    let inlined = inline_plain(
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

/// `inline fun f(): String { try { try { throw E() } catch (e: Throwable) { throw E() } } catch
/// (e: Throwable) { return "" } }`: both tries open on a line, and each handler is reached only by
/// the exception its range throws.
#[test]
fn a_handler_reached_only_by_an_exception_survives() {
    let mut node = MethodNode::new(ACC_STATIC, "f", "()Ljava/lang/String;");
    let [outer, inner, inner_handler, outer_handler] = [(); 4].map(|()| node.new_label());
    let throw = || {
        [
            Node::Insn(Insn::Type {
                op: 0xbb,
                class: "java/lang/Exception".into(),
            }),
            op(0x59),
            method(0xb7, "java/lang/Exception", "<init>", "()V"),
            op(0xbf),
        ]
    };
    node.nodes = vec![
        Node::Label(outer),
        Node::Line {
            line: 3,
            start: outer,
        },
        op(0x00),
        Node::Label(inner),
        Node::Line {
            line: 4,
            start: inner,
        },
        op(0x00),
    ];
    node.nodes.extend(throw());
    node.nodes
        .extend([Node::Label(inner_handler), var(0x3a, 0)]);
    node.nodes.extend(throw());
    node.nodes.extend([
        Node::Label(outer_handler),
        var(0x3a, 0),
        Node::Insn(Insn::Ldc(Constant::String("".into()))),
        op(0xb0),
    ]);
    let block = |start, end, handler| TryCatchBlock {
        start,
        end,
        handler,
        catch_type: Some("java/lang/Throwable".into()),
    };
    node.try_catch_blocks = vec![
        block(inner, inner_handler, inner_handler),
        block(outer, outer_handler, outer_handler),
    ];
    node.max_locals = 1;
    let inlined =
        inline_plain(&node, &temporaries(&[]), false, 0, &Default::default()).expect("inlines");
    let instructions: Vec<&Insn> = inlined.instructions().collect();
    // The leading nop, the two try nops, three instructions of each `throw E()` and its athrow,
    // two catch stores, the ldc and the return turned into a jump to the end.
    assert_eq!(instructions.len(), 16, "{instructions:?}");
    assert_eq!(inlined.try_catch_blocks.len(), 2);
    let positions = preparation::label_positions(&inlined);
    for block in &inlined.try_catch_blocks {
        let start = positions[block.start.index()].expect("a placed start");
        assert_eq!(inlined.nodes[start + 1], op(0x00), "{:?}", inlined.nodes);
    }
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
        unsupported_shape(&reified, ObjectRegeneration::Declined),
        Some(super::callee_shape::UnsupportedShape::ClassReification),
    );
    reified.nodes = vec![method("needClassReification", "(I)V", false)];
    assert_eq!(
        unsupported_shape(&reified, ObjectRegeneration::Declined),
        None
    );
    reified.nodes = vec![method("needClassReification", "()V", true)];
    assert_eq!(
        unsupported_shape(&reified, ObjectRegeneration::Declined),
        None
    );

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

fn method(op: u8, owner: &str, name: &str, desc: &str) -> Node {
    Node::Insn(Insn::Method {
        op,
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: op == 0xb9,
    })
}

fn checkcast(class: &str) -> Node {
    Node::Insn(Insn::Type {
        op: 0xc0,
        class: class.to_string(),
    })
}

/// `inline fun f(block: (Int) -> Int): Int = block(1)` as kotlinc compiles it.
fn apply_to_one() -> MethodNode {
    let mut node = MethodNode::new(ACC_STATIC, "f", "(Lkotlin/jvm/functions/Function1;)I");
    let (start, end) = (node.new_label(), node.new_label());
    node.nodes = vec![
        Node::Label(start),
        Node::Line { line: 7, start },
        var(0x19, 0),
        op(0x04),
        method(
            0xb8,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        ),
        method(
            0xb9,
            "kotlin/jvm/functions/Function1",
            "invoke",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
        ),
        checkcast("java/lang/Number"),
        method(0xb6, "java/lang/Number", "intValue", "()I"),
        op(0xac),
        Node::Label(end),
    ];
    node.local_variables = vec![local(
        "block",
        "Lkotlin/jvm/functions/Function1;",
        start,
        end,
        0,
    )];
    node.max_locals = 1;
    node.max_stack = 2;
    node
}

/// `{ it + base }`, capturing an `Int` `base`: its parameter in slot 0, the captured value in 1.
fn plus_captured() -> Lambda {
    let mut node = MethodNode::new(ACC_STATIC, "f$lambda", "(II)I");
    let (start, end) = (node.new_label(), node.new_label());
    node.nodes = vec![
        Node::Label(start),
        Node::Line { line: 2, start },
        var(0x15, 0),
        var(0x15, 1),
        op(0x60),
        op(0xac),
        Node::Label(end),
    ];
    node.local_variables = vec![local("it", "I", start, end, 0)];
    node.max_locals = 2;
    node.max_stack = 2;
    Lambda {
        node,
        parameter_types: vec!["I".to_string()],
        return_type: "I".to_string(),
        captured: 0..1,
    }
}

#[test]
fn an_invoke_of_an_inline_lambda_becomes_the_lambdas_body() {
    let parameters = Parameters {
        parameters: vec![Parameter {
            category: Category::Reference,
            binding: Binding::Lambda(0),
        }],
        captured: vec![Parameter {
            category: Category::Int,
            binding: Binding::CallerLocal {
                slot: 2,
                category: Category::Int,
                checkcast: None,
            },
        }],
    };
    let inlined = inline(
        &apply_to_one(),
        &parameters,
        &[plus_captured()],
        false,
        5,
        &Default::default(),
        InliningContext {
            lines: &mut OwnLines,
            objects: &mut NoObjects,
        },
    )
    .expect("inlines");
    let instructions: Vec<Node> = inlined
        .instructions()
        .filter(|insn| !matches!(insn, Insn::Jump { .. }))
        .cloned()
        .map(Node::Insn)
        .collect();
    assert_eq!(
        instructions,
        vec![
            op(0x00),
            op(0x04),
            method(
                0xb8,
                "java/lang/Integer",
                "valueOf",
                "(I)Ljava/lang/Integer;"
            ),
            // The argument, coerced back to the lambda's `Int` and stored above the body's
            // parameters (the lambda and its captured value), then the lambda's body reading it
            // and the caller's `base`.
            checkcast("java/lang/Number"),
            method(0xb6, "java/lang/Number", "intValue", "()I"),
            var(0x36, 5),
            var(0x15, 5),
            var(0x15, 2),
            op(0x60),
            op(0x00),
            method(
                0xb8,
                "java/lang/Integer",
                "valueOf",
                "(I)Ljava/lang/Integer;"
            ),
            checkcast("java/lang/Number"),
            method(0xb6, "java/lang/Number", "intValue", "()I"),
            op(0x00),
        ]
    );
    let locals: Vec<(&str, u16)> = inlined
        .local_variables
        .iter()
        .map(|local| (local.name.as_str(), local.slot))
        .collect();
    assert_eq!(locals, vec![("it", 5)]);
    let lines: Vec<u16> = inlined
        .nodes
        .iter()
        .filter_map(|node| match node {
            Node::Line { line, .. } => Some(*line),
            _ => None,
        })
        .collect();
    // The body's line, the lambda's, and the body's again after the lambda.
    assert_eq!(lines, vec![7, 2, 7]);
}

/// Regenerates the `n`th object as `Main$g$$inlined$f$n` with an `n`-`int` constructor, recording
/// what it was asked to copy.
#[derive(Default)]
struct NumberingObjects {
    asked: Vec<(String, String)>,
}

impl AnonymousObjects for NumberingObjects {
    fn regenerate(&mut self, class: &str, desc: &str) -> Result<(String, String), InlineError> {
        self.asked.push((class.to_string(), desc.to_string()));
        let n = self.asked.len();
        Ok((
            format!("Main$g$$inlined$f${n}"),
            format!("({})V", "I".repeat(n)),
        ))
    }
}

fn new(class: &str) -> Node {
    Node::Insn(Insn::Type {
        op: 0xbb,
        class: class.to_string(),
    })
}

fn init(owner: &str, desc: &str) -> Node {
    Node::Insn(Insn::Method {
        op: 0xb7,
        owner: owner.to_string(),
        name: "<init>".to_string(),
        desc: desc.to_string(),
        interface: false,
    })
}

/// The class each `new` and the owner and descriptor of each `<init>` call in `node`.
fn constructions(node: &MethodNode) -> Vec<String> {
    node.instructions()
        .filter_map(|insn| match insn {
            Insn::Type { op: 0xbb, class } => Some(format!("new {class}")),
            Insn::Method {
                owner, name, desc, ..
            } if name == "<init>" => Some(format!("init {owner}{desc}")),
            _ => None,
        })
        .collect()
}

#[test]
fn a_nested_construction_pairs_each_new_with_the_call_that_initializes_it() {
    // `A(B())`: A's `new` comes first, B's constructor call first.
    let mut node = MethodNode::new(ACC_STATIC, "f", "()V");
    node.nodes = vec![
        new("lib/A$f$1"),
        op(0x59),
        new("lib/B$f$2"),
        op(0x59),
        init("lib/B$f$2", "()V"),
        init("lib/A$f$1", "(Ljava/lang/Object;)V"),
        op(0x57),
        op(0xb1),
    ];
    let mut objects = NumberingObjects::default();
    object_regeneration::regenerate_objects(&mut node, &mut objects).expect("regenerates");
    assert_eq!(
        objects.asked,
        [
            ("lib/A$f$1".to_string(), "(Ljava/lang/Object;)V".to_string()),
            ("lib/B$f$2".to_string(), "()V".to_string()),
        ]
    );
    assert_eq!(
        constructions(&node),
        [
            "new Main$g$$inlined$f$1",
            "new Main$g$$inlined$f$2",
            "init Main$g$$inlined$f$2(II)V",
            "init Main$g$$inlined$f$1(I)V",
        ]
    );
}

#[test]
fn two_outstanding_instances_of_one_class_each_get_their_own_copy() {
    // `A(A())` of one original class: the inner instance is initialized first.
    let mut node = MethodNode::new(ACC_STATIC, "f", "()V");
    node.nodes = vec![
        new("lib/A$f$1"),
        op(0x59),
        new("lib/A$f$1"),
        op(0x59),
        init("lib/A$f$1", "()V"),
        init("lib/A$f$1", "(Ljava/lang/Object;)V"),
        op(0x57),
        op(0xb1),
    ];
    let mut objects = NumberingObjects::default();
    object_regeneration::regenerate_objects(&mut node, &mut objects).expect("regenerates");
    assert_eq!(
        objects.asked,
        [
            ("lib/A$f$1".to_string(), "(Ljava/lang/Object;)V".to_string()),
            ("lib/A$f$1".to_string(), "()V".to_string()),
        ]
    );
    assert_eq!(
        constructions(&node),
        [
            "new Main$g$$inlined$f$1",
            "new Main$g$$inlined$f$2",
            "init Main$g$$inlined$f$2(II)V",
            "init Main$g$$inlined$f$1(I)V",
        ]
    );
}

#[test]
fn a_constructor_call_without_its_new_is_refused() {
    let mut node = MethodNode::new(ACC_STATIC, "f", "(Ljava/lang/Object;)V");
    node.max_locals = 1;
    node.nodes = vec![var(0x19, 0), init("lib/A$f$1", "()V"), op(0xb1)];
    assert_eq!(
        object_regeneration::regenerate_objects(&mut node, &mut NumberingObjects::default()),
        Err(InlineError::UnpairedAnonymousObject)
    );
}

#[test]
fn a_new_no_constructor_call_initializes_is_refused() {
    let mut node = MethodNode::new(ACC_STATIC, "f", "()V");
    node.nodes = vec![new("lib/A$f$1"), op(0x57), op(0xb1)];
    assert_eq!(
        object_regeneration::regenerate_objects(&mut node, &mut NumberingObjects::default()),
        Err(InlineError::UnpairedAnonymousObject)
    );
}

#[test]
fn a_malformed_descriptor_at_the_call_site_declines_the_regeneration() {
    let mut node = MethodNode::new(ACC_STATIC, "f", "()V");
    node.nodes = vec![
        new("lib/A$f$1"),
        op(0x59),
        init("lib/A$f$1", "()V"),
        op(0x01),
        Node::Insn(Insn::Field {
            op: 0xb5,
            owner: "lib/A$f$1".to_string(),
            name: "x".to_string(),
            desc: "Llib/A$f$1".to_string(),
        }),
        op(0xb1),
    ];
    assert_eq!(
        object_regeneration::regenerate_objects(&mut node, &mut NumberingObjects::default()),
        Err(InlineError::Regeneration(RegenerationError::Malformed(
            MalformedType("Llib/A$f$1".to_string())
        )))
    );
}
