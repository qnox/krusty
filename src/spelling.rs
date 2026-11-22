//! Source spelling of declared types. The model lives in `krusty-core`; the parts that read parser
//! types stay with the frontend.

pub use krusty_core::spelled::*;

/// Re-key an already-checked, source-shaped occurrence tree onto one active parser type.
/// The tree contains no names or semantic types; it is only the structural handoff for
/// annotation applications whose arguments were folded before temporary source coordinates
/// were released.
pub(crate) fn record_checked_source_facts(
    annotations: &mut RecordedTypeAnnotations,
    reference: &crate::ast::TypeRef,
    facts: &Spelled,
) -> bool {
    if facts
        .annotations
        .iter()
        .any(|annotation| matches!(annotation, TypeUseAnnotation::Bound(_)))
    {
        return false;
    }
    annotations.record(reference.span.lo, facts.annotations.clone());
    if facts.unsafe_variance {
        annotations.record_unsafe_variance(reference.span.lo);
    }
    if let Some(name) = facts.parameter_name.as_deref() {
        annotations.record_parameter_name(reference.span.lo, name);
    }
    let children = if !reference.fun_params.is_empty() || reference.name == "<fun>" {
        let mut children = reference.fun_params.iter().collect::<Vec<_>>();
        if !reference.fun_suspend() {
            let Some(result) = reference.arg.as_deref() else {
                return false;
            };
            children.push(result);
        }
        children
    } else {
        reference.targs.iter().collect()
    };
    children.len() == facts.args.len()
        && children
            .into_iter()
            .zip(&facts.args)
            .all(|(child, facts)| record_checked_source_facts(annotations, child, facts))
}

/// What one source file says about its type occurrences beyond their resolved types: the alias
/// spellings the parse seam parked when it expanded the file's own aliases away, and the
/// annotations `@Metadata` records on each occurrence.
#[derive(Clone, Copy)]
pub(crate) struct SourceSpellings<'a> {
    pub aliases: &'a crate::fx_hash::HashMap<crate::diag::Span, crate::ast::TypeRef>,
    pub annotations: &'a RecordedTypeAnnotations,
}
