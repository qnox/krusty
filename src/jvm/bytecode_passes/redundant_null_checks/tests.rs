use super::*;
use crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors;
use crate::jvm::method_node::{Constant, LabelId, TryCatchBlock};

/// One instruction of a test body; a jump names the instruction it goes to by number.
enum I {
    Insn(Insn),
    Jump(u8, usize),
}

fn op(op: u8) -> I {
    I::Insn(Insn::Op(op))
}

fn jump(op: u8, to: usize) -> I {
    I::Jump(op, to)
}

fn aload(slot: u16) -> I {
    I::Insn(Insn::Var { op: ALOAD, slot })
}

fn astore(slot: u16) -> I {
    I::Insn(Insn::Var { op: ASTORE, slot })
}

fn istore(slot: u16) -> I {
    I::Insn(Insn::Var { op: ISTORE, slot })
}

fn ldc(value: &str) -> I {
    I::Insn(Insn::Ldc(Constant::String(value.into())))
}

fn type_insn(op: u8, class: &str) -> I {
    I::Insn(Insn::Type {
        op,
        class: class.to_string(),
    })
}

fn call(op: u8, owner: &str, name: &str, desc: &str) -> I {
    I::Insn(Insn::Method {
        op,
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: op == INVOKEINTERFACE,
    })
}

fn intrinsic(name: &str, desc: &str) -> I {
    call(INVOKESTATIC, INTRINSICS, name, desc)
}

fn check() -> I {
    intrinsic("checkNotNull", "(Ljava/lang/Object;)V")
}

fn check_with_message() -> I {
    intrinsic("checkNotNull", OBJECT_AND_MESSAGE)
}

fn check_parameter() -> I {
    intrinsic("checkNotNullParameter", OBJECT_AND_MESSAGE)
}

fn check_expression() -> I {
    intrinsic("checkNotNullExpressionValue", OBJECT_AND_MESSAGE)
}

fn marker(kind: i32) -> [I; 3] {
    [
        I::Insn(Insn::Int {
            op: BIPUSH,
            operand: kind,
        }),
        ldc("T"),
        intrinsic("reifiedOperationMarker", "(ILjava/lang/String;)V"),
    ]
}

/// A static method `desc` of `insns`, a label in front of each, and the labels.
fn method_of(insns: Vec<I>, desc: &str, max_locals: u16) -> (MethodNode, Vec<LabelId>) {
    let mut method = MethodNode::new(0x0009, "f", desc);
    method.max_locals = max_locals;
    let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
    for (k, insn) in insns.into_iter().enumerate() {
        method.nodes.push(Node::Label(labels[k]));
        method.nodes.push(Node::Insn(resolve(insn, &labels)));
    }
    method
        .nodes
        .push(Node::Label(*labels.last().expect("an end label")));
    (method, labels)
}

fn resolve(insn: I, labels: &[LabelId]) -> Insn {
    match insn {
        I::Insn(insn) => insn,
        I::Jump(op, to) => Insn::Jump {
            op,
            target: labels[to],
        },
    }
}

fn run_method(method: &MethodNode) -> Option<Rewritten> {
    eliminate(method, "T", &ValueClassDescriptors::default()).expect("the analysis completes")
}

fn instructions(nodes: &[Node]) -> Vec<Insn> {
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Insn(insn) => Some(insn.clone()),
            _ => None,
        })
        .collect()
}

