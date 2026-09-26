use super::*;
use crate::jvm::method_node::LabelId;

const ILOAD: u8 = 0x15;
const LLOAD: u8 = 0x16;
const IRETURN: u8 = 0xac;
const RETURN: u8 = 0xb1;
const L2I: u8 = 0x88;
const ICONST_2: u8 = 0x05;

/// One node of a test body; `At(k)` places label `k`, and a jump names the label it goes to.
enum N {
    At(usize),
    Op(u8),
    Var(u8, u16),
    Jump(u8, usize),
    /// `invokestatic owner.name desc`.
    Static(&'static str, &'static str, &'static str),
    /// `getstatic owner.name desc`.
    GetStatic(&'static str, &'static str, &'static str),
}

use N::*;

fn body(desc: &str, max_locals: u16, nodes: &[N]) -> MethodNode {
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
            Static(owner, name, desc) => Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: owner.to_string(),
                name: name.to_string(),
                desc: desc.to_string(),
                interface: false,
            }),
            GetStatic(owner, name, desc) => Node::Insn(Insn::Field {
                op: GETSTATIC,
                owner: owner.to_string(),
                name: name.to_string(),
                desc: desc.to_string(),
            }),
        })
        .collect();
    method
}

/// Run the pass over `nodes` and compare with `expected`; `changed` is what the pass reports.
fn propagates(desc: &str, max_locals: u16, nodes: &[N], expected: &[N], changed: bool) {
    let mut method = body(desc, max_locals, nodes);
    let wanted = body(desc, max_locals, expected);
    assert_eq!(propagate(&mut method, "T"), Ok(changed));
    assert_eq!(method.nodes, wanted.nodes);
}

/// `inline fun sel(c: Boolean, a: Int, b: Int) = if (c) a else b` as a statement: the result of
/// either branch is popped at the merge.
fn discarded_select(then: N, otherwise: N) -> Vec<N> {
    vec![
        Var(ILOAD, 0),
        Jump(IFEQ, 0),
        then,
        Jump(GOTO, 1),
        At(0),
        otherwise,
        At(1),
        Op(POP),
        Op(RETURN),
    ]
}

#[test]
fn a_popped_merge_of_loads_becomes_nops() {
    propagates(
        "(ZII)V",
        3,
        &discarded_select(Var(ILOAD, 1), Var(ILOAD, 2)),
        &[
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            Op(NOP),
            Jump(GOTO, 1),
            At(0),
            Op(NOP),
            At(1),
            Op(NOP),
            Op(RETURN),
        ],
        true,
    );
}

#[test]
fn a_popped_box_pops_what_it_boxed() {
    // The load stays: the boxing consumed it, so it is the `pop` that pops it now.
    propagates(
        "(I)V",
        1,
        &[
            Var(ILOAD, 0),
            Static("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;"),
            Op(POP),
            Op(RETURN),
        ],
        &[Var(ILOAD, 0), Op(POP), Op(NOP), Op(RETURN)],
        true,
    );
}

#[test]
fn a_popped_conversion_pops_its_wide_input() {
    propagates(
        "(J)V",
        2,
        &[Var(LLOAD, 0), Op(L2I), Op(POP), Op(RETURN)],
        &[Var(LLOAD, 0), Op(POP2), Op(NOP), Op(RETURN)],
        true,
    );
}

#[test]
fn a_popped_unit_instance_goes() {
    propagates(
        "()V",
        0,
        &[
            GetStatic("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;"),
            Op(POP),
            Op(RETURN),
        ],
        &[Op(NOP), Op(NOP), Op(RETURN)],
        true,
    );
}

#[test]
fn a_popped_call_result_stays_popped() {
    // Moving the `pop` after the call would not shorten anything (`longerWhenFusedWithPop`).
    propagates(
        "()V",
        0,
        &[Static("T", "g", "()I"), Op(POP), Op(RETURN)],
        &[Static("T", "g", "()I"), Op(POP), Op(RETURN)],
        false,
    );
}

#[test]
fn a_call_merged_with_a_load_gets_its_own_pop() {
    // One pure push against one call balances out: the load goes, the call pops its own result.
    propagates(
        "(ZI)V",
        2,
        &discarded_select(Var(ILOAD, 1), Static("T", "g", "()I")),
        &[
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            Op(NOP),
            Jump(GOTO, 1),
            At(0),
            Static("T", "g", "()I"),
            Op(POP),
            At(1),
            Op(NOP),
            Op(RETURN),
        ],
        true,
    );
}

#[test]
fn a_value_another_path_returns_is_not_touched() {
    // `iload_1` reaches the `pop` on one path and the `ireturn` on the other.
    let nodes = [
        Var(ILOAD, 1),
        Var(ILOAD, 0),
        Jump(IFEQ, 0),
        Op(POP),
        Op(ICONST_0),
        Op(IRETURN),
        At(0),
        Op(IRETURN),
    ];
    propagates("(ZI)I", 2, &nodes, &nodes, false);
}

#[test]
fn the_values_a_dup_x1_reaches_are_not_touched() {
    // `iconst_1` is only ever popped, but the `dup_x1` reaches below it.
    let nodes = [
        Op(ICONST_1),
        Op(ICONST_2),
        Op(DUP_X1),
        Op(POP),
        Op(POP),
        Op(POP),
        Op(RETURN),
    ];
    propagates("()V", 0, &nodes, &nodes, false);
}

#[test]
fn a_pop2_is_left() {
    // A statement `if (c) a else b` of `Long`s (kotlinc keeps it the same way).
    let nodes = [
        Var(ILOAD, 0),
        Jump(IFEQ, 0),
        Var(LLOAD, 1),
        Jump(GOTO, 1),
        At(0),
        Var(LLOAD, 3),
        At(1),
        Op(POP2),
        Op(RETURN),
    ];
    propagates("(ZJJ)V", 5, &nodes, &nodes, false);
}

#[test]
fn a_method_with_neither_pop_nor_pure_push_is_not_analyzed() {
    let nodes = [Op(RETURN)];
    propagates("()V", 0, &nodes, &nodes, false);
}

/// The pass visits nodes in index order to avoid revisiting the rest of a method after every
/// joining branch; the frames and the instructions it must not touch are those the stack order
/// finds.
#[test]
fn the_index_order_finds_what_the_stack_order_finds() {
    let method = body(
        "(I)I",
        3,
        &[
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            Op(ICONST_2),
            Var(ISTORE, 1),
            At(0),
            Var(ILOAD, 0),
            Jump(IFEQ, 1),
            Op(ICONST_2),
            Var(ISTORE, 2),
            At(1),
            Var(ILOAD, 1),
            Op(POP),
            At(2),
            Var(ILOAD, 2),
            Var(ISTORE, 1),
            Var(ILOAD, 0),
            Jump(IFNE, 2),
            Var(ILOAD, 1),
            Op(IRETURN),
        ],
    );
    let run = |in_index_order| {
        let mut interpreter = HazardsTracking::new(method.nodes.len());
        let frames = analyze_with(
            &method,
            "Owner",
            &mut interpreter,
            &mut PlainFrames,
            AnalyzerOptions {
                in_index_order,
                ..AnalyzerOptions::default()
            },
        )
        .expect("the body analyzes");
        (frames, interpreter.dont_touch)
    };
    assert_eq!(run(true), run(false));
}
