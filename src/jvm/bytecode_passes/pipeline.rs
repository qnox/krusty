//! kotlinc's optimizer pass sequence (`OptimizationMethodVisitor.performTransformations`) over a
//! [`MethodNode`]: one ordered list, [`ORDER`], that names every optimization kotlinc runs and runs
//! the ones krusty has.
//!
//! kotlinc 2.4.20 runs, after its mandatory normalization: `CapturedVars`, `RedundantNullCheck`,
//! `RedundantCheckCast`, `ConstantCondition`, `RedundantBoxing`, `TemporaryVariablesElimination`,
//! `StackPeephole`, `PopBackwardPropagation`, `DeadCode`, `RedundantGoto`, `RedundantNopsCleanup`,
//! `NegatedJumps`, `RedundantCheckcastsBeforeAastore`, then `DeadCode` once more, and closes the
//! slots left unused (`removeUnusedLocalVariables`, which kotlinc also runs after several passes;
//! closing the gaps once at the end numbers the slots the same). A pass krusty does not have yet
//! would be a named step that changes nothing, so the list reads as kotlinc's does. Each step runs
//! over what the one before it left.
//!
//! The class-file boundary (`classfile::method_rewrite`) builds the node, supplies the facts only it
//! has ([`PassContext`]), and lays the result out again.

use std::collections::BTreeSet;

use super::redundant_boxing::{self, ValueClasses};
use super::{
    captured_vars, checkcasts_before_aastore, constant_conditions, dead_code, local_slots,
    negated_jumps, pop_backward, redundant_checkcasts, redundant_gotos, redundant_nops,
    redundant_null_checks, stack_peephole, temporaries,
};
use crate::jvm::method_node::{LabelId, MethodNode};

/// One step of kotlinc's optimizer, named after its transformer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// `CapturedVarsOptimizationMethodTransformer` (see `captured_vars`).
    CapturedVars,
    /// `RedundantNullCheckMethodTransformer` (see `redundant_null_checks`).
    RedundantNullCheck,
    /// `RedundantCheckCastEliminationMethodTransformer` (see `redundant_checkcasts`).
    RedundantCheckCast,
    /// `ConstantConditionEliminationMethodTransformer` (see `constant_conditions`).
    ConstantCondition,
    /// `RedundantBoxingMethodTransformer` (see `redundant_boxing`).
    RedundantBoxing,
    /// `TemporaryVariablesEliminationTransformer` (see `temporaries`).
    TemporaryVariables,
    /// `StackPeepholeOptimizationsTransformer` (see `stack_peephole`).
    StackPeephole,
    /// `PopBackwardPropagationTransformer` (see `pop_backward`).
    PopBackwardPropagation,
    /// The `DeadCodeEliminationMethodTransformer` in the middle of the list (see `dead_code`).
    DeadCode,
    /// `RedundantGotoMethodTransformer` (see `redundant_gotos`).
    RedundantGoto,
    /// `RedundantNopsCleanupMethodTransformer` (see `redundant_nops`).
    RedundantNops,
    /// `NegatedJumpsMethodTransformer` (see `negated_jumps`).
    NegatedJumps,
    /// `RedundantCheckcastsBeforeAastoreMethodTransformer` (see `checkcasts_before_aastore`).
    RedundantCheckcastsBeforeAastore,
    /// The `DeadCodeEliminationMethodTransformer` every method ends with (see `dead_code`).
    FinalDeadCode,
    /// `removeUnusedLocalVariables`: the slots left unused close up (see `local_slots`).
    UnusedLocalSlots,
}

/// The steps in the order they run: kotlinc's.
pub(crate) const ORDER: &[Pass] = &[
    Pass::CapturedVars,
    Pass::RedundantNullCheck,
    Pass::RedundantCheckCast,
    Pass::ConstantCondition,
    Pass::RedundantBoxing,
    Pass::TemporaryVariables,
    Pass::StackPeephole,
    Pass::PopBackwardPropagation,
    Pass::DeadCode,
    Pass::RedundantGoto,
    Pass::RedundantNops,
    Pass::NegatedJumps,
    Pass::RedundantCheckcastsBeforeAastore,
    Pass::FinalDeadCode,
    Pass::UnusedLocalSlots,
];

