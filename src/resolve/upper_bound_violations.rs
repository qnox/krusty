//! kotlinc's `UPPER_BOUND_VIOLATED`: a type argument that is not a subtype of its parameter's
//! declared upper bounds, substituted with the arguments of the same application
//! (`I<T : Comparable<T>>` applied as `I<Any>` requires `Comparable<Any>`). A star projection is
//! not checked; an `in`/`out` projection checks its projected type and is reported from the
//! projection keyword.
//!
//! The checker checks each classifier application it resolves itself. A declared type that Pass 1
//! finalized is not resolved again: its written syntax is walked beside the finalized type and the
//! alias spellings Pass 1 recorded for it, and each classifier application found there is checked
//! by the same operation.
//!
//! Until `ForbidUpperBoundsViolationOnTypeOperatorAndParameterBounds`, kotlinc does not check the
//! outermost application of a type operator's target (`a as B<String>`) or of a callable's
//! type-parameter bound (`fun <T : B<String>> f()`); the arguments nested inside them are checked.

use super::type_parameter_owners::{
    classifier_type_parameter_owner, mentioned_type_parameters, TypeParameterNumbering,
};
use super::{function_type_ref_shape, Checker, CheckerScope, ScopeKind};
use crate::ast::TypeRef;
use crate::diag::Span;
use crate::libraries::LibraryType;
use crate::spelling::Spelled;
use crate::symbol_source::SymbolSource as _;
use crate::types::{Ty, TypeName};

#[cfg(test)]
mod tests;

/// Check finalized top-level type-alias targets while their source syntax is still live. Stable
/// declaration identity owns the semantic expansion; the parallel syntax supplies only spans.
pub(crate) fn check_finalized_top_level_type_alias_bounds(
    files: &[crate::ast::File],
    index: &crate::fir::ResolvedModuleIndex,
    symbols: &super::SymbolTable,
    diags: &mut crate::diag::DiagSink,
) {
    for (file_index, file) in files.iter().enumerate() {
        let source = crate::fir::SourceFileId::from_raw(file_index as u32);
        let mut declarations = index
            .source_inventory(source)
            .iter()
            .copied()
            .filter(|declaration| {
                index
                    .declaration_header(*declaration)
                    .is_some_and(|header| {
                        header.owner.is_none()
                            && header.kind == crate::fir::DeclarationKind::TypeAlias
                    })
            })
            .collect::<Vec<_>>();
        declarations.sort_unstable_by_key(|declaration| {
            index
                .source_order(*declaration)
                .expect("an inventoried top-level type alias has source order")
        });
        if declarations.is_empty() {
            continue;
        }

        // Earlier declaration validation may reject one alias while retaining its parser syntax
        // for the diagnostic ledger. The declaration anchor's sibling is the stable inventory edge
        // to that syntax; source order only orders the stable side and never substitutes for
        // identity by zipping lists whose rejected members differ.
        let declarations = declarations
            .into_iter()
            .filter_map(|declaration| {
                let header = index.type_alias_header(declaration)?;
                let anchor = index
                    .declaration_anchor(declaration)
                    .expect("an inventoried top-level type alias has an anchor");
                assert_eq!(anchor.source, source);
                assert_eq!(anchor.owner, None);
                assert_eq!(anchor.kind, crate::fir::DeclarationKind::TypeAlias);
                let sibling = usize::try_from(anchor.sibling)
                    .expect("a type-alias sibling fits the active parser arena");
                let syntax = file
                    .type_alias_decls
                    .get(sibling)
                    .expect("a stable type-alias sibling names its exact parser declaration");
                Some((syntax, header))
            })
            .collect::<Vec<_>>();

        diags.set_file(file_index as u32);
        let mut checker = super::make_checker_with_index(
            file,
            file_index as u32,
            Some(files),
            symbols,
            Some(index),
            None,
            None,
            diags,
        );
        checker.push_file_policies(index.file_lexical_policies(source));
        for (syntax, header) in declarations {
            checker.check_type_alias_declaration_bounds(
                &syntax.target,
                header.expansion.get(),
                &header.expansion_spelling,
            );
        }
    }
}

/// Transient context owned by upper-bound checking while one type reference is resolved.
#[derive(Default)]
pub(super) struct UpperBoundViolationState {
    /// The type reference whose outermost application is exempt from the upper-bound check: a type
    /// operator's target or a callable's type-parameter bound.
    unchecked_reference: Option<Span>,
    /// A typealias declaration's right-hand side is being checked: an argument that is a type
    /// parameter or carries arguments is checked only through its nested applications.
    alias_declaration: bool,
}

