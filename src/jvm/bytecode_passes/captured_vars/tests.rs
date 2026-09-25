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
