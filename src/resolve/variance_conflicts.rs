//! kotlinc's `FirClassVarianceChecker`: a classifier type parameter declared `in` or `out` may occur
//! only in positions its variance allows, `TYPE_VARIANCE_CONFLICT_ERROR` and, for a direct argument
//! of a type written through a `typealias`, `TYPE_VARIANCE_CONFLICT_IN_EXPANDED_TYPE`.
//!
//! The checker runs for every classifier, local, inner and anonymous ones included. It walks:
//! - the classifier's own type-parameter bounds and its supertypes, in `out` position;
//! - each non-private member function and property: its own type-parameter bounds, context
//!   parameters, extension receiver and (functions only) value parameters in `in` position, and its
//!   result in `out` position, `invariant` for a `var`. An omitted result type is the declaration's
//!   inferred type, reported at the declaration's start. Constructors are not walked: a classifier's
//!   own type is always consistent with its variance, and their value parameters are not checked.
//!
//! Nested classifiers are checked as classifiers of their own; an inner or local classifier's
//! members see the declared variance of the enclosing classifier formals they capture.
//!
//! Inside a type, an argument's position combines the classifier formal's declared variance with
//! the use-site projection (kotlinc's `EnrichedProjectionKind`): `out` keeps the position, `in`
//! flips it, an invariant combination is `invariant`, and a star or conflicting projection is not
//! walked. A function type is `FunctionN<in P…, out R>` over its context parameters, receiver,
//! value parameters and result. A type parameter occurrence marked `@UnsafeVariance` is not
//! reported. A definitely-non-null type and a flexible (platform) type are walked only with
//! `ReportTypeVarianceConflictsInDnnAndFlexible`, the flexible type through its lower bound.
//!
//! A conflict is reported at the source type argument found BY INDEX among the written type
//! arguments, as kotlinc's `extractArgumentsTypeRefAndSource` does: a typealias reference whose
//! expansion reorders or adds arguments reports at the nearest written reference that has one.

use std::collections::HashMap;
use std::rc::Rc;

use super::*;
use crate::types::TypeVariance;

/// A position a type occurs in, kotlinc's `Variance` read as a position.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    In,
    Out,
    Invariant,
}

impl Position {
    fn opposite(self) -> Self {
        match self {
            Position::In => Position::Out,
            Position::Out => Position::In,
            Position::Invariant => Position::Invariant,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Position::In => "in",
            Position::Out => "out",
            Position::Invariant => "invariant",
        }
    }

    /// kotlinc's `Variance.allowsPosition`: an `in` formal fits only `in` positions, an `out`
    /// formal only `out` positions, and an invariant one fits everywhere.
    fn allows(self, declared: TypeVariance) -> bool {
        match declared {
            TypeVariance::Invariant => true,
            TypeVariance::In => self == Position::In,
            TypeVariance::Out => self == Position::Out,
        }
    }

    /// The position of a type argument whose formal is declared `parameter` and whose use-site
    /// projection is `argument`, or `None` for a conflicting projection, which kotlinc's
    /// `EnrichedProjectionKind` reads as a star.
    fn of_argument(self, parameter: TypeVariance, argument: TypeVariance) -> Option<Self> {
        let effective = match (parameter, argument) {
            (TypeVariance::Invariant, argument) => argument,
            (parameter, TypeVariance::Invariant) => parameter,
            (parameter, argument) if parameter == argument => argument,
            _ => return None,
        };
        Some(match effective {
            TypeVariance::Out => self,
            TypeVariance::In => self.opposite(),
            TypeVariance::Invariant => Position::Invariant,
        })
    }
}

fn declared_word(variance: TypeVariance) -> &'static str {
    match variance {
        TypeVariance::In => "in",
        TypeVariance::Out => "out",
        TypeVariance::Invariant => "invariant",
    }
}

/// The written arguments of a type reference, in the order of the type's semantic arguments: a
/// classifier's type arguments, or a function type's context parameters, receiver, value parameters
/// and result.
fn written_arguments(reference: &TypeRef) -> Vec<&TypeRef> {
    if function_type_ref_shape(reference).is_some() {
        reference
            .fun_params
            .iter()
            .chain(reference.arg.as_deref())
            .collect()
    } else {
        reference.targs.iter().collect()
    }
}

