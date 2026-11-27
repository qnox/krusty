//! kotlinc's temporary-variable elimination (`TemporaryVariablesEliminationTransformer`) over a
//! [`MethodNode`].
//!
//! kotlinc's JVM backend writes a temporary (an unnamed local with no `LocalVariableTable` entry) as
//! an ordinary `store`/`load` pair, then this pass rewrites the pairs whose value can stay on the
//! operand stack. A body that still carries suspend inline markers is left as it is, as kotlinc
//! leaves it, until its state machine is built. In order:
//!
//! - `xload; pop` (`pop2` for a two-word value) becomes a `nop`, then every `nop` that need not
//!   preserve a debug/protected boundary is removed ([`trivial_cleanup`]);
//! - a null check whose value the path after it loads again keeps that value on the stack
//!   ([`null_check_folds`]);
//! - the temporaries are found ([`temporary_values`]) and each one's value is kept on the stack where
//!   the rules allow ([`stack_rules`]).
//!
//! Patterns match raw adjacency, as kotlinc's do: a kept label between the instructions defeats
//! them (see [`adjacency`]). Safe-call-chain reshaping and unused-LVT cleanup are separate kotlinc
//! optimizations and are deliberately not approximated here: a body with a null check kotlinc could
//! fold but this pass cannot is left as it was, and a local whose range a rewrite empties goes with
//! the final dead-code step (`prepareForEmitting`). The one representation-boundary exception is a
//! post-coroutine safe-call receiver that krusty deliberately stored before FixStack ran: after the
//! transformer, its selector line may separate the null check from the reload and a terminal arm
//! may stand immediately before the null target. The caller identifies that transformed body, and
//! the pass folds only the exact generated store/load shape.

mod adjacency;
mod null_check_folds;
mod shapes;
mod stack_rules;
mod temporary_values;
#[cfg(test)]
mod tests;
mod trivial_cleanup;

use std::collections::BTreeSet;
use std::ops::Range;

use crate::jvm::bytecode_passes::coroutines::markers::is_suspend_inline_marker;
use crate::jvm::method_node::{LabelId, MethodNode};
use adjacency::Body;

/// What the pass left for the passes after it.
#[derive(Debug, PartialEq)]
pub(crate) struct Elimination {
    /// The labels now standing after a `pop` a null-check fold put in front of the instruction
    /// they stood at; the `goto` and jump passes leave jumps to them alone.
    pub pinned: BTreeSet<LabelId>,
}

/// Rewrite `method`'s temporaries; `None` when nothing applies or the body is outside what the
/// analysis models, leaving `method` as it was.
pub(crate) fn eliminate(
    method: &mut MethodNode,
    preserved_unreachable_labels: &BTreeSet<LabelId>,
    post_coroutine: bool,
) -> Option<Elimination> {
    run(
        method,
        preserved_unreachable_labels,
        post_coroutine,
        &(0..0),
    )
}

/// The pass over a body about to be inlined (`preprocessNodeBeforeInline`). The slots in `owned`
/// hold the caller's variables the body reaches in place, which the caller reads after the body,
/// so a store to one is never a temporary.
pub(crate) fn eliminate_before_inlining(method: &mut MethodNode, owned: &Range<u16>) {
    run(method, &BTreeSet::new(), false, owned);
}

fn run(
    method: &mut MethodNode,
    preserved_unreachable_labels: &BTreeSet<LabelId>,
    post_coroutine: bool,
    owned: &Range<u16>,
) -> Option<Elimination> {
    // A body with suspend inline markers waits for its state machine: kotlinc leaves it untouched.
    if method.nodes.iter().any(is_suspend_inline_marker) {
        return None;
    }
    let original = method.nodes.clone();
    let mut body = Body::take(method);
    let rewritten = rewrite(
        &mut body,
        method,
        preserved_unreachable_labels,
        post_coroutine,
        owned,
    );
    method.nodes = match rewritten {
        Some(_) => body.list.to_nodes(),
        None => original,
    };
    rewritten
}

fn rewrite(
    body: &mut Body,
    method: &mut MethodNode,
    preserved_unreachable_labels: &BTreeSet<LabelId>,
    post_coroutine: bool,
    owned: &Range<u16>,
) -> Option<Elimination> {
    let mut changed = trivial_cleanup::remove_discarded_loads(body);
    changed |= trivial_cleanup::remove_nops(body);
    let folds = null_check_folds::fold(body, preserved_unreachable_labels, post_coroutine)?;
    changed |= folds.folded;
    let mut analyzed = method.clone();
    analyzed.nodes = body.list.to_nodes();
    let found = temporary_values::temporaries(&analyzed, owned)?;
    let instructions = body.instructions();
    let temporaries = found
        .into_iter()
        .map(|(store, loads)| {
            (
                instructions[store],
                loads.into_iter().map(|load| instructions[load]).collect(),
            )
        })
        .collect();
    changed |= stack_rules::apply(body, temporaries);
    if !changed {
        return None;
    }
    let pinned = null_check_folds::place_after_pops(body, method, &folds.null_targets);
    Some(Elimination { pinned })
}
