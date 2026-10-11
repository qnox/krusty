//! kotlinc's `FirOperatorModifierChecker` for the operator conventions krusty checks: a declared
//! `operator fun` whose name selects a convention must have that convention's shape.
//!
//! Operator conventions are selected by name, so the checks here read the declared name. Each
//! failure is `INAPPLICABLE_OPERATOR_MODIFIER` at the `operator` keyword, worded as kotlinc's
//! `OperatorFunctionChecks` explains it. An `equals` in an anonymous object or an enum entry that
//! does not override `Any.equals` is the deprecation `INAPPLICABLE_OPERATOR_MODIFIER_WARNING` until
//! `ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects` makes it the error.

use super::*;

/// Where an `operator fun` is declared, as far as its convention checks read it.
#[derive(Clone, Copy)]
pub(super) enum OperatorDeclarationSite {
    /// A top-level or local function: it has no dispatch receiver.
    NonMember,
    /// A member of a classifier.
    Member {
        /// The classifier is an anonymous object or an enum entry's body, which kotlinc models as
        /// an anonymous object, rather than a class, interface or named object.
        anonymous_owner: bool,
        /// The function overrides `Any.equals`: no receiver, context or type parameters, and one
        /// value parameter of type `Any?`.
        overrides_any_equals: bool,
    },
}

/// Whether a member with these facts overrides `Any.equals(other: Any?)`. `parameters` are the
/// function's resolved parameter types, context parameters first.
pub(super) fn overrides_any_equals(function: &FunDecl, parameters: &[Ty]) -> bool {
    function.name == "equals"
        && function.receiver.is_none()
        && function.context_count == 0
        && function.type_params.is_empty()
        && function.params.len() == 1
        && !function.params[0].is_vararg
        && parameters.first().copied() == Some(Ty::nullable(Ty::Obj(crate::types::wk::any(), &[])))
}

impl Checker<'_> {
    /// Check the `operator` members of `cl` that have no body. Pass 2 checks a member through its
    /// body, so an abstract or interface member is checked here, from its written signature.
    pub(super) fn check_bodyless_operator_declarations(
        &mut self,
        scope: &CheckerScope<'_>,
        cl: &ClassDecl,
        declaration: DeclId,
    ) {
        let anonymous_owner = self.file.is_anonymous_object_class(declaration);
        for function in &cl.methods {
            if !matches!(function.body, FunBody::None) || !function.is_operator() {
                continue;
            }
            let ret = function
                .ret
                .as_ref()
                .map_or(Ty::Unit, |ret| self.type_ref_ty_silent(scope, ret));
            let parameters = function
                .params
                .iter()
                .map(|parameter| self.type_ref_ty_silent(scope, &parameter.ty))
                .collect::<Vec<_>>();
            let site = OperatorDeclarationSite::Member {
                anonymous_owner,
                overrides_any_equals: overrides_any_equals(function, &parameters),
            };
            self.check_operator_declaration(function, ret, site);
        }
    }

    pub(super) fn check_operator_declaration(
        &mut self,
        function: &FunDecl,
        ret: Ty,
        site: OperatorDeclarationSite,
    ) {
        // A delegated-property convention is called from a GENERATED accessor, which has no scope
        // to fill an implicit context from. The declaration is rejected here, so the property that
        // uses it reports no applicable convention rather than reaching a call whose argument list
        // cannot be mapped onto the declaration's slots.
        if function.is_operator()
            && crate::resolve::delegated_properties::DELEGATE_CONVENTION_NAMES
                .contains(&function.name.as_str())
            && !crate::resolve::delegated_properties::is_usable_delegate_convention(
                true,
                function.context_count,
            )
        {
            self.diags.error(
                function
                    .context_span
                    .expect("a function with context parameters must retain its clause span"),
                "context parameters on delegation operators are unsupported.".to_string(),
            );
        }
        if !function.is_operator() {
            return;
        }
        let operator = function
            .operator_span
            .expect("an operator function must retain its modifier span");
        match function.name.as_str() {
            "hasNext" if !matches!(ret.canonical_semantic(), Ty::Boolean | Ty::Error) => {
                self.diags.error(
                    operator,
                    "'operator' modifier is not applicable to function: must return 'Boolean'.",
                );
            }
            // `Checks.member`, then `Checks.EqualsOverridesEqualsOfAny`.
            "equals" => match site {
                OperatorDeclarationSite::NonMember => self.diags.error(
                    operator,
                    "'operator' modifier is not applicable to function: must be a member function.",
                ),
                OperatorDeclarationSite::Member {
                    overrides_any_equals: true,
                    ..
                } => {}
                OperatorDeclarationSite::Member {
                    anonymous_owner,
                    overrides_any_equals: false,
                } => {
                    let message = "'operator' modifier is not applicable to function: must \
                                   override 'equals()' in Any.";
                    if anonymous_owner {
                        self.report_deprecation_error(
                            operator,
                            "ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects",
                            self.file
                                .forbid_operator_equals_in_enum_entries_and_anonymous_objects,
                            "INAPPLICABLE_OPERATOR_MODIFIER_WARNING",
                            message,
                        );
                    } else {
                        self.diags.error(operator, message);
                    }
                }
            },
            _ => {}
        }
    }
}