/// One typealias application: the alias's formals and the arguments the use site wrote for them.
struct AliasUse<'a> {
    formals: Vec<String>,
    arguments: &'a [TypeRef],
    expansion_spelling: crate::spelling::Spelled,
}

/// Where one level of a walked type came from, which decides whether it is a typealias expansion
/// (kotlinc's abbreviation attribute) and whether it carries `@UnsafeVariance`.
#[derive(Clone)]
enum Origin<'a> {
    /// No source syntax describes this level: an inferred type.
    Inferred,
    /// The level the source wrote.
    Written(&'a TypeRef),
    /// A level of an alias's right-hand side, in terms of the alias's formals.
    Expansion {
        template: Ty,
        spelling: crate::spelling::Spelled,
        alias: Rc<AliasUse<'a>>,
    },
    /// The `Array<out E>` a `vararg` parameter of element type `E` declares.
    Vararg(&'a TypeRef),
}

/// The facts the walk reads about one level of a type.
struct Level<'a> {
    /// The level is a typealias expansion: its direct arguments report the abbreviated message.
    expansion: bool,
    unsafe_variance: bool,
    /// Origins of the level's semantic arguments, positionally.
    arguments: Vec<Origin<'a>>,
}

impl Level<'_> {
    fn inferred() -> Self {
        Level {
            expansion: false,
            unsafe_variance: false,
            arguments: Vec::new(),
        }
    }
}

/// One level of a walked type: the position it occurs in, the written reference paired with it by
/// index (when there is one), the span a conflict at this level reports at, whether the enclosing
/// level was a typealias expansion, and where the level came from.
struct Site<'r, 'a> {
    position: Position,
    written: Option<&'r TypeRef>,
    span: Span,
    abbreviated: bool,
    origin: &'r Origin<'a>,
}

/// One conflicting occurrence, rendered once its containing type is known.
struct Conflict {
    span: Span,
    parameter: &'static str,
    declared: TypeVariance,
    position: Position,
    abbreviated: bool,
}

/// The walk's lexical inputs: the scope source references resolve in, the declared variance of
/// each classifier formal in scope, and the language feature.
struct Walk<'s, 'a> {
    scope: &'s CheckerScope<'a>,
    variances: &'s HashMap<String, TypeVariance>,
    dnn_and_flexible: bool,
}

/// One position a member declares a type in.
struct Occurrence<'r> {
    reference: Option<&'r TypeRef>,
    ty: Ty,
    position: Position,
    /// Where an inferred type reports: the declaration's start.
    span: Span,
    vararg: bool,
}

