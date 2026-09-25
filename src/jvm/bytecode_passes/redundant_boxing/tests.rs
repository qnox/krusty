use super::*;
use crate::jvm::bytecode_passes::opcodes::*;
use crate::jvm::method_node::{Insn, LabelId, LocalVariable, MethodNode, Node};

const LSTORE: u8 = 0x37;
const LRETURN: u8 = 0xad;

struct NoValueClasses;

impl ValueClasses for NoValueClasses {
    fn underlying_type(&self, _internal_name: &str) -> Option<String> {
        None
    }
}

/// `value class Meters(val value: Int)`.
struct Meters;

impl ValueClasses for Meters {
    fn underlying_type(&self, internal_name: &str) -> Option<String> {
        (internal_name == "Meters").then(|| "I".to_string())
    }
}

fn op(op: u8) -> Insn {
    Insn::Op(op)
}

fn var(op: u8, slot: u16) -> Insn {
    Insn::Var { op, slot }
}

fn call(op: u8, owner: &str, name: &str, desc: &str) -> Insn {
    Insn::Method {
        op,
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: op == INVOKEINTERFACE,
    }
}

fn checkcast(class: &str) -> Insn {
    Insn::Type {
        op: CHECKCAST,
        class: class.to_string(),
    }
}

fn int_value_of() -> Insn {
    call(
        INVOKESTATIC,
        "java/lang/Integer",
        "valueOf",
        "(I)Ljava/lang/Integer;",
    )
}

fn number_int_value() -> Insn {
    call(INVOKEVIRTUAL, "java/lang/Number", "intValue", "()I")
}

#[test]
fn boxing_recognizers_require_the_complete_jvm_signature() {
    let coroutine_boxing = call(
        INVOKESTATIC,
        "kotlin/coroutines/jvm/internal/Boxing",
        "boxInt",
        "(I)Ljava/lang/Integer;",
    );
    assert!(super::recognizers::is_boxing(
        &coroutine_boxing,
        &NoValueClasses
    ));
    let wrong_boxing_result = call(
        INVOKESTATIC,
        "kotlin/coroutines/jvm/internal/Boxing",
        "boxInt",
        "(I)Ljava/lang/Long;",
    );
    assert!(!super::recognizers::is_boxing(
        &wrong_boxing_result,
        &NoValueClasses
    ));

    assert!(super::recognizers::is_unboxing(
        &number_int_value(),
        &NoValueClasses
    ));
    let wrong_unboxing_result = call(INVOKEVIRTUAL, "java/lang/Number", "intValue", "()J");
    assert!(!super::recognizers::is_unboxing(
        &wrong_unboxing_result,
        &NoValueClasses
    ));

    let next = call(
        INVOKEINTERFACE,
        "java/util/Iterator",
        "next",
        "()Ljava/lang/Object;",
    );
    assert!(super::recognizers::is_interface_next(&next));
    let next_with_argument = call(
        INVOKEINTERFACE,
        "java/util/Iterator",
        "next",
        "(I)Ljava/lang/Object;",
    );
    assert!(!super::recognizers::is_interface_next(&next_with_argument));
}

/// A static method of `desc` over `insns`, with a label before the first and after the last so
/// local variables can span the body.
struct Body {
    method: MethodNode,
    start: LabelId,
    end: LabelId,
}

impl Body {
    fn new(desc: &str, max_locals: u16, insns: Vec<Insn>) -> Body {
        let mut method = MethodNode::new(0x0009, "f", desc);
        method.max_locals = max_locals;
        let start = method.new_label();
        let end = method.new_label();
        method.nodes.push(Node::Label(start));
        method.nodes.extend(insns.into_iter().map(Node::Insn));
        method.nodes.push(Node::Label(end));
        Body { method, start, end }
    }

    /// A local variable over the whole body.
    fn local(mut self, name: &str, desc: &str, slot: u16) -> Body {
        self.method.local_variables.push(LocalVariable {
            name: name.to_string(),
            desc: desc.to_string(),
            start: self.start,
            end: self.end,
            slot,
        });
        self
    }

    fn eliminate(&mut self, value_classes: &dyn ValueClasses) -> bool {
        eliminate(&mut self.method, "A", value_classes).expect("the analysis completes")
    }

    fn instructions(&self) -> Vec<Insn> {
        self.method.instructions().cloned().collect()
    }