impl Checker<'_> {
    /// Check the written arguments of `reference`, which the checker resolved to the classifier
    /// application `resolved`. Arguments nested in `reference` are applications of their own.
    pub(super) fn check_type_reference_bounds(&mut self, reference: &TypeRef, resolved: Ty) {
        if self.upper_bound_violations.unchecked_reference == Some(reference.span) {
            return;
        }
        self.check_written_application_bounds(&reference.targs, resolved);
    }

    /// Check the written type `arguments` of a classifier application outside a type reference
    /// (the left side of a callable reference), which resolved to `resolved`.
    pub(super) fn check_written_application_bounds(&mut self, arguments: &[TypeRef], resolved: Ty) {
        let Ty::Obj(classifier, resolved) = resolved.non_null() else {
            return;
        };
        let Some(declaration) = self.fed_source().classifier(classifier) else {
            return;
        };
        self.check_classifier_argument_bounds(
            classifier,
            &declaration,
            arguments,
            resolved.as_slice(),
        );
    }

    /// [`Self::check_type_parameter_bound`] for a callable's type parameter, whose outermost
    /// application kotlinc checks only under
    /// `ForbidUpperBoundsViolationOnTypeOperatorAndParameterBounds`.
    pub(super) fn check_callable_type_parameter_bound(
        &mut self,
        scope: &CheckerScope<'_>,
        bound: &TypeRef,
    ) -> Ty {
        self.with_unchecked_outermost_bounds(bound, |checker| {
            checker.check_type_parameter_bound(scope, bound)
        })
    }

    /// Check every declared bound of one callable after its type-parameter scope is installed.
    pub(super) fn check_callable_type_parameter_bounds(
        &mut self,
        scope: &CheckerScope<'_>,
        bounds: &[(String, TypeRef)],
    ) {
        for (_, bound) in bounds {
            self.check_callable_type_parameter_bound(scope, bound);
        }
    }

    /// Run `resolve` with the outermost application of `reference` exempt from the bound check.
    pub(super) fn with_unchecked_outermost_bounds<R>(
        &mut self,
        reference: &TypeRef,
        resolve: impl FnOnce(&mut Self) -> R,
    ) -> R {
        if self
            .file
            .forbid_upper_bounds_violation_on_type_operator_and_parameter_bounds
        {
            return resolve(self);
        }
        let previous = self
            .upper_bound_violations
            .unchecked_reference
            .replace(reference.span);
        let result = resolve(self);
        self.upper_bound_violations.unchecked_reference = previous;
        result
    }

    /// Check a declared type that Pass 1 finalized: `reference` is its written syntax, `resolved`
    /// the finalized type, and `spelled` the alias spellings Pass 1 recorded for it.
    pub(super) fn check_finalized_type_bounds(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
        spelled: &Spelled,
    ) {
        if reference.is_star_projection() {
            return;
        }
        let resolved = match resolved {
            Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
            other => other,
        }
        .non_null();
        if let Some(alias) = spelled.alias {
            let arguments = spelled
                .alias_args
                .iter()
                .map(|(resolved, _)| *resolved)
                .collect::<Vec<_>>();
            if let Some((formals, template)) = self.source_alias_expansion(alias) {
                self.check_alias_application_bounds(
                    &formals,
                    template,
                    &reference.targs,
                    &arguments,
                    reference.span,
                );
            }
            // The written arguments of an alias application are applications of their own.
            for (argument, (resolved, spelled)) in reference.targs.iter().zip(&spelled.alias_args) {
                self.check_finalized_type_bounds(argument, *resolved, spelled);
            }
            return;
        }
        if let Some(shape) = function_type_ref_shape(reference) {
            let Ty::Fun(signature) = resolved else {
                return;
            };
            let components = shape.params.iter().zip(signature.params.iter().copied());
            let ret = shape.ret.map(|ret| (ret, signature.ret));
            for (index, (component, resolved)) in components.chain(ret).enumerate() {
                self.check_finalized_type_bounds(component, resolved, spelled.arg(index));
            }
            return;
        }
        let Ty::Obj(classifier, arguments) = resolved else {
            return;
        };
        if let Some(declaration) = self.fed_source().classifier(classifier) {
            self.check_classifier_argument_bounds(
                classifier,
                &declaration,
                &reference.targs,
                arguments.as_slice(),
            );
        }
        for (index, (argument, resolved)) in reference.targs.iter().zip(arguments).enumerate() {
            self.check_finalized_type_bounds(argument, *resolved, spelled.arg(index));
        }
    }

    /// Check the application of a typealias with `formals` and expansion `template` to the written
    /// `arguments`, which resolved to `resolved` (projections kept), at `reference`.
    ///
    /// kotlinc checks the expansion's outermost application in full. Below it, only the arguments
    /// that are exactly a formal are checked, and only the applications that mention a formal are
    /// entered: the rest of the expansion is the declaration's own, checked where it is declared.
    /// A violating argument that is exactly a formal is reported at the use-site argument written
    /// for it; any other at the alias reference.
    pub(super) fn check_alias_application_bounds(
        &mut self,
        formals: &[String],
        template: Ty,
        arguments: &[TypeRef],
        resolved: &[Ty],
        reference: Span,
    ) {
        if self.upper_bound_violations.unchecked_reference == Some(reference) {
            return;
        }
        let bindings = formals
            .iter()
            .cloned()
            .zip(resolved.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        let application = AliasApplication {
            formals,
            bindings: &bindings,
            arguments,
            reference,
        };
        self.check_alias_template_bounds(&application, template, true);
    }

    fn check_alias_template_bounds(
        &mut self,
        application: &AliasApplication<'_>,
        template: Ty,
        outermost: bool,
    ) {
        let template = match template {
            Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
            other => other,
        }
        .non_null();
        let components = match template {
            Ty::Obj(classifier, template_arguments) => {
                if let Some(declaration) = self.fed_source().classifier(classifier) {
                    let formals = bounded_formals(&declaration);
                    if formals.iter().any(|formal| !formal.bounds.is_empty()) {
                        let resolved = template_arguments
                            .iter()
                            .map(|argument| {
                                crate::types::ty_subst_alias_expansion(
                                    *argument,
                                    application.bindings,
                                )
                            })
                            .collect::<Vec<_>>();
                        let spans = template_arguments
                            .iter()
                            .zip(&resolved)
                            .map(|(argument, resolved)| {
                                let span = match application.formal_argument(*argument) {
                                    Some(written) => type_argument_span(written),
                                    None => outermost.then_some(application.reference),
                                };
                                span.filter(|_| {
                                    !self.upper_bound_violations.alias_declaration
                                        || is_alias_checked(*resolved)
                                })
                            })
                            .collect::<Vec<_>>();
                        let owner = classifier_type_parameter_owner(
                            &self.fed_source(),
                            classifier,
                            &declaration,
                        );
                        self.check_type_argument_bounds(&formals, &owner, &resolved, &spans);
                    }
                }
                template_arguments.to_vec()
            }
            Ty::Fun(signature) => signature
                .params
                .iter()
                .copied()
                .chain(std::iter::once(signature.ret))
                .collect(),
            _ => Vec::new(),
        };
        for component in components {
            if application.mentions_formal(component) {
                self.check_alias_template_bounds(application, component, false);
            }
        }
    }

    /// Check the right-hand side of a typealias declaration, which Pass 1 finalized as `expansion`
    /// with the alias spellings `spelled`. A type argument that is a type parameter or has type
    /// arguments of its own is not checked there; the applications nested in it are.
    pub(super) fn check_type_alias_declaration_bounds(
        &mut self,
        target: &TypeRef,
        expansion: Ty,
        spelled: &Spelled,
    ) {
        let previous = std::mem::replace(&mut self.upper_bound_violations.alias_declaration, true);
        self.check_finalized_type_bounds(target, expansion, spelled);
        self.check_finalized_package_type_arguments(target, expansion, spelled);
        self.upper_bound_violations.alias_declaration = previous;
    }

    /// Run `resolve`, the resolution of a typealias declaration's right-hand side, under the
    /// declaration's bound-check rule (see [`Self::check_type_alias_declaration_bounds`]).
    pub(super) fn resolving_alias_declaration<R>(
        &mut self,
        resolve: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous = std::mem::replace(&mut self.upper_bound_violations.alias_declaration, true);
        let result = resolve(self);
        self.upper_bound_violations.alias_declaration = previous;
        result
    }

    /// Resolve one lexical typealias target under the declaration-site bound rule.
    pub(super) fn resolve_alias_declaration_target(
        &mut self,
        scope: &CheckerScope<'_>,
        target: &TypeRef,
    ) -> Ty {
        self.resolving_alias_declaration(|checker| checker.type_ref_ty(scope, target))
    }

    /// Check the written `arguments` of the application of `classifier`, whose resolved arguments
    /// (projections kept, own arguments first) are `resolved`.
    fn check_classifier_argument_bounds(
        &mut self,
        classifier: TypeName,
        declaration: &LibraryType,
        arguments: &[TypeRef],
        resolved: &[Ty],
    ) -> bool {
        let formals = bounded_formals(declaration);
        if formals.iter().all(|formal| formal.bounds.is_empty()) {
            return false;
        }
        let spans = arguments
            .iter()
            .zip(resolved.iter().copied().chain(std::iter::repeat(Ty::Error)))
            .map(|(argument, resolved)| {
                type_argument_span(argument).filter(|_| {
                    !self.upper_bound_violations.alias_declaration || is_alias_checked(resolved)
                })
            })
            .collect::<Vec<_>>();
        let owner = classifier_type_parameter_owner(&self.fed_source(), classifier, declaration);
        self.check_type_argument_bounds(&formals, &owner, resolved, &spans)
    }

    /// Report a constructor call whose explicit type arguments violate `classifier`'s bounds;
    /// `display` renders the rejected constructor.
    pub(super) fn report_explicit_constructor_type_argument_bounds(
        &mut self,
        call: crate::ast::ExprId,
        classifier: TypeName,
        has_value_arguments: bool,
        explicit: &[Ty],
        display: &str,
    ) -> bool {
        let Some(written) = self
            .file
            .call_type_args
            .get(&call.0)
            .map(|syntax| syntax.arguments.as_slice())
            .filter(|written| written.len() == explicit.len())
        else {
            return false;
        };
        let Some(declaration) = self.fed_source().classifier(classifier) else {
            return false;
        };
        if !self.check_classifier_argument_bounds(classifier, &declaration, written, explicit) {
            return false;
        }
        if has_value_arguments {
            self.report_bound_inapplicable_candidate(call, display);
        }
        true
    }

    fn report_bound_inapplicable_candidate(&mut self, call: crate::ast::ExprId, display: &str) {
        self.diags.error(
            self.call_callee_name_span(call),
            format!("inapplicable candidate(s): {display}"),
        );
    }

    /// Report each argument of `resolved` (parallel to `formals`, projections kept) that violates
    /// its formal's substituted bounds, at the parallel `spans`; a `None` span is not checked.
    /// `owner` words the formals' declaration.
    fn check_type_argument_bounds(
        &mut self,
        formals: &[BoundedFormal<'_>],
        owner: &str,
        resolved: &[Ty],
        spans: &[Option<Span>],
    ) -> bool {
        let mut violated = false;
        let bindings = formals
            .iter()
            .zip(resolved)
            .map(|(formal, argument)| (formal.identity.to_string(), *argument))
            .collect::<crate::symbol_resolver::GSigBinds>();
        for ((formal, argument), span) in formals.iter().zip(resolved).zip(spans) {
            let Some(span) = *span else { continue };
            let actual = match *argument {
                Ty::StarProjection(_) => continue,
                Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
                other => other,
            };
            if !is_determined(actual) {
                continue;
            }
            let bounds = formal
                .bounds
                .iter()
                .map(|bound| crate::types::ty_subst_applied_arguments(*bound, &bindings))
                .filter(|bound| *bound != Ty::nullable(Ty::obj_name(crate::types::wk::any())))
                .collect::<Vec<_>>();
            if bounds.is_empty() || !bounds.iter().copied().all(is_determined) {
                continue;
            }
            let context = crate::assignable::TyCtx::new();
            if bounds
                .iter()
                .all(|bound| crate::assignable::is_subtype(&context, self, actual, *bound))
            {
                continue;
            }
            let message =
                self.upper_bound_violation_message(formal.identity, owner, &bounds, actual);
            self.diags.error(span, message);
            violated = true;
        }
        violated
    }

    /// kotlinc's message, with same-named type parameters numbered across the bound, the actual
    /// argument, and the parameter, in that order. Several bounds are their intersection.
    fn upper_bound_violation_message(
        &self,
        parameter: &'static str,
        owner: &str,
        bounds: &[Ty],
        actual: Ty,
    ) -> String {
        let mut mentioned = Vec::new();
        for bound in bounds {
            mentioned_type_parameters(*bound, &mut mentioned);
        }
        mentioned_type_parameters(actual, &mut mentioned);
        mentioned.push(parameter);
        let numbering = TypeParameterNumbering::new(mentioned);
        let context = bounds
            .iter()
            .copied()
            .chain(std::iter::once(actual))
            .collect::<Vec<_>>();
        let bound = bounds
            .iter()
            .map(|bound| self.numbered_diagnostic_type_name(*bound, &context, &numbering))
            .collect::<Vec<_>>()
            .join(" & ");
        format!(
            "type argument is not within its bounds: type parameter '{} (of {owner})' must be \
             subtype of '{bound}', but actual: '{}'.",
            numbering.name(parameter),
            self.numbered_diagnostic_type_name(actual, &context, &numbering),
        )
    }
}

impl<'a> Checker<'a> {
    /// Check stable aliases owned by `declaration`. Pass 1's declaration identities and source
    /// order pair retained syntax with finalized headers; target spellings are never resolved again.
    pub(super) fn check_stable_nested_type_alias_bounds(
        &mut self,
        class: &crate::ast::ClassDecl,
        declaration: crate::fir::DeclarationId,
    ) {
        let Some(index) = self.resolved_index else {
            return;
        };
        let mut declarations = index
            .owned_declarations(declaration)
            .iter()
            .copied()
            .filter(|owned| index.type_alias_header(*owned).is_some())
            .collect::<Vec<_>>();
        declarations.sort_unstable_by_key(|owned| index.source_order(*owned).unwrap_or(u32::MAX));
        if declarations.len() != class.type_aliases.len() {
            return;
        }
        let aliases = class
            .type_aliases
            .iter()
            .cloned()
            .zip(declarations)
            .filter_map(|(syntax, declaration)| {
                index
                    .type_alias_header(declaration)
                    .cloned()
                    .map(|header| (syntax, header))
            })
            .collect::<Vec<_>>();
        for (syntax, header) in aliases {
            self.check_type_alias_declaration_bounds(
                &syntax.target,
                header.expansion.get(),
                &header.expansion_spelling,
            );
        }
    }

    /// Check the supertypes of `class`, which Pass 1 finalized in the header of `declaration`.
    pub(super) fn check_finalized_supertype_bounds(
        &mut self,
        class: &crate::ast::ClassDecl,
        declaration: crate::fir::DeclarationId,
    ) -> bool {
        let Some(header) = self
            .resolved_index
            .and_then(|index| index.classifier_header(declaration))
        else {
            return false;
        };
        if class.base_class.is_some() != class.base_class_span.is_some() {
            return false;
        }
        let spellings = self.finalized_declaration_spellings(Some(declaration));
        if let (Some(name), Some(span)) = (class.base_class.as_ref(), class.base_class_span) {
            if header.interfaces.len() != class.supertypes.len() {
                return false;
            }
            for (index, (reference, resolved)) in class
                .supertypes
                .iter()
                .zip(header.interfaces.iter())
                .enumerate()
            {
                self.record_finalized_declaration_type(
                    reference,
                    resolved.get(),
                    spellings.supertype(index),
                );
            }
            let Some(superclass) = header.superclass else {
                return false;
            };
            // A superclass constructor call keeps its written type apart from the call; only the
            // arguments and the reference's span take part in the check.
            let reference = TypeRef {
                name: name.clone(),
                flags: crate::ast::TrFlags::default(),
                arg: None,
                targs: class.base_type_args.clone(),
                span,
                fun_params: Box::new([]),
                fun_context_count: 0,
                qualifier_arguments: Default::default(),
            };
            self.record_finalized_declaration_type(
                &reference,
                superclass.get(),
                &spellings.superclass,
            );
        } else {
            let resolved = header.declared_supertypes().collect::<Vec<_>>();
            if resolved.len() != class.supertypes.len() {
                return false;
            }
            for (index, (reference, resolved)) in class.supertypes.iter().zip(resolved).enumerate()
            {
                self.record_finalized_declaration_type(
                    reference,
                    resolved.get(),
                    spellings.supertype(index),
                );
            }
        }
        true
    }

    /// Check the declared types of a member without a body (an abstract or interface function or
    /// property), which no body check visits: `receiver`, the value `parameters` (with whether each
    /// is a vararg), and the declared result `ret`, against the header Pass 1 finalized for
    /// `declaration`.
    pub(super) fn check_bodyless_member_bounds(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: Option<crate::fir::DeclarationId>,
        receiver: Option<&TypeRef>,
        parameters: &[(&TypeRef, bool)],
        ret: Option<&TypeRef>,
        getter_ret: Option<&TypeRef>,
    ) -> bool {
        let Some((declaration, index)) = declaration.zip(self.resolved_index) else {
            return false;
        };
        let spellings = self.finalized_declaration_spellings_for_binding(Some(declaration));
        let finalized_receiver = index
            .callable_for_declaration(declaration)
            .and_then(|callable| callable.shape.extension_receiver)
            .or_else(|| {
                index
                    .property_for_declaration(declaration)
                    .and_then(|property| index.property(property))
                    .and_then(|property| property.extension_receiver)
            });
        if receiver.is_some() != finalized_receiver.is_some() {
            return false;
        }
        if let (Some(reference), Some(finalized)) = (receiver, finalized_receiver) {
            self.record_or_bind_deferred_declaration_type(
                scope,
                declaration,
                reference,
                finalized.get(),
                spellings
                    .as_ref()
                    .and_then(|spellings| spellings.receiver()),
            );
        }
        let Some(signature) = index.signature(declaration) else {
            return false;
        };
        if signature.parameters.len() != parameters.len() {
            return false;
        }
        for (position, ((reference, is_vararg), finalized)) in parameters
            .iter()
            .zip(signature.parameters.iter())
            .enumerate()
        {
            self.record_or_bind_deferred_parameter_type(
                scope,
                declaration,
                reference,
                *is_vararg,
                finalized.get(),
                spellings
                    .as_ref()
                    .and_then(|spellings| spellings.param(position)),
            );
        }
        if let Some(reference) = ret {
            self.record_or_bind_deferred_declaration_type(
                scope,
                declaration,
                reference,
                signature.result.get(),
                spellings.as_ref().and_then(|spellings| spellings.ret()),
            );
        }
        if let Some(reference) = getter_ret {
            self.record_or_bind_deferred_declaration_type(
                scope,
                declaration,
                reference,
                signature.result.get(),
                spellings
                    .as_ref()
                    .and_then(|spellings| spellings.getter_ret()),
            );
        }
        true
    }

    /// [`Self::check_bodyless_member_bounds`] for a method of the class rung `scope`, when it has
    /// no body, under its own type parameters as [`Self::check_method`] binds them.
    pub(super) fn check_bodyless_method_bounds(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &crate::ast::FunDecl,
        declaration: Option<crate::fir::DeclarationId>,
    ) {
        if !matches!(function.body, crate::ast::FunBody::None) {
            return;
        }
        let method_scope = scope.child(ScopeKind::Function { receiver: None });
        let finalized_local_header =
            declaration
                .zip(self.resolved_index)
                .is_some_and(|(declaration, index)| {
                    index.declaration_header(declaration).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    }) && !self.has_deferred_local_header(Some(declaration))
                });
        let deferred_local_header = self.has_deferred_local_header(declaration);
        let visible = method_scope.visible_tparams();
        let parameters = if finalized_local_header {
            declaration
                .and_then(|declaration| {
                    self.finalized_type_parameters(&visible, declaration, &function.type_params)
                })
                .unwrap_or_else(|| {
                    if let Some(declaration) = declaration {
                        self.failed_local_declaration_bindings.insert(declaration);
                    }
                    visible.clone()
                })
        } else if deferred_local_header {
            self.bind_deferred_type_parameters(
                &method_scope,
                &function.type_params,
                &function.type_param_bounds,
                function.signature_span.lo,
                true,
            )
        } else {
            visible
                .symbolic_extended_with(
                    &function.type_params,
                    &function.type_param_bounds,
                    &|name| self.select_classifier(&method_scope, name).found(),
                )
                .alpha_renamed_declaration(
                    &function.type_params,
                    self.compilation_id,
                    self.file_index,
                    function.signature_span.lo,
                )
        };
        method_scope.declare_tparams(&function.type_params, &parameters, |name| {
            function.reified_type_params.contains(name)
        });
        let owned = self.publish_function_type_parameter_owner(&method_scope, function);
        if finalized_local_header {
            if let Some(declaration) = declaration {
                if !self.record_finalized_type_parameter_bounds(
                    &method_scope,
                    declaration,
                    &function.type_params,
                    &function.type_param_bounds,
                ) {
                    self.failed_local_declaration_bindings.insert(declaration);
                }
            }
        } else if !deferred_local_header {
            self.check_callable_type_parameter_bounds(&method_scope, &function.type_param_bounds);
        }
        if deferred_local_header {
            if let Some(receiver) = function.receiver.as_ref() {
                self.check_declaration_type(&method_scope, receiver);
            }
            for parameter in &function.params {
                self.check_declaration_type(&method_scope, &parameter.ty);
            }
            if let Some(result) = function.ret.as_ref() {
                self.check_declaration_type(&method_scope, result);
            }
            self.restore_type_parameter_owners(owned);
            return;
        }
        let value_parameters = function
            .params
            .iter()
            .map(|parameter| (&parameter.ty, parameter.is_vararg))
            .collect::<Vec<_>>();
        if !self.check_bodyless_member_bounds(
            &method_scope,
            declaration,
            function.receiver.as_ref(),
            &value_parameters,
            function.ret.as_ref(),
            None,
        ) {
            if let Some(declaration) = declaration {
                self.failed_local_declaration_bindings.insert(declaration);
            }
        }
        self.restore_type_parameter_owners(owned);
    }

    /// [`Self::check_bodyless_member_bounds`] for a property of the class rung `scope`, when it
    /// has neither an initializer, a delegate, a backing field, nor an accessor body.
    pub(super) fn check_bodyless_property_bounds(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &crate::ast::PropDecl,
        declaration: Option<crate::fir::DeclarationId>,
    ) {
        let bodyless = property.init.is_none()
            && property.delegate.is_none()
            && property.explicit_backing_field.is_none()
            && property.getter.is_none()
            && property
                .setter
                .as_ref()
                .is_none_or(|setter| setter.body.is_none());
        if !bodyless {
            return;
        }
        let property_scope = scope.child(ScopeKind::Function { receiver: None });
        let finalized_local_header =
            declaration
                .zip(self.resolved_index)
                .is_some_and(|(declaration, index)| {
                    index.declaration_header(declaration).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    }) && !self.has_deferred_local_header(Some(declaration))
                });
        let deferred_local_header = self.has_deferred_local_header(declaration);
        let visible = property_scope.visible_tparams();
        let parameters = if finalized_local_header {
            declaration
                .and_then(|declaration| {
                    self.finalized_type_parameters(&visible, declaration, &property.type_params)
                })
                .unwrap_or_else(|| {
                    if let Some(declaration) = declaration {
                        self.failed_local_declaration_bindings.insert(declaration);
                    }
                    visible.clone()
                })
        } else if deferred_local_header {
            self.bind_deferred_type_parameters(
                &property_scope,
                &property.type_params,
                &property.type_param_bounds,
                property.span.lo,
                true,
            )
        } else {
            visible
                .symbolic_extended_with(
                    &property.type_params,
                    &property.type_param_bounds,
                    &|name| self.select_classifier(&property_scope, name).found(),
                )
                .alpha_renamed_declaration(
                    &property.type_params,
                    self.compilation_id,
                    self.file_index,
                    property.span.lo,
                )
        };
        property_scope.declare_property_tparams(property, &parameters);
        let owned = self.publish_property_type_parameter_owner(&property_scope, property);
        if finalized_local_header {
            if let Some(declaration) = declaration {
                if !self.record_finalized_type_parameter_bounds(
                    &property_scope,
                    declaration,
                    &property.type_params,
                    &property.type_param_bounds,
                ) {
                    self.failed_local_declaration_bindings.insert(declaration);
                }
            }
        } else if !deferred_local_header {
            self.check_callable_type_parameter_bounds(&property_scope, &property.type_param_bounds);
        }
        if deferred_local_header {
            if let Some(receiver) = property.receiver.as_ref() {
                self.check_declaration_type(&property_scope, receiver);
            }
            for parameter in &property.context_params {
                self.check_declaration_type(&property_scope, &parameter.ty);
            }
            let property_type = property
                .ty
                .as_ref()
                .map(|declared| self.check_declaration_type(&property_scope, declared));
            let getter_type = property
                .getter_ty
                .as_ref()
                .map(|declared| self.check_declaration_type(&property_scope, declared));
            if let (Some(property_type), Some(getter_type)) = (property_type, getter_type) {
                if property_type != Ty::Error
                    && getter_type != Ty::Error
                    && property_type != getter_type
                {
                    self.diags.error(
                        property
                            .getter_ty
                            .as_ref()
                            .expect("getter annotation exists")
                            .span,
                        format!(
                            "getter return type '{}' must be equal to the property type '{}'",
                            getter_type.source_name(),
                            property_type.source_name(),
                        ),
                    );
                }
            }
            self.restore_type_parameter_owners(owned);
            return;
        }
        let context_parameters = property
            .context_params
            .iter()
            .map(|parameter| (&parameter.ty, parameter.is_vararg))
            .collect::<Vec<_>>();
        if !self.check_bodyless_member_bounds(
            &property_scope,
            declaration,
            property.receiver.as_ref(),
            &context_parameters,
            property.ty.as_ref(),
            property.getter_ty.as_ref(),
        ) {
            if let Some(declaration) = declaration {
                self.failed_local_declaration_bindings.insert(declaration);
            }
        }
        self.restore_type_parameter_owners(owned);
    }

    /// The alias spellings Pass 1 recorded for the declared types of `declaration`; none for a
    /// declaration without a finalized header or whose types spell no alias.
    pub(super) fn finalized_declaration_spellings(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
    ) -> &'a crate::spelling::DeclaredSpellings {
        declaration
            .zip(self.resolved_index)
            .and_then(|(declaration, index)| index.declaration_spellings(declaration))
            .unwrap_or(crate::spelling::DeclaredSpellings::NONE)
    }
}

