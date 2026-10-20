//! kotlinc's readable rendering of a declaration inside a diagnostic.
//!
//! `NOTHING_TO_OVERRIDE` and the abstract-member diagnostics list declarations one per line in the
//! form kotlinc's FIR renderer produces for readability:
//!
//! ```text
//! context(c: T) suspend fun <R : Comparable<R!>!> String.name(vararg xs: T, y: Int = ...): R!
//! var <Q> List<Q>.name: Q
//! ```
//!
//! Only `suspend`, `reified` and `vararg` survive among the modifiers; visibility, modality,
//! `operator`, `infix` and annotations are dropped, and a default value is `= ...`. Types use
//! classifier short names (`Map.Entry`), expand aliases, and spell a Java type's flexibility as
//! kotlinc does: `String!`, `(Mutable)List<T!>!`, `Array<(out) String!>!`.

use crate::libraries::{CallSig, FunctionInfo, LibraryMember, PropertyInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{CollectionKind, MappedCollection, Ty, TypeName, TypeVariance};

/// The tail of `NOTHING_TO_OVERRIDE`: each candidate as substituted at the supertype, one per line,
/// under kotlinc's heading, or nothing when there is no candidate.
pub(super) fn potential_overrides<'a>(
    source: &dyn SymbolSource,
    index: Option<&crate::fir::ResolvedModuleIndex>,
    candidates: impl Iterator<Item = &'a FunctionInfo>,
) -> String {
    let mut declarations = Vec::<&FunctionInfo>::new();
    for candidate in candidates {
        // Each direct supertype is queried independently. A declaration inherited through both
        // sides of a diamond consequently reaches this boundary twice, but it is still one
        // potential signature. Stable declaration identities, rather than its rendered shape,
        // decide that the two views are the same declaration.
        if !declarations
            .iter()
            .any(|declaration| same_function_declaration(declaration, candidate))
        {
            declarations.push(candidate);
        }
    }
    let candidates = declarations
        .into_iter()
        .map(|candidate| {
            let defaults = potential_override_defaults(source, index, candidate);
            format!(
                "\n{}",
                function_declaration(
                    source,
                    candidate,
                    &defaults,
                    LegacyContextStyle::UnusedVariable
                )
            )
        })
        .collect::<String>();
    if candidates.is_empty() {
        candidates
    } else {
        format!(" Potential signatures for overriding:{candidates}")
    }
}

/// A function as its classifier declares it.
pub(super) fn declared_function(source: &dyn SymbolSource, function: &FunctionInfo) -> String {
    function_declaration(
        source,
        function,
        &function.call_sig.param_defaults,
        LegacyContextStyle::UnusedVariable,
    )
}

/// A function declaration in an access diagnostic. Kotlinc renders a legacy context receiver here
/// as its bare type, unlike the `<unused var>@Type` spelling used by override diagnostics.
pub(super) fn access_function(source: &dyn SymbolSource, function: &FunctionInfo) -> String {
    function_declaration(
        source,
        function,
        &function.call_sig.param_defaults,
        LegacyContextStyle::BareType,
    )
}

/// One function declaration, as kotlinc lists it under a diagnostic.
fn function_declaration(
    source: &dyn SymbolSource,
    function: &FunctionInfo,
    declared_defaults: &[bool],
    legacy_context_style: LegacyContextStyle,
) -> String {
    let signature = function.semantic_signature();
    FunctionDeclarationShape {
        name: &function.callable.name,
        suspend: function.flags.suspend || function.callable.suspend,
        first_reified: function.flags.reified,
        // Member specialization stores the applied dispatch type in `receiver`; that is not a
        // source extension receiver and must never enter the declaration spelling.
        receiver: function
            .is_extension()
            .then(|| function.semantic_receiver())
            .flatten(),
        context_count: function.context_count,
        static_member: false,
        signature: &signature,
        call_sig: &function.call_sig,
        declared_defaults,
        legacy_context_style,
    }
    .render(source)
}

