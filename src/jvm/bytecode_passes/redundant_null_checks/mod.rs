//! kotlinc's `RedundantNullCheckMethodTransformer` over a [`MethodNode`]: a null check or
//! `instanceof` whose operand is known to be `null`, or known not to be, is folded.
//!
//! One round (`TransformerPass.run`) writes into a copy of the method what each check of a local
//! teaches about it (`assumptions`), runs the nullability analysis over that copy (`nullability`),
//! and rewrites the checks of the method whose operand the analysis knows (`rewrite`). The rounds
//! repeat while a round folds a jump or an `instanceof`, since a folded branch can make another
//! operand known. A method with no null jump, `instanceof` or `checkNotNull*` call is left as it is.
//!
//! The pass runs on the method as emitted, where the redundant-cast pass also selects its casts, so
//! it reports where each node of its result came from.

mod assumptions;
mod nullability;
mod rewrite;

#[cfg(test)]
mod tests;

use super::analysis::{analyze, opcode, AnalyzerError};
use super::opcodes::*;
use super::redundant_boxing::ValueClasses;
use crate::jvm::method_node::{Insn, MethodNode, Node};
use assumptions::Listing;
use nullability::{NullValue, Nullability, NullabilityInterpreter};
use rewrite::Rewrite;

const INTRINSICS: &str = "kotlin/jvm/internal/Intrinsics";
const OBJECT_AND_MESSAGE: &str = "(Ljava/lang/Object;Ljava/lang/String;)V";

/// The `Intrinsics` null checks, as kotlinc recognizes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    /// `checkNotNull(Object)`.
    NotNull,
    /// `checkNotNull(Object, String)`.
    NotNullWithMessage,
    /// `checkNotNullParameter` / `checkParameterIsNotNull`.
    Parameter,
    /// `checkNotNullExpressionValue` / `checkExpressionValueIsNotNull`.
    ExpressionValue,
}

impl Check {
    fn of(insn: &Insn) -> Option<Check> {
        let Insn::Method {
            op: INVOKESTATIC,
            owner,
            name,
            desc,
            ..
        } = insn
        else {
            return None;
        };
        if owner != INTRINSICS {
            return None;
        }
        Some(match (name.as_str(), desc.as_str()) {
            ("checkNotNull", "(Ljava/lang/Object;)V") => Check::NotNull,
            ("checkNotNull", OBJECT_AND_MESSAGE) => Check::NotNullWithMessage,
            ("checkParameterIsNotNull" | "checkNotNullParameter", OBJECT_AND_MESSAGE) => {
                Check::Parameter
            }
            (
                "checkExpressionValueIsNotNull" | "checkNotNullExpressionValue",
                OBJECT_AND_MESSAGE,
            ) => Check::ExpressionValue,
            _ => return None,
        })
    }
}

/// `isThrowIntrinsic`: a call of an `Intrinsics` helper that never returns.
fn is_throw_intrinsic(insn: &Insn) -> bool {
    matches!(insn, Insn::Method { op: INVOKESTATIC, owner, name, .. }
    if owner == INTRINSICS
        && matches!(
            name.as_str(),
            "throwNpe"
                | "throwUninitializedProperty"
                | "throwUninitializedPropertyAccessException"
                | "throwAssert"
                | "throwIllegalArgument"
                | "throwIllegalState"
                | "throwParameterIsNullException"
                | "throwUndefinedForReified"
        ))
}

/// `isInstanceOfOrNullCheck`.
fn is_instance_of_or_null_check(insn: &Insn) -> bool {
    matches!(opcode(insn), INSTANCEOF | IFNULL | IFNONNULL)
}

/// `isOptimizable`: an instruction a round can rewrite.
fn is_optimizable(insn: &Insn) -> bool {
    is_instance_of_or_null_check(insn)
        || matches!(
            Check::of(insn),
            Some(Check::NotNull | Check::NotNullWithMessage | Check::ExpressionValue)
        )
}

