use super::*;
use crate::jvm::method_node::{Constant, LabelId};

const ILOAD: u8 = 0x15;
const ISTORE: u8 = 0x36;
const IRETURN: u8 = 0xac;
const ICONST_1: u8 = 0x04;
const ICONST_2: u8 = 0x05;
const ICONST_3: u8 = 0x06;
const NOP: u8 = 0x00;

/// One node of a test body; `At(k)` places label `k`, and a jump names the label it goes to.
enum N {
    At(usize),
    Line(u16, usize),
    Op(u8),
    Int(u8, i32),
    Ldc(i32),
    Var(u8, u16),
    Iinc(u16, i16),
    Jump(u8, usize),
}

use N::*;

fn body(desc: &str, max_locals: u16, nodes: &[N]) -> (MethodNode, Vec<LabelId>) {
    let mut method = MethodNode::new(0x0009, "f", desc);
    method.max_locals = max_locals;
    let labels: Vec<LabelId> = (0..8).map(|_| method.new_label()).collect();
    method.nodes = nodes
        .iter()
        .map(|node| match *node {
            At(k) => Node::Label(labels[k]),
            Line(line, k) => Node::Line {
                line,
                start: labels[k],
            },
            Op(op) => Node::Insn(Insn::Op(op)),
            Int(op, operand) => Node::Insn(Insn::Int { op, operand }),
            Ldc(value) => Node::Insn(Insn::Ldc(Constant::Int(value))),
            Var(op, slot) => Node::Insn(Insn::Var { op, slot }),
            Iinc(slot, delta) => Node::Insn(Insn::Iinc { slot, delta }),
            Jump(op, k) => Node::Insn(Insn::Jump {
                op,
                target: labels[k],
            }),
        })
        .collect();
    (method, labels)
}

/// Run the pass over `nodes` and compare with `expected`; `changed` is what the pass reports.
fn folds(desc: &str, max_locals: u16, nodes: &[N], expected: &[N], changed: bool) {
    let (mut method, _) = body(desc, max_locals, nodes);
    let (wanted, _) = body(desc, max_locals, expected);
    assert_eq!(eliminate(&mut method, "T"), Ok(changed));
    assert_eq!(method.nodes, wanted.nodes);
}

/// `inline fun pick(b: Boolean) = if (b) 1 else 2` inlined with `b` bound to `value`.
fn pick(value: u8) -> Vec<N> {
    vec![
        Op(value),
        Var(ISTORE, 0),
        Var(ILOAD, 0),
        Jump(IFEQ, 0),
        Op(ICONST_1),
        Jump(GOTO, 1),
        At(0),
        Op(ICONST_2),
        At(1),
        Op(IRETURN),
    ]
}

#[test]
fn a_true_argument_keeps_the_then_branch_and_pops_the_condition() {
    folds(
        "()I",
        1,
        &pick(ICONST_1),
        &[
            Op(ICONST_1),
            Var(ISTORE, 0),
            Var(ILOAD, 0),
            Op(POP),
            Op(ICONST_1),
            Jump(GOTO, 1),
            At(0),
            At(1),
            Op(IRETURN),
        ],
        true,
    );
}

#[test]
fn a_false_argument_jumps_to_the_else_branch() {
    folds(
        "()I",
        1,
        &pick(ICONST_0),
        &[
            Op(ICONST_0),
            Var(ISTORE, 0),
            Var(ILOAD, 0),
            Op(POP),
            Jump(GOTO, 0),
            At(0),
            Op(ICONST_2),
            At(1),
            Op(IRETURN),
        ],
        true,
    );
}

#[test]
fn a_comparison_of_two_constants_pops_both() {
    // `val a = 3; val b = 40000; return if (a < b) a else b`.
    let source = [
        Op(ICONST_3),
        Var(ISTORE, 0),
        Ldc(40000),
        Var(ISTORE, 1),
        Var(ILOAD, 0),
        Var(ILOAD, 1),
        Jump(IF_ICMPGE, 0),
        Var(ILOAD, 0),
        Jump(GOTO, 1),
        At(0),
        Var(ILOAD, 1),
        At(1),
        Op(IRETURN),
    ];
    folds(
        "()I",
        2,
        &source,
        &[
            Op(ICONST_3),
            Var(ISTORE, 0),
            Ldc(40000),
            Var(ISTORE, 1),
            Var(ILOAD, 0),
            Var(ILOAD, 1),
            Op(POP),
            Op(POP),
            Var(ILOAD, 0),
            Jump(GOTO, 1),
            At(0),
            At(1),
            Op(IRETURN),
        ],
        true,
    );
}