/// A selected instance member as its declaration is rendered in an access diagnostic.
///
/// Selection has already normalized the provider into [`LibraryMember`]. Its generic signature is
/// the declaration-owned source shape; a non-generic member's `params` are already semantic. This
/// boundary therefore consumes one exact shape and never infers source arity from names, identities,
/// or physical backend parameters.
pub(super) fn declared_member(source: &dyn SymbolSource, member: &LibraryMember) -> String {
    let signature = member
        .generic_sig
        .clone()
        .unwrap_or_else(|| crate::libraries::GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: member.semantic_extension_receiver(),
            params: member.params.clone(),
            ret: member.ret,
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        });
    FunctionDeclarationShape {
        name: &member.name,
        suspend: member.suspend(),
        first_reified: member.reified,
        receiver: member.semantic_extension_receiver(),
        context_count: member.context_count,
        static_member: false,
        signature: &signature,
        call_sig: &member.call_sig,
        declared_defaults: &member.call_sig.param_defaults,
        legacy_context_style: LegacyContextStyle::BareType,
    }
    .render(source)
}

/// A selected constructor as its classifier declares it. Selection specializes `params` and
/// `ret`; the declaration-owned generic signature retains the source type parameters and result.
pub(super) fn declared_constructor(
    source: &dyn SymbolSource,
    owner: TypeName,
    member: &LibraryMember,
) -> String {
    let signature = member
        .generic_sig
        .clone()
        .unwrap_or_else(|| crate::libraries::GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: member.params.clone(),
            ret: Ty::obj_name(owner),
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        });
    let type_parameters = type_parameters_display(
        source,
        signature.formals.iter().enumerate().map(|(index, formal)| {
            (
                formal.as_str(),
                signature
                    .formal_bounds
                    .get(index)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                false,
            )
        }),
    );
    let type_parameters = type_parameters.trim_end();
    let parameters = signature
        .params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            let name = member
                .call_sig
                .param_names
                .get(index)
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("p{index}"));
            let default = if member.call_sig.param_defaults.get(index) == Some(&true) {
                " = ..."
            } else {
                ""
            };
            format!("{name}: {}{default}", readable_type(source, *ty))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "constructor{type_parameters}({parameters}): {}",
        readable_type(source, signature.ret)
    )
}

struct FunctionDeclarationShape<'a> {
    name: &'a str,
    suspend: bool,
    first_reified: bool,
    receiver: Option<Ty>,
    context_count: usize,
    static_member: bool,
    signature: &'a crate::libraries::GenericSig,
    call_sig: &'a CallSig,
    declared_defaults: &'a [bool],
    legacy_context_style: LegacyContextStyle,
}

#[derive(Clone, Copy)]
enum LegacyContextStyle {
    UnusedVariable,
    BareType,
}

impl FunctionDeclarationShape<'_> {
    fn render(&self, source: &dyn SymbolSource) -> String {
        let context_count = self.context_count.min(self.signature.params.len());
        let names = &self.call_sig.param_names;
        // A classfile without parameter names gives kotlinc's `p0`, `p1`, … by value-parameter ordinal.
        let parameter_name = |index: usize| -> String {
            names
                .get(index)
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("p{}", index.saturating_sub(context_count)))
        };
        let context = context_display(
            source,
            self.signature.params[..context_count]
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    (
                        self.call_sig.parameter_identities.get(index),
                        parameter_name(index),
                        *ty,
                    )
                }),
            self.legacy_context_style,
        );
        let suspend = if self.suspend { "suspend " } else { "" };
        let static_prefix = if self.static_member { "static " } else { "" };
        let reified = |index: usize| {
            self.call_sig
                .reified_type_parameter_ordinals
                .contains(&(index as u32))
                || (self.first_reified && index == 0)
        };
        let type_parameters = type_parameters_display(
            source,
            self.signature
                .formals
                .iter()
                .enumerate()
                .map(|(index, formal)| {
                    (
                        formal.as_str(),
                        self.signature
                            .formal_bounds
                            .get(index)
                            .map(Vec::as_slice)
                            .unwrap_or_default(),
                        reified(index),
                    )
                }),
        );
        let receiver = self
            .receiver
            .map(|receiver| format!("{}.", readable_type(source, receiver)))
            .unwrap_or_default();
        let parameters = self.signature.params[context_count..]
            .iter()
            .enumerate()
            .map(|(offset, ty)| {
                let index = context_count + offset;
                let vararg = self.call_sig.vararg_index == Some(index);
                let ty = if vararg {
                    ty.array_read_elem().unwrap_or(*ty)
                } else {
                    *ty
                };
                let default = if self.declared_defaults.get(index).copied().unwrap_or(false) {
                    " = ..."
                } else {
                    ""
                };
                format!(
                    "{}{}: {}{default}",
                    if vararg { "vararg " } else { "" },
                    parameter_name(index),
                    readable_type(source, ty)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{context}{static_prefix}{suspend}fun {type_parameters}{receiver}{}({parameters}): {}",
            self.name,
            readable_type(source, self.signature.ret)
        )
    }
}