    fn locals(&self) -> Vec<(&str, u16)> {
        self.method
            .local_variables
            .iter()
            .map(|variable| (variable.desc.as_str(), variable.slot))
            .collect()
    }
}

#[test]
fn a_box_kept_in_a_local_and_unboxed_is_never_made() {
    // val boxed: Int? = i; return boxed as Int
    let mut body = Body::new(
        "(I)I",
        2,
        vec![
            var(ILOAD, 0),
            int_value_of(),
            var(ASTORE, 1),
            var(ALOAD, 1),
            checkcast("java/lang/Number"),
            number_int_value(),
            op(IRETURN),
        ],
    )
    .local("boxed", "Ljava/lang/Integer;", 1);
    assert!(body.eliminate(&NoValueClasses));
    assert_eq!(
        body.instructions(),
        vec![var(ILOAD, 0), var(ISTORE, 1), var(ILOAD, 1), op(IRETURN)]
    );
    assert_eq!(body.locals(), vec![("I", 1)]);
}

#[test]
fn a_box_passed_to_a_call_stays() {
    let mut body = Body::new(
        "(I)V",
        1,
        vec![
            var(ILOAD, 0),
            int_value_of(),
            call(INVOKESTATIC, "A", "take", "(Ljava/lang/Object;)V"),
            op(RETURN),
        ],
    );
    let before = body.instructions();
    assert!(!body.eliminate(&NoValueClasses));
    assert_eq!(body.instructions(), before);
}

#[test]
fn a_long_box_widens_its_slot_and_moves_the_slots_above_it() {
    let mut body = Body::new(
        "(J)J",
        4,
        vec![
            var(LLOAD, 0),
            call(
                INVOKESTATIC,
                "java/lang/Long",
                "valueOf",
                "(J)Ljava/lang/Long;",
            ),
            var(ASTORE, 2),
            op(ICONST_0),
            var(ISTORE, 3),
            var(ALOAD, 2),
            call(INVOKEVIRTUAL, "java/lang/Long", "longValue", "()J"),
            op(LRETURN),
        ],
    )
    .local("boxed", "Ljava/lang/Long;", 2)
    .local("flag", "I", 3);
    assert!(body.eliminate(&NoValueClasses));
    assert_eq!(
        body.instructions(),
        vec![
            var(LLOAD, 0),
            var(LSTORE, 2),
            op(ICONST_0),
            var(ISTORE, 4),
            var(LLOAD, 2),
            op(LRETURN)
        ]
    );
    assert_eq!(body.locals(), vec![("J", 2), ("I", 4)]);
    assert_eq!(body.method.max_locals, 5);
}

#[test]
fn an_unboxing_to_another_primitive_becomes_a_conversion() {
    let mut body = Body::new(
        "(I)J",
        1,
        vec![
            var(ILOAD, 0),
            int_value_of(),
            checkcast("java/lang/Number"),
            call(INVOKEVIRTUAL, "java/lang/Number", "longValue", "()J"),
            op(LRETURN),
        ],
    );
    assert!(body.eliminate(&NoValueClasses));
    assert_eq!(
        body.instructions(),
        vec![var(ILOAD, 0), op(I2L), op(LRETURN)]
    );
}

#[test]
fn are_equal_of_two_int_boxes_fuses_with_its_branch() {
    let mut method = MethodNode::new(0x0009, "f", "(II)I");
    method.max_locals = 2;
    let differ = method.new_label();
    let are_equal = call(
        INVOKESTATIC,
        "kotlin/jvm/internal/Intrinsics",
        "areEqual",
        "(Ljava/lang/Object;Ljava/lang/Object;)Z",
    );
    let mut nodes = vec![
        var(ILOAD, 0),
        int_value_of(),
        var(ILOAD, 1),
        int_value_of(),
        are_equal,
        Insn::Jump {
            op: IFEQ,
            target: differ,
        },
        op(ICONST_1),
        op(IRETURN),
    ]
    .into_iter()
    .map(Node::Insn)
    .collect::<Vec<_>>();
    nodes.push(Node::Label(differ));
    nodes.push(Node::Insn(op(ICONST_0)));
    nodes.push(Node::Insn(op(IRETURN)));
    method.nodes = nodes;
    assert!(eliminate(&mut method, "A", &NoValueClasses).expect("the analysis completes"));
    assert_eq!(
        method.instructions().cloned().collect::<Vec<_>>(),
        vec![
            var(ILOAD, 0),
            var(ILOAD, 1),
            Insn::Jump {
                op: IF_ICMPNE,
                target: differ
            },
            op(ICONST_1),
            op(IRETURN),
            op(ICONST_0),
            op(IRETURN),
        ]
    );
}

