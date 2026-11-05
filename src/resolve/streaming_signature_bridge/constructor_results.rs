//! Constructor signature results from selected declaration evidence.

use super::*;

/// Select the language-defined SAM construction for a classifier call. Qualified and unqualified
/// classifier spellings share this path; the qualifier affects only how the classifier was found,
/// never whether its abstract method is applicable.
pub(super) fn selected_sam_constructor_result(
    semantics: &ProductionSignatureSemantics<'_>,
    scope: crate::fir::SignatureScope,
    internal: crate::types::TypeName,
    actual: Ty,
    type_arguments: &[Ty],
) -> Option<Ty> {
    let module =
        crate::module_symbols::ModuleSymbols::for_file(semantics.table, scope.source.raw());
    let source = crate::symbol_source::CompositeSource::new(vec![
        &module as &dyn crate::symbol_source::SymbolSource,
        &*semantics.table.libraries as &dyn crate::symbol_source::SymbolSource,
    ]);
    let classifier = crate::symbol_source::SymbolSource::classifier(&source, internal)?;
    if !type_arguments.is_empty() && type_arguments.len() != classifier.type_params.len() {
        return None;
    }
    let target = if type_arguments.is_empty() {
        Ty::obj_name(internal)
    } else {
        Ty::obj_args_name(internal, type_arguments)
    };
    let signature = crate::symbol_resolver::semantic_sam_signature(&source, target)?;
    super::super::select_sam_constructor(&source, signature, actual).map(|selected| selected.result)
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        selected: &crate::symbol_resolver::SelectedConstructorDeclaration,
        arguments: &[Ty],
        bound_outer: Option<Ty>,
        explicit_type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        use crate::symbol_resolver::selected_constructor::unlisted_constructor_type;
        let member = &selected.declaration;
        let owner = member.owner.ok_or_else(Self::failure)?;
        let Some(class) = self.table.class_by_type_name(owner) else {
            let result =
                unlisted_constructor_type(selected, owner, explicit_type_arguments, expected);
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        };
        // Constructor selection has already applied explicit arguments, literal adaptation,
        // generic bounds, and overload mapping to `member.ret`. Seed those selected classifier
        // arguments before filling any still-unbound variable from the raw expression types; doing
        // the latter first would incorrectly turn `C<T : Long>(0)` into `C<Int>`.
        let selected_arguments = if member.ret.obj_internal() == Some(owner) {
            member.ret.type_args()
        } else {
            &[]
        };
        let mut bindings = class
            .type_params
            .iter()
            .map(crate::types::TyParamName::from)
            .zip(selected_arguments.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        // A selected constructor result can contain an early partial solution (for example the
        // first `T` occurrence in `PairBox("x", null)`). It is not a completed classifier result
        // until every argument occurrence has contributed. Infer into a fresh map, then replace
        // only classifier slots not fixed by a written type argument; this preserves
        // `C<Number>(1)` while completing `PairBox("x", null)` to `PairBox<String?>`.
        let declared_parameters: std::borrow::Cow<'_, [Ty]> = member
            .generic_sig
            .as_ref()
            .filter(|signature| signature.params.len() == arguments.len())
            .map(|signature| std::borrow::Cow::Borrowed(signature.params.as_slice()))
            .unwrap_or_else(|| {
                std::borrow::Cow::Owned(
                    class
                        .ctor_param_shapes
                        .iter()
                        .map(|parameter| parameter.0)
                        .collect(),
                )
            });
        let mut inferred = crate::symbol_resolver::GSigBinds::default();
        for (parameter, argument) in declared_parameters.iter().zip(arguments) {
            crate::symbol_resolver::unify_inferred_ty(*parameter, *argument, &mut inferred);
        }
        let mut completed_bindings = bindings.clone();
        for (index, formal) in class.type_params.iter().enumerate() {
            let explicitly_fixed = explicit_type_arguments
                .get(index)
                .is_some_and(|argument| *argument != Ty::Error);
            if !explicitly_fixed {
                if let Some(inferred) = inferred.get(formal.as_str()).copied() {
                    completed_bindings.insert(crate::types::TyParamName::new(formal), inferred);
                }
            }
        }
        // The fresh completion above sees compact expression result types, but no longer carries
        // literal provenance. It may therefore infer `Int` from the raw `0` after constructor
        // selection correctly adapted that literal to a declared `T : Long` parameter. Accept the
        // completed map only when it still satisfies the constructor's declaration bounds;
        // otherwise the selected semantic result remains authoritative. This is generic
        // constructor inference—value classes use the same path as ordinary classes.
        let completed_is_valid = member.generic_sig.as_ref().is_none_or(|signature| {
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            crate::symbol_resolver::generic_bindings_satisfy_bounds(
                signature,
                &completed_bindings,
                |actual, bound| {
                    crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        &oracle,
                        actual,
                        bound,
                    )
                },
            )
        });
        if completed_is_valid {
            bindings = completed_bindings;
        }
        let formal_bounds = class
            .type_param_bounds
            .iter()
            .map(|bound| {
                (*bound != Ty::Error)
                    .then_some(*bound)
                    .into_iter()
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        crate::symbol_resolver::seed_unbound_constructor_result_from_symbols(
            &source,
            owner,
            &class.type_params,
            &formal_bounds,
            expected,
            &mut bindings,
        );
        let mut result_arguments = class
            .type_params
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                bindings.get(formal.as_str()).copied().unwrap_or_else(|| {
                    class
                        .type_param_bounds
                        .get(index)
                        .copied()
                        .filter(|bound| *bound != Ty::Error)
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                })
            })
            .collect::<Vec<_>>();
        if !class.captured_type_parameters.type_params.is_empty() {
            let receivers = bound_outer
                .into_iter()
                .chain(self.implicit_receivers(scope));
            let receiver_bindings = receivers
                .flat_map(|receiver| self.table.applied_type_hierarchy(receiver.non_null()))
                .filter_map(|(receiver_owner, applied, _)| {
                    self.table
                        .class_by_type_name(receiver_owner)
                        .map(|declaration| declaration.type_parameter_bindings(applied))
                })
                .collect::<Vec<_>>();
            for (ordinal, captured) in class
                .captured_type_parameters
                .type_params
                .iter()
                .enumerate()
            {
                let argument = receiver_bindings
                    .iter()
                    .find_map(|bindings| bindings.get(captured.as_str()).copied())
                    .unwrap_or_else(|| {
                        Ty::ty_param(
                            captured,
                            class
                                .captured_type_parameters
                                .type_param_bounds
                                .get(ordinal)
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        )
                    });
                result_arguments.push(argument);
            }
        }
        let result = Ty::obj_args_name(owner, &result_arguments);
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }
}