impl Checker<'_> {
    /// Run kotlinc's class variance checker over `cl`, whose classifier is `owner`, in its class
    /// scope.
    pub(super) fn check_class_variance(
        &mut self,
        scope: &CheckerScope<'_>,
        cl: &ClassDecl,
        d: DeclId,
        owner: Option<TypeName>,
    ) {
        if self.discover_anonymous_captures || self.signature_defaults_only {
            return;
        }
        if let Some(owner) = owner {
            self.publish_enclosing_type_parameter_owners(d, owner);
        }
        let variances = self.lexical_formal_variances(owner);
        if variances
            .values()
            .all(|variance| *variance == TypeVariance::Invariant)
        {
            return;
        }
        let walk = Walk {
            scope,
            variances: &variances,
            dnn_and_flexible: self.file.report_type_variance_conflicts_in_dnn_and_flexible,
        };
        let mut occurrences = Vec::new();
        for (_, bound) in cl.type_param_bounds() {
            occurrences.push(self.bound_occurrence(bound, Position::Out));
        }
        let base = cl.base_class.as_deref().map(|base| {
            base_class_type_ref(
                base,
                &cl.base_type_args,
                cl.base_class_span.unwrap_or(cl.span),
            )
        });
        for supertype in base.iter().chain(&cl.supertypes) {
            occurrences.push(self.supertype_occurrence(supertype, Position::Out));
        }
        self.report_variance_conflicts(&walk, occurrences);
        for property in cl.props.iter().filter(|property| property.is_property) {
            if property.visibility == Visibility::Private {
                continue;
            }
            let policy_depth = self.push_declaration_policies(
                scope,
                &property.annotations,
                &property.annotation_args,
            );
            let position = if property.is_var {
                Position::Invariant
            } else {
                Position::Out
            };
            let occurrence = self.parameter_occurrence(&property.ty, property.is_vararg, position);
            self.report_variance_conflicts(&walk, vec![occurrence]);
            self.active_lexical_policies.truncate(policy_depth);
        }
        for (index, method) in cl.methods.iter().enumerate() {
            if method.visibility != Visibility::Private {
                self.check_method_variance(&walk, d, owner, index, method);
            }
        }
        for (index, property) in cl.body_props.iter().enumerate() {
            if property.visibility != Visibility::Private {
                self.check_property_variance(&walk, d, cl.props.len(), index, property);
            }
        }
    }

    /// The declared variance of every classifier formal visible in the class being checked: its
    /// own and captured formals and those of every lexically enclosing classifier, keyed by the
    /// formal's semantic identity. A function's formals are invariant and never listed.
    fn lexical_formal_variances(&self, owner: Option<TypeName>) -> HashMap<String, TypeVariance> {
        let mut variances = HashMap::new();
        let source = self.fed_source();
        for classifier in owner.iter().chain(&self.lexical_class_context) {
            let Some(record) = source.classifier(*classifier) else {
                continue;
            };
            let parameters = &record.type_parameters;
            for (name, variance) in parameters
                .type_params()
                .iter()
                .zip(parameters.type_param_variances())
            {
                variances.entry(name.clone()).or_insert(*variance);
            }
        }
        variances
    }

    fn check_method_variance(
        &mut self,
        walk: &Walk<'_, '_>,
        d: DeclId,
        owner: Option<TypeName>,
        index: usize,
        method: &FunDecl,
    ) {
        let method_scope = walk.scope.child(ScopeKind::Function { receiver: None });
        let scope = &method_scope;
        let parameters = scope
            .visible_tparams()
            .symbolic_extended_with(&method.type_params, &method.type_param_bounds, &|name| {
                self.select_classifier(scope, name).found()
            })
            .alpha_renamed_declaration(
                &method.type_params,
                self.compilation_id,
                self.file_index,
                method.signature_span.lo,
            );
        scope.declare_tparams(&method.type_params, &parameters, |name| {
            method.reified_type_params.contains(name)
        });
        let owned = self.publish_function_type_parameter_owner(scope, method);
        let mut occurrences = Vec::new();
        for (_, bound) in &method.type_param_bounds {
            occurrences.push(self.bound_occurrence(bound, Position::In));
        }
        for parameter in &method.params {
            occurrences.push(self.parameter_occurrence(
                &parameter.ty,
                parameter.is_vararg,
                Position::In,
            ));
        }
        if let Some(receiver) = &method.receiver {
            occurrences.push(self.declared_occurrence(receiver, Position::In));
        }
        match &method.ret {
            Some(result) => occurrences.push(self.declared_occurrence(result, Position::Out)),
            None if matches!(method.body, FunBody::Expr(_)) => {
                let declaration = self.member_declaration(d, owner, index);
                let source_member = crate::libraries::SourceMember::Class {
                    file: self.file_index,
                    owner: d.0,
                    method: u32::try_from(index).expect("too many class methods"),
                };
                if let Some(ty) = self.declared_member_result(declaration, source_member) {
                    occurrences.push(Occurrence {
                        reference: None,
                        ty,
                        position: Position::Out,
                        span: method.declaration_span,
                        vararg: false,
                    });
                }
            }
            None => {}
        }
        let walk = Walk {
            scope,
            variances: walk.variances,
            dnn_and_flexible: walk.dnn_and_flexible,
        };
        let policy_depth =
            self.push_declaration_policies(scope, &method.annotations, &method.annotation_args);
        self.report_variance_conflicts(&walk, occurrences);
        self.restore_type_parameter_owners(owned);
        self.active_lexical_policies.truncate(policy_depth);
    }

    fn check_property_variance(
        &mut self,
        walk: &Walk<'_, '_>,
        d: DeclId,
        primary_parameter_count: usize,
        index: usize,
        property: &PropDecl,
    ) {
        let property_scope = walk.scope.child(ScopeKind::Function { receiver: None });
        let scope = &property_scope;
        let parameters = scope
            .visible_tparams()
            .symbolic_extended_with(
                &property.type_params,
                &property.type_param_bounds,
                &|name| self.select_classifier(scope, name).found(),
            )
            .alpha_renamed_declaration(
                &property.type_params,
                self.compilation_id,
                self.file_index,
                property.span.lo,
            );
        scope.declare_property_tparams(property, &parameters);
        let owned = self.publish_property_type_parameter_owner(scope, property);
        let mut occurrences = Vec::new();
        for (_, bound) in &property.type_param_bounds {
            occurrences.push(self.bound_occurrence(bound, Position::In));
        }
        for parameter in &property.context_params {
            occurrences.push(self.declared_occurrence(&parameter.ty, Position::In));
        }
        let position = if property.is_var {
            Position::Invariant
        } else {
            Position::Out
        };
        match property.declared_ty() {
            Some(declared) => occurrences.push(self.declared_occurrence(declared, position)),
            None => {
                let declaration = self.active_declarations.and_then(|active| {
                    active.class_body_property_declaration(
                        d,
                        u32::try_from(index).expect("too many class properties"),
                    )
                });
                let source_member = crate::libraries::SourceMember::ClassProperty {
                    file: self.file_index,
                    owner: d.0,
                    property: u32::try_from(primary_parameter_count + index)
                        .expect("too many class properties"),
                };
                let ty = self.declared_member_result(declaration, source_member);
                if let Some(ty) = ty {
                    occurrences.push(Occurrence {
                        reference: None,
                        ty,
                        position,
                        span: property.declaration_span,
                        vararg: false,
                    });
                }
            }
        }
        if let Some(receiver) = &property.receiver {
            occurrences.push(self.declared_occurrence(receiver, Position::In));
        }
        let walk = Walk {
            scope,
            variances: walk.variances,
            dnn_and_flexible: walk.dnn_and_flexible,
        };
        let policy_depth =
            self.push_declaration_policies(scope, &property.annotations, &property.annotation_args);
        self.report_variance_conflicts(&walk, occurrences);
        self.restore_type_parameter_owners(owned);
        self.active_lexical_policies.truncate(policy_depth);
    }

    /// The result a member published, or the one this body unit inferred for a member of a local
    /// classifier, which has no finalized signature.
    fn declared_member_result(
        &self,
        declaration: Option<crate::fir::DeclarationId>,
        source_member: crate::libraries::SourceMember,
    ) -> Option<Ty> {
        declaration
            .and_then(|declaration| self.resolved_index?.signature(declaration))
            .map(|signature| signature.result.get())
            .or_else(|| self.checked_source_member_result(Some(source_member), declaration))
    }

    fn declared_occurrence<'r>(
        &self,
        reference: &'r TypeRef,
        position: Position,
    ) -> Occurrence<'r> {
        Occurrence {
            reference: Some(reference),
            ty: self
                .resolved_declaration_types
                .get(&(reference.span.lo, reference.span.hi))
                .or_else(|| {
                    self.resolved_type_tys
                        .get(&(reference.span.lo, reference.span.hi))
                })
                .copied()
                .unwrap_or(Ty::Error),
            position,
            span: reference.span,
            vararg: false,
        }
    }

    fn bound_occurrence<'r>(&self, reference: &'r TypeRef, position: Position) -> Occurrence<'r> {
        Occurrence {
            reference: Some(reference),
            ty: self
                .resolved_type_bounds
                .get(&(reference.span.lo, reference.span.hi))
                .map_or(Ty::Error, |(ty, _)| *ty),
            position,
            span: reference.span,
            vararg: false,
        }
    }

    fn supertype_occurrence<'r>(
        &self,
        reference: &'r TypeRef,
        position: Position,
    ) -> Occurrence<'r> {
        Occurrence {
            reference: Some(reference),
            ty: self
                .resolved_type_tys
                .get(&(reference.span.lo, reference.span.hi))
                .copied()
                .unwrap_or(Ty::Error),
            position,
            span: reference.span,
            vararg: false,
        }
    }

    /// A value parameter's type: a `vararg` parameter declares `Array<out E>` over its written
    /// element type `E` (a primitive element declares its primitive array, which has no formal).
    fn parameter_occurrence<'r>(
        &self,
        reference: &'r TypeRef,
        vararg: bool,
        position: Position,
    ) -> Occurrence<'r> {
        let element = self
            .resolved_declaration_types
            .get(&(reference.span.lo, reference.span.hi))
            .or_else(|| {
                self.resolved_type_tys
                    .get(&(reference.span.lo, reference.span.hi))
            })
            .copied()
            .unwrap_or(Ty::Error);
        if !vararg
            || !matches!(
                element.non_null(),
                Ty::Obj(..) | Ty::TyParam(..) | Ty::Fun(_)
            )
        {
            return Occurrence {
                vararg: false,
                ..self.declared_occurrence(reference, position)
            };
        }
        Occurrence {
            reference: Some(reference),
            ty: Ty::obj_args_name(crate::types::wk::array(), &[Ty::out_projection(element)]),
            position,
            span: reference.span,
            vararg: true,
        }
    }

    fn report_variance_conflicts(&mut self, walk: &Walk<'_, '_>, occurrences: Vec<Occurrence<'_>>) {
        for occurrence in occurrences {
            if occurrence.ty.mentions_error() || occurrence.ty.mentions_pending() {
                continue;
            }
            let (written, origin) = match occurrence.reference {
                Some(element) if occurrence.vararg => (None, Origin::Vararg(element)),
                Some(reference) => (Some(reference), Origin::Written(reference)),
                None => (None, Origin::Inferred),
            };
            let mut conflicts = Vec::new();
            let site = Site {
                position: occurrence.position,
                written,
                span: occurrence.span,
                abbreviated: false,
                origin: &origin,
            };
            self.walk_variance(walk, occurrence.ty, &site, &mut conflicts);
            if conflicts.is_empty() {
                continue;
            }
            let containing =
                self.diagnostic_type_name(self.displayed_type(walk, occurrence.ty, &origin), &[]);
            for conflict in conflicts {
                let diagnostic = if conflict.abbreviated {
                    "TYPE_VARIANCE_CONFLICT_IN_EXPANDED_TYPE"
                } else {
                    "TYPE_VARIANCE_CONFLICT_ERROR"
                };
                if self.suppresses_diagnostic(diagnostic) {
                    continue;
                }
                let kind = if conflict.abbreviated {
                    "abbreviated type"
                } else {
                    "type"
                };
                self.diags.error(
                    conflict.span,
                    format!(
                        "type parameter '{}' is declared as '{}' but occurs in '{}' position in \
                         {kind} '{containing}'.",
                        crate::types::type_parameter_source_name(conflict.parameter),
                        declared_word(conflict.declared),
                        conflict.position.word(),
                    ),
                );
            }
        }
    }

    /// Walk `ty` at `site`, collecting every conflicting type parameter occurrence.
    fn walk_variance(
        &self,
        walk: &Walk<'_, '_>,
        ty: Ty,
        site: &Site<'_, '_>,
        conflicts: &mut Vec<Conflict>,
    ) {
        match ty {
            Ty::Nullable(inner) => self.walk_variance(walk, *inner, site, conflicts),
            Ty::DefinitelyNotNull(inner) | Ty::PlatformNullable(inner) if walk.dnn_and_flexible => {
                self.walk_variance(walk, *inner, site, conflicts)
            }
            Ty::Intersection(parts) => {
                for part in parts {
                    self.walk_variance(walk, *part, site, conflicts);
                }
            }
            Ty::TyParam(parameter, _) => {
                // A written `T & Any` resolves to the formal with its bound made non-null; it is
                // still kotlinc's definitely-non-null type.
                if written_definitely_non_null(site.origin) && !walk.dnn_and_flexible {
                    return;
                }
                let Some(declared) = walk.variances.get(parameter.as_str()).copied() else {
                    return;
                };
                if site.position.allows(declared)
                    || self.variance_level(walk, site.origin).unsafe_variance
                {
                    return;
                }
                conflicts.push(Conflict {
                    span: site.span,
                    parameter: parameter.as_str(),
                    declared,
                    position: site.position,
                    abbreviated: site.abbreviated,
                });
            }
            Ty::Obj(classifier, arguments) if !arguments.is_empty() => {
                let Some(record) = self.fed_source().classifier(classifier) else {
                    return;
                };
                let formals = record.type_parameters.type_param_variances();
                let arguments = arguments
                    .iter()
                    .enumerate()
                    .filter_map(|(index, argument)| {
                        let (projection, argument) = match *argument {
                            Ty::StarProjection(_) => return None,
                            Ty::InProjection(inner) => (TypeVariance::In, *inner),
                            Ty::OutProjection(inner) => (TypeVariance::Out, *inner),
                            argument => (TypeVariance::Invariant, argument),
                        };
                        let position = site
                            .position
                            .of_argument(*formals.get(index)?, projection)?;
                        Some((index, argument, position))
                    });
                self.walk_variance_arguments(walk, site, arguments.collect(), conflicts);
            }
            Ty::Fun(signature) => {
                let components = signature
                    .params
                    .iter()
                    .map(|parameter| (*parameter, site.position.opposite()))
                    .chain(std::iter::once((signature.ret, site.position)))
                    .enumerate()
                    .map(|(index, (component, position))| (index, component, position))
                    .collect();
                self.walk_variance_arguments(walk, site, components, conflicts);
            }
            _ => {}
        }
    }

    /// Walk the `(index, type, position)` arguments of the level at `site`. Each argument reports
    /// at the written argument with the same index, or where its level reports when there is none.
    fn walk_variance_arguments(
        &self,
        walk: &Walk<'_, '_>,
        site: &Site<'_, '_>,
        arguments: Vec<(usize, Ty, Position)>,
        conflicts: &mut Vec<Conflict>,
    ) {
        let level = self.variance_level(walk, site.origin);
        let written = site.written.map(written_arguments).unwrap_or_default();
        for (index, argument, position) in arguments {
            let written = written.get(index).copied();
            let argument_site = Site {
                position,
                written,
                span: written.map_or(site.span, |reference| reference.span),
                abbreviated: level.expansion,
                origin: level.arguments.get(index).unwrap_or(&Origin::Inferred),
            };
            self.walk_variance(walk, argument, &argument_site, conflicts);
        }
    }

    /// `ty` as kotlinc renders the declared type: a written `T & Any` is shown as the
    /// definitely-non-null type it declares.
    fn displayed_type(&self, walk: &Walk<'_, '_>, ty: Ty, origin: &Origin<'_>) -> Ty {
        match ty {
            Ty::TyParam(..) if written_definitely_non_null(origin) => {
                Ty::DefinitelyNotNull(crate::types::intern_ty(ty))
            }
            Ty::Nullable(inner) => Ty::nullable(self.displayed_type(walk, *inner, origin)),
            Ty::Obj(classifier, arguments) if !arguments.is_empty() => {
                let level = self.variance_level(walk, origin);
                let arguments = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        let origin = level.arguments.get(index).unwrap_or(&Origin::Inferred);
                        match *argument {
                            Ty::InProjection(inner) => {
                                Ty::in_projection(self.displayed_type(walk, *inner, origin))
                            }
                            Ty::OutProjection(inner) => {
                                Ty::out_projection(self.displayed_type(walk, *inner, origin))
                            }
                            Ty::StarProjection(_) => *argument,
                            argument => self.displayed_type(walk, argument, origin),
                        }
                    })
                    .collect::<Vec<_>>();
                Ty::obj_args_name(classifier, &arguments)
            }
            Ty::Fun(signature) => {
                let level = self.variance_level(walk, origin);
                let mut components = signature
                    .params
                    .iter()
                    .copied()
                    .chain(std::iter::once(signature.ret))
                    .enumerate()
                    .map(|(index, component)| {
                        let origin = level.arguments.get(index).unwrap_or(&Origin::Inferred);
                        self.displayed_type(walk, component, origin)
                    })
                    .collect::<Vec<_>>();
                let ret = components.pop().unwrap_or(signature.ret);
                Ty::fun_with_shape(
                    components,
                    ret,
                    signature.context_count,
                    signature.has_receiver,
                    signature.suspend,
                )
            }
            ty => ty,
        }
    }

    /// The facts kotlinc's cone type carries for the level `origin` describes.
    fn variance_level<'a>(&self, walk: &Walk<'_, '_>, origin: &Origin<'a>) -> Level<'a> {
        match origin {
            Origin::Inferred => Level::inferred(),
            Origin::Vararg(element) => Level {
                expansion: false,
                unsafe_variance: false,
                arguments: vec![Origin::Written(element)],
            },
            Origin::Written(reference) => {
                let unsafe_variance = self.has_unsafe_variance(reference);
                if let Some(selected) = self.written_alias(reference) {
                    let alias = Rc::new(AliasUse {
                        formals: selected.formals,
                        arguments: &reference.targs,
                        expansion_spelling: selected.expansion_spelling,
                    });
                    let mut level =
                        expansion_level(selected.expansion, &alias.expansion_spelling, &alias);
                    level.expansion = true;
                    level.unsafe_variance |= unsafe_variance;
                    return level;
                }
                Level {
                    expansion: false,
                    unsafe_variance,
                    arguments: written_arguments(reference)
                        .into_iter()
                        .map(Origin::Written)
                        .collect(),
                }
            }
            Origin::Expansion {
                template,
                spelling,
                alias,
            } => {
                let template_unsafe = spelling_has_unsafe_variance(spelling);
                match alias_formal(*template, alias) {
                    Some(argument) => {
                        let mut level = self.variance_level(walk, &Origin::Written(argument));
                        level.unsafe_variance |= template_unsafe;
                        level
                    }
                    None => expansion_level(*template, spelling, alias),
                }
            }
        }
    }

    /// The exact alias template retained when ordinary resolution selected this occurrence.
    fn written_alias(
        &self,
        reference: &TypeRef,
    ) -> Option<super::alias_projection::SelectedAliasApplication> {
        self.selected_type_alias_applications
            .get(&(reference.span.lo, reference.span.hi))
            .filter(|alias| alias.formals.len() == reference.targs.len())
            .cloned()
    }

    fn has_unsafe_variance(&self, reference: &TypeRef) -> bool {
        self.resolved_unsafe_variance_occurrences
            .contains(&(reference.span.lo, reference.span.hi))
    }
}