/// The instructions the pass leaves of `insns` against `expected` (jumps in both name the
/// instructions of `insns` by number); `expected: None` when the pass changes nothing.
fn assert_rewrites(insns: Vec<I>, desc: &str, max_locals: u16, expected: Option<Vec<I>>) {
    let (method, labels) = method_of(insns, desc, max_locals);
    let expected = expected.map(|expected| {
        expected
            .into_iter()
            .map(|insn| resolve(insn, &labels))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        run_method(&method).map(|rewritten| instructions(&rewritten.nodes)),
        expected
    );
}

#[test]
fn an_elvis_on_a_known_string_always_takes_the_value() {
    // `inline fun orD(x: String?) = x ?: "d"` inlined on "a".
    assert_rewrites(
        vec![
            ldc("a"),
            astore(0),
            aload(0),
            op(DUP),
            jump(IFNONNULL, 8),
            op(POP),
            ldc("d"),
            op(NOP),
            op(ARETURN),
        ],
        "()Ljava/lang/String;",
        1,
        Some(vec![
            ldc("a"),
            astore(0),
            aload(0),
            jump(GOTO, 8),
            op(POP),
            ldc("d"),
            op(NOP),
            op(ARETURN),
        ]),
    );
}

#[test]
fn an_instanceof_of_null_is_false() {
    // `inline fun isS(x: Any?) = x is String` inlined on `null`.
    assert_rewrites(
        vec![
            op(ACONST_NULL),
            astore(0),
            aload(0),
            type_insn(INSTANCEOF, "java/lang/String"),
            op(IRETURN),
        ],
        "()Z",
        1,
        Some(vec![op(ACONST_NULL), astore(0), op(ICONST_0), op(IRETURN)]),
    );
}

#[test]
fn a_checked_parameter_copied_to_a_local_is_known_non_null() {
    // `fun t3(s: String): Int { val t: String? = s; return if (t == null) 0 else 1 }`
    assert_rewrites(
        vec![
            aload(0),
            ldc("s"),
            check_parameter(),
            aload(0),
            astore(1),
            aload(1),
            jump(IFNONNULL, 9),
            op(ICONST_0),
            jump(GOTO, 10),
            op(ICONST_1),
            op(IRETURN),
        ],
        "(Ljava/lang/String;)I",
        2,
        Some(vec![
            aload(0),
            ldc("s"),
            check_parameter(),
            aload(0),
            astore(1),
            jump(GOTO, 9),
            op(ICONST_0),
            jump(GOTO, 10),
            op(ICONST_1),
            op(IRETURN),
        ]),
    );
}

#[test]
fn a_second_test_of_a_local_on_its_non_null_edge_goes() {
    assert_rewrites(
        vec![
            aload(0),
            jump(IFNULL, 6),
            aload(0),
            jump(IFNULL, 6),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ],
        "(Ljava/lang/Object;)Z",
        1,
        Some(vec![
            aload(0),
            jump(IFNULL, 6),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ]),
    );
}

#[test]
fn an_unknown_operand_keeps_its_test() {
    assert_rewrites(
        vec![
            aload(0),
            jump(IFNULL, 4),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ],
        "(Ljava/lang/Object;)Z",
        1,
        None,
    );
}

#[test]
fn a_folded_jump_sharpens_the_next_round() {
    // In the first round the local meets `null` from the (still live) fall-through at 4; once the
    // jump at 3 is a `goto`, that path is dead and the second round folds the `ifnull` at 7.
    assert_rewrites(
        vec![
            ldc("s"),
            astore(1),
            aload(1),
            jump(IFNONNULL, 6),
            op(ACONST_NULL),
            astore(1),
            aload(1),
            jump(IFNULL, 10),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ],
        "()Z",
        2,
        Some(vec![
            ldc("s"),
            astore(1),
            jump(GOTO, 6),
            op(ACONST_NULL),
            astore(1),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ]),
    );
}

#[test]
fn an_instanceof_of_a_new_object_of_its_class_is_true() {
    assert_rewrites(
        vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            type_insn(INSTANCEOF, "Foo"),
            op(IRETURN),
        ],
        "()Z",
        0,
        Some(vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            op(POP),
            op(ICONST_1),
            op(IRETURN),
        ]),
    );
}

#[test]
fn a_cast_keeps_its_operands_class_for_instanceof() {
    // A `checkcast` keeps its operand's value: the value is still a `Foo`, not a `Bar`.
    assert_rewrites(
        vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            type_insn(CHECKCAST, "Bar"),
            type_insn(INSTANCEOF, "Bar"),
            op(IRETURN),
        ],
        "()Z",
        0,
        None,
    );
}

#[test]
fn a_reified_instanceof_placeholder_stays() {
    let mut insns = vec![op(ACONST_NULL)];
    insns.extend(marker(3));
    insns.extend([type_insn(INSTANCEOF, "java/lang/Object"), op(IRETURN)]);
    assert_rewrites(insns, "()Z", 0, None);
}

#[test]
fn a_type_of_placeholder_is_not_null() {
    let mut insns = Vec::from(marker(6));
    insns.extend([
        op(ACONST_NULL),
        astore(0),
        aload(0),
        jump(IFNULL, 9),
        op(ICONST_1),
        op(IRETURN),
        op(ICONST_0),
        op(IRETURN),
    ]);
    assert_rewrites(insns, "()Z", 1, None);
}