/// What the passes need to know of the method beyond its body.
pub(crate) struct PassContext<'a> {
    /// The internal name of the class the method belongs to.
    pub owner: &'a str,
    /// The value classes whose boxes the boxing pass recognizes.
    pub value_classes: &'a dyn ValueClasses,
    /// The slots `this` and the parameters take, which slot compaction leaves where they are.
    pub parameter_slots: u16,
}

/// What the passes did to the method.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// No pass changed anything.
    Unchanged,
    /// A pass met a body outside what its analysis models; what the method holds is no longer a
    /// result to keep.
    Declined,
    /// Some pass changed the body.
    Changed {
        /// Per local variable, in the order the method had them, whether the final dead-code pass
        /// removed it because its range holds no instruction (`prepareForEmitting`).
        removed_locals: Vec<bool>,
    },
}

/// The state the steps hand on to the ones after them.
#[derive(Default)]
struct Run {
    /// The labels the temporaries pass pinned; the `goto` and jump passes leave jumps to them.
    pinned: BTreeSet<LabelId>,
    removed_locals: Vec<bool>,
    changed: bool,
}

impl Run {
    /// Run `pass`; `None` when its analysis does not model the body.
    fn step(
        &mut self,
        pass: Pass,
        method: &mut MethodNode,
        context: &PassContext<'_>,
    ) -> Option<()> {
        let changed = match pass {
            Pass::CapturedVars => captured_vars::eliminate(method, context.owner).ok()?,
            Pass::RedundantNullCheck => {
                redundant_null_checks::eliminate(method, context.owner, context.value_classes)
                    .ok()?
            }
            Pass::RedundantCheckCast => {
                redundant_checkcasts::eliminate(method, context.owner).ok()?
            }
            Pass::ConstantCondition => {
                constant_conditions::eliminate(method, context.owner).ok()?
            }
            Pass::RedundantBoxing => {
                redundant_boxing::eliminate(method, context.owner, context.value_classes).ok()?
            }
            Pass::TemporaryVariables => match temporaries::eliminate(method) {
                Some(done) => {
                    self.pinned = done.pinned;
                    true
                }
                None => false,
            },
            Pass::StackPeephole => stack_peephole::optimize(method),
            Pass::PopBackwardPropagation => pop_backward::propagate(method, context.owner).ok()?,
            Pass::DeadCode => dead_code::eliminate(method),
            Pass::RedundantGoto => redundant_gotos::remove(method, &self.pinned),
            Pass::RedundantNops => redundant_nops::remove(method),
            Pass::NegatedJumps => negated_jumps::negate(method, &self.pinned),
            Pass::RedundantCheckcastsBeforeAastore => checkcasts_before_aastore::remove(method),
            Pass::FinalDeadCode => match dead_code::eliminate_for_emitting(method) {
                Some(dead) => {
                    self.removed_locals = dead.removed_locals;
                    true
                }
                None => false,
            },
            Pass::UnusedLocalSlots => {
                let parameters: BTreeSet<u16> = (0..context.parameter_slots).collect();
                local_slots::compact(method, &parameters)
            }
        };
        self.changed |= changed;
        Some(())
    }
}

