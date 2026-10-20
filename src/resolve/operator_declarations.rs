//! Declaration-side operator convention checks.
//!
//! A declared operator name selects the convention being checked. Whether a member implements
//! `Any.equals`, however, is an already-resolved overridden-symbol fact. It deliberately differs
//! from a backend-valid override edge: explicit-`override`, suspend compatibility and covariant
//! results are checked separately. This module never reconstructs that relation from source
//! parameter or result spellings.

use super::*;
use crate::types::TyKind;

#[derive(Clone, Copy)]
pub(super) enum OperatorDeclarationSite {
    NonMember,
    /// Exact stable declaration of a member. A missing identity means the caller is a legacy
    /// signature-inference traversal, not an alternate path for deciding override semantics.
    Member(Option<crate::fir::DeclarationId>),
}

#[derive(Clone, Default)]
struct EqualsMemberFacts {
    anonymous_owner: bool,
    any_equals_override: Option<crate::fir::ResolvedOperatorEqualsOverride>,
    typed_value_class_equals: bool,
    value_class_equals_hint: Option<String>,
}

fn selected_any_equals_override(
    index: &crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    active_local_relations: &crate::fx_hash::HashMap<
        crate::fir::DeclarationId,
        crate::fir::ResolvedOperatorEqualsOverride,
    >,
) -> Option<crate::fir::ResolvedOperatorEqualsOverride> {
    let owner = index.declaration_header(declaration)?.owner?;
    let owner = index.declaration_header(owner)?;
    // A body-local classifier's stable signature is published only after its bounded checker
    // finishes. During that checker, its normalized active-method relation is authoritative. An
    // ordinary/enum owner was finalized in Pass 1 and reads only the immutable module relation.
    let selected = if owner.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS) {
        active_local_relations.get(&declaration).copied()
    } else {
        index.operator_equals_override(declaration)
    }?;
    (selected.overridden_owner == crate::types::wk::any()
        && selected.overridden_semantic_role
            == Some(crate::types::SemanticCallRole::KotlinAnyEquals))
    .then_some(selected)
}

fn value_class_equals_hint(
    index: &crate::fir::ResolvedModuleIndex,
    owner: crate::fir::DeclarationId,
) -> Option<String> {
    let header = index.declaration_header(owner)?;
    if header.kind != crate::fir::DeclarationKind::Classifier
        || !(header.flags.has(crate::fir::DeclarationFlags::VALUE)
            || header.flags.has(crate::fir::DeclarationFlags::FULL_VALUE))
    {
        return None;
    }
    let mut rendered = index.declaration_name(owner)?.to_string();
    let mut parameter_count = 0u32;
    while index.type_parameter(owner, parameter_count).is_some() {
        parameter_count += 1;
    }
    if parameter_count != 0 {
        rendered.push('<');
        rendered.push_str(
            &(0..parameter_count)
                .map(|_| "*")
                .collect::<Vec<_>>()
                .join(", "),
        );
        rendered.push('>');
    }
    Some(rendered)
}

fn equals_member_facts(
    index: &crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    active_local_relations: &crate::fx_hash::HashMap<
        crate::fir::DeclarationId,
        crate::fir::ResolvedOperatorEqualsOverride,
    >,
) -> EqualsMemberFacts {
    let Some(header) = index.declaration_header(declaration) else {
        return EqualsMemberFacts::default();
    };
    let Some(owner) = header.owner else {
        return EqualsMemberFacts::default();
    };
    let Some(owner_header) = index.declaration_header(owner) else {
        return EqualsMemberFacts::default();
    };
    let anonymous_owner = owner_header.kind == crate::fir::DeclarationKind::EnumEntry
        || (owner_header.kind == crate::fir::DeclarationKind::Classifier
            && owner_header
                .flags
                .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT));
    let any_equals_override =
        selected_any_equals_override(index, declaration, active_local_relations);
    let typed_value_class_equals =
        index
            .callable_for_declaration(declaration)
            .is_some_and(|callable| {
                if owner_header.kind != crate::fir::DeclarationKind::Classifier
                    || !owner_header.flags.has(crate::fir::DeclarationFlags::VALUE)
                    || callable.shape.context_parameter_count != 0
                    || callable.shape.extension_receiver.is_some()
                    || index.type_parameter(declaration, 0).is_some()
                {
                    return false;
                }
                let Some(owner) = index.classifier_identity(owner) else {
                    return false;
                };
                let Some(signature) = index.signature(declaration) else {
                    return false;
                };
                matches!(signature.parameters.as_ref(), [parameter]
            if matches!(parameter.get().kind(), TyKind::Obj(classifier, _) if classifier == owner))
                    && matches!(signature.result.get(), Ty::Boolean | Ty::Nothing)
            });
    EqualsMemberFacts {
        anonymous_owner,
        any_equals_override,
        typed_value_class_equals,
        value_class_equals_hint: value_class_equals_hint(index, owner),
    }
}

