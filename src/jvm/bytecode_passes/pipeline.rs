//! kotlinc's optimizer pass sequence (`OptimizationMethodVisitor.performTransformations`) over a
//! [`MethodNode`]: one ordered list, [`ORDER`], that names every optimization kotlinc runs and runs
//! the ones krusty has.
//!
//! kotlinc 2.4.20 runs, after its mandatory normalization: `CapturedVars`, `RedundantNullCheck`,
//! `RedundantCheckCast`, `ConstantCondition`, `RedundantBoxing`, `TemporaryVariablesElimination`,
//! `StackPeephole`, `PopBackwardPropagation`, `DeadCode`, `RedundantGoto`, `RedundantNopsCleanup`,
//! `NegatedJumps`, `RedundantCheckcastsBeforeAastore`, then `DeadCode` once more, and closes the
//! slots left unused (`removeUnusedLocalVariables`). A pass krusty does not have yet is a named step
//! that changes nothing, so the list reads as kotlinc's does.
//!
//! One step is out of kotlinc's place: the redundant-null-check and redundant-cast passes select
//! over the method as emitted (the cast pass asks the verifier's frames of the emitted bytes, by
//! instruction number), so they run before `CapturedVars`, and what they select is removed together
//! before the next step.
//!
//! The class-file boundary (`classfile::method_rewrite`) builds the node, supplies the facts only it
//! has ([`PassContext`]), and lays the result out again.

use std::collections::BTreeSet;

use super::redundant_boxing::{self, ValueClasses};
use super::redundant_checkcasts::{self, StackTops};
use super::{
    captured_vars, checkcasts_before_aastore, dead_code, local_slots, negated_jumps,
    redundant_gotos, redundant_nops, redundant_null_checks, stack_peephole, temporaries,
};
use crate::jvm::method_node::{LabelId, MethodNode};

/// One step of kotlinc's optimizer, named after its transformer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// `RedundantNullCheckMethodTransformer`, its `checkNotNull*` calls only (see
    /// `redundant_null_checks`).
    RedundantNullCheck,
    /// `RedundantCheckCastEliminationMethodTransformer` (see `redundant_checkcasts`).
    RedundantCheckCast,
    /// `CapturedVarsOptimizationMethodTransformer` (see `captured_vars`).
    CapturedVars,
    /// `ConstantConditionEliminationMethodTransformer`: not ported yet.
    ConstantCondition,
    /// `RedundantBoxingMethodTransformer` (see `redundant_boxing`).
    RedundantBoxing,
    /// `TemporaryVariablesEliminationTransformer` (see `temporaries`).
    TemporaryVariables,
    /// `StackPeepholeOptimizationsTransformer` (see `stack_peephole`).
    StackPeephole,
    /// `PopBackwardPropagationTransformer`: not ported yet.
    PopBackwardPropagation,
    /// The `DeadCodeEliminationMethodTransformer` in the middle of the list: not run yet.
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

/// The steps in the order they run: kotlinc's, except that the two passes judging the method as
/// emitted come before `CapturedVars` (see the module documentation).
pub(crate) const ORDER: &[Pass] = &[
    Pass::RedundantNullCheck,
    Pass::RedundantCheckCast,
    Pass::CapturedVars,
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
    /// What the verifier holds on top of the stack before each instruction of the method as
    /// emitted, asked only when the method has a cast to judge.
    pub stack_tops: &'a dyn Fn() -> Option<&'a dyn StackTops>,
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
        /// removed it with the last instruction of its range.
        removed_locals: Vec<bool>,
    },
}

/// The state the steps hand on to the ones after them.
#[derive(Default)]
struct Run {
    /// Node positions the passes judging the method as emitted selected, not removed yet.
    emitted_selection: BTreeSet<usize>,
    /// The labels the temporaries pass pinned; the `goto` and jump passes leave jumps to them.
    pinned: BTreeSet<LabelId>,
    removed_locals: Vec<bool>,
    changed: bool,
}

impl Run {
    /// Remove what the passes judging the method as emitted selected, together.
    fn remove_emitted_selection(&mut self, method: &mut MethodNode) {
        if self.emitted_selection.is_empty() {
            return;
        }
        let selection = std::mem::take(&mut self.emitted_selection);
        let mut position = 0;
        method.nodes.retain(|_| {
            position += 1;
            !selection.contains(&(position - 1))
        });
        self.changed = true;
    }

    /// Run `pass`; `None` when its analysis does not model the body.
    fn step(
        &mut self,
        pass: Pass,
        method: &mut MethodNode,
        context: &PassContext<'_>,
    ) -> Option<()> {
        if !matches!(pass, Pass::RedundantNullCheck | Pass::RedundantCheckCast) {
            self.remove_emitted_selection(method);
        }
        let changed = match pass {
            Pass::RedundantNullCheck => {
                self.emitted_selection
                    .extend(redundant_null_checks::select(method));
                false
            }
            Pass::RedundantCheckCast => {
                self.emitted_selection
                    .extend(redundant_checkcasts::select(method, context.stack_tops));
                false
            }
            Pass::CapturedVars => captured_vars::eliminate(method, context.owner).ok()?,
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
            Pass::RedundantGoto => redundant_gotos::remove(method, &self.pinned),
            Pass::RedundantNops => redundant_nops::remove(method),
            Pass::NegatedJumps => negated_jumps::negate(method, &self.pinned),
            Pass::RedundantCheckcastsBeforeAastore => checkcasts_before_aastore::remove(method),
            Pass::FinalDeadCode => match dead_code::eliminate(method) {
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
            Pass::ConstantCondition | Pass::PopBackwardPropagation | Pass::DeadCode => false,
        };
        self.changed |= changed;
        Some(())
    }
}

/// Run [`ORDER`] over `method`.
pub(crate) fn optimize(method: &mut MethodNode, context: &PassContext<'_>) -> Outcome {
    let mut run = Run::default();
    for &pass in ORDER {
        if run.step(pass, method, context).is_none() {
            return Outcome::Declined;
        }
    }
    run.remove_emitted_selection(method);
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
    use crate::jvm::method_node::{Insn, Node};

    #[test]
    fn the_order_is_kotlincs_with_the_emitted_method_checks_first() {
        assert_eq!(
            ORDER,
            [
                Pass::RedundantNullCheck,
                Pass::RedundantCheckCast,
                Pass::CapturedVars,
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
        // kotlinc 2.4.20's `OptimizationMethodVisitor.performTransformations`, with `CapturedVars`
        // moved behind the two passes that judge the method as emitted.
        let mut kotlinc = ORDER.to_vec();
        kotlinc.retain(|&pass| pass != Pass::CapturedVars);
        kotlinc.insert(0, Pass::CapturedVars);
        assert_eq!(
            kotlinc,
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

    fn context<'a>(
        value_classes: &'a ValueClassDescriptors,
        stack_tops: &'a dyn Fn() -> Option<&'a dyn StackTops>,
    ) -> PassContext<'a> {
        PassContext {
            owner: "T",
            value_classes,
            parameter_slots: 0,
            stack_tops,
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
        let no_tops = || None;
        let outcome = optimize(&mut method, &context(&value_classes, &no_tops));
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
    fn a_body_no_pass_changes_is_unchanged() {
        let mut method = MethodNode::new(0x0009, "f", "()I");
        method.nodes = vec![Node::Insn(Insn::Op(0x03)), Node::Insn(Insn::Op(0xac))];
        let value_classes = ValueClassDescriptors::default();
        let no_tops = || None;
        assert_eq!(
            optimize(&mut method, &context(&value_classes, &no_tops)),
            Outcome::Unchanged
        );
    }
}
