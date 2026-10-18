//! Checked type-publication facts for declarations whose type is inferred from a body value.
//!
//! The checker owns the value's selected Java enhancement and the provider-normalized upper face
//! of every flexible type occurrence. It joins those facts to a stable declaration identity here.
//! The compiler metadata handoff consumes that explicit contract; it never reopens an AST body or
//! asks a backend to reconstruct provider semantics.

use super::{ty_mentions_param, Checker, Signature, TypeInfo};
use crate::ast::ExprId;
use crate::fir::DeclarationId;
use crate::fx_hash::HashMap;
use crate::spelling::Spelled;
use crate::types::{Ty, TypeName};

/// The provider-owned flexible occurrences carried by one checked value. Unlike [`Ty`], this is
/// publication-only provenance: ordinary Kotlin inference must continue to see a selected source
/// argument as its rigid semantic type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FlexibleValue {
    /// Exact checked semantic type whose classifier owns `shape`. Contextual adaptation may later
    /// change an expression's visible type; projection must still start from this identity rather
    /// than correlate same-position arguments across different classifiers.
    source: Option<Ty>,
    /// Provider lower face selected for declaration publication. Unlike `source`, this identity is
    /// not rewritten when ordinary flow views the current value through a supertype.
    publication: Option<FlexiblePublication>,
    upper: Option<Ty>,
    shape: FlexibleShape,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum FlexibleShape {
    #[default]
    None,
    Obj(TypeName, Vec<FlexibleValue>),
    Fun {
        context_count: usize,
        has_receiver: bool,
        params: Vec<FlexibleValue>,
        ret: Box<FlexibleValue>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FlexiblePublication {
    lower: Ty,
    shape: FlexibleShape,
}

impl FlexibleValue {
    fn is_none(&self) -> bool {
        self.publication.is_none()
            && self.upper.is_none()
            && match &self.shape {
                FlexibleShape::None => true,
                FlexibleShape::Obj(_, arguments) => arguments.iter().all(FlexibleValue::is_none),
                FlexibleShape::Fun { params, ret, .. } => {
                    params.iter().all(FlexibleValue::is_none) && ret.is_none()
                }
            }
    }

    fn without_head(mut self) -> Self {
        self.publication = None;
        self.upper = None;
        self
    }

    fn inferred_declaration_ty(&self, fallback: Ty) -> Ty {
        self.publication
            .as_ref()
            .map(|publication| Ty::platform_nullable(publication.lower))
            .unwrap_or(fallback)
    }

    /// Facts present on every value reaching one join survive the join. A mismatched upper face is
    /// not a publishable flexible interval.
    fn meet(&self, other: &Self) -> Self {
        let shape = match (&self.shape, &other.shape) {
            (FlexibleShape::Obj(left_owner, left), FlexibleShape::Obj(right_owner, right))
                if left_owner == right_owner && left.len() == right.len() =>
            {
                FlexibleShape::Obj(
                    *left_owner,
                    left.iter()
                        .zip(right)
                        .map(|(left, right)| left.meet(right))
                        .collect(),
                )
            }
            (
                FlexibleShape::Fun {
                    context_count: left_contexts,
                    has_receiver: left_receiver,
                    params: left_params,
                    ret: left_ret,
                },
                FlexibleShape::Fun {
                    context_count: right_contexts,
                    has_receiver: right_receiver,
                    params: right_params,
                    ret: right_ret,
                },
            ) if left_contexts == right_contexts
                && left_receiver == right_receiver
                && left_params.len() == right_params.len() =>
            {
                FlexibleShape::Fun {
                    context_count: *left_contexts,
                    has_receiver: *left_receiver,
                    params: left_params
                        .iter()
                        .zip(right_params)
                        .map(|(left, right)| left.meet(right))
                        .collect(),
                    ret: Box::new(left_ret.meet(right_ret)),
                }
            }
            _ => FlexibleShape::None,
        };
        let (publication, upper) =
            if self.publication == other.publication && self.upper == other.upper {
                (self.publication.clone(), self.upper)
            } else {
                (None, None)
            };
        FlexibleValue {
            source: if self.source == other.source {
                self.source
            } else {
                None
            },
            publication,
            upper,
            shape,
        }
    }

    fn overlay(&self, other: &Self) -> Self {
        let shape = match (&self.shape, &other.shape) {
            (FlexibleShape::None, shape) | (shape, FlexibleShape::None) => shape.clone(),
            (FlexibleShape::Obj(left_owner, left), FlexibleShape::Obj(right_owner, right))
                if left_owner == right_owner && left.len() == right.len() =>
            {
                FlexibleShape::Obj(
                    *left_owner,
                    left.iter()
                        .zip(right)
                        .map(|(left, right)| left.overlay(right))
                        .collect(),
                )
            }
            (
                FlexibleShape::Fun {
                    context_count: left_contexts,
                    has_receiver: left_receiver,
                    params: left_params,
                    ret: left_ret,
                },
                FlexibleShape::Fun {
                    context_count: right_contexts,
                    has_receiver: right_receiver,
                    params: right_params,
                    ret: right_ret,
                },
            ) if left_contexts == right_contexts
                && left_receiver == right_receiver
                && left_params.len() == right_params.len() =>
            {
                FlexibleShape::Fun {
                    context_count: *left_contexts,
                    has_receiver: *left_receiver,
                    params: left_params
                        .iter()
                        .zip(right_params)
                        .map(|(left, right)| left.overlay(right))
                        .collect(),
                    ret: Box::new(left_ret.overlay(right_ret)),
                }
            }
            _ => FlexibleShape::None,
        };
        let preferred_interval = |preferred: &FlexibleValue, fallback: &FlexibleValue| {
            let preferred_interval = (&preferred.publication, preferred.upper);
            let fallback_interval = (&fallback.publication, fallback.upper);
            if preferred.publication.is_none() && preferred.upper.is_none() {
                (fallback.publication.clone(), fallback.upper)
            } else if (fallback.publication.is_none() && fallback.upper.is_none())
                || preferred_interval == fallback_interval
            {
                (preferred.publication.clone(), preferred.upper)
            } else {
                (None, None)
            }
        };
        let (publication, upper) = match (&self.shape, &other.shape) {
            (FlexibleShape::None, _) => preferred_interval(other, self),
            (_, FlexibleShape::None) => preferred_interval(self, other),
            _ => preferred_interval(self, other),
        };
        FlexibleValue {
            source: match (&self.shape, &other.shape) {
                (FlexibleShape::None, _) => other.source.or(self.source),
                (_, FlexibleShape::None) => self.source.or(other.source),
                _ if self.source == other.source => self.source,
                _ => None,
            },
            publication,
            upper,
            shape,
        }
    }

    fn apply(&self, ty: Ty, spelling: &mut Spelled) {
        if let Some(upper) = self.upper {
            spelling.flexible_upper_bound = Some(upper);
        }
        let ty = match ty.non_null() {
            Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) => {
                *inner
            }
            other => other,
        };
        if let (FlexibleShape::Obj(fact_owner, arguments), Ty::Obj(owner, type_arguments)) =
            (&self.shape, ty)
        {
            if *fact_owner != owner || arguments.len() != type_arguments.len() {
                return;
            }
            if spelling.args.len() < arguments.len() {
                spelling.args.resize_with(arguments.len(), Spelled::default);
            }
            for ((spelling, fact), ty) in spelling
                .args
                .iter_mut()
                .zip(arguments)
                .zip(type_arguments.iter().copied())
            {
                fact.apply(ty, spelling);
            }
        } else if let (
            FlexibleShape::Fun {
                context_count,
                has_receiver,
                params,
                ret,
            },
            Ty::Fun(signature),
        ) = (&self.shape, ty)
        {
            if signature.suspend
                || *context_count != signature.context_count
                || *has_receiver != signature.has_receiver
                || params.len() != signature.params.len()
            {
                return;
            }
            let count = signature.params.len() + 1;
            if spelling.args.len() < count {
                spelling.args.resize_with(count, Spelled::default);
            }
            for ((spelling, fact), ty) in spelling
                .args
                .iter_mut()
                .zip(params)
                .zip(signature.params.iter().copied())
            {
                fact.apply(ty, spelling);
            }
            ret.apply(signature.ret, &mut spelling.args[signature.params.len()]);
        }
    }

    fn apply_to_inferred_declaration(&self, ty: Ty, spelling: &mut Spelled) {
        if let Some(publication) = &self.publication {
            FlexibleValue {
                source: Some(publication.lower),
                publication: None,
                upper: None,
                shape: publication.shape.clone(),
            }
            .apply(ty, spelling);
        } else {
            self.apply(ty, spelling);
        }
    }

    fn rigid_shape(ty: Ty) -> Self {
        match ty {
            Ty::Nullable(inner)
            | Ty::PlatformNullable(inner)
            | Ty::DefinitelyNotNull(inner)
            | Ty::InProjection(inner)
            | Ty::OutProjection(inner)
            | Ty::StarProjection(inner) => Self::rigid_shape(*inner),
            Ty::Obj(owner, arguments) => FlexibleValue {
                source: Some(ty),
                publication: None,
                upper: None,
                shape: FlexibleShape::Obj(
                    owner,
                    arguments.iter().copied().map(Self::rigid_shape).collect(),
                ),
            },
            Ty::Fun(signature) if !signature.suspend => FlexibleValue {
                source: Some(ty),
                publication: None,
                upper: None,
                shape: FlexibleShape::Fun {
                    context_count: signature.context_count,
                    has_receiver: signature.has_receiver,
                    params: signature
                        .params
                        .iter()
                        .copied()
                        .map(Self::rigid_shape)
                        .collect(),
                    ret: Box::new(Self::rigid_shape(signature.ret)),
                },
            },
            _ => FlexibleValue::default(),
        }
    }

    fn semantic(ty: Ty, upper_bound: &dyn Fn(Ty) -> Ty) -> Self {
        match ty {
            Ty::PlatformNullable(inner) => {
                let lower = inner.platform_lower_bound();
                let mut fact = Self::semantic(*inner, upper_bound);
                fact.publication = Some(FlexiblePublication {
                    lower,
                    shape: fact.shape.clone(),
                });
                fact.upper = Some(upper_bound(lower));
                fact
            }
            Ty::Nullable(inner) | Ty::DefinitelyNotNull(inner) => {
                Self::semantic(*inner, upper_bound).without_head()
            }
            Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) => {
                Self::semantic(*inner, upper_bound)
            }
            Ty::Obj(owner, arguments) => FlexibleValue {
                source: Some(ty),
                publication: None,
                upper: None,
                shape: FlexibleShape::Obj(
                    owner,
                    arguments
                        .iter()
                        .copied()
                        .map(|argument| Self::semantic(argument, upper_bound))
                        .collect(),
                ),
            },
            Ty::Fun(signature) if !signature.suspend => FlexibleValue {
                source: Some(ty),
                publication: None,
                upper: None,
                shape: FlexibleShape::Fun {
                    context_count: signature.context_count,
                    has_receiver: signature.has_receiver,
                    params: signature
                        .params
                        .iter()
                        .copied()
                        .map(|parameter| Self::semantic(parameter, upper_bound))
                        .collect(),
                    ret: Box::new(Self::semantic(signature.ret, upper_bound)),
                },
            },
            // A suspend function's metadata result occupies its continuation slot. This sidecar
            // deliberately does not reconstruct that physical layout.
            Ty::Fun(_) => FlexibleValue::default(),
            _ => FlexibleValue::default(),
        }
    }

    /// Pair a declaration-owned result with the already-selected semantic result. Recursion is
    /// permitted only across the same classifier and arity. Regular function types retain their
    /// semantic parameter/result shape; suspend functions are declined because their result is
    /// encoded under the physical continuation slot and publication must not guess that layout.
    fn selected(declared: Ty, selected: Ty, upper_bound: &dyn Fn(Ty) -> Ty) -> FlexibleValue {
        let mut head = None;
        let (declared, selected) = match (declared, selected) {
            (Ty::PlatformNullable(declared), Ty::PlatformNullable(selected)) => {
                head = Some(upper_bound(*selected));
                (*declared, *selected)
            }
            // A nullable solution collapses both faces to one rigid type.
            (Ty::PlatformNullable(declared), Ty::Nullable(selected)) => (*declared, *selected),
            (Ty::PlatformNullable(declared), selected @ (Ty::Obj(..) | Ty::Fun(_))) => {
                head = Some(upper_bound(selected));
                (*declared, selected)
            }
            (Ty::PlatformNullable(declared), selected) => (*declared, selected),
            (Ty::Nullable(declared), Ty::Nullable(selected))
            | (Ty::DefinitelyNotNull(declared), Ty::DefinitelyNotNull(selected))
            | (Ty::InProjection(declared), Ty::InProjection(selected))
            | (Ty::OutProjection(declared), Ty::OutProjection(selected))
            | (Ty::StarProjection(declared), Ty::StarProjection(selected)) => {
                (*declared, *selected)
            }
            pair => pair,
        };

        if matches!(declared, Ty::TyParam(_, bound) if matches!(*bound, Ty::PlatformNullable(_))) {
            let upper =
                (!selected.is_nullable()).then(|| upper_bound(selected.platform_lower_bound()));
            // A Java type-parameter occurrence is flexible as a whole, but its selected lower face
            // still owns a usable class/function shape. Keep that shape so a later member selection
            // can project the occurrence through its receiver (for example `List<T!>.get()` where
            // `T` was inferred as a function type) without reopening the Java declaration.
            let shape = FlexibleValue::rigid_shape(selected).shape;
            return FlexibleValue {
                source: Some(selected),
                publication: upper.map(|_| FlexiblePublication {
                    lower: selected.platform_lower_bound(),
                    shape: shape.clone(),
                }),
                upper,
                shape,
            };
        }

        let shape = match (declared, selected) {
            (Ty::Obj(declared_owner, declared), Ty::Obj(selected_owner, selected))
                if declared_owner == selected_owner && declared.len() == selected.len() =>
            {
                FlexibleShape::Obj(
                    declared_owner,
                    declared
                        .iter()
                        .copied()
                        .zip(selected.iter().copied())
                        .map(|(declared, selected)| {
                            FlexibleValue::selected(declared, selected, upper_bound)
                        })
                        .collect(),
                )
            }
            (Ty::Fun(declared), Ty::Fun(selected))
                if !declared.suspend
                    && !selected.suspend
                    && declared.context_count == selected.context_count
                    && declared.has_receiver == selected.has_receiver
                    && declared.params.len() == selected.params.len() =>
            {
                FlexibleShape::Fun {
                    context_count: declared.context_count,
                    has_receiver: declared.has_receiver,
                    params: declared
                        .params
                        .iter()
                        .copied()
                        .zip(selected.params.iter().copied())
                        .map(|(declared, selected)| {
                            FlexibleValue::selected(declared, selected, upper_bound)
                        })
                        .collect(),
                    ret: Box::new(FlexibleValue::selected(
                        declared.ret,
                        selected.ret,
                        upper_bound,
                    )),
                }
            }
            _ => FlexibleShape::None,
        };
        FlexibleValue {
            source: (!matches!(&shape, FlexibleShape::None) || head.is_some()).then_some(selected),
            publication: head.map(|_| FlexiblePublication {
                lower: selected.platform_lower_bound(),
                shape: shape.clone(),
            }),
            upper: head,
            shape,
        }
    }

    fn substitute(
        &self,
        declared: Ty,
        binding: &dyn Fn(&str) -> Option<FlexibleValue>,
    ) -> FlexibleValue {
        match declared {
            Ty::TyParam(name, _) => binding(name)
                .map(|argument| self.overlay(&argument))
                .unwrap_or_else(|| self.clone()),
            Ty::Nullable(inner) | Ty::DefinitelyNotNull(inner) => {
                self.substitute(*inner, binding).without_head()
            }
            Ty::PlatformNullable(inner)
            | Ty::InProjection(inner)
            | Ty::OutProjection(inner)
            | Ty::StarProjection(inner) => self.substitute(*inner, binding),
            Ty::Obj(owner, arguments) => match &self.shape {
                FlexibleShape::Obj(fact_owner, facts)
                    if owner == *fact_owner && arguments.len() == facts.len() =>
                {
                    FlexibleValue {
                        source: self.source,
                        publication: self.publication.clone(),
                        upper: self.upper,
                        shape: FlexibleShape::Obj(
                            owner,
                            arguments
                                .iter()
                                .copied()
                                .zip(facts)
                                .map(|(argument, fact)| fact.substitute(argument, binding))
                                .collect(),
                        ),
                    }
                }
                _ => FlexibleValue {
                    source: self.source,
                    publication: self.publication.clone(),
                    upper: self.upper,
                    shape: FlexibleShape::None,
                },
            },
            Ty::Fun(signature) if !signature.suspend => match &self.shape {
                FlexibleShape::Fun {
                    context_count,
                    has_receiver,
                    params,
                    ret,
                } if *context_count == signature.context_count
                    && *has_receiver == signature.has_receiver
                    && params.len() == signature.params.len() =>
                {
                    FlexibleValue {
                        source: self.source,
                        publication: self.publication.clone(),
                        upper: self.upper,
                        shape: FlexibleShape::Fun {
                            context_count: *context_count,
                            has_receiver: *has_receiver,
                            params: signature
                                .params
                                .iter()
                                .copied()
                                .zip(params)
                                .map(|(parameter, fact)| fact.substitute(parameter, binding))
                                .collect(),
                            ret: Box::new(ret.substitute(signature.ret, binding)),
                        },
                    }
                }
                _ => FlexibleValue {
                    source: self.source,
                    publication: self.publication.clone(),
                    upper: self.upper,
                    shape: FlexibleShape::None,
                },
            },
            Ty::Fun(_) => FlexibleValue::default(),
            _ => self.clone(),
        }
    }
}