/// Run [`ORDER`] over `method`.
pub(crate) fn optimize(method: &mut MethodNode, context: &PassContext<'_>) -> Outcome {
    let mut run = Run {
        removed_locals: vec![false; method.local_variables.len()],
        ..Run::default()
    };
    for &pass in ORDER {
        if run.step(pass, method, context).is_none() {
            return Outcome::Declined;
        }
    }
    if !run.changed {
        return Outcome::Unchanged;
    }
    Outcome::Changed {
        removed_locals: run.removed_locals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors;
    use crate::jvm::method_node::{Insn, LocalVariable, Node};

    #[test]
    fn the_order_is_kotlincs() {
        // kotlinc 2.4.20's `OptimizationMethodVisitor.performTransformations`.
        assert_eq!(
            ORDER,
            [
                Pass::CapturedVars,
                Pass::RedundantNullCheck,
                Pass::RedundantCheckCast,
                Pass::ConstantCondition,
                Pass::RedundantBoxing,
                Pass::TemporaryVariables,
                Pass::StackPeephole,
                Pass::PopBackwardPropagation,
                Pass::DeadCode,
                Pass::RedundantGoto,
                Pass::RedundantNops,
                Pass::NegatedJumps,
                Pass::RedundantCheckcastsBeforeAastore,
                Pass::FinalDeadCode,
                Pass::UnusedLocalSlots,
            ]
        );
    }

    fn context(value_classes: &ValueClassDescriptors) -> PassContext<'_> {
        PassContext {
            owner: "T",
            value_classes,
            parameter_slots: 0,
        }
    }

    #[test]
    fn the_goto_step_leaves_its_nop_to_the_nops_step() {
        // `iconst_0; goto L; L: ireturn`: the `goto` becomes a `nop`, which the next step removes.
        let mut method = MethodNode::new(0x0009, "f", "()I");
        let after = method.new_label();
        method.nodes = vec![
            Node::Insn(Insn::Op(0x03)),
            Node::Insn(Insn::Jump {
                op: 0xa7,
                target: after,
            }),
            Node::Label(after),
            Node::Insn(Insn::Op(0xac)),
        ];
        let mut gotos_only = method.clone();
        assert!(redundant_gotos::remove(&mut gotos_only, &BTreeSet::new()));
        assert_eq!(
            gotos_only.instructions().cloned().collect::<Vec<_>>(),
            [Insn::Op(0x03), Insn::Op(0x00), Insn::Op(0xac)]
        );
        let value_classes = ValueClassDescriptors::default();
        let outcome = optimize(&mut method, &context(&value_classes));
        assert_eq!(
            outcome,
            Outcome::Changed {
                removed_locals: Vec::new()
            }
        );
        assert_eq!(
            method.instructions().cloned().collect::<Vec<_>>(),
            [Insn::Op(0x03), Insn::Op(0xac)]
        );
    }

    #[test]
    fn a_discarded_select_collapses_before_the_goto_step() {
        // `if (c) a else b` as a statement: the pop step leaves `nop`s for the loads and the `pop`,
        // and the `goto` over the other branch then leads only past `nop`s.
        let mut method = MethodNode::new(0x0009, "f", "(ZII)V");
        method.max_locals = 3;
        let (otherwise, merge) = (method.new_label(), method.new_label());
        let iload = |slot| Node::Insn(Insn::Var { op: 0x15, slot });
        method.nodes = vec![
            iload(0),
            Node::Insn(Insn::Jump {
                op: 0x99,
                target: otherwise,
            }),
            iload(1),
            Node::Insn(Insn::Jump {
                op: 0xa7,
                target: merge,
            }),
            Node::Label(otherwise),
            iload(2),
            Node::Label(merge),
            Node::Insn(Insn::Op(0x57)),
            Node::Insn(Insn::Op(0xb1)),
        ];
        let value_classes = ValueClassDescriptors::default();
        assert_eq!(
            optimize(&mut method, &context(&value_classes)),
            Outcome::Changed {
                removed_locals: Vec::new()
            }
        );
        assert_eq!(
            method.instructions().cloned().collect::<Vec<_>>(),
            [
                Insn::Var { op: 0x15, slot: 0 },
                Insn::Jump {
                    op: 0x99,
                    target: otherwise
                },
                Insn::Op(0xb1)
            ]
        );
    }

    #[test]
    fn a_cast_of_a_ref_element_goes_once_the_ref_is_a_local() {
        // `var s: Any = "a"` captured by an inlined lambda, then `s as String`: the cast reads the
        // `ObjectRef`'s `Object` element, and only once `CapturedVars` has made the element a local
        // holding the `String` does the cast pass see that the cast is redundant.
        const OBJECT_REF: &str = "kotlin/jvm/internal/Ref$ObjectRef";
        let element = |op| {
            Node::Insn(Insn::Field {
                op,
                owner: OBJECT_REF.into(),
                name: "element".into(),
                desc: "Ljava/lang/Object;".into(),
            })
        };
        let mut method = MethodNode::new(0x0009, "f", "()Ljava/lang/Object;");
        method.max_locals = 1;
        method.nodes = vec![
            Node::Insn(Insn::Type {
                op: 0xbb,
                class: OBJECT_REF.into(),
            }),
            Node::Insn(Insn::Op(0x59)),
            Node::Insn(Insn::Method {
                op: 0xb7,
                owner: OBJECT_REF.into(),
                name: "<init>".into(),
                desc: "()V".into(),
                interface: false,
            }),
            Node::Insn(Insn::Var { op: 0x3a, slot: 0 }),
            Node::Insn(Insn::Var { op: 0x19, slot: 0 }),
            Node::Insn(Insn::Ldc(crate::jvm::method_node::Constant::String(
                "a".into(),
            ))),
            element(0xb5),
            Node::Insn(Insn::Var { op: 0x19, slot: 0 }),
            element(0xb4),
            Node::Insn(Insn::Type {
                op: 0xc0,
                class: "java/lang/String".into(),
            }),
            Node::Insn(Insn::Op(0xb0)),
        ];
        let value_classes = ValueClassDescriptors::default();
        assert!(matches!(
            optimize(&mut method, &context(&value_classes)),
            Outcome::Changed { .. }
        ));
        assert!(
            !method
                .instructions()
                .any(|insn| matches!(insn, Insn::Type { op: 0xc0, .. })),
            "{:?}",
            method.nodes
        );
    }

    #[test]
    fn a_body_no_pass_changes_is_unchanged() {
        let mut method = MethodNode::new(0x0009, "f", "()I");
        method.nodes = vec![Node::Insn(Insn::Op(0x03)), Node::Insn(Insn::Op(0xac))];
        let value_classes = ValueClassDescriptors::default();
        assert_eq!(
            optimize(&mut method, &context(&value_classes)),
            Outcome::Unchanged
        );
    }

    #[test]
    fn a_named_local_with_an_empty_range_keeps_its_store_and_goes_at_the_end() {
        // `if (x) { val unused = 5 }; return 1`: the local's range opens after its store and closes
        // at the block's end, so it holds no instruction. kotlinc keeps the entry through its
        // passes, which makes the store a named local's and not a temporary's, and drops the entry
        // only when the method is written (`prepareForEmitting`).
        let mut method = MethodNode::new(0x0019, "t4", "(Z)I");
        method.max_locals = 2;
        let (start, opened, closed, end) = (
            method.new_label(),
            method.new_label(),
            method.new_label(),
            method.new_label(),
        );
        method.nodes = vec![
            Node::Label(start),
            Node::Insn(Insn::Var { op: 0x15, slot: 0 }),
            Node::Insn(Insn::Jump {
                op: 0x99,
                target: closed,
            }),
            Node::Insn(Insn::Op(0x08)),
            Node::Insn(Insn::Var { op: 0x36, slot: 1 }),
            Node::Label(opened),
            Node::Label(closed),
            Node::Insn(Insn::Op(0x04)),
            Node::Insn(Insn::Op(0xac)),
            Node::Label(end),
        ];
        let local = |name: &str, desc: &str, start, end, slot| LocalVariable {
            name: name.into(),
            desc: desc.into(),
            start,
            end,
            slot,
        };
        method.local_variables = vec![
            local("x", "Z", start, end, 0),
            local("unused", "I", opened, closed, 1),
        ];
        let value_classes = ValueClassDescriptors::default();
        assert_eq!(
            optimize(&mut method, &context(&value_classes)),
            Outcome::Changed {
                removed_locals: vec![false, true]
            }
        );
        assert_eq!(
            method.instructions().cloned().collect::<Vec<_>>(),
            [
                Insn::Var { op: 0x15, slot: 0 },
                Insn::Jump {
                    op: 0x99,
                    target: closed
                },
                Insn::Op(0x08),
                Insn::Var { op: 0x36, slot: 1 },
                Insn::Op(0x04),
                Insn::Op(0xac),
            ]
        );
        assert_eq!(method.local_variables, [local("x", "Z", start, end, 0)]);
    }
}
