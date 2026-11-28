//! Secondary-constructor delegation cycles: a `this(...)` chain that returns to a constructor it
//! started from is reported at the first constructor of each cycle.

use super::*;

impl Checker<'_> {
    pub(super) fn validate_ctor_delegation_graph(
        &mut self,
        class: DeclId,
        declaration: &ClassDecl,
    ) {
        let edges = declaration
            .secondary_ctors
            .iter()
            .enumerate()
            .map(|(index, _)| {
                self.resolved_ctor_delegations
                    .get(&(class, index))
                    .and_then(|resolved| match &resolved.target {
                        ResolvedCtorDelegationTarget::ThisSecondary { index, .. } => Some(*index),
                        _ => None,
                    })
            })
            .collect::<Vec<_>>();
        for cycle in constructor_delegation_cycles(&edges) {
            if let Some(&index) = cycle.first() {
                self.diags.error(
                    declaration.secondary_ctors[index].span,
                    "constructor delegation cycle".to_string(),
                );
            }
            for index in cycle {
                self.resolved_ctor_delegations.remove(&(class, index));
            }
        }
    }
}

fn constructor_delegation_cycles(edges: &[Option<usize>]) -> Vec<Vec<usize>> {
    let mut state = vec![0u8; edges.len()];
    let mut cycles = Vec::new();
    for start in 0..edges.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(node) = current.filter(|&node| node < edges.len()) {
            if state[node] != 0 {
                if state[node] == 1 {
                    if let Some(offset) = path.iter().position(|&candidate| candidate == node) {
                        cycles.push(path[offset..].to_vec());
                    }
                }
                break;
            }
            state[node] = 1;
            path.push(node);
            current = edges[node];
        }
        for node in path {
            state[node] = 2;
        }
    }
    cycles
}
