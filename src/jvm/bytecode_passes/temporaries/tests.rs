use std::collections::BTreeMap;

use super::null_check_folds::{analysis_within_limit, ANALYSIS_COMPLEXITY_LIMIT};
use super::shapes::is_expression_null_check;
use super::*;
use crate::jvm::method_node::{Constant, Insn, LocalVariable, Node, TryCatchBlock};

const NOP: u8 = 0x00;
const ACONST_NULL: u8 = 0x01;
const ICONST_0: u8 = 0x03;
const ICONST_1: u8 = 0x04;
const LCONST_0: u8 = 0x09;
const POP: u8 = 0x57;
const DUP: u8 = 0x59;
const SWAP: u8 = 0x5f;
const IFEQ: u8 = 0x99;
const GOTO: u8 = 0xa7;
const IRETURN: u8 = 0xac;
const LRETURN: u8 = 0xad;
const ARETURN: u8 = 0xb0;
const RETURN: u8 = 0xb1;
const ATHROW: u8 = 0xbf;
const IFNULL: u8 = 0xc6;
const IFNONNULL: u8 = 0xc7;

/// An instruction of a test body; a jump names the instruction whose label it targets.
#[derive(Clone)]
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

fn var(op: u8, slot: u16) -> I {
    I::Insn(Insn::Var { op, slot })
}

fn aload(slot: u16) -> I {
    var(0x19, slot)
}

fn astore(slot: u16) -> I {
    var(0x3a, slot)
}

fn call(name: &str) -> I {
    I::Insn(Insn::Method {
        op: 0xb6,
        owner: "fixture/Owner".to_string(),
        name: name.to_string(),
        desc: "()Ljava/lang/Object;".to_string(),
        interface: false,
    })
}

/// A static field of type `desc`.
fn field(desc: &str) -> I {
    I::Insn(Insn::Field {
        op: 0xb2,
        owner: "fixture/Owner".to_string(),
        name: "f".to_string(),
        desc: desc.to_string(),
    })
}

fn string() -> I {
    I::Insn(Insn::Ldc(Constant::String("value".into())))
}

fn intrinsic(name: &str, desc: &str) -> I {
    I::Insn(Insn::Method {
        op: 0xb8,
        owner: "kotlin/jvm/internal/Intrinsics".to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: false,
    })
}

fn expression_check() -> I {
    intrinsic(
        "checkNotNullExpressionValue",
        "(Ljava/lang/Object;Ljava/lang/String;)V",
    )
}

fn cast() -> I {
    I::Insn(Insn::Type {
        op: 0xc0,
        class: "java/lang/String".to_string(),
    })
}

/// A body of instructions with a label in front of each and one after the last, and the lines,
/// ranges and removals a test adds to it.
struct Code {
    method: MethodNode,
    labels: Vec<LabelId>,
}

impl Code {
    fn new(insns: &[I]) -> Code {
        let mut method = MethodNode::new(0x0009, "f", "(Ljava/lang/Object;I)Ljava/lang/Object;");
        method.max_locals = 4;
        let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
        for (k, insn) in insns.iter().enumerate() {
            method.nodes.push(Node::Label(labels[k]));
            method.nodes.push(Node::Insn(labelled_insn(insn, &labels)));
        }
        method.nodes.push(Node::Label(labels[insns.len()]));
        Code { method, labels }
    }

    fn position(&self, label: LabelId) -> usize {
        self.method
            .nodes
            .iter()
            .position(|node| *node == Node::Label(label))
            .expect("placed")
    }

    /// A line number starting at instruction `k`.
    fn line(mut self, k: usize) -> Code {
        let start = self.labels[k];
        let at = self.position(start);
        self.method.nodes.insert(
            at + 1,
            Node::Line {
                line: k as u16 + 1,
                start,
            },
        );
        self
    }

    /// A named local in `slot` over instructions `[start, end)`.
    fn local(mut self, start: usize, end: usize, slot: u16) -> Code {
        self.method.local_variables.push(LocalVariable {
            name: format!("v{slot}"),
            desc: "Ljava/lang/Object;".to_string(),
            start: self.labels[start],
            end: self.labels[end],
            slot,
        });
        self
    }