impl Checker<'_> {
    pub(super) fn selected_any_equals_override(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<crate::fir::ResolvedOperatorEqualsOverride> {
        selected_any_equals_override(
            self.resolved_index?,
            declaration,
            &self.checked_operator_equals_relations,
        )
    }

    pub(super) fn recognize_any_equals_override(
        &mut self,
        function: &FunDecl,
        declaration: Option<crate::fir::DeclarationId>,
        implementation: Option<&crate::libraries::FunctionInfo>,
    ) -> bool {
        let Some(selected) =
            declaration.and_then(|declaration| self.selected_any_equals_override(declaration))
        else {
            return false;
        };
        let Some(implementation) = implementation else {
            return true;
        };
        let target = format!(
            "fun equals(other: Any?): {}",
            selected.overridden_result.get().source_name()
        );
        let owner = selected.overridden_owner.render().replace('/', ".");
        if function.is_suspend() != selected.overridden_suspend {
            let implementation_kind = if function.is_suspend() {
                "suspend"
            } else {
                "non-suspend"
            };
            let target_kind = if selected.overridden_suspend {
                "suspend"
            } else {
                "non-suspend"
            };
            self.diags.error(
                function.name_span,
                format!(
                    "{implementation_kind} function '{}' cannot override {target_kind} function \
                     '{target}' defined in '{owner}'.",
                    function.name
                ),
            );
            return true;
        }
        let actual = implementation
            .ret
            .apply(implementation.callable.ret)
            .canonical_semantic();
        let implementation = if actual == Ty::Error {
            None
        } else {
            let source = self.fed_source();
            (!crate::symbol_resolver::resolution_subtype(
                &source,
                actual,
                selected.overridden_result.get(),
            ))
            .then(|| declaration_rendering::declared_function(&source, implementation))
        };
        if let Some(implementation) = implementation {
            self.diags.error(
                function
                    .ret
                    .as_ref()
                    .map_or(function.name_span, |result| result.span),
                format!(
                    "return type of '{}' is not a subtype of the return type of the overridden \
                     member '{target}' defined in '{owner}'.",
                    implementation
                ),
            );
        }
        true
    }

    /// Check class members with no executable body. Ordinary Pass-2 work enters methods through
    /// their bodies, so interface and abstract declarations are handled once at their owner.
    pub(super) fn check_bodyless_operator_declarations(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        parser_declaration: DeclId,
        owner: Option<TypeName>,
    ) {
        for (method_index, function) in class.methods.iter().enumerate() {
            if !matches!(function.body, FunBody::None) || !function.is_operator() {
                continue;
            }
            let declaration = self.member_declaration(parser_declaration, owner, method_index);
            let ret = declaration
                .and_then(|declaration| self.resolved_index?.signature(declaration))
                .map_or_else(
                    || {
                        function
                            .ret
                            .as_ref()
                            .map_or(Ty::Unit, |ret| self.type_ref_ty_silent(scope, ret))
                    },
                    |signature| signature.result.get(),
                );
            let suppression_depth = self.push_declaration_policies(
                scope,
                &function.annotations,
                &function.annotation_args,
            );
            self.check_operator_declaration(
                function,
                ret,
                OperatorDeclarationSite::Member(declaration),
            );
            self.active_lexical_policies.truncate(suppression_depth);
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
            "equals" => match site {
                OperatorDeclarationSite::NonMember => self.diags.error(
                    operator,
                    "'operator' modifier is not applicable to function: must be a member function.",
                ),
                OperatorDeclarationSite::Member(declaration) => {
                    let facts = declaration.zip(self.resolved_index).map_or_else(
                        EqualsMemberFacts::default,
                        |(declaration, index)| {
                            equals_member_facts(
                                index,
                                declaration,
                                &self.checked_operator_equals_relations,
                            )
                        },
                    );
                    if facts.any_equals_override.is_some() {
                        if !function.is_override() {
                            self.diags.error(
                                function.name_span,
                                format!(
                                    "'{}' hides member of supertype 'Any' and needs an 'override' \
                                     modifier.",
                                    function.name
                                ),
                            );
                        }
                        return;
                    }
                    if self.file.custom_equals_in_value_classes && facts.typed_value_class_equals {
                        return;
                    }
                    let message = if self.file.custom_equals_in_value_classes {
                        facts.value_class_equals_hint.as_deref().map_or_else(
                            || {
                                "'operator' modifier is not applicable to function: must override \
                                 'equals()' in Any."
                                    .to_string()
                            },
                            |owner| {
                                format!(
                                    "'operator' modifier is not applicable to function: must \
                                     override 'equals()' in Any or define 'equals(other: \
                                     {owner}): Boolean'."
                                )
                            },
                        )
                    } else {
                        "'operator' modifier is not applicable to function: must override \
                         'equals()' in Any."
                            .to_string()
                    };
                    if facts.anonymous_owner {
                        self.report_deprecation_error(
                            operator,
                            self.file
                                .forbid_operator_equals_in_enum_entries_and_anonymous_objects,
                            self.file.inapplicable_operator_modifier_warning.as_deref(),
                            crate::diag::CompilerWarningName::InapplicableOperatorModifier,
                            &message,
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
