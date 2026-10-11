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
//! kotlinc does not check the outermost application of a type operator's target (`a as B<String>`)
//! or of a callable's type-parameter bound (`fun <T : B<String>> f()`); the arguments nested inside
//! them are checked.

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

impl Checker<'_> {
    /// Check the written arguments of `reference`, which the checker resolved to the classifier
    /// application `resolved`. Arguments nested in `reference` are applications of their own.
    pub(super) fn check_type_reference_bounds(&mut self, reference: &TypeRef, resolved: Ty) {
        if self.unchecked_bounds_reference == Some(reference.span) {
            return;
        }
        let Ty::Obj(classifier, resolved) = resolved.non_null() else {
            return;
        };
        let Some(declaration) = self.fed_source().classifier(classifier) else {
            return;
        };
        self.check_classifier_argument_bounds(classifier, &declaration, &reference.targs, resolved);
    }

    /// [`Self::check_type_parameter_bound`] for a callable's type parameter, whose outermost
    /// application kotlinc does not check.
    pub(super) fn check_callable_type_parameter_bound(
        &mut self,
        scope: &CheckerScope<'_>,
        bound: &TypeRef,
    ) -> Ty {
        self.with_unchecked_outermost_bounds(bound, |checker| {
            checker.check_type_parameter_bound(scope, bound)
        })
    }

    /// Run `resolve` with the outermost application of `reference` exempt from the bound check.
    pub(super) fn with_unchecked_outermost_bounds<R>(
        &mut self,
        reference: &TypeRef,
        resolve: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous = self.unchecked_bounds_reference.replace(reference.span);
        let result = resolve(self);
        self.unchecked_bounds_reference = previous;
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
        if spelled.alias.is_some() {
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
                arguments,
            );
        }
        for (index, (argument, resolved)) in reference.targs.iter().zip(arguments).enumerate() {
            self.check_finalized_type_bounds(argument, *resolved, spelled.arg(index));
        }
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
        let spans = arguments.iter().map(type_argument_span).collect::<Vec<_>>();
        let owner = classifier_type_parameter_owner(&self.fed_source(), classifier, declaration);
        self.check_type_argument_bounds(&formals, &owner, resolved, &spans)
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
    /// Check the supertypes of `class`, which Pass 1 finalized in the header of `declaration`.
    pub(super) fn check_finalized_supertype_bounds(
        &mut self,
        class: &crate::ast::ClassDecl,
        declaration: crate::fir::DeclarationId,
    ) {
        let Some(header) = self
            .resolved_index
            .and_then(|index| index.classifier_header(declaration))
        else {
            return;
        };
        let spellings = self.finalized_declaration_spellings(Some(declaration));
        if header.interfaces.len() == class.supertypes.len() {
            for (index, (reference, resolved)) in class
                .supertypes
                .iter()
                .zip(header.interfaces.iter())
                .enumerate()
            {
                self.check_finalized_type_bounds(
                    reference,
                    resolved.get(),
                    spellings.supertype(index),
                );
            }
        }
        if let (Some(name), Some(span), Some(superclass)) = (
            class.base_class.as_ref(),
            class.base_class_span,
            header.superclass,
        ) {
            // A superclass constructor call keeps its written type apart from the call; only the
            // arguments and the reference's span take part in the check.
            let reference = TypeRef {
                name: name.clone(),
                flags: crate::ast::TrFlags::default(),
                arg: None,
                targs: class.base_type_args.clone(),
                span,
                fun_params: Vec::new(),
                fun_context_count: 0,
            };
            self.check_finalized_type_bounds(&reference, superclass.get(), &spellings.superclass);
        }
    }

    /// Check the declared types of a member without a body (an abstract or interface function or
    /// property), which no body check visits: `receiver`, the value `parameters` (with whether each
    /// is a vararg), and the declared result `ret`, against the header Pass 1 finalized for
    /// `declaration`.
    pub(super) fn check_bodyless_member_bounds(
        &mut self,
        declaration: Option<crate::fir::DeclarationId>,
        receiver: Option<&TypeRef>,
        parameters: &[(&TypeRef, bool)],
        ret: Option<&TypeRef>,
    ) {
        let Some((declaration, index)) = declaration.zip(self.resolved_index) else {
            return;
        };
        let spellings = self.finalized_declaration_spellings(Some(declaration));
        if let Some(reference) = receiver {
            let finalized = index
                .callable_for_declaration(declaration)
                .and_then(|callable| callable.shape.extension_receiver)
                .or_else(|| {
                    index
                        .property_for_declaration(declaration)
                        .and_then(|property| index.property(property))
                        .and_then(|property| property.extension_receiver)
                });
            if let Some(finalized) = finalized {
                self.check_finalized_type_bounds(reference, finalized.get(), &spellings.receiver);
            }
        }
        let Some(signature) = index.signature(declaration) else {
            return;
        };
        if signature.parameters.len() == parameters.len() {
            for (position, ((reference, is_vararg), finalized)) in parameters
                .iter()
                .zip(signature.parameters.iter())
                .enumerate()
            {
                let finalized = finalized.get();
                let declared = if *is_vararg {
                    finalized.non_null().array_read_elem().unwrap_or(finalized)
                } else {
                    finalized
                };
                self.check_finalized_type_bounds(reference, declared, spellings.param(position));
            }
        }
        if let Some(reference) = ret {
            self.check_finalized_type_bounds(reference, signature.result.get(), &spellings.ret);
        }
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
        let parameters = method_scope
            .visible_tparams()
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
            );
        method_scope.declare_tparams(&function.type_params, &parameters, |name| {
            function.reified_type_params.contains(name)
        });
        let owned = self.publish_function_type_parameter_owner(&method_scope, function);
        for (_, bound) in &function.type_param_bounds {
            self.check_callable_type_parameter_bound(&method_scope, bound);
        }
        let value_parameters = function
            .params
            .iter()
            .map(|parameter| (&parameter.ty, parameter.is_vararg))
            .collect::<Vec<_>>();
        self.check_bodyless_member_bounds(
            declaration,
            function.receiver.as_ref(),
            &value_parameters,
            function.ret.as_ref(),
        );
        self.retire_type_parameter_owners(&owned);
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
        let parameters = property_scope
            .visible_tparams()
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
            );
        property_scope.declare_property_tparams(property, &parameters);
        let owned = self.publish_property_type_parameter_owner(&property_scope, property);
        for (_, bound) in &property.type_param_bounds {
            self.check_callable_type_parameter_bound(&property_scope, bound);
        }
        self.check_bodyless_member_bounds(
            declaration,
            property.receiver.as_ref(),
            &[],
            property.declared_ty(),
        );
        self.retire_type_parameter_owners(&owned);
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