#[test]
fn every_condition_is_decided_as_the_jvm_would() {
    for (op, value, jumps) in [
        (IFEQ, 0, true),
        (IFNE, 0, false),
        (IFLT, -1, true),
        (IFGE, -1, false),
        (IFGT, 0, false),
        (IFLE, 0, true),
    ] {
        assert_eq!(holds_against_zero(op, value), jumps, "{op:#x} {value}");
    }
    for (op, first, second, jumps) in [
        (IF_ICMPEQ, 2, 2, true),
        (IF_ICMPNE, 2, 2, false),
        (IF_ICMPLT, -3, 2, true),
        (IF_ICMPGE, -3, 2, false),
        (IF_ICMPGT, 2, 2, false),
        (IF_ICMPLE, 2, 2, true),
    ] {
        assert_eq!(holds(op, first, second), jumps, "{op:#x} {first} {second}");
    }
}

#[test]
fn a_comparison_with_a_known_zero_becomes_a_jump_on_the_first_operand() {
    // `x > limit` with `limit` bound to `0`.
    folds(
        "(I)I",
        2,
        &[
            Op(ICONST_0),
            Var(ISTORE, 1),
            Var(ILOAD, 0),
            Var(ILOAD, 1),
            Jump(IF_ICMPLE, 0),
            Op(ICONST_1),
            Op(IRETURN),
            At(0),
            Op(ICONST_2),
            Op(IRETURN),
        ],
        &[
            Op(ICONST_0),
            Var(ISTORE, 1),
            Var(ILOAD, 0),
            Var(ILOAD, 1),
            Op(POP),
            Jump(IFLE, 0),
            Op(ICONST_1),
            Op(IRETURN),
            At(0),
            Op(ICONST_2),
            Op(IRETURN),
        ],
        true,
    );
}

#[test]
fn a_known_first_operand_against_an_unknown_one_stays() {
    // kotlinc looks for the zero on top of the stack only.
    let source = [
        Op(ICONST_0),
        Var(ILOAD, 0),
        Jump(IF_ICMPLE, 0),
        Op(ICONST_1),
        Op(IRETURN),
        At(0),
        Op(ICONST_2),
        Op(IRETURN),
    ];
    folds("(I)I", 1, &source, &source, false);
}

#[test]
fn equal_constants_meeting_stay_known_and_different_ones_do_not() {
    // `(if (p) k else j)` then a jump on the result, for `k`, `j` pushed by `bipush`/`sipush`.
    let merged = |then: N, otherwise: N| {
        vec![
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            then,
            Jump(GOTO, 1),
            At(0),
            otherwise,
            At(1),
            Jump(IFNE, 2),
            Op(ICONST_1),
            Op(IRETURN),
            At(2),
            Op(ICONST_2),
            Op(IRETURN),
        ]
    };
    folds(
        "(I)I",
        1,
        &merged(Int(BIPUSH, 7), Int(SIPUSH, 7)),
        &[
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            Int(BIPUSH, 7),
            Jump(GOTO, 1),
            At(0),
            Int(SIPUSH, 7),
            At(1),
            Op(POP),
            Jump(GOTO, 2),
            At(2),
            Op(ICONST_2),
            Op(IRETURN),
        ],
        true,
    );
    let different = merged(Int(BIPUSH, 7), Int(SIPUSH, 0));
    folds("(I)I", 1, &different, &different, false);
}

#[test]
fn an_incremented_local_is_no_longer_known() {
    let source = [
        Op(ICONST_0),
        Var(ISTORE, 0),
        Iinc(0, 1),
        Var(ILOAD, 0),
        Jump(IFEQ, 0),
        Op(ICONST_1),
        Op(IRETURN),
        At(0),
        Op(ICONST_2),
        Op(IRETURN),
    ];
    folds("()I", 1, &source, &source, false);
}

#[test]
fn unreachable_code_goes_with_its_line_but_not_its_label() {
    // A method with an `int` jump and constant loses its dead code even when no jump folds.
    folds(
        "(I)I",
        1,
        &[
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            Op(ICONST_1),
            Op(IRETURN),
            At(1),
            Line(9, 1),
            Op(NOP),
            At(0),
            Op(ICONST_2),
            Op(IRETURN),
        ],
        &[
            Var(ILOAD, 0),
            Jump(IFEQ, 0),
            Op(ICONST_1),
            Op(IRETURN),
            At(1),
            At(0),
            Op(ICONST_2),
            Op(IRETURN),
        ],
        true,
    );
}

#[test]
fn a_method_without_an_int_constant_is_left_as_it_is() {
    // kotlinc's `hasOptimizableConditions`: no constant, so not even the dead `nop` goes.
    let source = [
        Var(ILOAD, 0),
        Jump(IFEQ, 0),
        Var(ILOAD, 0),
        Op(IRETURN),
        Op(NOP),
        At(0),
        Var(ILOAD, 0),
        Op(IRETURN),
    ];
    folds("(I)I", 1, &source, &source, false);
}