/// One formal of the declaration a type argument list applies to.
struct BoundedFormal<'a> {
    /// The formal's semantic identity.
    identity: &'static str,
    /// Its declared upper bounds, over the declaration's own formals.
    bounds: &'a [Ty],
}

/// Where kotlinc reports a violating argument: the written type, or the projection keyword that
/// starts a projected one. A star projection is never checked.
fn type_argument_span(argument: &TypeRef) -> Option<Span> {
    if argument.is_star_projection() {
        None
    } else if argument.in_projection() {
        Some(Span::new(
            argument.span.lo.saturating_sub(3),
            argument.span.hi,
        ))
    } else if argument.out_projection() {
        Some(Span::new(
            argument.span.lo.saturating_sub(4),
            argument.span.hi,
        ))
    } else {
        Some(argument.span)
    }
}

/// A type the check can decide: resolved and no longer pending inference.
fn is_determined(ty: Ty) -> bool {
    !ty.contains_error() && !ty.mentions_pending()
}

/// The own formals of a classifier declaration with their declared bounds.
fn bounded_formals(declaration: &LibraryType) -> Vec<BoundedFormal<'_>> {
    let own = declaration.own_type_parameter_count;
    assert!(
        own <= declaration.type_params().len() && own <= declaration.type_param_bounds().len(),
        "classifier-owned formals and bounds must contain the declared own prefix"
    );
    declaration.type_params()[..own]
        .iter()
        .zip(&declaration.type_param_bounds()[..own])
        .map(|(identity, bounds)| BoundedFormal {
            identity: crate::types::intern(identity),
            bounds,
        })
        .collect()
}