/// A selected classifier-static function in an access diagnostic.
pub(super) fn declared_static_function(
    source: &dyn SymbolSource,
    function: &FunctionInfo,
) -> String {
    let signature = function.semantic_signature();
    FunctionDeclarationShape {
        name: &function.callable.name,
        suspend: function.flags.suspend || function.callable.suspend,
        first_reified: function.flags.reified,
        receiver: None,
        context_count: function.context_count,
        static_member: true,
        signature: &signature,
        call_sig: &function.call_sig,
        declared_defaults: &function.call_sig.param_defaults,
        legacy_context_style: LegacyContextStyle::BareType,
    }
    .render(source)
}

/// Whether two specialized views retain one exact source/dependency declaration identity.
pub(super) fn same_function_declaration(left: &FunctionInfo, right: &FunctionInfo) -> bool {
    if let (Some(left), Some(right)) = (left.stable_declaration, right.stable_declaration) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (left.source_member, right.source_member) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (
        left.callable.external_identity,
        right.callable.external_identity,
    ) {
        return left == right;
    }
    false
}

/// Whether two property views retain one exact source/dependency declaration identity.
pub(super) fn same_property_declaration(left: &PropertyInfo, right: &PropertyInfo) -> bool {
    if let (Some(left), Some(right)) = (left.stable_declaration, right.stable_declaration) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (left.source_member, right.source_member) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (
        left.getter.external_property_identity,
        right.getter.external_property_identity,
    ) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (
        left.getter.external_identity,
        right.getter.external_identity,
    ) {
        return left == right;
    }
    false
}

/// Whether `declaration` is the exact property `obligation` or an exact callable override of it.
/// A mapped Java method such as `AbstractCollection.size()` is normalized to the semantic
/// `Collection.size` property, but keeps its own external property identity. The common hierarchy
/// has already recorded the overridden declaration identity on that exact getter; consume that
/// edge rather than equating the two properties by spelling or JVM shape.
pub(super) fn property_declares_obligation(
    declaration: &PropertyInfo,
    obligation: &PropertyInfo,
) -> bool {
    if same_property_declaration(declaration, obligation) {
        return true;
    }
    if declaration.kind != crate::libraries::PropKind::Member
        || obligation.kind != crate::libraries::PropKind::Member
    {
        return false;
    }
    declaration.getter.overridden_declarations.contains(
        &crate::symbol_resolver::property_declaration_identity(obligation),
    ) || obligation.getter.overridden_declarations.contains(
        &crate::symbol_resolver::property_declaration_identity(declaration),
    )
}

/// Defaults written on this exact declaration, not defaults made available by an overridden
/// declaration. Hierarchy normalization deliberately merges inherited default availability into a
/// candidate for call selection. Diagnostics instead render declaration syntax, so read the direct
/// classifier declaration by stable identity. A member specialized from a generic owner is a
/// substituted copy in kotlinc and carries no displayed defaults.
fn potential_override_defaults(
    source: &dyn SymbolSource,
    index: Option<&crate::fir::ResolvedModuleIndex>,
    candidate: &FunctionInfo,
) -> Vec<bool> {
    let owner = match candidate.callable.declaration_owner {
        Some(crate::types::SemanticCallableOwner::Classifier(owner)) => owner,
        _ => candidate.callable.owner,
    };
    let Some(classifier) = source.classifier(owner) else {
        return Vec::new();
    };
    if !classifier.type_params().is_empty() {
        return Vec::new();
    }
    let inherited_source_default = index.is_some_and(|index| {
        candidate
            .stable_declaration
            .and_then(|declaration| index.callable_for_declaration(declaration))
            .is_some_and(|callable| index.callable_default_provider(callable.id).is_some())
    });
    if inherited_source_default || candidate.callable.external_default_provider.is_some() {
        return Vec::new();
    }
    classifier
        .declared_callables
        .get(&candidate.callable.name)
        .and_then(|callables| {
            callables
                .functions()
                .iter()
                .find(|declaration| same_function_declaration(declaration, candidate))
        })
        .map(|declaration| declaration.call_sig.param_defaults)
        .unwrap_or_default()
        .to_vec()
}

