//! Which modules compiling some modules needs, and the order to compile them in: those modules and
//! every module their main fragments depend on for compilation, transitively, each after the
//! modules it depends on, the project's order deciding between modules that are ready together.

use std::collections::BTreeSet;

use crate::resolution::{ModuleDeclarations, Target};

/// The modules each module's main fragment compiles against directly, by index, in declaration
/// order.
pub(super) fn compile_dependencies(modules: &[ModuleDeclarations]) -> Vec<Vec<usize>> {
    modules
        .iter()
        .map(|module| {
            let mut dependencies = Vec::new();
            for declaration in &module.main {
                if let Target::Module(index) = declaration.target {
                    if declaration.compile && !dependencies.contains(&index) {
                        dependencies.push(index);
                    }
                }
            }
            dependencies
        })
        .collect()
}

/// The modules compiling the modules `targets` needs, each once, in the order to compile them,
/// given each module's direct compile `dependencies`; or, when they depend on each other in a
/// cycle, the modules of the first cycle found, starting and ending with the same module.
pub(super) fn compile_order(
    dependencies: &[Vec<usize>],
    targets: &[usize],
) -> Result<Vec<usize>, Vec<usize>> {
    let count = dependencies.len();
    let mut needed = vec![false; count];
    for &target in targets {
        find_cycle(dependencies, target, &mut needed)?;
    }
    // Each needed module waits for the needed modules it depends on.
    let mut waiting: Vec<usize> = (0..count)
        .map(|index| {
            if needed[index] {
                dependencies[index].len()
            } else {
                0
            }
        })
        .collect();
    let mut ready: BTreeSet<usize> = (0..count)
        .filter(|&index| needed[index] && waiting[index] == 0)
        .collect();
    let mut order = Vec::new();
    while let Some(next) = ready.pop_first() {
        order.push(next);
        for (index, depends) in dependencies.iter().enumerate() {
            if needed[index] && depends.contains(&next) {
                waiting[index] -= 1;
                if waiting[index] == 0 {
                    ready.insert(index);
                }
            }
        }
    }
    Ok(order)
}

/// Mark every module `start` needs in `needed`, failing with the first cycle among them.
fn find_cycle(
    dependencies: &[Vec<usize>],
    start: usize,
    needed: &mut [bool],
) -> Result<(), Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Open,
        Done,
    }
    let mut state = vec![State::New; dependencies.len()];
    // The path from `start` to the module being visited, each with its next dependency to visit.
    let mut path: Vec<(usize, usize)> = vec![(start, 0)];
    state[start] = State::Open;
    needed[start] = true;
    while let Some((module, next)) = path.last_mut() {
        let module = *module;
        let Some(&dependency) = dependencies[module].get(*next) else {
            state[module] = State::Done;
            path.pop();
            continue;
        };
        *next += 1;
        match state[dependency] {
            State::New => {
                state[dependency] = State::Open;
                needed[dependency] = true;
                path.push((dependency, 0));
            }
            State::Open => {
                let from = path
                    .iter()
                    .position(|(open, _)| *open == dependency)
                    .unwrap_or(0);
                let mut cycle: Vec<usize> = path[from..].iter().map(|(open, _)| *open).collect();
                cycle.push(dependency);
                return Err(cycle);
            }
            State::Done => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{compile_order as order, find_cycle};

    #[test]
    fn dependencies_come_first_and_the_project_order_breaks_ties() {
        // 0 needs 3 and 1; 1 needs 2; 3 needs nothing; 4 is not needed.
        let dependencies = [vec![3, 1], vec![2], vec![], vec![], vec![0]];
        assert_eq!(order(&dependencies, &[0]), Ok(vec![2, 1, 3, 0]));
        assert_eq!(order(&dependencies, &[4]), Ok(vec![2, 1, 3, 0, 4]));
        assert_eq!(order(&dependencies, &[3]), Ok(vec![3]));
    }

    #[test]
    fn several_targets_compile_the_union_of_what_they_need_once() {
        // 0 needs 2; 1 needs 2 and 3; 4 needs nothing and is not asked for.
        let dependencies = [vec![2], vec![2, 3], vec![], vec![], vec![]];
        assert_eq!(order(&dependencies, &[1, 0]), Ok(vec![2, 0, 3, 1]));
        assert_eq!(order(&dependencies, &[0, 0, 2]), Ok(vec![2, 0]));
    }

    #[test]
    fn a_cycle_is_named_from_where_it_closes() {
        let dependencies = [vec![1], vec![2], vec![1], vec![3]];
        let mut needed = vec![false; 4];
        assert_eq!(
            find_cycle(&dependencies, 0, &mut needed),
            Err(vec![1, 2, 1])
        );
        assert_eq!(order(&dependencies, &[3]), Err(vec![3, 3]));
        assert_eq!(order(&dependencies, &[3, 0]), Err(vec![3, 3]));
    }
}
