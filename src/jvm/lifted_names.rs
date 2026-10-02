//! kotlinc's names for the methods it lifts lambdas and local functions into.
//!
//! `LocalDeclarationsLowering` names a lifted callable after the declarations around it, joined by
//! `$`: the outermost declaration (`outer`, `_init_` for constructors and initializers, `_get_x_`
//! for an accessor), then one segment per enclosing local callable, then its own. A lambda's
//! segment is `lambda$N`; a local function's is its name, or `name$N` when that name is already
//! taken in the sequence. `N` counts the lambdas and name-clashing local functions of the sequence
//! in source order, including lambdas that are spliced at inline call sites and so never become a
//! method. A suspend lambda becomes a class of its own and takes no number.
//! A local delegated-property accessor takes that sequence's next number as `lambda-N`.
//!
//! Every lambda numbers what it contains on its own, local functions nested in it included, and
//! spells a lambda among them as the bare number: `one$lambda$0$0`, `one$lambda$0$lf$1`. A local
//! function does not: a lambda in `loc` takes the enclosing count, `one$loc$lambda$2`.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrFile, IrLiftingSequence};

/// Number every lifting sequence of the file and record kotlinc's lifted name of each function
/// lowered from a lambda or local function. A callable inside one that is not lifted (the body of a
/// suspend lambda) gets none.
pub(crate) fn number(ir: &mut IrFile) {
    let mut segments = HashMap::<(&IrLiftingSequence, u32), String>::new();
    for (sequence, entries) in &ir.lifting_sequences {
        let mut scopes = HashMap::<Option<u32>, (u32, HashSet<&str>)>::new();
        for (&position, entry) in entries {
            if !entry.lifted {
                continue;
            }
            let (next, used) = scopes.entry(entry.scope).or_default();
            let segment = match entry.kind {
                crate::lifting_provenance::LiftingCallableKind::Lambda => {
                    assert!(
                        entry.name.is_none(),
                        "an unnamed lifting role gained a spelling"
                    );
                    match entry.scope {
                        None => format!("lambda${}", take(next)),
                        Some(_) => take(next).to_string(),
                    }
                }
                crate::lifting_provenance::LiftingCallableKind::LocalDelegatedPropertyAccessor => {
                    assert!(
                        entry.name.is_none(),
                        "a delegated accessor gained a spelling"
                    );
                    format!("lambda-{}", take(next))
                }
                crate::lifting_provenance::LiftingCallableKind::LocalFunction => {
                    let name = entry
                        .name
                        .as_deref()
                        .expect("a source local function retains its spelling");
                    if !used.insert(name) {
                        format!("{name}${}", take(next))
                    } else {
                        name.to_string()
                    }
                }
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
pub(crate) fn realize(
    ir: &mut IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
) {
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
        .map(|function| super::ir_emit::function_descriptor(ir, override_results, function as u32))
        .collect::<Vec<_>>();
    loop {
        let mut holders = HashMap::<(Option<usize>, &str, &str), Vec<u32>>::new();
        for (index, function) in ir.functions.iter().enumerate() {
            let id = index as u32;
            // Foreign inline templates and typed helper prototypes have a declaration owner in
            // another file. They cannot collide with members emitted by this facade.
            if ir.inline_only_fns.contains(&id) {
                continue;
            }
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

/// The next number of a scope's counter.
fn take(next: &mut u32) -> u32 {
    *next += 1;
    *next - 1
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
