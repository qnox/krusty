//! Type parameters a signature scope can name: a declaration's own formals, by declaration kind,
//! and the lexically enclosing declarations' formals.

use super::*;

impl ProductionSignatureSemantics<'_> {
    /// Property type parameters in the order the whole-table scans used to prefer them: a top-level
    /// source property, then a top-level extension, then a member extension. The first recorded
    /// declaration wins, including a property with no formals, so a miss still means "not a
    /// property" and falls through to the header-only parameters.
    pub(super) fn property_type_parameters(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<super::DeclarationTypeParameters> {
        let missing = self
            .property_type_parameters_by_declaration
            .borrow()
            .is_none();
        if missing {
            let mut parameters = crate::fx_hash::HashMap::default();
            let record = |parameters: &mut crate::fx_hash::HashMap<
                crate::fir::DeclarationId,
                super::DeclarationTypeParameters,
            >,
                          declaration: crate::fir::DeclarationId,
                          formals: &[crate::types::TyParamName],
                          formal_bounds: &[Ty]| {
                parameters
                    .entry(declaration)
                    .or_insert_with(|| super::DeclarationTypeParameters {
                        formals: formals.to_vec(),
                        bounds: formal_bounds
                            .iter()
                            .copied()
                            .map(|bound| vec![bound])
                            .collect(),
                    });
            };
            for property in self.table.source_props.values() {
                if let Some(declaration) = property.stable_declaration {
                    record(
                        &mut parameters,
                        declaration,
                        &property.formals,
                        &property.formal_bounds,
                    );
                }
            }
            for property in self.table.ext_props.values().flatten() {
                if let Some(declaration) = property.stable_declaration {
                    record(
                        &mut parameters,
                        declaration,
                        &property.formals,
                        &property.formal_bounds,
                    );
                }
            }
            for class in self.table.classes.values() {
                for property in class.member_ext_props.values().flatten() {
                    if let Some(declaration) = property.stable_declaration() {
                        record(
                            &mut parameters,
                            declaration,
                            property.type_params(),
                            property.type_param_bounds(),
                        );
                    }
                }
            }
            *self.property_type_parameters_by_declaration.borrow_mut() = Some(parameters);
        }
        self.property_type_parameters_by_declaration
            .borrow()
            .as_ref()
            .and_then(|parameters| parameters.get(&declaration).cloned())
    }

    /// Every type parameter fixed at `scope`: those of the declaration that owns the scope and of
    /// each lexically enclosing declaration, including the formals a local or anonymous classifier
    /// captures.
    pub(super) fn lexically_visible_type_parameters(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Result<crate::fx_hash::HashSet<&'static str>, crate::fir::DiagnosticId> {
        // Signature type resolution is the authority on what a type reference here can name,
        // header-only declarations included. The declaration walk below adds the formals a nearer
        // same-named one shadows: they cannot be named, but still reach this scope through members.
        let mut visible = self.with_signature_type_scope(scope, |lexical| {
            lexical
                .visible_tparams()
                .binding_types()
                .filter_map(|ty| ty.ty_param_name())
                .collect::<crate::fx_hash::HashSet<_>>()
        })?;
        let mut owner = Some(scope.owner);
        while let Some(declaration) = owner {
            if let Some(generic) = self
                .callable_signature(declaration)
                .and_then(|signature| signature.generic_sig.as_ref())
            {
                visible.extend(generic.formals.iter().map(|formal| formal.as_str()));
            }
            if let Some(parameters) = self.property_type_parameters(declaration) {
                visible.extend(parameters.formals.iter().map(|formal| formal.as_str()));
            }
            if let Some(class) = self.classifier_signature(declaration) {
                visible.extend(
                    class
                        .type_parameters
                        .type_params()
                        .iter()
                        .chain(class.captured_type_parameters.type_params())
                        .map(|formal| formal.as_str()),
                );
            }
            owner = self
                .headers
                .declarations
                .anchor(declaration)
                .and_then(|anchor| anchor.owner);
        }
        Ok(visible)
    }
}