#[test]
fn a_local_that_also_holds_another_value_keeps_its_box() {
    // var boxed: Any = i; if (c) boxed = "s"; return boxed
    let mut method = MethodNode::new(0x0009, "f", "(IZ)Ljava/lang/Object;");
    method.max_locals = 3;
    let (start, join, end) = (method.new_label(), method.new_label(), method.new_label());
    method.nodes = vec![
        Node::Label(start),
        Node::Insn(var(ILOAD, 0)),
        Node::Insn(int_value_of()),
        Node::Insn(var(ASTORE, 2)),
        Node::Insn(var(ILOAD, 1)),
        Node::Insn(Insn::Jump {
            op: IFEQ,
            target: join,
        }),
        Node::Insn(Insn::Ldc(crate::jvm::method_node::Constant::String(
            "s".into(),
        ))),
        Node::Insn(var(ASTORE, 2)),
        Node::Label(join),
        Node::Insn(var(ALOAD, 2)),
        Node::Insn(checkcast("java/lang/Number")),
        Node::Insn(number_int_value()),
        Node::Insn(op(POP)),
        Node::Insn(op(ACONST_NULL)),
        Node::Insn(op(ARETURN)),
        Node::Label(end),
    ];
    method.local_variables.push(LocalVariable {
        name: "boxed".to_string(),
        desc: "Ljava/lang/Object;".to_string(),
        start,
        end,
        slot: 2,
    });
    let before = method.clone();
    assert!(!eliminate(&mut method, "A", &NoValueClasses).expect("the analysis completes"));
    assert_eq!(method, before);
}

#[test]
fn a_progression_iterator_yields_unboxed_elements() {
    let mut body = Body::new(
        "(Lkotlin/ranges/IntRange;)I",
        2,
        vec![
            var(ALOAD, 0),
            call(
                INVOKEINTERFACE,
                "java/lang/Iterable",
                "iterator",
                "()Ljava/util/Iterator;",
            ),
            var(ASTORE, 1),
            var(ALOAD, 1),
            call(
                INVOKEINTERFACE,
                "java/util/Iterator",
                "next",
                "()Ljava/lang/Object;",
            ),
            checkcast("java/lang/Number"),
            number_int_value(),
            op(IRETURN),
        ],
    );
    assert!(body.eliminate(&NoValueClasses));
    assert_eq!(
        body.instructions(),
        vec![
            var(ALOAD, 0),
            call(
                INVOKEINTERFACE,
                "java/lang/Iterable",
                "iterator",
                "()Ljava/util/Iterator;",
            ),
            var(ASTORE, 1),
            var(ALOAD, 1),
            checkcast("kotlin/collections/IntIterator"),
            call(
                INVOKEVIRTUAL,
                "kotlin/collections/IntIterator",
                "nextInt",
                "()I"
            ),
            op(IRETURN),
        ]
    );
}

#[test]
fn a_value_class_box_unboxed_again_is_never_made() {
    let mut body = Body::new(
        "(I)I",
        1,
        vec![
            var(ILOAD, 0),
            call(INVOKESTATIC, "Meters", "box-impl", "(I)LMeters;"),
            op(DUP),
            op(POP),
            call(INVOKEVIRTUAL, "Meters", "unbox-impl", "()I"),
            op(IRETURN),
        ],
    );
    assert!(body.eliminate(&Meters));
    assert_eq!(
        body.instructions(),
        vec![var(ILOAD, 0), op(DUP), op(POP), op(IRETURN)]
    );
}

#[test]
fn a_value_class_box_is_not_a_number() {
    let mut body = Body::new(
        "(I)I",
        1,
        vec![
            var(ILOAD, 0),
            call(INVOKESTATIC, "Meters", "box-impl", "(I)LMeters;"),
            checkcast("java/lang/Number"),
            op(POP),
            op(ICONST_0),
            op(IRETURN),
        ],
    );
    assert!(!body.eliminate(&Meters));
}
