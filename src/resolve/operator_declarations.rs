//! Declaration-side operator convention checks.
//!
//! A declared operator name selects the convention being checked. Whether a member implements
//! `Any.equals`, however, is an already-resolved overridden-symbol fact. It deliberately differs
//! from a backend-valid override edge: explicit-`override`, suspend compatibility and covariant
//! results are checked separately. This module never reconstructs that relation from source
//! parameter or result spellings.

use super::assignment_targets::ASSIGNMENT_CONVENTION;
use super::*;
use crate::types::TyKind;

#[derive(Clone, Copy)]
pub(super) enum OperatorDeclarationSite {
    /// A top-level or local function, with its extension receiver.
    NonMember(Option<Ty>),
    /// Exact stable declaration of a member. A missing identity means the caller is a legacy
    /// signature-inference traversal, not an alternate path for deciding override semantics. The
    /// type is the receiver a call binds: the extension receiver, else the dispatch receiver.
    Member(Option<crate::fir::DeclarationId>, Option<Ty>),
}

/// The return an override must meet when a Java method's declared return is a type parameter
/// and the applied return is that parameter's non-null type argument wrapped as a platform type.
/// `None` leaves the ordinary flexible return in place.
fn java_type_parameter_override_return(
    source: &dyn crate::symbol_source::SymbolSource,
    candidate: &crate::libraries::FunctionInfo,
) -> Option<Ty> {
    let declared = candidate.callable.declared_ret.or_else(|| {
        let record = source.classifier(candidate.callable.owner)?;
        let member = record
            .members
            .iter()
            .find(|member| member.name == candidate.callable.name)?;
        Some(
            member
                .generic_sig
                .as_ref()
                .map(|signature| signature.ret)
                .unwrap_or(member.ret),
        )
    })?;
    let type_parameter = match declared.kind() {
        TyKind::TyParam(_, _) | TyKind::Var(_, _) => true,
        TyKind::PlatformNullable(parameter) => parameter.is_ty_param(),
        _ => false,
    };
    if !type_parameter {
        return None;
    }
    let applied = candidate
        .ret
        .apply(candidate.callable.ret)
        .canonical_semantic();
    match applied.kind() {
        TyKind::PlatformNullable(inner) if !inner.is_nullable() => Some(inner),
        _ => None,
    }
}

impl OperatorDeclarationSite {
    /// The receiver a call of the declaration binds, if any.
    fn call_receiver(self) -> Option<Ty> {
        match self {
            OperatorDeclarationSite::NonMember(receiver)
            | OperatorDeclarationSite::Member(_, receiver) => receiver,
        }
    }
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

    /// A Java method declared as `T get()` and applied at `Supplier<String>` is flexible `String!`
    /// at a call, but an override is checked against the non-null type argument `String`.
    /// `Nothing?` and `String?` are not subtypes of that argument. A concrete Java `String!`
    /// (`Face.name`) is not a type parameter and stays flexible, so `String?` remains a valid override.
    pub(super) fn report_java_type_argument_override_return(
        &mut self,
        function: &FunDecl,
        owner_label: &str,
        implementation: Option<&crate::libraries::FunctionInfo>,
        candidates: &[crate::libraries::FunctionInfo],
        inferred_return: Option<Ty>,
    ) {
        let Some(implementation) = implementation else {
            return;
        };
        let source = self.fed_source();
        let Some(candidate) = candidates.iter().find(|candidate| {
            candidate.visibility != Visibility::Private
                && !candidate.flags.is_final
                && crate::symbol_resolver::function_input_shapes_match(
                    &source,
                    candidate,
                    implementation,
                )
        }) else {
            return;
        };
        let Some(required) = java_type_parameter_override_return(&source, candidate) else {
            return;
        };
        let mut implementation = implementation.clone();
        // An inferred `override fun get() = ...` still carries the unresolved header return when
        // the override edge is recorded. The body type is the return kotlinc renders.
        if let Some(inferred_return) = inferred_return {
            if let Some(signature) = implementation.generic_sig.as_mut() {
                signature.ret = inferred_return;
            }
            implementation.callable.ret = inferred_return;
            implementation.ret.class = None;
        }
        let actual = implementation
            .ret
            .apply(implementation.callable.ret)
            .canonical_semantic();
        if crate::symbol_resolver::resolution_subtype(&source, actual, required) {
            return;
        }
        let rendered_implementation =
            declaration_rendering::declared_function(&source, &implementation);
        let mut target_function = candidate.clone();
        if let Some(signature) = target_function.generic_sig.as_mut() {
            signature.ret = required;
        }
        target_function.callable.ret = required;
        let target = declaration_rendering::declared_function(&source, &target_function);
        self.diags.error(
            function
                .ret
                .as_ref()
                .map_or(function.name_span, |result| result.span),
            format!(
                "return type of '{rendered_implementation}' is not a subtype of the return type \
                 of the overridden member '{target}' defined in '{owner_label}'."
            ),
        );
    }