    /// Instructions `[start, end)` protected by the handler at instruction `handler`.
    fn handler(mut self, start: usize, end: usize, handler: usize) -> Code {
        self.method.try_catch_blocks.push(TryCatchBlock {
            start: self.labels[start],
            end: self.labels[end],
            handler: self.labels[handler],
            catch_type: None,
        });
        self
    }

    /// The body with the instructions an earlier pass removed gone, their labels left standing.
    fn without(mut self, removed: &[usize]) -> Code {
        let mut index = 0;
        self.method.nodes.retain(|node| {
            if !matches!(node, Node::Insn(_)) {
                return true;
            }
            index += 1;
            !removed.contains(&(index - 1))
        });
        self
    }

    fn insns(&self, insns: &[I]) -> Vec<Insn> {
        insns
            .iter()
            .map(|insn| labelled_insn(insn, &self.labels))
            .collect()
    }

    /// The rewritten instructions, or `None` when the pass declined and left the body alone.
    fn run(mut self) -> Option<Vec<Insn>> {
        let before = self.method.clone();
        let outcome = eliminate(&mut self.method);
        if outcome.is_none() {
            assert_eq!(
                self.method, before,
                "a declined pass leaves the method as it was"
            );
        }
        outcome.map(|_| self.method.instructions().cloned().collect())
    }

    /// The instructions left, whether or not the pass itself changed anything.
    fn run_after_earlier_passes(mut self) -> Vec<Insn> {
        eliminate(&mut self.method);
        self.method.instructions().cloned().collect()
    }
}

fn labelled_insn(insn: &I, labels: &[LabelId]) -> Insn {
    match insn {
        I::Insn(insn) => insn.clone(),
        I::Jump(op, to) => Insn::Jump {
            op: *op,
            target: labels[*to],
        },
    }
}

#[test]
fn a_cast_removal_survives_when_the_temporary_pass_declines() {
    let insns = [
        op(ACONST_NULL),
        cast(),
        op(POP),
        aload(0),
        jump(IFNULL, 6),
        op(RETURN),
        op(RETURN),
    ];
    let code = Code::new(&insns).without(&[1]);
    let expected = code.insns(&[
        op(ACONST_NULL),
        op(POP),
        aload(0),
        jump(IFNULL, 6),
        op(RETURN),
        op(RETURN),
    ]);
    assert_eq!(code.run(), None);
    let code = Code::new(&insns).without(&[1]);
    assert_eq!(code.run_after_earlier_passes(), expected);
}

/// `x?.call()` behind a cast an earlier pass removed. `arrival` adds an unreachable jump to the
/// cast's label, `line` a line number there: either keeps the boundary the cast stood at.
fn safe_call_with_cast_boundary(arrival: bool, line: bool) {
    let mut insns = vec![
        aload(0),
        astore(1),
        aload(1),
        cast(),
        jump(IFNULL, 8),
        aload(1),
        call("call"),
        jump(GOTO, 8),
        op(RETURN),
    ];
    if arrival {
        insns.push(jump(GOTO, 3));
    }
    let mut code = Code::new(&insns);
    if line {
        code = code.line(3);
    }
    let code = code.without(&[3]);
    let mut expected = vec![
        aload(0),
        astore(1),
        aload(1),
        jump(IFNULL, 8),
        aload(1),
        call("call"),
        jump(GOTO, 8),
        op(RETURN),
    ];
    if arrival {
        expected.push(jump(GOTO, 3));
    }
    let expected = code.insns(&expected);
    assert_eq!(code.run_after_earlier_passes(), expected);
}

#[test]
fn a_removed_cast_keeps_a_debug_boundary_for_the_next_pass() {
    safe_call_with_cast_boundary(false, true);
}

#[test]
fn a_removed_cast_keeps_a_branch_boundary_for_the_next_pass() {
    safe_call_with_cast_boundary(true, false);
}

