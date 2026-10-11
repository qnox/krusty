//! Superclass references that only the whole file can classify.

use crate::ast::{ClassKind, File};

/// A class with NO primary constructor names its base class WITHOUT parentheses (`class A : B { …
/// constructor(): super(…) }`) — the base arguments come from each secondary `super(…)`, not a
/// `: Base(args)` supertype entry. The parser can't tell a parenless class supertype from an interface
/// syntactically, so it parks every parenless supertype in `supertypes`; here, with the whole file
/// visible, we move a supertype that names a concrete file class into `base_class` for such classes.
pub(super) fn fixup_parenless_base_classes(file: &mut File) {
    use crate::ast::{CtorDelegation, Decl};
    let base_candidates: std::collections::HashSet<String> = file
        .decl_arena
        .iter()
        .filter_map(|d| match d {
            Decl::Class(c) if c.kind == ClassKind::Class || c.is_annotation() => {
                Some(c.name.clone())
            }
            _ => None,
        })
        .collect();
    let mut detached = Vec::new();
    for d in file.decl_arena.iter_mut() {
        if let Decl::Class(c) = d {
            if c.primary_ctor_annotations.is_some() || c.base_class.is_some() {
                continue;
            }
            let super_delegates = c.secondary_ctors.iter().any(|sc| {
                matches!(
                    sc.delegation,
                    CtorDelegation::Super(_) | CtorDelegation::None
                )
            });
            if !super_delegates {
                continue;
            }
            if let Some(pos) = c
                .supertypes
                .iter()
                .position(|s| base_candidates.contains(&s.name))
            {
                let base = c.supertypes.remove(pos);
                let mut reference = base.clone();
                reference.flags = reference.flags.with_superclass(true);
                detached.push(reference);
                c.base_class_span = Some(base.span);
                c.base_class = Some(base.name);
                c.base_type_args = base.targs;
            }
        }
    }
    file.detached_type_refs.extend(detached);
}