    /// An inferred override return is known only after the body is typed. The header walk records
    /// the edge while that return is still unresolved.
    pub(super) fn report_inferred_java_override_return(
        &mut self,
        scope: &CheckerScope<'_>,
        class: DeclId,
        class_decl: &ClassDecl,
        method: &FunDecl,
        method_index: usize,
        current_owner: Option<TypeName>,
        inferred_return: Ty,
    ) {
        if self.signature_defaults_only {
            return;
        }
        let Some(internal) = current_owner else {
            return;
        };
        let receiver = scope.this_ty().unwrap_or(Ty::obj_name(internal));
        let direct_supertypes = crate::symbol_resolver::with_implicit_any(
            internal,
            <Self as crate::assignable::TypeOracle>::direct_supertypes(self, receiver),
        );
        let source_member = crate::libraries::SourceMember::Class {
            file: self.file_index,
            owner: class.0,
            method: method_index as u32,
        };
        let declaration = self.member_declaration(class, current_owner, method_index);
        let implementation = match self.override_implementation(
            scope,
            class,
            method,
            receiver,
            declaration,
            source_member,
        ) {
            local_method_dependencies::OverrideImplementation::Bound(implementation) => {
                *implementation
            }
            local_method_dependencies::OverrideImplementation::Absent
            | local_method_dependencies::OverrideImplementation::Unbound => return,
        };
        if self.recognize_any_equals_override(method, declaration, Some(&implementation)) {
            return;
        }
        let candidates = {
            let source = self.fed_source();
            direct_supertypes
                .iter()
                .flat_map(|supertype| {
                    crate::symbol_resolver::member_declarations_in_hierarchy(
                        &source,
                        *supertype,
                        &method.name,
                    )
                    .into_parts()
                    .0
                    .overloads
                })
                .collect::<Vec<_>>()
        };
        let owner_label = if self.file.is_anonymous_object_class(class) {
            "<anonymous>"
        } else {
            class_decl.name.as_str()
        };
        self.report_java_type_argument_override_return(
            method,
            owner_label,
            Some(&implementation),
            &candidates,
            Some(inferred_return),
        );
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
            if !matches!(function.body, FunBody::None)
                || !(function.is_operator() || function.name == ASSIGNMENT_CONVENTION)
            {
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
            let receiver = match &function.receiver {
                Some(receiver) => Some(self.type_ref_ty_silent(scope, receiver)),
                None => owner.map(Ty::obj_name),
            };
            self.check_operator_declaration(
                function,
                ret,
                OperatorDeclarationSite::Member(declaration, receiver),
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
        self.check_assignment_declaration(function, ret, site.call_receiver());
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
                OperatorDeclarationSite::NonMember(_) => self.diags.error(
                    operator,
                    "'operator' modifier is not applicable to function: must be a member function.",
                ),
                OperatorDeclarationSite::Member(declaration, _) => {
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
                            DeprecationNames {
                                error: crate::diag::names::INAPPLICABLE_OPERATOR_MODIFIER,
                                warning: crate::diag::names::INAPPLICABLE_OPERATOR_MODIFIER_WARNING,
                            },
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
