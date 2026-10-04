//! Source spellings of INFERRED declaration results.
//!
//! kotlinc's constructor call through a `typealias` (`val sb = StringBuilder()`, with
//! `kotlin.text.StringBuilder = java.lang.StringBuilder`) produces the expanded class type
//! abbreviated by the alias, and a declaration whose type is inferred from that call records the
//! abbreviation in `@Metadata` exactly as a declared `val sb: StringBuilder` would. Signature
//! solving is the only phase that sees both the alias the callee named and the declaration the
//! call's result became, so it records the pair here; the spelling never takes part in the
//! semantic result.

use super::*;
use crate::spelling::Spelled;

/// Alias spellings gathered while signature expressions are evaluated.
#[derive(Default)]
pub(super) struct InferredResultSpellings {
    /// The abbreviated result of each constructor call selected through a `typealias`, with the
    /// expanded type it abbreviates.
    by_call: HashMap<crate::fir::OriginId, (Ty, Spelled)>,
    /// Inferred declaration results whose root expression is one of those calls.
    by_declaration: HashMap<crate::fir::DeclarationId, Spelled>,
}

impl ProductionSignatureSemantics<'_> {
    /// Record that the constructor call `origin` named the alias `alias` and produced `result`.
    /// The alias arguments are the alias's formals as `result` binds them in the alias's own
    /// right-hand side; a formal left unbound has no argument to abbreviate with, so nothing is
    /// recorded.
    pub(super) fn record_alias_constructor_result(
        &self,
        origin: crate::fir::OriginId,
        alias: TypeName,
        result: Ty,
    ) {
        let source_template = self.table.alias_expansion_spellings.get(&alias);
        let library_alias = self.table.libraries.type_alias_expansion(alias);
        let Some(template @ (_, formals, expansion)) = source_template
            .map(|(rhs, formals, expansion)| (rhs, formals.as_slice(), *expansion))
            .or_else(|| {
                library_alias.as_ref().map(|library| {
                    (
                        &library.expansion_spelling,
                        library.formals.as_slice(),
                        library.expansion,
                    )
                })
            })
        else {
            return;
        };
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        crate::symbol_resolver::unify_ty(expansion, result, &mut bindings);
        let Some(alias_args) = formals
            .iter()
            .map(|formal| Some((*bindings.get(formal)?, Spelled::default())))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let args = super::super::expansion_arg_spellings(Some(template), &alias_args);
        let spelling = Spelled {
            definitely_non_null: false,
            alias: Some(alias),
            alias_args,
            args,
        };
        self.inferred_result_spellings
            .borrow_mut()
            .by_call
            .insert(origin, (result, spelling));
    }

    /// The declaration's inferred `result` came from the call `root_call`; keep that call's alias
    /// spelling when the published result is still the type the call abbreviated.
    pub(super) fn record_inferred_result_spelling(
        &self,
        declaration: crate::fir::DeclarationId,
        root_call: crate::fir::OriginId,
        result: crate::fir::ResolvedTy,
    ) {
        let mut spellings = self.inferred_result_spellings.borrow_mut();
        let Some((abbreviated, spelling)) = spellings.by_call.get(&root_call) else {
            return;
        };
        if *abbreviated != result.get() {
            return;
        }
        let spelling = spelling.clone();
        spellings.by_declaration.insert(declaration, spelling);
    }
}

/// Publish each recorded spelling as the declaration's result spelling. Only a declaration whose
/// header leaves its result to inference takes one: a declared type keeps what its source spelled.
pub(super) fn publish_inferred_result_spellings(
    index: &mut crate::fir::ResolvedModuleIndex,
    headers: &crate::fir::StreamedHeaderModule,
    spellings: InferredResultSpellings,
) {
    for (declaration, spelling) in spellings.by_declaration {
        let inferred = headers
            .syntax
            .declaration(declaration)
            .is_some_and(|header| match header.kind {
                crate::fir::HeaderDeclarationKind::Callable { result, .. } => {
                    matches!(result, crate::fir::HeaderResultType::Inferred)
                }
                crate::fir::HeaderDeclarationKind::Property { declared_type, .. } => {
                    declared_type.is_none()
                }
                _ => false,
            });
        if inferred {
            index.publish_inferred_result_spelling(declaration, spelling);
        }
    }
}