/// The written argument an alias right-hand-side level stands for, when the level is one of the
/// alias's formals (`X`, `X?`, a projected `out X`).
fn alias_formal<'a>(template: Ty, alias: &AliasUse<'a>) -> Option<&'a TypeRef> {
    let Ty::TyParam(parameter, _) = unwrapped(template) else {
        return None;
    };
    let index = alias
        .formals
        .iter()
        .position(|formal| formal.as_str() == parameter)?;
    alias.arguments.get(index)
}

/// The level of an alias right-hand side `template`: never itself an expansion in kotlinc's model,
/// whose argument levels are either further right-hand-side levels or the use site's arguments.
fn expansion_level<'a>(
    template: Ty,
    spelling: &crate::spelling::Spelled,
    alias: &Rc<AliasUse<'a>>,
) -> Level<'a> {
    let arguments = match unwrapped(template) {
        Ty::Obj(_, arguments) => arguments.to_vec(),
        Ty::Fun(signature) => signature
            .params
            .iter()
            .copied()
            .chain(std::iter::once(signature.ret))
            .collect(),
        _ => Vec::new(),
    };
    Level {
        expansion: false,
        unsafe_variance: spelling_has_unsafe_variance(spelling),
        arguments: arguments
            .into_iter()
            .enumerate()
            .map(|(index, template)| Origin::Expansion {
                template,
                spelling: spelling.arg(index).clone(),
                alias: Rc::clone(alias),
            })
            .collect(),
    }
}

/// Whether a finalized alias-template node carries the semantic `@UnsafeVariance` marker, either
/// directly or inherited from an alias used in that template. The metadata publication boundary
/// has already resolved these applications to qualified identities.
fn spelling_has_unsafe_variance(spelling: &crate::spelling::Spelled) -> bool {
    spelling.unsafe_variance
        || spelling
            .annotations
            .iter()
            .chain(&spelling.expansion_annotations)
            .any(|annotation| {
                matches!(
                    annotation,
                    crate::spelling::TypeUseAnnotation::Checked(annotation)
                        if annotation.annotation == crate::types::wk::unsafe_variance()
                )
            })
}

/// A right-hand-side level without its nullability and use-site projection, which are not levels
/// of their own.
fn unwrapped(ty: Ty) -> Ty {
    match ty {
        Ty::Nullable(inner)
        | Ty::DefinitelyNotNull(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => unwrapped(*inner),
        ty => ty,
    }
}

fn written_definitely_non_null(origin: &Origin<'_>) -> bool {
    matches!(origin, Origin::Written(reference) if reference.definitely_non_null())
}
