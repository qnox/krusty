//! kotlinc's names for the methods it lifts lambdas and local functions into.
//!
//! `LocalDeclarationsLowering` names a lifted callable after the declarations around it, joined by
//! `$`: the outermost declaration (`outer`, `_init_` for constructors and initializers, `_get_x_`
//! for an accessor), then one segment per enclosing local callable, then its own. A lambda's
//! segment is `lambda$N`; a local function's is its name, or `name$N` when that name is already
//! taken in the sequence. `N` counts the lambdas and name-clashing local functions of the sequence
//! in source order, including lambdas that are spliced at inline call sites and so never become a
//! method. A suspend lambda becomes a class of its own and takes no number.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrFile, IrLiftingSequence};

/// Number every lifting sequence of the file and record kotlinc's lifted name of each function
/// lowered from a lambda or local function. A callable inside one that is not lifted (the body of a
/// suspend lambda) gets none.
pub(crate) fn number(ir: &mut IrFile) {
    let mut segments = HashMap::<(&IrLiftingSequence, u32), String>::new();
    for (sequence, entries) in &ir.lifting_sequences {
        let mut next = 0u32;
        let mut used = HashSet::new();
        for (&position, entry) in entries {
            if !entry.lifted {
                continue;
            }
            let segment = match &entry.name {
                None => {
                    next += 1;
                    format!("lambda${}", next - 1)
                }
                Some(name) if !used.insert(name.clone()) => {
                    next += 1;
                    format!("{name}${}", next - 1)
                }
                Some(name) => name.to_string(),
            };
            segments.insert((sequence, position), segment);
        }
    }
    let names = ir
        .lifted_functions
        .iter()
        .filter_map(|(&function, (sequence, site))| {
            let mut name = container_segment(&site.container);
            for step in site.path.iter() {
                name.push('$');
                name.push_str(segments.get(&(sequence, step.position))?);
            }
            Some((function, name))
        })
        .collect();
    ir.lifted_names = names;
}

/// Rename every function [`number`] named to its lifted name, once the functions are placed. One
/// whose lifted name and descriptor another method of the same class already has keeps its name:
/// kotlinc's names are distinct within the class it places them in, and a callable placed
/// elsewhere must not collide.
pub(crate) fn realize(ir: &mut IrFile) {
    let mut renames = ir.lifted_names.clone();
    let owners = ir
        .classes
        .iter()
        .enumerate()
        .flat_map(|(class, declaration)| {
            declaration
                .methods
                .iter()
                .map(move |&method| (method, class))
        })
        .collect::<HashMap<_, _>>();
    let descriptors = (0..ir.functions.len())
        .map(|function| super::ir_emit::function_descriptor(ir, function as u32))
        .collect::<Vec<_>>();
    loop {
        let mut holders = HashMap::<(Option<usize>, &str, &str), Vec<u32>>::new();
        for (index, function) in ir.functions.iter().enumerate() {
            let id = index as u32;
            let name = renames
                .get(&id)
                .map_or(function.name.as_str(), String::as_str);
            holders
                .entry((owners.get(&id).copied(), name, &descriptors[index]))
                .or_default()
                .push(id);
        }
        let clashing = holders
            .into_values()
            .filter(|holders| holders.len() > 1)
            .flatten()
            .filter(|function| renames.contains_key(function))
            .collect::<Vec<_>>();
        if clashing.is_empty() {
            break;
        }
        for function in clashing {
            renames.remove(&function);
        }
    }
    for (function, name) in renames {
        ir.functions[function as usize].name = name;
    }
}

/// The outermost declaration's name as the first segment: kotlinc replaces the characters a
/// special name carries (`<init>`, `<get-x>`, `x$delegate`) with `_`.
fn container_segment(container: &str) -> String {
    container
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | '-' | '$') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::container_segment;

    #[test]
    fn special_container_names_take_underscores() {
        assert_eq!(container_segment("<init>"), "_init_");
        assert_eq!(container_segment("<get-top>"), "_get_top_");
        assert_eq!(container_segment("lz$delegate"), "lz_delegate");
        assert_eq!(container_segment("outer"), "outer");
    }
}