#[test]
fn a_reified_safe_cast_may_make_null() {
    // `x as? T` on a new object: the placeholder cast's result is not known to be non-null.
    let mut insns = vec![
        type_insn(NEW, "Foo"),
        op(DUP),
        call(INVOKESPECIAL, "Foo", "<init>", "()V"),
    ];
    insns.extend(marker(2));
    insns.extend([
        type_insn(CHECKCAST, "java/lang/Object"),
        op(DUP),
        check(),
        op(ARETURN),
    ]);
    assert_rewrites(insns, "()Ljava/lang/Object;", 0, None);
}

#[test]
fn a_cast_of_a_checked_parameter_needs_no_null_check() {
    // `fun q(a: Any) = a as String`
    assert_rewrites(
        vec![
            aload(0),
            ldc("a"),
            check_parameter(),
            aload(0),
            op(DUP),
            ldc("m"),
            check_with_message(),
            type_insn(CHECKCAST, "java/lang/String"),
            op(ARETURN),
        ],
        "(Ljava/lang/Object;)Ljava/lang/String;",
        1,
        Some(vec![
            aload(0),
            ldc("a"),
            check_parameter(),
            aload(0),
            type_insn(CHECKCAST, "java/lang/String"),
            op(ARETURN),
        ]),
    );
}

#[test]
fn an_unchecked_parameter_keeps_its_null_check() {
    assert_rewrites(
        vec![
            aload(0),
            op(DUP),
            ldc("m"),
            check_with_message(),
            op(ARETURN),
        ],
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        1,
        None,
    );
}

#[test]
fn a_string_constant_needs_no_expression_check() {
    // The checked value is the `dup` before the message: it goes with the message and the call.
    assert_rewrites(
        vec![ldc("s"), op(DUP), ldc("m"), check_expression(), op(ARETURN)],
        "()Ljava/lang/Object;",
        0,
        Some(vec![ldc("s"), op(ARETURN)]),
    );
}

#[test]
fn an_expression_check_pops_a_value_it_cannot_unload() {
    // The checked copy of the new object is left by the constructor call, not by a load or `dup`
    // right before the message, so it is popped instead.
    assert_rewrites(
        vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            ldc("m"),
            check_expression(),
            op(ARETURN),
        ],
        "()Ljava/lang/Object;",
        0,
        Some(vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            op(POP),
            op(ARETURN),
        ]),
    );
}

#[test]
fn matching_non_null_branch_stores_keep_the_fact_at_the_merge() {
    assert_rewrites(
        vec![
            aload(0),
            jump(IFNULL, 5),
            ldc("a"),
            astore(1),
            jump(GOTO, 7),
            ldc("b"),
            astore(1),
            aload(1),
            op(DUP),
            check(),
            op(ARETURN),
        ],
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        2,
        Some(vec![
            aload(0),
            jump(IFNULL, 5),
            ldc("a"),
            astore(1),
            jump(GOTO, 7),
            ldc("b"),
            astore(1),
            aload(1),
            op(ARETURN),
        ]),
    );
}

#[test]
fn a_store_of_an_int_forgets_a_slot() {
    // A slot reused for an `int` and then for `null` holds `null` at the test.
    assert_rewrites(
        vec![
            ldc("s"),
            astore(0),
            op(ICONST_1),
            istore(0),
            op(ACONST_NULL),
            astore(0),
            aload(0),
            jump(IFNULL, 10),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ],
        "()Z",
        1,
        Some(vec![
            ldc("s"),
            astore(0),
            op(ICONST_1),
            istore(0),
            op(ACONST_NULL),
            astore(0),
            jump(GOTO, 10),
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ]),
    );
}

