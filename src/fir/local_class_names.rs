//! Stable backend-neutral naming provenance for source classifiers declared in executable code.

use super::{DeclarationId, SourceFileId};
use crate::ast::{DeclId, File};

/// Exact lexical context needed by a target to invent a physical local-class name.
///
/// The frontend records source declaration segments and the shared generated-artifact ordinal; it
/// does not format a facade, separator, or synthetic class spelling. `lexical_owner` is a stable
/// source classifier declaration, or `None` when the source file owns the executable context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalClassNameProvenance {
    pub source: SourceFileId,
    pub lexical_owner: Option<DeclarationId>,
    pub segments: Box<[String]>,
    pub ordinal: Option<u32>,
}

pub(super) fn stabilize_local_class_names(
    file: &File,
    source: SourceFileId,
    source_declarations: &[Option<DeclarationId>],
    nested_owners: &std::collections::HashMap<DeclId, DeclarationId>,
) -> Vec<(DeclarationId, LocalClassNameProvenance)> {
    let stable = |declaration: DeclId| {
        file.decls
            .iter()
            .position(|candidate| *candidate == declaration)
            .and_then(|position| source_declarations.get(position).copied().flatten())
    };
    let mut plans = file
        .local_class_name_provenance
        .iter()
        .filter_map(|(&declaration, provenance)| {
            Some((
                stable(declaration)?,
                LocalClassNameProvenance {
                    source,
                    lexical_owner: provenance.lexical_owner.and_then(stable),
                    segments: provenance.segments.clone().into_boxed_slice(),
                    ordinal: provenance.ordinal,
                },
            ))
        })
        .collect::<Vec<_>>();
    // Ordinary nested declarations already have a backend-neutral nested classifier identity.
    // Only descendants of a local/anonymous owner need to follow that owner's target realization.
    // Iterate to a fixed point because the parser map is intentionally unordered and nesting may be
    // more than one classifier deep.
    let mut pending = file
        .hoisted_classifier_source_names
        .iter()
        .filter_map(|(&declaration, source_name)| {
            Some((
                stable(declaration)?,
                nested_owners.get(&declaration).copied()?,
                source_name,
            ))
        })
        .collect::<Vec<_>>();
    loop {
        let local_owners = plans
            .iter()
            .map(|(declaration, _)| *declaration)
            .collect::<std::collections::HashSet<_>>();
        let existing = local_owners.clone();
        let mut additions = Vec::new();
        let before = pending.len();
        pending.retain(|(declaration, owner, source_name)| {
            if existing.contains(declaration) {
                return false;
            }
            if !local_owners.contains(owner) {
                return true;
            }
            additions.push((
                *declaration,
                LocalClassNameProvenance {
                    source,
                    lexical_owner: Some(*owner),
                    segments: vec![(*source_name).clone()].into_boxed_slice(),
                    ordinal: None,
                },
            ));
            false
        });
        plans.extend(additions);
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    plans.sort_unstable_by_key(|(declaration, _)| *declaration);
    plans
}