/// Publication facts owned by checked value identities. A fact is frozen when its expression is
/// checked; inferred declarations consume this table instead of reopening their source bodies.
#[derive(Default)]
pub(super) struct FlexibleValues {
    expressions: HashMap<ExprId, FlexibleValue>,
    /// Declaration-owned publication evidence retained for source reads in the same compilation.
    /// The stable declaration identity, not a property spelling, joins the producing body to a
    /// later selected read.
    declaration_results: HashMap<DeclarationId, FlexibleValue>,
    /// Value-flow facts staged by the expression checker that owns the relevant branch/operand
    /// decision. The final expression recorder consumes this checked handoff without reading AST.
    staged: HashMap<ExprId, FlexibleValue>,
    declarations: HashMap<u32, FlexibleLocalDeclaration>,
    reads: HashMap<ExprId, FlexibleValue>,
    receivers: HashMap<ExprId, ExprId>,
    explicit_type_arguments: crate::fx_hash::HashSet<ExprId>,
    /// Checked safe-call results have a nullable outer face even when the selected member itself
    /// returned a platform value. Nested argument occurrences remain owned by that selected value.
    nullable_results: crate::fx_hash::HashSet<ExprId>,
    constructor_declarations:
        HashMap<ExprId, super::constructor_commitment::ConstructorDeclarationShape>,
}