/// Nullability frames retained across the method. This is a hard ceiling, not a heuristic: without
/// it a legal 65,535-byte method with 65,535 local slots could allocate billions of analysis cells.
const ANALYSIS_CELL_LIMIT: usize = 50 * 1024 * 1024;

fn analysis_within_limit(nodes: usize, locals: usize) -> bool {
    nodes
        .checked_add(1)
        .and_then(|points| points.checked_mul(locals.max(1)))
        .is_some_and(|cells| cells <= ANALYSIS_CELL_LIMIT)
}

/// The method the pass made: its nodes, and for each the position of the node it was in the method
/// the pass received (`None` for a `pop` the pass added).
#[derive(Debug, PartialEq)]
pub(crate) struct Rewritten {
    pub(crate) nodes: Vec<Node>,
    pub(crate) origins: Vec<Option<usize>>,
}

/// Run the pass over `method`, a member of `owner`; `None` when it changes nothing.
pub(crate) fn eliminate(
    method: &MethodNode,
    owner: &str,
    value_classes: &dyn ValueClasses,
) -> Result<Option<Rewritten>, AnalyzerError> {
    if !analysis_within_limit(method.nodes.len(), usize::from(method.max_locals)) {
        return Ok(None);
    }
    let mut working = method.clone();
    let mut origins: Vec<Option<usize>> = (0..method.nodes.len()).map(Some).collect();
    let mut edited = false;
    loop {
        let round = run_round(&mut working, &mut origins, owner, value_classes)?;
        edited |= round.edited;
        if !round.changes {
            break;
        }
    }
    Ok(edited.then_some(Rewritten {
        nodes: working.nodes,
        origins,
    }))
}

struct Round {
    changes: bool,
    edited: bool,
}

/// `TransformerPass.run`.
fn run_round(
    method: &mut MethodNode,
    origins: &mut Vec<Option<usize>>,
    owner: &str,
    value_classes: &dyn ValueClasses,
) -> Result<Round, AnalyzerError> {
    if !method.instructions().any(is_optimizable) {
        return Ok(Round {
            changes: false,
            edited: false,
        });
    }
    let listing = Listing::of(method);
    let known = analyze_nullabilities(&listing, method, owner, value_classes)?;
    let mut rewrite = Rewrite::new(method, origins, listing);
    rewrite.apply(&known);
    Ok(Round {
        changes: rewrite.changes,
        edited: rewrite.edited,
    })
}

/// `analyzeNullabilities`: each check whose operand the analysis knows is `null` or non-null, by
/// node position, in order.
fn analyze_nullabilities(
    listing: &Listing,
    method: &MethodNode,
    owner: &str,
    value_classes: &dyn ValueClasses,
) -> Result<Vec<(usize, NullValue)>, AnalyzerError> {
    let assumed = assumptions::inject(listing, method);
    let mut interpreter = NullabilityInterpreter::new(value_classes, assumed.placements);
    let frames = analyze(&assumed.method, owner, &mut interpreter)?;
    let mut known = Vec::new();
    for (at, node) in method.nodes.iter().enumerate() {
        let Node::Insn(insn) = node else {
            continue;
        };
        let Some(frame) = &frames[assumed.moved_to[at]] else {
            continue;
        };
        let depth = if is_instance_of_or_null_check(insn) {
            0
        } else {
            match Check::of(insn) {
                Some(Check::NotNull) => 0,
                Some(Check::NotNullWithMessage | Check::ExpressionValue) => 1,
                Some(Check::Parameter) | None => continue,
            }
        };
        let Some(value) = frame
            .stack
            .len()
            .checked_sub(depth + 1)
            .map(|index| &frame.stack[index])
        else {
            continue;
        };
        if value.nullability() != Nullability::Nullable {
            known.push((at, value.clone()));
        }
    }
    Ok(known)
}