/// Whether a typealias declaration's right-hand side checks an argument that resolved to
/// `argument`: not a type parameter and without type arguments of its own.
fn is_alias_checked(argument: Ty) -> bool {
    let argument = match argument {
        Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
        other => other,
    }
    .non_null();
    !argument.is_ty_param() && argument.type_args().is_empty()
}

/// One application of a typealias, as its expansion's bound check reads it.
struct AliasApplication<'a> {
    formals: &'a [String],
    bindings: &'a crate::symbol_resolver::GSigBinds,
    arguments: &'a [TypeRef],
    reference: Span,
}

impl AliasApplication<'_> {
    /// The written use-site argument when the template argument `template` is exactly a formal.
    fn formal_argument(&self, template: Ty) -> Option<&TypeRef> {
        let Ty::TyParam(parameter, _) = template else {
            return None;
        };
        let index = self.formals.iter().position(|formal| formal == parameter)?;
        self.arguments.get(index)
    }

    /// Whether `template` mentions one of the alias's formals.
    fn mentions_formal(&self, template: Ty) -> bool {
        let mut mentioned = Vec::new();
        mentioned_type_parameters(template, &mut mentioned);
        mentioned
            .iter()
            .any(|parameter| self.formals.iter().any(|formal| formal == parameter))
    }
}
