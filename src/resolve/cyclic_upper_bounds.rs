//! kotlinc's `CYCLIC_GENERIC_UPPER_BOUND`: type parameters of one declaration whose upper bounds
//! name one another (`class A<B : B>`, `class C<X : Y, Y : X>`).
//!
//! The rule works on resolved bounds: a bound that resolved to another type parameter of the same
//! declaration is an edge between the two. A bound that merely mentions a parameter
//! (`Comparable<X>`) resolves to a classifier application and is not an edge. A parameter is on a
//! cycle when its edges lead back to it; the cycle is the parameters that reach one another, listed
//! in declaration order.

/// Resolved bound `bound` of the declaration, which `owner` declares and which resolved to the
/// declaration's own parameter `target`; both are declaration ordinals.
#[derive(Clone, Copy)]
pub(super) struct ParameterBound {
    pub(super) owner: usize,
    pub(super) target: usize,
}

/// What kotlinc reports for one declaration's bounds.
pub(super) struct CyclicBounds {
    /// For each bound, in the order given, the cycle it closes rendered as kotlinc names it.
    pub(super) closing: Vec<Option<String>>,
    /// For each type parameter, the cycle it sits on.
    pub(super) members: Vec<Option<String>>,
}

impl CyclicBounds {
    pub(super) fn is_empty(&self) -> bool {
        self.closing.iter().all(Option::is_none)
    }
}

/// The message kotlinc prints for a cycle of `names`.
pub(super) fn message(names: &str) -> String {
    format!("type parameter has cyclic upper bounds: {names}.")
}

/// Find the cycles among `parameters` formed by `bounds`, where `bounds[i]` is `None` for a bound
/// that is not another parameter of the declaration.
pub(super) fn find(parameters: &[String], bounds: &[Option<ParameterBound>]) -> CyclicBounds {
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
        assert!(found.is_empty());
        assert_eq!(found.members, [None, None]);
    }
}
