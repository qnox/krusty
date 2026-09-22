//! Detection and reporting of cyclic class hierarchies.
//!
//! A cycle can only be seen once every declaration in the module is known, so it is decided here
//! over the collected table rather than while a single classifier is being built. Breaking the
//! cycle keeps later traversal total; the diagnostic names the declaration that closed it.

use super::*;

/// Report a class whose supertype chain reaches itself, and CUT the chain.
///
/// A cyclic hierarchy is ill-formed source — kotlinc answers `object Foo : Foo()` with "cycle in
/// supertypes and/or containing declarations detected" — but krusty's walkers are recursive, and a
/// cycle sent one of them into unbounded recursion until the stack was exhausted. The process died
/// on a SIGBUS with no diagnostic at all, which is how it presented on 13 modules of
/// intellij-community: a same-named import that does not resolve leaves the supertype bound to the
/// declaration itself.
///
/// Individual walkers carry their own `seen` sets where they were known to need them; cutting the
/// link here means the walkers that never grew a guard cannot meet the cycle. Traversal uses the
/// same normalized direct-supertype model as ordinary inheritance: source declarations come from
/// the module table and dependency declarations come from the semantic platform boundary.
pub(in crate::resolve) fn break_supertype_cycles(
    table: &mut SymbolTable,
    diags: &mut DiagSink,
    mut diagnostic_span: impl FnMut(TypeName, &ClassSig, &HashMap<TypeName, usize>) -> Span,
) {
    let graph = supertype_graph(table);
    let (component_of, cyclic_components) = supertype_components(&graph);
    let mut cyclic: Vec<TypeName> = table
        .classes
        .iter()
        .filter(|(internal, sig)| {
            sig.source_decl.is_some()
                && component_of
                    .get(internal)
                    .is_some_and(|component| cyclic_components.contains(component))
        })
        .map(|(internal, _)| *internal)
        .collect();
    // `table.classes` is a `HashMap`, so its iteration order varies run to run. Every other
    // diagnostic path reports in source order, and so does kotlinc; without this a rebuild reorders
    // the errors for no reason.
    cyclic.sort_by_key(|internal| {
        table
            .classes
            .get(internal)
            .map(|sig| {
                (
                    sig.source_file,
                    diagnostic_span(*internal, sig, &component_of).lo,
                )
            })
            .unwrap_or_default()
    });
    for internal in cyclic {
        let component = component_of.get(&internal).copied();
        let same_component = |parent: TypeName| {
            component.is_some() && component_of.get(&parent).copied() == component
        };
        if let Some(sig) = table.classes.get(&internal) {
            let file = sig.source_file;
            let span = diagnostic_span(internal, sig, &component_of);
            diags.set_file(file);
            diags.error(
                span,
                "cycle in supertypes and/or containing declarations detected.".to_string(),
            );
        }
        // Cut ONLY the edges that close a cycle. Clearing every interface as well made an innocent
        // `interface Marker` disappear from a cyclic class, and its members with it — krusty then
        // reported `'m' overrides nothing` on a method kotlinc accepts.
        let super_is_cyclic = table
            .classes
            .get(&internal)
            .and_then(|sig| sig.super_internal)
            .is_some_and(same_component);
        let cyclic_interfaces: std::collections::HashSet<TypeName> = table
            .classes
            .get(&internal)
            .map(|sig| {
                sig.interfaces
                    .iter_ids()
                    .filter(|parent| same_component(*parent))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(sig) = table.classes.get_mut(&internal) {
            if super_is_cyclic {
                sig.super_internal = None;
                sig.super_type_args.clear();
            }
            if !cyclic_interfaces.is_empty() {
                let kept: Vec<(TypeName, Vec<Ty>)> = sig
                    .interfaces
                    .iter_ids()
                    .enumerate()
                    .filter(|(_, parent)| !cyclic_interfaces.contains(parent))
                    .map(|(ordinal, parent)| {
                        (
                            parent,
                            sig.interface_type_args
                                .get(ordinal)
                                .cloned()
                                .unwrap_or_default(),
                        )
                    })
                    .collect();
                let mut interfaces = crate::types::TypeNameList::new();
                let mut interface_type_args = Vec::with_capacity(kept.len());
                for (parent, arguments) in kept {
                    interfaces.push_name(parent);
                    interface_type_args.push(arguments);
                }
                sig.interfaces = interfaces;
                sig.interface_type_args = interface_type_args;
            }
        }
    }
}

pub(in crate::resolve) fn supertype_graph(table: &SymbolTable) -> HashMap<TypeName, Vec<TypeName>> {
    let mut graph = HashMap::new();
    let mut pending = table.classes.keys().copied().collect::<Vec<_>>();
    while let Some(current) = pending.pop() {
        if graph.contains_key(&current) {
            continue;
        }
        let parents = direct_supertypes(table, current);
        pending.extend(parents.iter().copied());
        graph.insert(current, parents);
    }
    graph
}

/// Iterative Kosaraju traversal. Hierarchies can be arbitrarily deep in malformed inputs, so the
/// validator itself must not put one Rust stack frame per declaration.
pub(in crate::resolve) fn supertype_components(
    graph: &HashMap<TypeName, Vec<TypeName>>,
) -> (HashMap<TypeName, usize>, std::collections::HashSet<usize>) {
    let mut visited = std::collections::HashSet::new();
    let mut order = Vec::with_capacity(graph.len());
    for root in graph.keys().copied() {
        if !visited.insert(root) {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        while let Some((current, next)) = stack.last_mut() {
            let parents = graph.get(current).map(Vec::as_slice).unwrap_or_default();
            if let Some(parent) = parents.get(*next).copied() {
                *next += 1;
                if visited.insert(parent) {
                    stack.push((parent, 0));
                }
            } else {
                order.push(*current);
                stack.pop();
            }
        }
    }

    let mut reverse: HashMap<TypeName, Vec<TypeName>> = graph
        .keys()
        .copied()
        .map(|name| (name, Vec::new()))
        .collect();
    for (child, parents) in graph {
        for parent in parents {
            reverse.entry(*parent).or_default().push(*child);
        }
    }

    let mut component_of = HashMap::new();
    let mut cyclic = std::collections::HashSet::new();
    let mut next_component = 0;
    for root in order.into_iter().rev() {
        if component_of.contains_key(&root) {
            continue;
        }
        let component = next_component;
        next_component += 1;
        let mut members = Vec::new();
        let mut stack = vec![root];
        component_of.insert(root, component);
        while let Some(current) = stack.pop() {
            members.push(current);
            for child in reverse.get(&current).into_iter().flatten() {
                if !component_of.contains_key(child) {
                    component_of.insert(*child, component);
                    stack.push(*child);
                }
            }
        }
        if members.len() > 1
            || graph
                .get(&root)
                .is_some_and(|parents| parents.contains(&root))
        {
            cyclic.insert(component);
        }
    }
    (component_of, cyclic)
}

/// Where to report a cyclic hierarchy.
///
/// kotlinc names the supertype itself (`open class B : C()` → the column of `C`). Both called base
/// classes and parenless supertypes retain the reference's source span. Select only an edge in the
/// declaration's cyclic component, so an innocent base or interface cannot receive the diagnostic.
pub(in crate::resolve) fn compact_cycle_span(
    internal: TypeName,
    sig: &ClassSig,
    headers: &crate::fir::StreamedHeaderModule,
    component_of: &HashMap<TypeName, usize>,
) -> Span {
    let component = component_of.get(&internal).copied();
    let same_component =
        |parent: TypeName| component.is_some() && component_of.get(&parent).copied() == component;
    let Some(declaration) = sig.stable_declaration else {
        return Span::new(0, 0);
    };
    let Some(header) = headers.syntax.declaration(declaration) else {
        return Span::new(0, 0);
    };
    let crate::fir::HeaderDeclarationKind::Classifier {
        base, supertypes, ..
    } = header.kind
    else {
        return Span::new(0, 0);
    };
    if sig.super_internal.is_some_and(same_component) {
        return base
            .and_then(|base| headers.syntax.ty(base))
            .map(|ty| ty.span)
            .unwrap_or_else(|| Span::new(0, 0));
    }
    sig.interfaces
        .iter_ids()
        .position(same_component)
        .and_then(|index| headers.syntax.type_operands(supertypes).get(index))
        .and_then(|supertype| headers.syntax.ty(*supertype))
        .map(|ty| ty.span)
        .unwrap_or_else(|| Span::new(0, 0))
}

pub(in crate::resolve) fn legacy_cycle_span(
    internal: TypeName,
    sig: &ClassSig,
    files: &[File],
    component_of: &HashMap<TypeName, usize>,
) -> Span {
    let component = component_of.get(&internal).copied();
    let same_component =
        |parent: TypeName| component.is_some() && component_of.get(&parent).copied() == component;
    sig.source_decl
        .and_then(|declaration| {
            let file = files.get(sig.source_file as usize)?;
            match file.decl(declaration) {
                Decl::Class(class) => {
                    if sig.super_internal.is_some_and(same_component) {
                        return class.base_class_span;
                    }
                    sig.interfaces
                        .iter_ids()
                        .position(same_component)
                        .and_then(|index| class.supertypes.get(index))
                        .map(|supertype| supertype.span)
                }
                _ => None,
            }
        })
        .unwrap_or_else(|| Span::new(0, 0))
}

fn direct_supertypes(table: &SymbolTable, current: TypeName) -> Vec<TypeName> {
    if let Some(signature) = table.classes.get(&current) {
        return signature
            .super_internal
            .into_iter()
            .chain(signature.interfaces.iter_ids())
            .collect();
    }
    table
        .libraries
        .classifier(current)
        .map(|classifier| classifier.supertypes.iter_ids().collect())
        .unwrap_or_default()
}