/// A property as its classifier declares it.
pub(super) fn declared_property(source: &dyn SymbolSource, property: &PropertyInfo) -> String {
    let context = context_display(
        source,
        property
            .getter
            .params
            .iter()
            .take(property.context_count)
            .enumerate()
            .map(|(index, ty)| {
                (
                    property.context_parameter_identities.get(index),
                    property
                        .context_param_names
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| format!("p{index}")),
                    *ty,
                )
            }),
        LegacyContextStyle::UnusedVariable,
    );
    let keyword = if property.setter.is_some() {
        "var"
    } else {
        "val"
    };
    let generic = property.getter.generic_sig.as_deref();
    let type_parameters = type_parameters_display(
        source,
        property.formals.iter().enumerate().map(|(index, formal)| {
            (
                formal.as_str(),
                generic
                    .expect("generic property has an accessor generic signature")
                    .formal_bounds
                    .get(index)
                    .expect("generic property bounds are parallel to its formals")
                    .as_slice(),
                false,
            )
        }),
    );
    let receiver = match property.kind {
        crate::libraries::PropKind::Extension | crate::libraries::PropKind::MemberExtension => {
            property
                .receiver
                .map(|receiver| format!("{}.", readable_type(source, receiver)))
                .unwrap_or_default()
        }
        _ => String::new(),
    };
    format!(
        "{context}{keyword} {type_parameters}{receiver}{}: {}",
        property.name,
        readable_type(source, property.ty)
    )
}

/// `context(a: A, _: B, <unused var>@C) ` with its trailing separator, or empty. Context role is a
/// typed declaration fact: the readable renderer distinguishes legacy receivers from named and
/// anonymous context parameters. A missing identity retains the provider's diagnostic name.
fn context_display<'a>(
    source: &dyn SymbolSource,
    parameters: impl Iterator<
        Item = (
            Option<&'a crate::fir::ResolvedParameterIdentity>,
            String,
            Ty,
        ),
    >,
    legacy_context_style: LegacyContextStyle,
) -> String {
    let parameters = parameters
        .map(|(identity, fallback_name, ty)| {
            let ty = readable_type(source, ty);
            match identity {
                Some(crate::fir::ResolvedParameterIdentity::LegacyContextReceiver { .. }) => {
                    match legacy_context_style {
                        LegacyContextStyle::UnusedVariable => format!("<unused var>@{ty}"),
                        LegacyContextStyle::BareType => ty,
                    }
                }
                Some(crate::fir::ResolvedParameterIdentity::ContextValue {
                    source_name, ..
                }) => format!("{source_name}: {ty}"),
                Some(crate::fir::ResolvedParameterIdentity::AnonymousContextParameter {
                    ..
                }) => format!("_: {ty}"),
                _ => format!("{fallback_name}: {ty}"),
            }
        })
        .collect::<Vec<_>>();
    if parameters.is_empty() {
        String::new()
    } else {
        format!("context({}) ", parameters.join(", "))
    }
}

/// `<reified A : X, Y, B> ` with its trailing separator, or empty. kotlinc folds every bound of a
/// formal into the list, `where` clauses included, and omits Kotlin's implicit `Any?`; a Java
/// formal's implicit bound is the flexible `Any!`, which it shows.
fn type_parameters_display<'a>(
    source: &dyn SymbolSource,
    formals: impl Iterator<Item = (&'a str, &'a [Ty], bool)>,
) -> String {
    let formals = formals
        .map(|(formal, bounds, reified)| {
            let bounds = bounds
                .iter()
                .copied()
                .filter(|bound| !implicit_upper_bound(*bound))
                .map(|bound| readable_type(source, bound))
                .collect::<Vec<_>>();
            format!(
                "{}{}{}",
                if reified { "reified " } else { "" },
                crate::types::type_parameter_source_name(formal),
                if bounds.is_empty() {
                    String::new()
                } else {
                    format!(" : {}", bounds.join(", "))
                }
            )
        })
        .collect::<Vec<_>>();
    if formals.is_empty() {
        String::new()
    } else {
        format!("<{}> ", formals.join(", "))
    }
}

