use super::*;

const INT_REF: &str = "kotlin/jvm/internal/Ref$IntRef";
const IADD: u8 = 0x60;
const IRETURN: u8 = 0xac;
const ICONST_1: u8 = 0x04;
const ICONST_5: u8 = 0x08;

fn op(op: u8) -> Node {
    Node::Insn(Insn::Op(op))
}

fn var(op: u8, slot: u16) -> Node {
    Node::Insn(Insn::Var { op, slot })
}

fn element(op: u8) -> Node {
    Node::Insn(Insn::Field {
        op,
        owner: INT_REF.into(),
        name: "element".into(),
        desc: "I".into(),
    })
}

fn new_int_ref() -> Vec<Node> {
    vec![
        Node::Insn(Insn::Type {
            op: NEW,
            class: INT_REF.into(),
        }),
        op(DUP),
        Node::Insn(Insn::Method {
            op: INVOKESPECIAL,
            owner: INT_REF.into(),
            name: "<init>".into(),
            desc: "()V".into(),
            interface: false,
        }),
        var(ASTORE, 0),
    ]
}

/// `var s = 5; s += 1; return s` with `s` captured by an inlined lambda: `s` lives in an `IntRef`.
fn captured_counter() -> MethodNode {
    let mut method = MethodNode::new(0x0008, "f", "()I");
    method.nodes = new_int_ref();
    method.nodes.extend([
        var(ALOAD, 0),
        op(ICONST_5),
        element(PUTFIELD),
        var(ALOAD, 0),
        var(ALOAD, 0),
        element(GETFIELD),
        op(ICONST_1),
        op(IADD),
        element(PUTFIELD),
        var(ALOAD, 0),
        element(GETFIELD),
        op(IRETURN),
    ]);
    method.max_locals = 1;
    method.max_stack = 3;
    method
}

#[test]
fn a_ref_that_never_escapes_becomes_a_local() {
    let mut method = captured_counter();
    assert_eq!(eliminate(&mut method, "T"), Ok(true));
    // Without a local variable entry the element takes a new slot, above the `Ref`'s.
    assert_eq!(
        method.nodes,
        vec![
            op(ICONST_5),
            var(ISTORE, 1),
            var(ILOAD, 1),
            op(ICONST_1),
            op(IADD),
            var(ISTORE, 1),
            var(ILOAD, 1),
            op(IRETURN),
        ]
    );
    assert_eq!(method.max_locals, 2);
}

/// Holder-first shape of an inlined capture: the line is on the holder's store, the range
/// opens after it, and the element is set inside the range. Unboxing plants the default store
/// under that line and leaves the range on the real initializer.
#[test]
fn an_unboxed_local_is_defaulted_before_its_range() {
    let mut method = MethodNode::new(0x0009, "capturedVar", "(I)I");
    let store_label = method.new_label();
    let range = method.new_label();
    let end = method.new_label();
    let mut created = new_int_ref();
    let holder_store = created.pop().expect("the holder is stored");
    method.nodes = created;
    method.nodes.extend([
        Node::Label(store_label),
        Node::Line {
            line: 2,
            start: store_label,
        },
        holder_store,
        Node::Label(range),
        var(ALOAD, 0),
        op(ICONST_5),
        element(PUTFIELD),
        var(ALOAD, 0),
        element(GETFIELD),
        Node::Label(end),
        op(IRETURN),
    ]);
    method.local_variables = vec![crate::jvm::method_node::LocalVariable {
        name: "x".to_string(),
        desc: format!("L{INT_REF};"),
        start: range,
        end,
        slot: 0,
    }];
    method.max_locals = 1;
    method.max_stack = 3;
    assert_eq!(eliminate(&mut method, "T"), Ok(true));
    let instructions: Vec<Insn> = method
        .nodes
        .iter()
        .filter_map(|node| match node {
            Node::Insn(insn) => Some(insn.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        instructions,
        vec![
            Insn::Op(0x03),
            Insn::Var {
                op: ISTORE,
                slot: 0,
            },
            Insn::Op(ICONST_5),
            Insn::Var {
                op: ISTORE,
                slot: 0,
            },
            Insn::Var { op: ILOAD, slot: 0 },
            Insn::Op(IRETURN),
        ]
    );
    let line_marks_default = method.nodes.iter().any(|node| {
        let Node::Line { line: 2, start } = node else {
            return false;
        };
        method
            .nodes
            .iter()
            .skip_while(|entry| !matches!(entry, Node::Label(label) if label == start))
            .nth(1)
            .is_some_and(|entry| matches!(entry, Node::Insn(Insn::Op(0x03))))
    });
    assert!(line_marks_default, "{:?}", method.nodes);
    assert_eq!(method.local_variables[0].desc, "I");
    assert_eq!(method.local_variables[0].slot, 0);
    assert_eq!(method.local_variables[0].start, range);
}

#[test]
fn a_ref_passed_to_a_call_stays() {
    let mut method = captured_counter();
    let at = method.nodes.len() - 3;
    method.nodes.splice(
        at..at,
        [
            var(ALOAD, 0),
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "T".into(),
                name: "g".into(),
                desc: "(Lkotlin/jvm/internal/Ref$IntRef;)V".into(),
                interface: false,
            }),
        ],
    );
    let before = method.clone();
    assert_eq!(eliminate(&mut method, "T"), Ok(false));
    assert_eq!(method, before);
}
