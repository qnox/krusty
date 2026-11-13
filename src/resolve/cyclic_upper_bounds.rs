//! kotlinc's `CYCLIC_GENERIC_UPPER_BOUND`: type parameters of one declaration whose upper bounds
//! name one another (`class A<B : B>`, `class C<X : Y, Y : X>`).
//!
//! The rule works on resolved bounds: a bound that resolved to another type parameter of the same
//! declaration is an edge between the two. A bound that merely mentions a parameter
//! (`Comparable<X>`) resolves to a classifier application and is not an edge. A parameter is on a
//! cycle when its edges lead back to it; the cycle is the parameters that reach one another, listed
//! in declaration order.

use crate::diag::Span;
use crate::types::{Ty, TyKind};

/// Resolved bound `bound` of the declaration, which `owner` declares and which resolved to the
/// declaration's own parameter `target`; both are declaration ordinals.
#[derive(Clone, Copy)]
struct ParameterBound {
    owner: usize,
    target: usize,
}

/// What kotlinc reports for one declaration's bounds.
struct CyclicBounds {
    /// For each bound, in the order given, the cycle it closes rendered as kotlinc names it.
    closing: Vec<Option<String>>,
    /// For each type parameter, the cycle it sits on.
    members: Vec<Option<String>>,
}

/// The message kotlinc prints for a cycle of `names`.
fn message(names: &str) -> String {
    format!("type parameter has cyclic upper bounds: {names}.")
}

/// Find the cycles among `parameters` formed by `bounds`, where `bounds[i]` is `None` for a bound
/// that is not another parameter of the declaration.
fn find(parameters: &[String], bounds: &[Option<ParameterBound>]) -> CyclicBounds {
    let count = parameters.len();
    let mut reach = vec![vec![false; count]; count];
    for edge in bounds.iter().flatten() {
        reach[edge.owner][edge.target] = true;
    }
    for via in 0..count {
        let through = reach[via].clone();
        for row in reach.iter_mut().filter(|row| row[via]) {
            for (slot, reaches) in row.iter_mut().zip(&through) {
                *slot |= *reaches;
            }
        }
    }
    let cycle_of = |member: usize| {
        reach[member][member].then(|| {
            (0..count)
                .filter(|other| reach[member][*other] && reach[*other][member])
                .map(|other| parameters[other].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
    };
    CyclicBounds {
        closing: bounds
            .iter()
            .map(|bound| {
                bound
                    .filter(|edge| reach[edge.target][edge.owner])
                    .and_then(|edge| cycle_of(edge.owner))
            })
            .collect(),
        members: (0..count).map(cycle_of).collect(),
    }
}

/// One written upper bound of a declaration, with the type it resolved to.
pub(super) struct WrittenBound<'a> {
    /// The source name of the type parameter that declares the bound.
    pub(super) owner: &'a str,
    pub(super) resolved: Ty,
    /// The bound as written.
    pub(super) span: Span,
    /// The declaring type parameter as its `<…>` list wrote it.
    pub(super) owner_span: Span,
}

/// What to report for a declaration, and which of its bounds are erroneous.
pub(super) struct Analysis {
    /// For each bound, in the order given, whether it closes a cycle.
    pub(super) cyclic: Vec<bool>,
    /// Where kotlinc reports, with its message: at each cyclic bound for a classifier, at each
    /// type parameter on a cycle for a callable.
    pub(super) reports: Vec<(Span, String)>,
}

/// Find the cycles among a declaration's `parameters` formed by its resolved `bounds`.
/// `parameter_of` maps the identity a bound resolved to onto the declaration's own parameter
/// ordinal; a bound that resolved to anything else is not an edge.
pub(super) fn analyze(
    parameters: &[String],
    bounds: &[WrittenBound<'_>],
    parameter_of: &dyn Fn(&str) -> Option<usize>,
    at_classifier_bound: bool,
) -> Analysis {
    let edges = bounds
        .iter()
        .map(|bound| {
            let (TyKind::TyParam(target, _) | TyKind::Var(target, _)) =
                bound.resolved.non_null_keeping_mutability().kind()
            else {
                return None;
            };
            Some(ParameterBound {
                owner: parameters.iter().position(|name| name == bound.owner)?,
                target: parameter_of(target.as_str())?,
            })
        })
        .collect::<Vec<_>>();
    let found = find(parameters, &edges);
    let mut reports = Vec::new();
    if at_classifier_bound {
        for (bound, names) in bounds.iter().zip(&found.closing) {
            if let Some(names) = names {
                reports.push((bound.span, message(names)));
            }
        }
    } else {
        for (parameter, names) in found.members.iter().enumerate() {
            let Some(names) = names else { continue };
            let declared = edges
                .iter()
                .position(|edge| edge.is_some_and(|edge| edge.owner == parameter));
            if let Some(declared) = declared {
                reports.push((bounds[declared].owner_span, message(names)));
            }
        }
    }
    Analysis {
        cyclic: found.closing.iter().map(Option::is_some).collect(),
        reports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(letters: &[&str]) -> Vec<String> {
        letters.iter().map(|letter| (*letter).to_owned()).collect()
    }

    fn edge(owner: usize, target: usize) -> Option<ParameterBound> {
        Some(ParameterBound { owner, target })
    }

    #[test]
    fn a_bound_naming_its_own_parameter_is_a_cycle_of_one() {
        let found = find(&names(&["B"]), &[edge(0, 0)]);
        assert_eq!(found.closing, [Some("B".to_owned())]);
        assert_eq!(found.members, [Some("B".to_owned())]);
    }

    #[test]
    fn a_cycle_lists_its_parameters_in_declaration_order_and_leaves_tails_out() {
        let found = find(
            &names(&["P", "Q", "R"]),
            &[edge(0, 1), edge(1, 0), edge(2, 0), None],
        );
        assert_eq!(
            found.closing,
            [Some("P, Q".to_owned()), Some("P, Q".to_owned()), None, None]
        );
        assert_eq!(
            found.members,
            [Some("P, Q".to_owned()), Some("P, Q".to_owned()), None]
        );
    }

    #[test]
    fn a_chain_of_bounds_is_not_a_cycle() {
        let found = find(&names(&["T", "U"]), &[edge(0, 1)]);
        assert!(found.closing.iter().all(Option::is_none));
        assert_eq!(found.members, [None, None]);
    }
}
