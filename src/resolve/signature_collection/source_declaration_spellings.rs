//! The source spellings signature collection needs before any name is bound.
//!
//! A supertype, a bound, or a visibility suppression is written as text and must be inventoried
//! before the type universe is complete. These spellings are lookup input only; nothing downstream
//! recovers a symbol from them.

use super::*;

/// Project `@Suppress` visibility flags onto stable declarations after annotation names have been
/// resolved. Signature solving runs without source AST ownership, so it consumes this compact fact;
/// source spellings and annotation expression ids do not cross the boundary.
pub(in crate::resolve) fn collect_stable_visibility_suppressions(
    table: &mut SymbolTable,
    headers: Option<&crate::fir::StreamedHeaderModule>,
) {
    let Some(headers) = headers else {
        return;
    };
    for stub in &headers.stubs {
        let mut suppressions = VisibilitySuppressions::default();
        for application in headers
            .file_visibility_suppressions(stub.source)
            .iter()
            .chain(headers.declaration_visibility_suppressions(stub.id))
        {
            let annotation = AnnotationRef {
                name: String::new(),
                span: application.annotation,
            };
            if !table
                .resolved_annotation(stub.source.raw(), &annotation)
                .is_some_and(|identity| identity == type_name("kotlin/Suppress"))
            {
                continue;
            }
            suppressions.invisible_reference |= application.invisible_reference;
            suppressions.invisible_member |= application.invisible_member;
            suppressions.optional_declaration_usage |= application.optional_declaration_usage;
        }
        if suppressions.has_any() {
            table
                .declaration_visibility_suppressions
                .insert(stub.id, suppressions);
        }
    }
}

/// Rebuild the declared base class as a [`TypeRef`] so it can be spelled like any other declared
/// type. `ClassDecl` stores it split into a bare name plus its type arguments, and only a whole
/// reference can carry an alias with its as-written arguments.
pub(in crate::resolve) fn base_class_type_ref(
    base: &str,
    type_args: &[TypeRef],
    span: Span,
) -> TypeRef {
    TypeRef {
        name: base.to_string(),
        flags: TrFlags::default(),
        arg: None,
        targs: type_args.to_vec(),
        span,
        fun_params: Vec::new(),
        fun_context_count: 0,
    }
}

/// A name-only type-parameter scope: [`spelling_of_ref`] asks a `TParams` exactly one question —
/// does this spelling name a type parameter (and therefore never an alias) — so the bounds it
/// carries are irrelevant here.
pub(in crate::resolve) fn spelling_scope(type_params: &[String]) -> TParams {
    TParams::from_bindings(
        type_params
            .iter()
            .map(|name| (name.clone(), Ty::nullable(Ty::obj("kotlin/Any")))),
    )
}