#[test]
fn a_throw_helper_still_reaches_its_exception_handler() {
    // Two protected ranges share one handler. The first reaches it with local 1 non-null; the
    // `Intrinsics` throw helper reaches it with local 1 null, before its injected `athrow`.
    let insns = vec![
        op(ICONST_0),                                       // 0
        jump(IFEQ, 5),                                      // 1
        ldc("s"),                                           // 2
        astore(1),                                          // 3
        jump(GOTO, 8),                                      // 4
        op(ACONST_NULL),                                    // 5
        astore(1),                                          // 6
        jump(GOTO, 10),                                     // 7
        call(INVOKESTATIC, "fixture/Owner", "call", "()V"), // 8
        jump(GOTO, 17),                                     // 9
        intrinsic("throwNpe", "()V"),                       // 10
        jump(GOTO, 17),                                     // 11
        op(POP),                                            // 12 handler
        aload(1),                                           // 13
        op(DUP),                                            // 14
        check(),                                            // 15
        op(ARETURN),                                        // 16
        op(RETURN),                                         // 17
    ];
    let (mut method, labels) = method_of(insns, "()Ljava/lang/Object;", 2);
    for (start, end) in [(8, 9), (10, 11)] {
        method.try_catch_blocks.push(TryCatchBlock {
            start: labels[start],
            end: labels[end],
            handler: labels[12],
            catch_type: None,
        });
    }
    assert_eq!(run_method(&method), None);
}

#[test]
fn a_line_number_separates_a_check_from_its_load() {
    // `aload_0; [line] ifnull`: ASM's node before the jump is the line number, so the jump
    // teaches nothing about local 0 and the check after it stays.
    let mut method = MethodNode::new(0x0009, "f", "(Ljava/lang/Object;)Ljava/lang/Object;");
    method.max_locals = 1;
    let line = method.new_label();
    let null = method.new_label();
    let I::Insn(check) = check() else {
        unreachable!("an instruction");
    };
    method.nodes = vec![
        Node::Insn(Insn::Var { op: ALOAD, slot: 0 }),
        Node::Label(line),
        Node::Line {
            line: 2,
            start: line,
        },
        Node::Insn(Insn::Jump {
            op: IFNULL,
            target: null,
        }),
        Node::Insn(Insn::Var { op: ALOAD, slot: 0 }),
        Node::Insn(Insn::Op(DUP)),
        Node::Insn(check),
        Node::Insn(Insn::Op(ARETURN)),
        Node::Label(null),
        Node::Insn(Insn::Op(ACONST_NULL)),
        Node::Insn(Insn::Op(ARETURN)),
    ];
    assert_eq!(run_method(&method), None);
}

#[test]
fn a_label_nothing_names_does_not_separate_a_check_from_its_load() {
    // Every instruction of the body has a label in front; only the jump's target is a node.
    assert_rewrites(
        vec![
            aload(0),
            jump(IFNULL, 6),
            aload(0),
            op(DUP),
            check(),
            op(ARETURN),
            op(ACONST_NULL),
            op(ARETURN),
        ],
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        1,
        Some(vec![
            aload(0),
            jump(IFNULL, 6),
            aload(0),
            op(ARETURN),
            op(ACONST_NULL),
            op(ARETURN),
        ]),
    );
}

#[test]
fn the_result_says_where_each_node_came_from() {
    let (method, _) = method_of(
        vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            type_insn(INSTANCEOF, "Foo"),
            op(IRETURN),
        ],
        "()Z",
        0,
    );
    let rewritten = run_method(&method).expect("the instanceof folds");
    assert_eq!(rewritten.nodes.len(), rewritten.origins.len());
    // Node 2k + 1 is instruction k; the `pop` is new and the `instanceof`'s node now holds
    // `iconst_1`.
    let origins: Vec<Option<usize>> = rewritten
        .nodes
        .iter()
        .zip(&rewritten.origins)
        .filter(|(node, _)| matches!(node, Node::Insn(_)))
        .map(|(_, origin)| *origin)
        .collect();
    assert_eq!(
        origins,
        vec![Some(1), Some(3), Some(5), None, Some(7), Some(9)]
    );
}

#[test]
fn the_intrinsics_calls_are_known_by_owner_name_and_descriptor() {
    let method = |owner: &str, name: &str, desc: &str| Insn::Method {
        op: INVOKESTATIC,
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: false,
    };
    assert_eq!(
        Check::of(&method(INTRINSICS, "checkNotNull", "(Ljava/lang/Object;)V")),
        Some(Check::NotNull)
    );
    assert_eq!(
        Check::of(&method(
            "fixture/Intrinsics",
            "checkNotNull",
            "(Ljava/lang/Object;)V"
        )),
        None
    );
    assert_eq!(
        Check::of(&method(
            INTRINSICS,
            "checkNotNull",
            "(Ljava/lang/Object;)Ljava/lang/Object;"
        )),
        None
    );
    // kotlinc knows a throw helper by owner and name alone.
    assert!(is_throw_intrinsic(&method(
        INTRINSICS,
        "throwNpe",
        "(Ljava/lang/String;)V"
    )));
    assert!(!is_throw_intrinsic(&method(
        "fixture/Intrinsics",
        "throwNpe",
        "()V"
    )));
    // `checkNotNullParameter` teaches, but is never itself rewritten.
    assert!(!is_optimizable(&method(
        INTRINSICS,
        "checkNotNullParameter",
        OBJECT_AND_MESSAGE
    )));
}