#[derive(Clone)]
struct FlexibleLocalDeclaration {
    /// The semantic storage type this publication fact describes. Assignments change a mutable
    /// local's current value, not its inferred declaration type; keeping the type beside the fact
    /// prevents a reused/stale flow identity from publishing a different declaration shape.
    declared: Ty,
    value: FlexibleValue,
}

/// A literal `null` is an expression-only sentinel. Once inference publishes it as a declaration
/// type, Kotlin's semantic type is the nullable bottom type `Nothing?`.
pub(super) fn inferred_declaration_ty(ty: Ty) -> Ty {
    if ty == Ty::Null {
        Ty::nullable(Ty::Nothing)
    } else {
        ty
    }
}

impl Signature {
    /// Publish one body-inferred result without replacing a declaration-backed symbolic result by
    /// the erased bound a later body pass may observe.
    pub(super) fn set_inferred_return(&mut self, ret: Ty) -> bool {
        let ret = inferred_declaration_ty(ret);
        let inferred_generic_ret = self.generic_sig.as_ref().map(|generic| {
            let declaration_bindings = generic
                .formals
                .iter()
                .enumerate()
                .map(|(index, formal)| {
                    (
                        crate::types::type_parameter_source_name(formal).to_string(),
                        Ty::ty_param(
                            formal,
                            generic
                                .formal_bounds
                                .get(index)
                                .and_then(|bounds| bounds.first())
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            let ret = crate::symbol_resolver::ty_subst_keep_unbound(ret, &declaration_bindings);
            if ty_mentions_param(generic.ret, &generic.formals)
                && !ty_mentions_param(ret, &generic.formals)
            {
                generic.ret
            } else {
                ret
            }
        });
        let changed = self.ret != ret
            || self
                .generic_sig
                .as_ref()
                .zip(inferred_generic_ret)
                .is_some_and(|(generic, inferred)| generic.ret != inferred);
        self.ret = ret;
        if let Some((generic, inferred)) = self.generic_sig.as_mut().zip(inferred_generic_ret) {
            generic.ret = inferred;
        }
        changed
    }
}

/// Metadata-only result-type facts keyed by the declaration that owns them.
#[derive(Default)]
pub(super) struct InferredDeclarationPublications {
    results: HashMap<DeclarationId, Spelled>,
}

impl Checker<'_> {
    fn flexible_upper_bound(&self, lower: Ty) -> Ty {
        crate::assignable::platform_shape_upper_bound(
            crate::assignable::TypeOracle::platform_flexible_upper_bound(self, lower),
        )
    }

    /// View one value's publication facts through an already-declared supertype. The hierarchy
    /// supplies exact classifier identities and symbolic argument expressions; no source spelling
    /// or positional cross-classifier correlation is involved.
    fn flexible_supertype_view(
        &self,
        actual: Ty,
        fact: &FlexibleValue,
        target: TypeName,
    ) -> Option<(Ty, FlexibleValue)> {
        // `actual` may already be contextually adapted (`MutableList<T>` read as `List<T>`), while
        // the publication shape still belongs to the selected producer's lower classifier. Start
        // from that exact checked source identity when present; never zip its arguments onto the
        // adapted classifier by position.
        let actual = match fact.source.unwrap_or(actual).non_null() {
            Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
            other => other,
        };
        let class = actual.kotlin_class_internal()?;
        if class == target {
            return Some((actual, fact.clone()));
        }
        let source = self.fed_source();
        let (_, applied, _) = crate::symbol_resolver::applied_hierarchy(&source, actual)
            .into_iter()
            .find(|(owner, _, _)| *owner == target)?;
        let declaration = self.resolver().classifier(class)?;
        let parameters = declaration.type_params();
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let symbolic = Ty::obj_args_name(
            class,
            &parameters
                .iter()
                .map(|parameter| Ty::ty_param(parameter, any))
                .collect::<Vec<_>>(),
        );
        let (_, symbolic_view, _) = crate::symbol_resolver::applied_hierarchy(&source, symbolic)
            .into_iter()
            .find(|(owner, _, _)| *owner == target)?;
        let argument_fact = |name: &str| {
            let index = parameters.iter().position(|parameter| parameter == name)?;
            match &fact.shape {
                FlexibleShape::Obj(owner, arguments)
                    if *owner == class && arguments.len() == parameters.len() =>
                {
                    arguments.get(index).cloned()
                }
                _ => None,
            }
        };
        let mut view =
            FlexibleValue::rigid_shape(applied).substitute(symbolic_view, &argument_fact);
        view.publication = fact.publication.clone();
        Some((applied, view))
    }

    fn constrain_flexible_parameters(
        &self,
        formals: &[String],
        declared: Ty,
        actual: Ty,
        fact: &FlexibleValue,
        constraints: &mut HashMap<String, Vec<FlexibleValue>>,
    ) {
        super::selected_result_publication::constrain_parameters(
            formals,
            declared,
            actual,
            fact,
            constraints,
            super::selected_result_publication::StarProjectionPolicy::Descend,
            &mut |actual, fact, owner| {
                let (view, view_fact) = self.flexible_supertype_view(actual, fact, owner)?;
                let FlexibleShape::Obj(fact_owner, facts) = view_fact.shape else {
                    return None;
                };
                (fact_owner == owner).then_some((view, facts))
            },
        );
    }

    fn bind_class_flexible_parameters_from(
        &self,
        owner: TypeName,
        actual: Ty,
        fact: &FlexibleValue,
        bindings: &mut HashMap<String, FlexibleValue>,
    ) {
        let Some((_, view)) = self.flexible_supertype_view(actual, fact, owner) else {
            return;
        };
        let Some(classifier) = self.resolver().classifier(owner) else {
            return;
        };
        let FlexibleShape::Obj(view_owner, arguments) = view.shape else {
            return;
        };
        if view_owner != owner || arguments.len() != classifier.type_params().len() {
            return;
        }
        for (formal, argument) in classifier.type_params().iter().zip(arguments) {
            if !argument.is_none() {
                bindings.insert(formal.clone(), argument);
            }
        }
    }

    fn bind_class_flexible_parameters(
        &self,
        owner: TypeName,
        receiver: ExprId,
        bindings: &mut HashMap<String, FlexibleValue>,
    ) {
        self.bind_class_flexible_parameters_from(
            owner,
            self.expr_types[receiver.0 as usize],
            &self.flexible_value(receiver),
            bindings,
        );
    }

    pub(super) fn record_flexible_local(
        &mut self,
        flow_identity: u32,
        initializer: ExprId,
        declared: Ty,
    ) {
        let fact = self.flexible_value(initializer);
        if fact.is_none() {
            self.flexible_values.declarations.remove(&flow_identity);
        } else {
            self.flexible_values.declarations.insert(
                flow_identity,
                FlexibleLocalDeclaration {
                    declared,
                    value: fact,
                },
            );
        }
    }

    pub(super) fn record_constructor_declaration_shape(
        &mut self,
        expression: ExprId,
        declaration: super::constructor_commitment::ConstructorDeclarationShape,
    ) {
        self.flexible_values
            .constructor_declarations
            .insert(expression, declaration);
    }

    pub(super) fn discard_constructor_declaration_shape(&mut self, expression: ExprId) {
        self.flexible_values
            .constructor_declarations
            .remove(&expression);
    }

    pub(super) fn record_flexible_local_assignment(&mut self, local: &super::Local) {
        // The ordinary data-flow engine owns the assigned value and every control-flow edge. This
        // sidecar owns only the inferred storage type's publication interval, which an assignment
        // does not change. Retain it exactly while this is the same mutable local declaration;
        // otherwise kill the stale identity instead of trying to reproduce the semantic CFG.
        let valid = matches!(local.origin, super::ReceiverFnValueOrigin::Local)
            && local.is_var
            && local.write_ty == Some(local.declared_ty)
            && self
                .flexible_values
                .declarations
                .get(&local.flow_identity)
                .is_some_and(|fact| fact.declared == local.declared_ty);
        if !valid {
            self.flexible_values
                .declarations
                .remove(&local.flow_identity);
        }
    }

    pub(super) fn record_flexible_local_read(
        &mut self,
        read: ExprId,
        local: &super::Local,
        read_ty: Ty,
    ) {
        if !matches!(local.origin, super::ReceiverFnValueOrigin::Local)
            || local.ty != local.declared_ty
        {
            self.flexible_values.reads.remove(&read);
            return;
        }
        let fact = self
            .flexible_values
            .declarations
            .get(&local.flow_identity)
            .filter(|fact| fact.declared == local.declared_ty)
            .map(|fact| fact.value.clone());
        let fact = match (fact, read_ty.non_null().kotlin_class_internal()) {
            (Some(fact), Some(target)) if read_ty != local.declared_ty => self
                .flexible_supertype_view(local.declared_ty, &fact, target)
                .filter(|(view, _)| view.non_null() == read_ty.non_null())
                .map(|(_, fact)| fact),
            (fact, _) => fact,
        };
        if let Some(fact) = fact.filter(|fact| !fact.is_none()) {
            self.flexible_values.reads.insert(read, fact);
        } else {
            self.flexible_values.reads.remove(&read);
        }
    }

    fn flexible_value(&self, expression: ExprId) -> FlexibleValue {
        self.flexible_values
            .expressions
            .get(&expression)
            .cloned()
            .unwrap_or_default()
    }

    /// The declaration type owned by a checked value. Ordinary flow may view the current value
    /// through a provider upper face, but that does not replace the lower classifier selected for
    /// an inferred declaration's flexible interval.
    pub(super) fn inferred_declaration_value_ty(&self, expression: ExprId, fallback: Ty) -> Ty {
        self.flexible_value(expression)
            .inferred_declaration_ty(fallback)
    }

    pub(super) fn record_flexible_call_inputs(
        &mut self,
        expression: ExprId,
        receiver: Option<ExprId>,
        explicit_type_arguments: bool,
    ) {
        if let Some(receiver) = receiver {
            self.flexible_values.receivers.insert(expression, receiver);
        } else {
            self.flexible_values.receivers.remove(&expression);
        }
        if explicit_type_arguments {
            self.flexible_values
                .explicit_type_arguments
                .insert(expression);
        } else {
            self.flexible_values
                .explicit_type_arguments
                .remove(&expression);
        }
    }

    pub(super) fn record_flexible_nullable_result(&mut self, expression: ExprId) {
        self.flexible_values.nullable_results.insert(expression);
    }

    pub(super) fn checked_call_receiver(&self, expression: ExprId) -> Option<ExprId> {
        self.flexible_values.receivers.get(&expression).copied()
    }

    pub(super) fn has_checked_explicit_type_arguments(&self, expression: ExprId) -> bool {
        self.flexible_values
            .explicit_type_arguments
            .contains(&expression)
    }

    pub(super) fn record_flexible_alias(
        &mut self,
        expression: ExprId,
        value: ExprId,
        without_head: bool,
    ) {
        let mut fact = self.flexible_value(value);
        if without_head {
            fact = fact.without_head();
        }
        self.stage_flexible_value(expression, fact);
    }

    pub(super) fn record_flexible_join(
        &mut self,
        expression: ExprId,
        values: impl IntoIterator<Item = ExprId>,
        joined: Ty,
        without_head: bool,
    ) {
        let target = joined.non_null().kotlin_class_internal();
        let mut facts = values
            .into_iter()
            .filter(|value| self.expr_types[value.0 as usize] != Ty::Nothing)
            .map(|value| {
                let actual = self.expr_types[value.0 as usize];
                let fact = self.flexible_value(value);
                target.map_or(fact.clone(), |target| {
                    self.flexible_supertype_view(actual, &fact, target)
                        .filter(|(view, _)| view.non_null() == joined.non_null())
                        .map_or_else(FlexibleValue::default, |(_, projected)| projected)
                })
            });
        let mut fact = facts
            .next()
            .map(|first| facts.fold(first, |known, next| known.meet(&next)))
            .unwrap_or_default();
        if without_head {
            fact = fact.without_head();
        }
        self.stage_flexible_value(expression, fact);
    }

    fn stage_flexible_value(&mut self, expression: ExprId, fact: FlexibleValue) {
        if fact.is_none() {
            self.flexible_values.staged.remove(&expression);
        } else {
            self.flexible_values.staged.insert(expression, fact);
        }
    }

    fn selected_call_flexible_publication(
        &self,
        expression: ExprId,
        call: &super::ResolvedCall,
    ) -> FlexibleValue {
        let target = call.selected_result_target();
        let signature = target.signature;
        // The committed call target owns the declaration shape, while the checked expression slot
        // owns its fully specialized result. A member target may still retain the provider's erased
        // or pre-contextual result (`T`/`Any`) after call inference selected a nested function or
        // generic class result; publication must pair the declaration with that checked value.
        let selected = self.expr_types[expression.0 as usize];
        let declared = target.declared;
        let fact = FlexibleValue::selected(declared, selected, &|lower| {
            self.flexible_upper_bound(lower)
        });
        let mut bindings = HashMap::default();
        match target.dispatch {
            Some(super::selected_result_publication::DispatchBinding::Explicit(owner)) => {
                if let Some(&receiver) = self.flexible_values.receivers.get(&expression) {
                    self.bind_class_flexible_parameters(owner, receiver, &mut bindings);
                }
            }
            Some(super::selected_result_publication::DispatchBinding::Implicit {
                owner,
                actual,
            }) => {
                let dispatch_fact =
                    FlexibleValue::semantic(actual, &|lower| self.flexible_upper_bound(lower));
                self.bind_class_flexible_parameters_from(
                    owner,
                    actual,
                    &dispatch_fact,
                    &mut bindings,
                );
            }
            None => {}
        }
        let Some(signature) = signature else {
            return fact.substitute(declared, &|formal| bindings.get(formal).cloned());
        };
        if self
            .flexible_values
            .explicit_type_arguments
            .contains(&expression)
        {
            return fact.substitute(declared, &|formal| bindings.get(formal).cloned());
        }
        let Some(arguments) = self.resolved_call_arg_slots.get(&expression) else {
            return fact.substitute(declared, &|formal| bindings.get(formal).cloned());
        };
        let mut constraints: HashMap<String, Vec<FlexibleValue>> = HashMap::default();
        if let (Some(parameter), Some(&receiver)) = (
            target
                .receiver_parameter
                .then_some(signature.receiver)
                .flatten(),
            self.flexible_values.receivers.get(&expression),
        ) {
            self.constrain_flexible_parameters(
                &signature.formals,
                parameter,
                self.expr_types[receiver.0 as usize],
                &self.flexible_value(receiver),
                &mut constraints,
            );
        }
        for argument in &arguments.argument_bindings {
            let Some(&parameter) = signature.params.get(argument.parameter) else {
                continue;
            };
            let parameter = if argument.vararg_element {
                parameter.array_read_elem().unwrap_or(parameter)
            } else {
                parameter
            };
            let actual = self.expr_types[argument.argument.0 as usize];
            let argument_fact = self.flexible_value(argument.argument);
            self.constrain_flexible_parameters(
                &signature.formals,
                parameter,
                actual,
                &argument_fact,
                &mut constraints,
            );
        }
        for (formal, facts) in constraints {
            let fact = facts
                .iter()
                .skip(1)
                .fold(facts[0].clone(), |known, next| known.meet(next));
            bindings.insert(formal, fact);
        }
        fact.substitute(declared, &|formal| bindings.get(formal).cloned())
    }

    pub(super) fn record_flexible_member_extension_property(
        &mut self,
        expression: ExprId,
        property: &super::MemberExtensionProperty,
    ) {
        let receiver = self.checked_call_receiver(expression);
        let signature = property.publication_signature.as_ref();
        let declared = signature.ret;
        let selected = property.ty;
        let mut fact = FlexibleValue::selected(declared, selected, &|lower| {
            self.flexible_upper_bound(lower)
        });
        if let Some(declaration_fact) = property
            .stable_declaration
            .and_then(|declaration| self.flexible_values.declaration_results.get(&declaration))
        {
            fact = fact.overlay(declaration_fact);
        }
        let mut bindings = HashMap::default();
        let dispatch_ty = property.dispatch_receiver.ty;
        let dispatch_fact =
            FlexibleValue::semantic(dispatch_ty, &|lower| self.flexible_upper_bound(lower));
        self.bind_class_flexible_parameters_from(
            property.owner,
            dispatch_ty,
            &dispatch_fact,
            &mut bindings,
        );
        let mut constraints = HashMap::default();
        if let (Some(parameter), Some(receiver)) = (signature.receiver, receiver) {
            self.constrain_flexible_parameters(
                &signature.formals,
                parameter,
                self.expr_types[receiver.0 as usize],
                &self.flexible_value(receiver),
                &mut constraints,
            );
        }
        for (formal, facts) in constraints {
            let met = facts
                .iter()
                .skip(1)
                .fold(facts[0].clone(), |known, next| known.meet(next));
            bindings.insert(formal, met);
        }
        self.stage_flexible_value(
            expression,
            fact.substitute(declared, &|formal| bindings.get(formal).cloned()),
        );
    }

    fn constructor_flexible_publication(&self, expression: ExprId) -> FlexibleValue {
        let selected = self.expr_types[expression.0 as usize];
        let Ty::Obj(owner, selected_arguments) = selected.non_null() else {
            return FlexibleValue::default();
        };
        if self
            .flexible_values
            .explicit_type_arguments
            .contains(&expression)
        {
            return FlexibleValue::rigid_shape(selected);
        }
        let Some(declaration) = self
            .flexible_values
            .constructor_declarations
            .get(&expression)
        else {
            return FlexibleValue::default();
        };
        if declaration.formals.len() != selected_arguments.len()
            || declaration.formals.len() != declaration.bounds.len()
        {
            return FlexibleValue::default();
        }
        let Some(arguments) = self.resolved_call_arg_slots.get(&expression) else {
            return FlexibleValue::default();
        };
        let mut constraints: HashMap<String, Vec<FlexibleValue>> = HashMap::default();
        for argument in &arguments.argument_bindings {
            let Some(&parameter) = declaration.parameters.get(argument.parameter) else {
                continue;
            };
            let parameter = if argument.vararg_element {
                parameter.array_read_elem().unwrap_or(parameter)
            } else {
                parameter
            };
            self.constrain_flexible_parameters(
                &declaration.formals,
                parameter,
                self.expr_types[argument.argument.0 as usize],
                &self.flexible_value(argument.argument),
                &mut constraints,
            );
        }
        let bindings = constraints
            .into_iter()
            .map(|(formal, facts)| {
                let fact = facts
                    .iter()
                    .skip(1)
                    .fold(facts[0].clone(), |known, next| known.meet(next));
                (formal, fact)
            })
            .collect::<HashMap<_, _>>();
        FlexibleValue::rigid_shape(selected).substitute(
            Ty::obj_args_name(
                owner,
                &declaration
                    .formals
                    .iter()
                    .zip(&declaration.bounds)
                    .map(|(formal, bound)| Ty::ty_param(formal, *bound))
                    .collect::<Vec<_>>(),
            ),
            &|formal| bindings.get(formal).cloned(),
        )
    }

    /// Freeze the publication provenance of one expression immediately after checking it. This
    /// consumes only checked identities and decisions staged by the expression-owning checker:
    /// selected calls/constructors, local reads, and already-joined value-flow facts. It never
    /// reopens source syntax.
    pub(super) fn record_flexible_value(&mut self, expression: ExprId) {
        let mut fact = if let Some(fact) = self.flexible_values.reads.get(&expression) {
            fact.clone()
        } else if let Some(fact) = self.flexible_values.staged.get(&expression) {
            fact.clone()
        } else if let Some(call) = self.resolved_calls.get(&expression) {
            self.selected_call_flexible_publication(expression, call)
        } else if self.resolved_constructors.contains_key(&expression) {
            self.constructor_flexible_publication(expression)
        } else {
            FlexibleValue::default()
        };
        if self.flexible_values.nullable_results.contains(&expression) {
            fact = fact.without_head();
        }
        if fact.is_none() {
            self.flexible_values.expressions.remove(&expression);
        } else {
            self.flexible_values.expressions.insert(expression, fact);
        }
    }

    /// Freeze the checked value facts of one inferred declaration before its active source unit is
    /// discarded. `ty` is the already-finalized semantic declaration type.
    pub(super) fn record_inferred_declaration_publication(
        &mut self,
        declaration: DeclarationId,
        value: ExprId,
        ty: Ty,
    ) {
        if ty == Ty::Error || ty.mentions_pending() {
            return;
        }
        let mut result = Spelled::default();
        let fact = self.flexible_value(value);
        if fact.is_none() {
            self.flexible_values
                .declaration_results
                .remove(&declaration);
        } else {
            self.flexible_values
                .declaration_results
                .insert(declaration, fact.clone());
        }
        result.record_flexible_upper_bounds(ty, &|lower| self.flexible_upper_bound(lower));
        fact.apply_to_inferred_declaration(ty, &mut result);
        result.mark_enhanced(&self.value_enhancement(value).declared());
        if result.is_none() {
            return;
        }
        self.inferred_declaration_publications
            .results
            .insert(declaration, result);
    }

    /// A property inferred from an enhanced Java value also commits that unchecked value to its
    /// published declaration type, so it needs the ordinary declaration-boundary guard.
    pub(super) fn record_inferred_property_publication(
        &mut self,
        declaration: DeclarationId,
        initializer: ExprId,
        ty: Ty,
    ) {
        self.record_inferred_declaration_publication(declaration, initializer, ty);
        self.narrow_enhanced_value(ty, initializer);
    }
}

impl TypeInfo {
    /// Metadata facts for the inferred result owned by `declaration`, if that result needs any.
    pub(crate) fn inferred_declaration_publication(
        &self,
        declaration: DeclarationId,
    ) -> Option<&Spelled> {
        self.inferred_declaration_publications
            .results
            .get(&declaration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared_list(variable: Ty) -> Ty {
        Ty::platform_nullable(Ty::obj_args("kotlin/collections/MutableList", &[variable]))
    }

    #[test]
    fn selected_java_variable_flexibility_is_publication_only() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let selected = declared_list(Ty::String);
        let fact = FlexibleValue::selected(declared_list(variable), selected, &|lower| lower);
        let mut spelling = Spelled::default();
        fact.apply(selected, &mut spelling);

        assert_eq!(spelling.flexible_upper_bound, Some(selected.non_null()));
        assert_eq!(spelling.args.len(), 1);
        assert_eq!(spelling.args[0].flexible_upper_bound, Some(Ty::String));
    }

    #[test]
    fn selected_java_variable_shape_survives_a_member_result_substitution() {
        let producer_variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let function = Ty::fun(vec![Ty::String], Ty::String);
        let producer = FlexibleValue::selected(
            declared_list(producer_variable),
            declared_list(function),
            &|lower| lower,
        );
        let FlexibleShape::Obj(_, arguments) = producer.shape else {
            panic!("selected Java list must retain its argument shape");
        };
        let element = arguments[0].clone();
        let member_variable = Ty::ty_param("E", Ty::obj("kotlin/Any"));
        let result = FlexibleValue::default().substitute(member_variable, &|formal| {
            (formal == "E").then(|| element.clone())
        });
        let mut spelling = Spelled::default();
        result.apply(function, &mut spelling);

        assert!(matches!(result.shape, FlexibleShape::Fun { .. }));
        assert_eq!(spelling.flexible_upper_bound, Some(function));
    }

    #[test]
    fn nullable_java_variable_binding_has_no_flexible_occurrence() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let nullable = Ty::nullable(Ty::String);
        let fact =
            FlexibleValue::selected(declared_list(variable), declared_list(nullable), &|lower| {
                lower
            });
        let mut spelling = Spelled::default();
        fact.apply(declared_list(nullable), &mut spelling);

        assert_eq!(spelling.args.len(), 1);
        assert_eq!(spelling.args[0].flexible_upper_bound, None);
    }

    #[test]
    fn rigid_selected_structures_keep_declared_java_intervals_at_each_depth() {
        let declared_leaf = Ty::platform_nullable(Ty::obj("sample/Leaf"));
        let selected_leaf = Ty::obj("sample/Leaf");
        let declared = Ty::platform_nullable(Ty::obj_args("sample/Box", &[declared_leaf]));
        let selected = Ty::obj_args("sample/Box", &[selected_leaf]);
        let fact = FlexibleValue::selected(declared, selected, &Ty::nullable);
        let mut spelling = Spelled::default();
        fact.apply(selected, &mut spelling);

        assert_eq!(
            fact.inferred_declaration_ty(selected),
            Ty::platform_nullable(selected)
        );
        assert_eq!(spelling.flexible_upper_bound, Some(Ty::nullable(selected)));
        assert_eq!(spelling.args.len(), 1);
        assert_eq!(
            spelling.args[0].flexible_upper_bound,
            Some(Ty::nullable(selected_leaf))
        );

        let nullable = Ty::nullable(selected_leaf);
        assert!(FlexibleValue::selected(declared_leaf, nullable, &Ty::nullable).is_none());
    }

    #[test]
    fn mismatched_classifier_or_arity_never_correlates_argument_facts() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        for selected in [
            Ty::platform_nullable(Ty::obj_args("sample/Other", &[Ty::String])),
            Ty::platform_nullable(Ty::obj_args(
                "kotlin/collections/MutableList",
                &[Ty::String, Ty::Int],
            )),
        ] {
            let fact = FlexibleValue::selected(declared_list(variable), selected, &|lower| lower);
            assert!(matches!(fact.shape, FlexibleShape::None));
        }
    }

    #[test]
    fn regular_function_shapes_preserve_flexible_parameters_and_results() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        for (context_count, has_receiver) in [(0, false), (0, true), (1, false)] {
            let parameters = vec![variable; context_count + usize::from(has_receiver) + 1];
            let declared = Ty::fun_with_shape(
                parameters.clone(),
                variable,
                context_count,
                has_receiver,
                false,
            );
            let selected = Ty::fun_with_shape(
                vec![Ty::String; parameters.len()],
                Ty::String,
                context_count,
                has_receiver,
                false,
            );
            let fact = FlexibleValue::selected(declared, selected, &|lower| lower);
            assert!(!fact.is_none());
            let mut spelling = Spelled::default();
            fact.apply(selected, &mut spelling);
            assert_eq!(spelling.args.len(), parameters.len() + 1);
            assert!(spelling
                .args
                .iter()
                .all(|argument| argument.flexible_upper_bound == Some(Ty::String)));
        }
    }

    #[test]
    fn suspend_function_shapes_are_declined_instead_of_guessing_continuation_layout() {
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        for (context_count, has_receiver) in [(0, false), (1, true)] {
            let parameters = vec![variable; context_count + usize::from(has_receiver) + 1];
            let declared = Ty::fun_with_shape(
                parameters.clone(),
                variable,
                context_count,
                has_receiver,
                true,
            );
            let selected = Ty::fun_with_shape(
                vec![Ty::String; parameters.len()],
                Ty::String,
                context_count,
                has_receiver,
                true,
            );
            assert!(FlexibleValue::selected(declared, selected, &|lower| lower).is_none());
        }
    }

    #[test]
    fn overlay_drops_conflicting_flexible_intervals_atomically() {
        let left = FlexibleValue {
            source: Some(Ty::String),
            publication: Some(FlexiblePublication {
                lower: Ty::String,
                shape: FlexibleShape::None,
            }),
            upper: Some(Ty::String),
            shape: FlexibleShape::None,
        };
        let right = FlexibleValue {
            source: Some(Ty::obj("sample/Other")),
            publication: Some(FlexiblePublication {
                lower: Ty::obj("sample/Other"),
                shape: FlexibleShape::None,
            }),
            upper: Some(Ty::nullable(Ty::obj("sample/Other"))),
            shape: FlexibleShape::None,
        };

        let overlaid = left.overlay(&right);
        assert_eq!(overlaid.publication, None);
        assert_eq!(overlaid.upper, None);
    }

    #[test]
    fn meet_drops_both_faces_of_a_partially_matching_flexible_interval() {
        let interval = FlexibleValue {
            source: Some(Ty::String),
            publication: Some(FlexiblePublication {
                lower: Ty::String,
                shape: FlexibleShape::None,
            }),
            upper: Some(Ty::nullable(Ty::String)),
            shape: FlexibleShape::None,
        };
        let same_upper = FlexibleValue {
            publication: Some(FlexiblePublication {
                lower: Ty::obj("sample/Other"),
                shape: FlexibleShape::None,
            }),
            ..interval.clone()
        };
        let same_lower = FlexibleValue {
            upper: Some(Ty::String),
            ..interval.clone()
        };

        for met in [interval.meet(&same_upper), interval.meet(&same_lower)] {
            assert_eq!(met.publication, None);
            assert_eq!(met.upper, None);
        }
    }

    #[test]
    fn a_forwarding_result_substitutes_the_argument_owned_fact() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let variable = Ty::ty_param("T", any);
        let selected = declared_list(Ty::String);
        let argument = FlexibleValue::selected(
            declared_list(Ty::ty_param(
                "E",
                Ty::platform_nullable(Ty::obj("kotlin/Any")),
            )),
            selected,
            &|lower| lower,
        );
        let result = FlexibleValue::default()
            .substitute(variable, &|name| (name == "T").then(|| argument.clone()));

        assert_eq!(result, argument);
    }

