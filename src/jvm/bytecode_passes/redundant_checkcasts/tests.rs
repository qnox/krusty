use super::*;
use crate::jvm::method_node::{LabelId, LocalVariable, TryCatchBlock};

const ACONST_NULL: u8 = 0x01;
const ASTORE: u8 = 0x3a;
const IFEQ: u8 = 0x99;
const GOTO: u8 = 0xa7;
const ARETURN: u8 = 0xb0;
const POP: u8 = 0x57;

/// One node of a test body; `At(k)` places label `k`, and a jump names the label it goes to.
enum N {
    At(usize),
    Op(u8),
    Var(u8, u16),
    Jump(u8, usize),
    Cast(&'static str),
    Ldc(&'static str),
    Marker,
}

use N::*;

fn body(desc: &str, max_locals: u16, nodes: &[N]) -> (MethodNode, Vec<LabelId>) {
    let mut method = MethodNode::new(0x0009, "f", desc);
    method.max_locals = max_locals;
    let labels: Vec<LabelId> = (0..4).map(|_| method.new_label()).collect();
    method.nodes = nodes
        .iter()
        .map(|node| match *node {
            At(k) => Node::Label(labels[k]),
            Op(op) => Node::Insn(Insn::Op(op)),
            Var(op, slot) => Node::Insn(Insn::Var { op, slot }),
            Jump(op, k) => Node::Insn(Insn::Jump {
                op,
                target: labels[k],
            }),
            Cast(class) => Node::Insn(Insn::Type {
                op: CHECKCAST,
                class: class.to_string(),
            }),
            Ldc(text) => Node::Insn(Insn::Ldc(crate::jvm::method_node::Constant::String(
                text.into(),
            ))),
            Marker => Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "kotlin/jvm/internal/Intrinsics".to_string(),
                name: "reifiedOperationMarker".to_string(),
                desc: "(ILjava/lang/String;)V".to_string(),
                interface: false,
            }),
        })
        .collect();
    (method, labels)
}

/// Run the pass over `nodes` and compare with `expected`; `changed` is what the pass reports.
fn eliminates(desc: &str, max_locals: u16, nodes: &[N], expected: &[N], changed: bool) {
    let (mut method, _) = body(desc, max_locals, nodes);
    let (wanted, _) = body(desc, max_locals, expected);
    assert_eq!(eliminate(&mut method, "T"), Ok(changed));
    assert_eq!(method.nodes, wanted.nodes);
}

#[test]
fn a_cast_to_the_exact_type_of_its_operand_goes() {
    // `(s as String)` of a `String` parameter.
    eliminates(
        "(Ljava/lang/String;)Ljava/lang/Object;",
        1,
        &[Var(ALOAD, 0), Cast("java/lang/String"), Op(ARETURN)],
        &[Var(ALOAD, 0), Op(ARETURN)],
        true,
    );
}

#[test]
fn a_cast_to_a_supertype_stays() {
    // Only the exact type makes a cast redundant; no hierarchy is consulted.
    let nodes = [Var(ALOAD, 0), Cast("java/lang/Object"), Op(ARETURN)];
    eliminates(
        "(Ljava/lang/String;)Ljava/lang/Object;",
        1,
        &nodes,
        &nodes,
        false,
    );
}

#[test]
fn a_cast_of_null_goes() {
    eliminates(
        "()Ljava/lang/Object;",
        0,
        &[Op(ACONST_NULL), Cast("Foo"), Op(ARETURN)],
        &[Op(ACONST_NULL), Op(ARETURN)],
        true,
    );
}

#[test]
fn a_merge_of_two_classes_is_object_and_a_merge_with_null_keeps_the_class() {
    // `if (c) "a" else x` merges `String` with `Foo` into `Object`: the cast stays.
    let merged = [
        Var(0x15, 0),
        Jump(IFEQ, 0),
        Ldc("a"),
        Jump(GOTO, 1),
        At(0),
        Var(ALOAD, 1),
        At(1),
        Cast("java/lang/String"),
        Op(ARETURN),
    ];
    eliminates("(ZLFoo;)Ljava/lang/Object;", 2, &merged, &merged, false);
    // `if (c) "a" else null` is still a `String`: the cast goes.
    eliminates(
        "(Z)Ljava/lang/Object;",
        1,
        &[
            Var(0x15, 0),
            Jump(IFEQ, 0),
            Ldc("a"),
            Jump(GOTO, 1),
            At(0),
            Op(ACONST_NULL),
            At(1),
            Cast("java/lang/String"),
            Op(ARETURN),
        ],
        &[
            Var(0x15, 0),
            Jump(IFEQ, 0),
            Ldc("a"),
            Jump(GOTO, 1),
            At(0),
            Op(ACONST_NULL),
            At(1),
            Op(ARETURN),
        ],
        true,
    );
}

#[test]
fn a_cast_to_a_multi_dimensional_array_stays_and_to_a_flat_one_goes() {
    let nested = [Var(ALOAD, 0), Cast("[[I"), Op(ARETURN)];
    eliminates("([[I)Ljava/lang/Object;", 1, &nested, &nested, false);
    eliminates(
        "([I)Ljava/lang/Object;",
        1,
        &[Var(ALOAD, 0), Cast("[I"), Op(ARETURN)],
        &[Var(ALOAD, 0), Op(ARETURN)],
        true,
    );
}

#[test]
fn a_method_with_a_reified_marker_keeps_its_casts() {
    let nodes = [
        Op(0x03),
        Ldc("T"),
        Marker,
        Var(ALOAD, 0),
        Cast("java/lang/String"),
        Op(ARETURN),
    ];
    eliminates(
        "(Ljava/lang/String;)Ljava/lang/Object;",
        1,
        &nodes,
        &nodes,
        false,
    );
}

#[test]
fn a_load_in_catch_code_has_the_type_the_local_table_declares() {
    // `x` (slot 1) holds the `Object` parameter; the table declares it a `String` over the whole
    // body. In code the entry reaches, its cast stays; in the handler, which only an exception
    // edge reaches, the load has the declared type and the cast goes.
    let nodes = [
        At(0),
        Var(ALOAD, 0),
        Var(ASTORE, 1),
        Var(ALOAD, 1),
        Cast("java/lang/String"),
        Op(ARETURN),
        At(1),
        Op(POP),
        Var(ALOAD, 1),
        Cast("java/lang/String"),
        Op(ARETURN),
        At(2),
    ];
    let (mut method, labels) = body("(Ljava/lang/Object;)Ljava/lang/Object;", 2, &nodes);
    method.try_catch_blocks.push(TryCatchBlock {
        start: labels[0],
        end: labels[1],
        handler: labels[1],
        catch_type: None,
    });
    method.local_variables.push(LocalVariable {
        name: "x".to_string(),
        desc: "Ljava/lang/String;".to_string(),
        start: labels[0],
        end: labels[2],
        slot: 1,
    });
    assert_eq!(eliminate(&mut method, "T"), Ok(true));
    let casts: Vec<usize> = method
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node, Node::Insn(Insn::Type { op: CHECKCAST, .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(casts, vec![4]);
}

#[test]
fn a_method_without_casts_is_left_alone() {
    let nodes = [Var(ALOAD, 0), Op(ARETURN)];
    eliminates(
        "(Ljava/lang/String;)Ljava/lang/Object;",
        1,
        &nodes,
        &nodes,
        false,
    );
}