#[test]
fn a_removed_cast_keeps_a_handler_boundary_for_the_next_pass() {
    let insns = [op(ACONST_NULL), cast(), op(NOP), op(POP), op(RETURN)];
    let code = Code::new(&insns).handler(1, 4, 4).without(&[1]);
    let expected = code.insns(&[op(ACONST_NULL), op(NOP), op(POP), op(RETURN)]);
    assert_eq!(code.run_after_earlier_passes(), expected);
}

#[test]
fn a_removed_null_check_keeps_its_debug_boundary_for_the_next_pass() {
    let insns = [
        aload(0),
        astore(1),
        aload(1),
        op(DUP),
        expression_check(),
        jump(IFNULL, 9),
        aload(1),
        call("call"),
        jump(GOTO, 9),
        op(RETURN),
    ];
    let code = Code::new(&insns).line(3).without(&[3, 4]);
    let expected = code.insns(&[
        aload(0),
        astore(1),
        aload(1),
        jump(IFNULL, 9),
        aload(1),
        call("call"),
        jump(GOTO, 9),
        op(RETURN),
    ]);
    assert_eq!(code.run_after_earlier_passes(), expected);
}

#[test]
fn a_load_removed_with_a_null_check_does_not_keep_its_temporary_alive() {
    let insns = [
        aload(0),
        astore(1),
        aload(1),
        intrinsic("checkNotNull", "(Ljava/lang/Object;)V"),
        op(RETURN),
    ];
    let code = Code::new(&insns).without(&[2, 3]);
    let expected = code.insns(&[aload(0), op(POP), op(RETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_store_followed_by_its_only_load_leaves_the_value_on_the_stack() {
    let code = Code::new(&[aload(0), astore(1), aload(1), op(ARETURN)]);
    let expected = code.insns(&[aload(0), op(ARETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_null_check_kotlinc_cannot_match_does_not_hold_back_the_temporaries() {
    // `aload; ifnonnull L` whose target starts with an `iconst` is no safe call to kotlinc's
    // matcher, so the method's other rules still apply: the temporary folds.
    let code = Code::new(&[
        aload(0),
        astore(1),
        aload(1),
        jump(IFNONNULL, 6),
        op(ICONST_0),
        op(IRETURN),
        op(ICONST_1),
        op(IRETURN),
    ]);
    let expected = code.insns(&[
        aload(0),
        jump(IFNONNULL, 6),
        op(ICONST_0),
        op(IRETURN),
        op(ICONST_1),
        op(IRETURN),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_protected_start_past_a_removed_instruction_is_not_the_null_targets_own() {
    // The `ifnonnull` target is an `aload; pop` the cleanup removes, so its label ends up in front
    // of the protected range's start. In kotlinc the two are still distinct labels: the target
    // reloads nothing, kotlinc's matcher leaves the check alone, and the temporary folds.
    let code = Code::new(&[
        aload(0),
        astore(1),
        aload(1),
        jump(IFNONNULL, 6),
        op(ICONST_0),
        op(IRETURN),
        aload(0),
        op(POP),
        op(ICONST_1),
        op(IRETURN),
        astore(2),
        aload(2),
        op(ATHROW),
    ])
    .handler(8, 10, 10);
    let expected = code.insns(&[
        aload(0),
        jump(IFNONNULL, 6),
        op(ICONST_0),
        op(IRETURN),
        op(ICONST_1),
        op(IRETURN),
        astore(2),
        aload(2),
        op(ATHROW),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_line_mark_between_store_and_load_does_not_intervene() {
    let code = Code::new(&[aload(0), astore(1), aload(1), op(ARETURN)]).line(2);
    let expected = code.insns(&[aload(0), op(ARETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_named_local_initializer_is_found_past_non_intervening_instructions() {
    let code = Code::new(&[aload(0), astore(1), op(ICONST_0), op(POP), op(RETURN)])
        .line(3)
        .local(3, 5, 1);
    assert_eq!(code.run(), None);
}

#[test]
fn a_branch_target_between_store_and_load_intervenes() {
    // The unreachable `goto` makes the load's label a branch target.
    let code = Code::new(&[aload(0), astore(1), aload(1), op(ARETURN), jump(GOTO, 2)]);
    assert_eq!(code.run(), None);
}

#[test]
fn a_value_loaded_after_another_load_is_swapped_into_place() {
    // astore_1; aload_0; aload_1; invokevirtual → aload_0; swap; invokevirtual
    let code = Code::new(&[
        aload(0),
        astore(1),
        aload(0),
        aload(1),
        call("call"),
        op(ARETURN),
    ]);
    let expected = code.insns(&[aload(0), aload(0), op(SWAP), call("call"), op(ARETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_one_word_static_before_the_load_is_swapped_into_place() {
    let code = Code::new(&[
        aload(0),
        astore(1),
        field("Ljava/lang/Object;"),
        aload(1),
        call("call"),
        op(RETURN),
    ]);
    let expected = code.insns(&[
        aload(0),
        field("Ljava/lang/Object;"),
        op(SWAP),
        call("call"),
        op(RETURN),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_two_word_static_before_the_load_is_not_swapped() {
    let code = Code::new(&[
        aload(0),
        astore(1),
        field("J"),
        aload(1),
        call("call"),
        op(RETURN),
    ]);
    assert_eq!(code.run(), None);
}

#[test]
fn a_line_mark_inside_the_swap_pattern_defeats_it() {
    let code = Code::new(&[
        aload(0),
        astore(1),
        field("Ljava/lang/Object;"),
        aload(1),
        call("call"),
        op(RETURN),
    ])
    .line(2);
    assert_eq!(code.run(), None);
}

#[test]
fn an_expression_null_check_keeps_its_value_on_the_stack() {
    let code = Code::new(&[
        aload(0),
        astore(1),
        aload(1),
        string(),
        expression_check(),
        aload(1),
        op(ARETURN),
    ]);
    let expected = code.insns(&[aload(0), op(DUP), string(), expression_check(), op(ARETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn expression_null_check_identity_includes_its_descriptor() {
    let check = |owner: &str, desc: &str| Insn::Method {
        op: 0xb8,
        owner: owner.to_string(),
        name: "checkNotNullExpressionValue".to_string(),
        desc: desc.to_string(),
        interface: false,
    };
    assert!(is_expression_null_check(&check(
        "kotlin/jvm/internal/Intrinsics",
        "(Ljava/lang/Object;Ljava/lang/String;)V",
    )));
    assert!(!is_expression_null_check(&check(
        "kotlin/jvm/internal/Intrinsics",
        "(Ljava/lang/Object;)V",
    )));
    assert!(!is_expression_null_check(&check(
        "fixture/Intrinsics",
        "(Ljava/lang/Object;Ljava/lang/String;)V",
    )));
}

#[test]
fn an_unread_temporary_is_popped() {
    let code = Code::new(&[aload(0), astore(1), op(RETURN)]);
    let expected = code.insns(&[aload(0), op(POP), op(RETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_load_discarded_at_once_is_removed() {
    let code = Code::new(&[aload(0), op(POP), op(RETURN)]);
    let expected = code.insns(&[op(RETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_load_discarded_at_a_protected_start_leaves_a_nop() {
    let code = Code::new(&[aload(0), op(POP), op(RETURN), astore(1), op(RETURN)]).handler(0, 2, 3);
    let expected = code.insns(&[op(NOP), op(RETURN), astore(1), op(RETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_load_discarded_at_a_debug_mark_moves_the_mark_to_what_follows() {
    let code = Code::new(&[aload(0), op(POP), op(RETURN)]).line(0);
    let expected = code.insns(&[op(RETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn an_unlabelled_nop_next_to_an_instruction_is_removed() {
    let code = Code::new(&[aload(0), op(NOP), op(ARETURN)]);
    let expected = code.insns(&[aload(0), op(ARETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_nop_separated_from_both_neighbors_by_labels_is_retained() {
    let code = Code::new(&[aload(0), op(NOP), op(ARETURN)]).line(1).line(2);
    assert_eq!(code.run(), None);
}

#[test]
fn a_safe_call_shape_is_left_for_the_complete_safe_call_transform() {
    let code = Code::new(&[aload(0), jump(IFNONNULL, 3), op(ACONST_NULL), op(ARETURN)]);
    assert_eq!(code.run(), None);
}

#[test]
fn a_safe_call_exposed_by_nop_cleanup_is_rewritten_whole() {
    let code = Code::new(&[
        aload(0),
        op(NOP),
        jump(IFNONNULL, 5),
        op(ACONST_NULL),
        op(ATHROW), // the jump is the target's only predecessor
        aload(0),
        op(ARETURN),
    ]);
    let expected = code.insns(&[
        aload(0),
        op(DUP),
        jump(IFNONNULL, 5),
        op(POP),
        op(ACONST_NULL),
        op(ATHROW),
        op(ARETURN),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_wide_temporary_keeps_its_two_word_value_on_the_stack() {
    // lconst_0; lstore_1; lload_1; lreturn
    let code = Code::new(&[op(LCONST_0), var(0x37, 1), var(0x16, 1), op(LRETURN)]);
    let expected = code.insns(&[op(LCONST_0), op(LRETURN)]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_catch_store_is_not_a_temporary() {
    let code = Code::new(&[op(ACONST_NULL), astore(1), aload(1), op(ARETURN)]).handler(0, 1, 1);
    assert_eq!(code.run(), None);
}

#[test]
fn an_unrecognized_catch_entry_declines_the_rewrite() {
    let code = Code::new(&[aload(0), op(POP), op(RETURN)]).handler(0, 2, 2);
    assert_eq!(code.run(), None);
}

#[test]
fn analysis_complexity_overflow_and_oversize_are_refused() {
    assert!(analysis_within_limit(10, 4, 3));
    assert!(!analysis_within_limit(ANALYSIS_COMPLEXITY_LIMIT, 2, 1));
    assert!(!analysis_within_limit(usize::MAX, 2, 2));
}

#[test]
fn a_value_read_on_two_paths_from_two_stores_is_not_a_temporary() {
    // 0 iload_1; 1 ifeq 5; 2 aload_0; 3 astore_2; 4 goto 7; 5 aconst_null; 6 astore_2;
    // 7 aload_2; 8 areturn — slot 2 merges two stores at 7.
    let code = Code::new(&[
        var(0x15, 1),
        jump(IFEQ, 5),
        aload(0),
        astore(2),
        jump(GOTO, 7),
        op(ACONST_NULL),
        astore(2),
        aload(2),
        op(ARETURN),
    ]);
    assert_eq!(code.run(), None);
}

#[test]
fn a_value_checked_non_null_stays_on_the_stack_for_its_target() {
    // `s?.length`: 0 aload_0; 1 astore_1; 2 aload_1; 3 ifnonnull 6; 4 aconst_null; 5 goto 8;
    // 6 aload_1; 7 invokevirtual; 8 areturn.
    let code = Code::new(&[
        aload(0),
        astore(1),
        aload(1),
        jump(IFNONNULL, 6),
        op(ACONST_NULL),
        jump(GOTO, 8),
        aload(1),
        call("length"),
        op(ARETURN),
    ]);
    let expected = code.insns(&[
        aload(0),
        op(DUP),
        jump(IFNONNULL, 6),
        op(POP),
        op(ACONST_NULL),
        jump(GOTO, 8),
        call("length"),
        op(ARETURN),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_line_number_at_the_non_null_target_keeps_the_reload() {
    let insns = [
        aload(0),
        jump(IFNONNULL, 4),
        op(ACONST_NULL),
        op(ATHROW),
        aload(0),
        op(ARETURN),
    ];
    assert_eq!(Code::new(&insns).line(4).run(), None);
    assert!(Code::new(&insns).run().is_some());
}

#[test]
fn a_return_in_front_of_the_non_null_target_keeps_the_reload() {
    // `if (s == null) return 0; return s.length`: kotlinc's `ireturn` is followed by a dead
    // `nop` that falls into the target, so the jump is not its only predecessor.
    let code = Code::new(&[
        aload(0),
        jump(IFNONNULL, 4),
        op(ICONST_0),
        op(IRETURN),
        aload(0),
        call("length"),
        op(IRETURN),
    ]);
    assert_eq!(code.run(), None);
}

#[test]
fn a_non_null_target_reloading_under_a_static_swaps_it_into_place() {
    let code = Code::new(&[
        aload(0),
        jump(IFNONNULL, 4),
        op(ACONST_NULL),
        op(ATHROW),
        field("Ljava/lang/Object;"),
        aload(0),
        call("call"),
        op(RETURN),
    ]);
    let expected = code.insns(&[
        aload(0),
        op(DUP),
        jump(IFNONNULL, 4),
        op(POP),
        op(ACONST_NULL),
        op(ATHROW),
        field("Ljava/lang/Object;"),
        op(SWAP),
        call("call"),
        op(RETURN),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn null_checks_sharing_a_target_all_keep_their_values() {
    // `b?.next?.name` folded onto one null label: 0 aload_0; 1 ifnull 10; 2 aload_0;
    // 3 invokevirtual next; 4 astore_1; 5 aload_1; 6 ifnull 10; 7 aload_1; 8 invokevirtual name;
    // 9 goto 11; 10 aconst_null; 11 areturn.
    let code = Code::new(&[
        aload(0),
        jump(IFNULL, 10),
        aload(0),
        call("next"),
        astore(1),
        aload(1),
        jump(IFNULL, 10),
        aload(1),
        call("name"),
        jump(GOTO, 11),
        op(ACONST_NULL),
        op(ARETURN),
    ]);
    let expected = code.insns(&[
        aload(0),
        op(DUP),
        jump(IFNULL, 10),
        call("next"),
        op(DUP),
        jump(IFNULL, 10),
        call("name"),
        jump(GOTO, 11),
        op(POP),
        op(ACONST_NULL),
        op(ARETURN),
    ]);
    assert_eq!(code.run(), Some(expected));
}

#[test]
fn a_null_target_also_reached_by_falling_through_is_left_alone() {
    // `if (x != null) x.run()`: the call falls into the target the check jumps to.
    let code = Code::new(&[aload(0), jump(IFNULL, 4), aload(0), call("run"), op(RETURN)]);
    assert_eq!(code.run(), None);
}

#[test]
fn a_large_method_without_null_check_candidates_keeps_existing_cleanup() {
    let mut insns = vec![aload(0); 8_000];
    insns.extend([aload(0), op(POP), op(ARETURN)]);
    let rewritten = Code::new(&insns)
        .run()
        .expect("the load/pop cleanup still applies");
    assert_eq!(rewritten.len(), 8_001);
    assert_eq!(rewritten.last(), Some(&Insn::Op(ARETURN)));
}

/// `n?.touch()` as a statement: 0 aload_0; 1 astore_1; 2 aload_1; 3 ifnull L; 4 aload_1;
/// 5 invokevirtual; 6 goto E; 7 return, with `L` then `E` standing at 7 after the label the tables
/// name. The `goto` jumps to `E`, not to `L`, so `L`'s only predecessor is the null check. `line`
/// puts a line number at 7 and a local's range end there.
struct StatementSafeCall {
    method: MethodNode,
    /// The label the tables name at 7, `L` and `E`.
    labels: [LabelId; 3],
    outcome: Option<Elimination>,
}

fn statement_safe_call(line: bool) -> StatementSafeCall {
    let mut code = Code::new(&[
        aload(0),
        astore(1),
        aload(1),
        jump(IFNULL, 7),
        aload(1),
        call("touch"),
        jump(GOTO, 7),
        op(RETURN),
    ]);
    if line {
        code = code.line(7).local(0, 7, 0);
    }
    let (null_target, join) = (code.method.new_label(), code.method.new_label());
    let at = code.position(code.labels[7]);
    let before_return = at
        + 1
        + code.method.nodes[at + 1..]
            .iter()
            .position(|node| matches!(node, Node::Insn(_)))
            .expect("the return");
    code.method.nodes.splice(
        before_return..before_return,
        [Node::Label(null_target), Node::Label(join)],
    );
    for node in &mut code.method.nodes {
        if let Node::Insn(Insn::Jump { op, target }) = node {
            *target = if *op == IFNULL { null_target } else { join };
        }
    }
    let outcome = eliminate(&mut code.method);
    StatementSafeCall {
        method: code.method,
        labels: [code.labels[7], null_target, join],
        outcome,
    }
}

impl StatementSafeCall {
    /// The nodes from the `goto` on.
    fn tail(&self) -> Vec<Node> {
        let goto = self
            .method
            .nodes
            .iter()
            .position(|node| matches!(node, Node::Insn(Insn::Jump { op: GOTO, .. })))
            .expect("the goto");
        self.method.nodes[goto..].to_vec()
    }
}

#[test]
fn a_label_standing_after_the_null_target_lands_after_its_pop() {
    let folded = statement_safe_call(false);
    let [tables, null_target, join] = folded.labels;
    assert_eq!(
        folded.outcome,
        Some(Elimination {
            pinned: BTreeSet::from([join])
        })
    );
    assert_eq!(
        folded.method.instructions().cloned().collect::<Vec<_>>(),
        vec![
            Insn::Var { op: 0x19, slot: 0 },
            Insn::Op(DUP),
            Insn::Jump {
                op: IFNULL,
                target: null_target
            },
            Insn::Method {
                op: 0xb6,
                owner: "fixture/Owner".to_string(),
                name: "touch".to_string(),
                desc: "()Ljava/lang/Object;".to_string(),
                interface: false,
            },
            Insn::Jump {
                op: GOTO,
                target: join
            },
            Insn::Op(POP),
            Insn::Op(RETURN),
        ]
    );
    let end = folded.method.nodes.last().expect("the end label").clone();
    assert_eq!(
        folded.tail(),
        vec![
            Node::Insn(Insn::Jump {
                op: GOTO,
                target: join
            }),
            Node::Label(tables),
            Node::Label(null_target),
            Node::Insn(Insn::Op(POP)),
            Node::Label(join),
            Node::Insn(Insn::Op(RETURN)),
            end,
        ]
    );
}

#[test]
fn a_line_and_a_local_bound_at_the_null_target_move_after_its_pop() {
    let folded = statement_safe_call(true);
    let [tables, null_target, join] = folded.labels;
    let pinned = folded.outcome.as_ref().expect("folds").pinned.clone();
    let debug = *pinned
        .iter()
        .find(|&&label| label != join)
        .expect("a label for what moved after the pop");
    assert_eq!(pinned, BTreeSet::from([join, debug]));
    let end = folded.method.nodes.last().expect("the end label").clone();
    assert_eq!(
        folded.tail(),
        vec![
            Node::Insn(Insn::Jump {
                op: GOTO,
                target: join
            }),
            Node::Label(tables),
            Node::Label(null_target),
            Node::Insn(Insn::Op(POP)),
            Node::Label(join),
            Node::Label(debug),
            Node::Line {
                line: 8,
                start: debug
            },
            Node::Insn(Insn::Op(RETURN)),
            end,
        ]
    );
    assert_eq!(folded.method.local_variables[0].end, debug);
}

#[test]
fn an_instruction_is_stepped_again_only_when_its_incoming_state_changed() {
    let analyze = |code: &Code| {
        let mut steps = 0;
        let found = temporary_values::analyze(&code.method, &mut || steps += 1).expect("analyzed");
        (found, steps)
    };

    // call ; ifnull 0 ; return — the back edge arrives with the state the loop was entered with.
    let steady = Code::new(&[call("g"), jump(IFNULL, 0), op(RETURN)]);
    let (found, steps) = analyze(&steady);
    assert!(found.is_empty());
    assert_eq!(steps, 3, "an unchanged loop is walked once");

    // call ; astore_2 ; aload_2 ; ifnull 0 ; return — the back edge brings the store to the header,
    // so only the header and the store it reaches are stepped again.
    let widening = Code::new(&[call("g"), astore(2), aload(2), jump(IFNULL, 0), op(RETURN)]);
    let (found, steps) = analyze(&widening);
    assert_eq!(found, BTreeMap::from([(1, vec![2])]));
    assert_eq!(steps, 7);
}