/// A declaration owner's formal list without the surrounding angle brackets. Upper bounds use
/// the same semantic renderer as every other declaration diagnostic, and Kotlin's implicit
/// `Any?` bound is omitted.
pub(super) fn type_parameter_owner_formals(
    source: &dyn SymbolSource,
    formals: &[String],
    bounds: &[Vec<Ty>],
    variances: &[TypeVariance],
) -> String {
    assert_eq!(
        formals.len(),
        bounds.len(),
        "declaration formals and bounds must be parallel"
    );
    formals
        .iter()
        .zip(bounds)
        .enumerate()
        .map(|(index, (formal, bounds))| {
            formal_display(
                source,
                variances.get(index).copied(),
                crate::types::type_parameter_source_name(formal),
                bounds,
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One declared formal: `in T : Bound`, omitting Kotlin's implicit `Any?` bound.
fn formal_display(
    source: &dyn SymbolSource,
    variance: Option<TypeVariance>,
    name: &str,
    bounds: &[Ty],
) -> String {
    let variance = match variance {
        Some(TypeVariance::In) => "in ",
        Some(TypeVariance::Out) => "out ",
        Some(TypeVariance::Invariant) | None => "",
    };
    let bounds = bounds
        .iter()
        .copied()
        .filter(|bound| !implicit_upper_bound(*bound))
        .map(|bound| readable_type(source, bound))
        .collect::<Vec<_>>();
    if bounds.is_empty() {
        format!("{variance}{name}")
    } else {
        format!("{variance}{name} : {}", bounds.join(", "))
    }
}

/// kotlinc's rendering of an `inner` class symbol, `inner class Inner<U, Outer(T) : B> : Super<T>`:
/// its own formals, then each formal it captures from an enclosing class as `Outer(T)`, then its
/// direct supertypes.
pub(super) fn inner_class_declaration(
    source: &dyn SymbolSource,
    internal: TypeName,
    shape: &crate::libraries::LibraryType,
) -> String {
    let own = shape.own_type_parameter_count;
    let formals = shape
        .type_params()
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let name = crate::types::type_parameter_source_name(formal);
            let name = if index < own {
                name.to_string()
            } else {
                format!("Outer({name})")
            };
            let bounds = shape
                .type_param_bounds()
                .get(index)
                .map_or(&[][..], Vec::as_slice);
            formal_display(
                source,
                shape.type_param_variances().get(index).copied(),
                &name,
                bounds,
            )
        })
        .collect::<Vec<_>>();
    let formals = if formals.is_empty() {
        String::new()
    } else {
        format!("<{}>", formals.join(", "))
    };
    // `Any` is a declared supertype only of a class that names no other one.
    let supertypes = shape
        .supertype_templates
        .iter()
        .filter(|supertype| !supertype.is_erased_top())
        .map(|supertype| readable_type(source, *supertype))
        .collect::<Vec<_>>();
    let supertypes = if supertypes.is_empty() {
        "Any".to_string()
    } else {
        supertypes.join(", ")
    };
    format!(
        "inner class {}{formals} : {supertypes}",
        internal.nested_segment_ref()
    )
}

/// The bound a Kotlin type parameter has when none is written.
fn implicit_upper_bound(bound: Ty) -> bool {
    matches!(bound, Ty::Nullable(inner) if inner.is_erased_top())
}

/// A type as kotlinc renders it for readability.
pub(super) fn readable_type(source: &dyn SymbolSource, ty: Ty) -> String {
    match ty {
        Ty::Obj(name, arguments) => classifier_display(source, name, arguments.as_slice(), false),
        Ty::Fun(function) => {
            let context = if function.context_count == 0 {
                String::new()
            } else {
                format!(
                    "context({}) ",
                    function.params[..function.context_count]
                        .iter()
                        .copied()
                        .map(|ty| readable_type(source, ty))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let mut values = &function.params[function.context_count.min(function.params.len())..];
            let receiver = if function.has_receiver {
                let receiver = values.first().copied();
                values = values.get(1..).unwrap_or_default();
                receiver
                    .map(|receiver| format!("{}.", function_receiver_display(source, receiver)))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            format!(
                "{}{context}{receiver}({}) -> {}",
                if function.suspend { "suspend " } else { "" },
                values
                    .iter()
                    .copied()
                    .map(|ty| readable_type(source, ty))
                    .collect::<Vec<_>>()
                    .join(", "),
                readable_type(source, function.ret)
            )
        }
        Ty::Nullable(inner) => match *inner {
            Ty::Fun(_) | Ty::DefinitelyNotNull(_) | Ty::Intersection(_) => {
                format!("({})?", readable_type(source, *inner))
            }
            Ty::TyParam(name, _) => {
                format!(
                    "{}?",
                    crate::types::type_parameter_source_name(name.as_str())
                )
            }
            inner => format!("{}?", readable_type(source, inner)),
        },
        Ty::PlatformNullable(inner) => match *inner {
            Ty::Obj(name, arguments) => {
                format!(
                    "{}!",
                    classifier_display(source, name, arguments.as_slice(), true)
                )
            }
            Ty::TyParam(name, _) => {
                format!(
                    "{}!",
                    crate::types::type_parameter_source_name(name.as_str())
                )
            }
            Ty::Fun(_) | Ty::DefinitelyNotNull(_) | Ty::Intersection(_) => {
                format!("({})!", readable_type(source, *inner))
            }
            inner => format!("{}!", readable_type(source, inner)),
        },
        Ty::InProjection(inner) => format!("in {}", readable_type(source, *inner)),
        Ty::OutProjection(inner) => format!("out {}", readable_type(source, *inner)),
        Ty::StarProjection(_) => "*".to_string(),
        Ty::TyParam(name, bound) => format!(
            "{}{}",
            crate::types::type_parameter_source_name(name.as_str()),
            if matches!(*bound, Ty::PlatformNullable(_)) {
                "!"
            } else {
                ""
            }
        ),
        Ty::DefinitelyNotNull(inner) => format!("{} & Any", readable_type(source, *inner)),
        Ty::Intersection(parts) => parts
            .iter()
            .copied()
            .map(|ty| readable_type(source, ty))
            .collect::<Vec<_>>()
            .join(" & "),
        other => other.source_name(),
    }
}

/// A function type's receiver is parenthesized when it would otherwise read as part of the
/// function type: a nullable, flexible, function or intersection type.
fn function_receiver_display(source: &dyn SymbolSource, receiver: Ty) -> String {
    let display = readable_type(source, receiver);
    match receiver {
        Ty::Nullable(_)
        | Ty::PlatformNullable(_)
        | Ty::Fun(_)
        | Ty::DefinitelyNotNull(_)
        | Ty::Intersection(_) => format!("({display})"),
        _ => display,
    }
}

/// A classifier by its nested short name with its arguments. A flexible Java collection names its
/// mutability `(Mutable)` on each mutable segment, and a flexible reference array marks its element
/// `(out)`, because the flexible type spans both readings.
fn classifier_display(
    source: &dyn SymbolSource,
    name: TypeName,
    arguments: &[Ty],
    flexible: bool,
) -> String {
    if name == crate::types::wk::java_object() {
        return classifier_segment_display(source, "Any", arguments, false);
    }
    if let Some(collection) = flexible
        .then(|| flexible_collection_display(source, name))
        .flatten()
    {
        return classifier_segment_display(source, collection, arguments, false);
    }
    let classifier = source.classifier(name);
    // FIR's diagnostic renderer keeps a Kotlin classifier's nested path as one classifier
    // application. Its arguments are likewise one innermost-first list:
    // `Outer<A>.Inner<B>` is shown as `Outer.Inner<B, A>`. Preserve the exact nested TypeName for
    // semantics and flatten only at this diagnostic boundary. Java nested classifiers continue to
    // use their declaration-owned segment arities below.
    if name.nested_owner().is_some()
        && classifier
            .as_ref()
            .is_some_and(|classifier| classifier.is_kotlin)
    {
        return classifier_segment_display(
            source,
            &name.segment_ref().replace('$', "."),
            arguments,
            false,
        );
    }
    if name.nested_owner().is_some()
        || classifier.is_some_and(|classifier| classifier.outer_instance.is_some())
    {
        return nested_classifier_display(source, name, arguments);
    }
    classifier_segment_display(
        source,
        &name.segment_ref().replace('$', "."),
        arguments,
        flexible && name == crate::types::wk::array(),
    )
}

/// A nested classifier's arguments are stored from the innermost declaration outwards: an inner
/// `Outer<A>.Inner<B>` is `Inner<B, A>`, while a non-inner nested declaration carries only its own
/// arguments. Declaration-owned arities partition that semantic list without inspecting a rendered
/// classifier spelling.
fn nested_classifier_display(
    source: &dyn SymbolSource,
    name: TypeName,
    arguments: &[Ty],
) -> String {
    let mut current = Some(name);
    let mut argument = 0usize;
    let mut segments = Vec::<(&str, &[Ty])>::new();
    while let Some(classifier_name) = current {
        let classifier = source.classifier(classifier_name);
        // An `inner` classifier publishes its captured enclosing classifier as a semantic identity.
        // Prefer that declaration fact: a classpath name may have been interned before its lexical
        // owner, so the name tree alone need not yet carry the exact nested-owner edge.
        let enclosing = classifier
            .as_ref()
            .and_then(|classifier| classifier.outer_instance)
            .or_else(|| classifier_name.nested_owner());
        let own_arity = classifier
            .as_ref()
            .map(|classifier| classifier.own_type_parameter_count)
            .unwrap_or(0);
        let remaining = arguments.len().saturating_sub(argument);
        // The final enclosing segment can be absent from a lazy federated source even though the
        // inner declaration published its stable identity. Arguments are stored innermost-out, so
        // after every inner segment has consumed its declaration-owned prefix, the remaining suffix
        // belongs to that outermost segment. Preserve that semantic partition without deriving an
        // arity from the classifier's rendered spelling.
        let count = if own_arity == 0 && enclosing.is_none() && !segments.is_empty() {
            remaining
        } else {
            own_arity.min(remaining)
        };
        let own_arguments = &arguments[argument..argument + count];
        segments.push((classifier_name.nested_segment_ref(), own_arguments));
        argument += count;
        current = enclosing;
    }
    // Metadata can omit the innermost declaration's own arity while still publishing the complete
    // captured argument list and enclosing chain. The enclosing classifiers' recorded slices remain
    // authoritative; assign only the unclaimed prefix to the innermost segment, then rebuild every
    // slice in semantic innermost-to-outermost order.
    if argument < arguments.len() {
        let mut counts = segments
            .iter()
            .map(|(_, arguments)| arguments.len())
            .collect::<Vec<_>>();
        counts[0] += arguments.len() - argument;
        let mut offset = 0;
        for ((_, segment_arguments), count) in segments.iter_mut().zip(counts) {
            *segment_arguments = &arguments[offset..offset + count];
            offset += count;
        }
    }
    segments
        .into_iter()
        .rev()
        .map(|(segment, arguments)| classifier_segment_display(source, segment, arguments, false))
        .collect::<Vec<_>>()
        .join(".")
}

fn classifier_segment_display(
    source: &dyn SymbolSource,
    base: &str,
    arguments: &[Ty],
    reference_array: bool,
) -> String {
    if arguments.is_empty() {
        return base.to_string();
    }
    let arguments = arguments
        .iter()
        .copied()
        .map(|argument| {
            let display = readable_type(source, argument);
            match argument {
                Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_) => display,
                _ if reference_array => format!("(out) {display}"),
                _ => display,
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{base}<{arguments}>")
}

/// The `(Mutable)` spelling of a flexible collection type, which a platform signature exposes as
/// both collection faces. The owning provider publishes this semantic role on the classifier;
/// diagnostics never reconstruct it from a Kotlin or JVM spelling.
fn flexible_collection_display(source: &dyn SymbolSource, name: TypeName) -> Option<&'static str> {
    let MappedCollection { kind, .. } = source.classifier(name)?.mapped_collection?;
    Some(match kind {
        CollectionKind::Iterator => "(Mutable)Iterator",
        CollectionKind::Iterable => "(Mutable)Iterable",
        CollectionKind::Collection => "(Mutable)Collection",
        CollectionKind::List => "(Mutable)List",
        CollectionKind::ListIterator => "(Mutable)ListIterator",
        CollectionKind::Set => "(Mutable)Set",
        CollectionKind::Map => "(Mutable)Map",
        CollectionKind::MapEntry => "(Mutable)Map.(Mutable)Entry",
    })
}

#[cfg(test)]
mod tests {
    use super::{context_display, readable_type, LegacyContextStyle};
    use crate::fir::ResolvedParameterIdentity;
    use crate::libraries::{EmptySymbolSource, LibraryType, ResolvedSymbols};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, CollectionKind, MappedCollection, Ty, TypeName};

    struct RoleSource {
        identity: TypeName,
        classifier: std::sync::Arc<LibraryType>,
    }

    impl SymbolSource for RoleSource {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
            let requested = namespace.existing_classifier(name);
            std::rc::Rc::new(ResolvedSymbols {
                classifier_name: (requested == Some(self.identity)).then_some(self.identity),
                classifier: (requested == Some(self.identity)).then(|| self.classifier.clone()),
                ..ResolvedSymbols::default()
            })
        }
    }

    struct NestedRoleSource {
        classifiers: [(TypeName, std::sync::Arc<LibraryType>); 2],
    }

    impl SymbolSource for NestedRoleSource {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
            let requested = namespace.existing_classifier(name);
            let classifier = requested.and_then(|requested| {
                self.classifiers
                    .iter()
                    .find(|(identity, _)| *identity == requested)
            });
            std::rc::Rc::new(ResolvedSymbols {
                classifier_name: classifier.map(|(identity, _)| *identity),
                classifier: classifier.map(|(_, classifier)| classifier.clone()),
                ..ResolvedSymbols::default()
            })
        }
    }

    #[test]
    fn context_display_uses_the_recorded_parameter_roles() {
        let named = ResolvedParameterIdentity::ContextValue {
            ordinal: 0,
            source_name: "named".into(),
        };
        let anonymous = ResolvedParameterIdentity::AnonymousContextParameter { ordinal: 1 };
        let legacy = ResolvedParameterIdentity::LegacyContextReceiver { ordinal: 2 };
        let source = EmptySymbolSource;

        assert_eq!(
            context_display(
                &source,
                [
                    (Some(&named), "wrong".to_string(), Ty::Int),
                    (Some(&anonymous), "wrong".to_string(), Ty::Long),
                    (Some(&legacy), "wrong".to_string(), Ty::Boolean),
                ]
                .into_iter(),
                LegacyContextStyle::UnusedVariable,
            ),
            "context(named: Int, _: Long, <unused var>@Boolean) "
        );
        assert_eq!(
            context_display(
                &source,
                [(Some(&legacy), "wrong".to_string(), Ty::Boolean)].into_iter(),
                LegacyContextStyle::BareType,
            ),
            "context(Boolean) "
        );
    }

    #[test]
    fn flexible_collection_display_consumes_the_provider_role_not_the_classifier_spelling() {
        let empty = EmptySymbolSource;
        let mut classifier = (*empty
            .classifier(crate::types::wk::any())
            .expect("core Any classifier"))
        .clone();
        classifier.mapped_collection = Some(MappedCollection {
            kind: CollectionKind::List,
            mutable: false,
        });
        let identity = type_name("fixture/OpaquePlatformType");
        let source = RoleSource {
            identity,
            classifier: std::sync::Arc::new(classifier),
        };

        assert_eq!(
            readable_type(
                &source,
                Ty::platform_nullable(Ty::obj_args_name(identity, &[Ty::String])),
            ),
            "(Mutable)List<String>!"
        );
    }

    #[test]
    fn java_declaration_variables_render_their_flexible_occurrence_once() {
        let source = EmptySymbolSource;
        let formal = Ty::ty_param("K", Ty::platform_nullable(Ty::obj("kotlin/Any")));

        assert_eq!(readable_type(&source, formal), "K!");
        assert_eq!(readable_type(&source, Ty::platform_nullable(formal)), "K!");
        assert_eq!(readable_type(&source, Ty::nullable(formal)), "K?");
    }

    #[test]
    fn nested_display_places_remaining_arguments_on_an_unloaded_outer_identity() {
        let empty = EmptySymbolSource;
        let outer = type_name("fixture/Outer");
        let inner = crate::types::type_name_nested_child(outer, "Inner");
        let mut classifier = (*empty
            .classifier(crate::types::wk::any())
            .expect("core Any classifier"))
        .clone();
        classifier.outer_instance = Some(outer);
        classifier.own_type_parameter_count = 1;
        let source = RoleSource {
            identity: inner,
            classifier: std::sync::Arc::new(classifier),
        };

        assert_eq!(
            readable_type(&source, Ty::obj_args_name(inner, &[Ty::Int, Ty::String])),
            "Outer<String>.Inner<Int>"
        );
    }

    #[test]
    fn nested_display_assigns_only_unclaimed_arguments_to_the_innermost_segment() {
        let empty = EmptySymbolSource;
        let outer = type_name("diag_2245/Envelope");
        let inner = crate::types::type_name_nested_child(outer, "Member");
        let mut outer_classifier = (*empty
            .classifier(crate::types::wk::any())
            .expect("core Any classifier"))
        .clone();
        outer_classifier.own_type_parameter_count = 1;
        let mut inner_classifier = outer_classifier.clone();
        inner_classifier.outer_instance = Some(outer);
        inner_classifier.own_type_parameter_count = 0;
        let source = NestedRoleSource {
            classifiers: [
                (outer, std::sync::Arc::new(outer_classifier)),
                (inner, std::sync::Arc::new(inner_classifier)),
            ],
        };

        assert_eq!(
            readable_type(&source, Ty::obj_args_name(inner, &[Ty::Int, Ty::String])),
            "Envelope<String>.Member<Int>"
        );
    }
}