#[test]
fn nullness_analysis_has_a_checked_size_ceiling() {
    assert!(analysis_within_limit(1_000, 50, 10));
    assert!(!analysis_within_limit(usize::MAX, 2, 2));
    assert!(!analysis_within_limit(65_535, 65_535, 0));
    // A method with few locals is still too wide when its operand stack is deep.
    assert!(!analysis_within_limit(65_535, 1, 65_535));
    assert!(!analysis_within_limit(1_000, usize::MAX, 1));
    assert!(!analysis_within_limit(1_000, 1, usize::MAX));
}

#[test]
fn a_method_too_wide_to_analyze_is_left_as_it_is() {
    // Foldable, but one frame per node at a 65,535-value stack is over the ceiling.
    let (mut method, _) = method_of(
        vec![
            type_insn(NEW, "Foo"),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            type_insn(INSTANCEOF, "Foo"),
            op(IRETURN),
        ],
        "()Z",
        0,
    );
    assert!(run_method(&method).is_some());
    method.max_stack = u16::MAX;
    method
        .nodes
        .splice(0..0, (0..1_000).map(|_| Node::Insn(Insn::Op(NOP))));
    assert_eq!(run_method(&method), None);
}

/// A call that has the reified-operation marker's owner and name but not its identity is ordinary
/// bytecode: the `instanceof` after it folds, and the call and its arguments stay.
#[test]
fn a_call_that_only_shares_the_markers_name_is_no_placeholder() {
    for interface in [false, true] {
        let desc = if interface {
            "(ILjava/lang/String;)V"
        } else {
            "(ILjava/lang/Object;)V"
        };
        let named = || {
            I::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: INTRINSICS.to_string(),
                name: "reifiedOperationMarker".to_string(),
                desc: desc.to_string(),
                interface,
            })
        };
        let kind = || {
            I::Insn(Insn::Int {
                op: BIPUSH,
                operand: 3,
            })
        };
        assert_rewrites(
            vec![
                op(ACONST_NULL),
                kind(),
                ldc("T"),
                named(),
                type_insn(INSTANCEOF, "java/lang/Object"),
                op(IRETURN),
            ],
            "()Z",
            0,
            Some(vec![
                op(ACONST_NULL),
                kind(),
                ldc("T"),
                named(),
                op(POP),
                op(ICONST_0),
                op(IRETURN),
            ]),
        );
    }
}

/// Every check of a long method folds in one pass over it: a round finds each check where the
/// analysis did, instead of searching the method for it.
#[test]
fn many_foldable_checks_rewrite_in_linear_time() {
    const CHECKS: usize = 20_000;
    let mut insns = vec![
        type_insn(NEW, "Foo"),
        op(DUP),
        call(INVOKESPECIAL, "Foo", "<init>", "()V"),
        astore(0),
    ];
    for _ in 0..CHECKS {
        insns.extend([aload(0), ldc("m"), check_with_message()]);
    }
    insns.extend([aload(0), op(ARETURN)]);
    let (mut method, _) = method_of(insns, "()Ljava/lang/Object;", 1);
    method.max_stack = 2;
    let started = std::time::Instant::now();
    let rewritten = run_method(&method).expect("every check folds");
    let elapsed = started.elapsed();
    assert_eq!(
        instructions(&rewritten.nodes),
        [
            type_insn(NEW, "Foo"),
            op(DUP),
            call(INVOKESPECIAL, "Foo", "<init>", "()V"),
            astore(0),
            aload(0),
            op(ARETURN),
        ]
        .into_iter()
        .map(|insn| resolve(insn, &[]))
        .collect::<Vec<_>>()
    );
    // Searching the method for each check, or shifting it on each removal, costs about
    // CHECKS * nodes = 2.4e9 steps here; the linear rewrite takes milliseconds.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "{CHECKS} checks took {elapsed:?}"
    );
}