    #[test]
    fn projected_result_shape_is_owned_by_the_resolved_target_classifier() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let variable = Ty::ty_param("T", any);
        let nested = FlexibleValue {
            source: Some(Ty::String),
            publication: None,
            upper: Some(Ty::String),
            shape: FlexibleShape::None,
        };
        let selected = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let declared = Ty::obj_args("kotlin/collections/List", &[variable]);
        let projected = FlexibleValue::rigid_shape(selected)
            .substitute(declared, &|name| (name == "T").then(|| nested.clone()));
        let expected_nested = FlexibleValue {
            shape: FlexibleShape::Obj(Ty::String.obj_internal().unwrap(), Vec::new()),
            ..nested
        };

        assert_eq!(projected.source, Some(selected));
        assert_eq!(
            projected.shape,
            FlexibleShape::Obj(
                Ty::obj("kotlin/collections/List").obj_internal().unwrap(),
                vec![expected_nested],
            )
        );
    }

    #[test]
    fn joins_do_not_correlate_slots_from_different_classifier_identities() {
        let nested = FlexibleValue {
            source: Some(Ty::String),
            publication: None,
            upper: Some(Ty::String),
            shape: FlexibleShape::None,
        };
        let left = FlexibleValue {
            source: None,
            publication: None,
            upper: None,
            shape: FlexibleShape::Obj(
                Ty::obj("sample/Left").obj_internal().unwrap(),
                vec![nested.clone()],
            ),
        };
        let right = FlexibleValue {
            source: None,
            publication: None,
            upper: None,
            shape: FlexibleShape::Obj(
                Ty::obj("sample/Right").obj_internal().unwrap(),
                vec![nested],
            ),
        };

        assert!(left.meet(&right).is_none());
    }

    #[test]
    fn substitution_does_not_correlate_slots_from_a_different_classifier() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let nested = FlexibleValue {
            source: Some(Ty::String),
            publication: None,
            upper: Some(Ty::String),
            shape: FlexibleShape::None,
        };
        let selected = FlexibleValue {
            source: None,
            publication: None,
            upper: None,
            shape: FlexibleShape::Obj(
                Ty::obj("sample/Other").obj_internal().unwrap(),
                vec![nested.clone()],
            ),
        };
        let declared = Ty::obj_args("sample/Box", &[Ty::ty_param("T", any)]);

        let result = selected.substitute(declared, &|name| (name == "T").then(|| nested.clone()));
        assert!(result.is_none());
    }

    #[test]
    fn upper_face_flow_does_not_replace_the_provider_publication_shape() {
        let provider = declared_list(Ty::String);
        let variable = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));
        let current = Ty::platform_nullable(Ty::obj_args("kotlin/collections/List", &[Ty::String]));
        let initializer =
            FlexibleValue::selected(declared_list(variable), provider, &|lower| lower);
        let projected = FlexibleValue {
            source: Some(current.non_null()),
            publication: initializer.publication.clone(),
            upper: None,
            shape: FlexibleValue::rigid_shape(current).shape,
        };

        assert_eq!(projected.inferred_declaration_ty(current), provider);
        let mut spelling = Spelled::default();
        spelling.record_flexible_upper_bounds(provider, &|lower| lower);
        projected.apply_to_inferred_declaration(provider, &mut spelling);
        assert_eq!(spelling.args[0].flexible_upper_bound, Some(Ty::String));
    }
}
