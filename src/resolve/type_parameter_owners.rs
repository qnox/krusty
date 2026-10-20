//! kotlinc's diagnostic wording for a declaration-owned type parameter: `T (of fun <T : B> f)` or
//! `X (of class Box<out X, in Y : Number>)`. The owner text lists every formal of the declaration
//! with its variance and declared bounds (an explicit `Any?` bound is the default and is omitted,
//! and `reified` is not part of the wording). Owners are keyed by the formal's semantic identity,
//! which is unique per declaration. A function's formals are named with their owner only while that
//! function is checked; a formal leaking out through an unsolved call keeps its bare name.

use super::{Checker, CheckerScope};
use crate::ast::{ClassDecl, ClassKind, DeclId, FunDecl, PropDecl, TypeRef};
use crate::fx_hash::HashMap;
use crate::libraries::LibraryType;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName, TypeVariance};

/// kotlinc's disambiguation of same-named type parameters within one diagnostic: when two distinct
/// parameters share a source name, each is written `T#N`, numbered in the order the diagnostic's
/// arguments first mention them. A name used by one parameter only stays bare.
#[derive(Default)]
pub(super) struct TypeParameterNumbering {
    numbers: HashMap<&'static str, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_publication_of_the_same_formal_restores_the_outer_diagnostic_state() {
        let identity = crate::types::intern("fixtures:map:R");
        let mut owners = HashMap::default();
        let mut uninferred = HashMap::default();

        let mut outer = TypeParameterOwnerScope::default();
        outer.publish(
            &mut owners,
            &mut uninferred,
            identity,
            "fun <R : Any!> map".to_string(),
            Some(true),
        );
        let mut inner = TypeParameterOwnerScope::default();
        inner.publish(
            &mut owners,
            &mut uninferred,
            identity,
            "fun <R : Any!> map".to_string(),
            None,
        );

        inner.restore(&mut owners, &mut uninferred);
        assert_eq!(
            owners.get(identity).map(String::as_str),
            Some("fun <R : Any!> map")
        );
        assert_eq!(uninferred.get(identity), Some(&true));

        outer.restore(&mut owners, &mut uninferred);
        assert!(!owners.contains_key(identity));
        assert!(!uninferred.contains_key(identity));
    }
}

/// Previous diagnostic state for one bounded owner publication. Recursive or nested checks of the
/// same declaration reuse its stable formal identities, so leaving the inner check must restore the
/// outer value rather than delete it.
#[derive(Default)]
pub(super) struct TypeParameterOwnerScope {
    prior: Vec<(&'static str, Option<String>, Option<bool>)>,
}

impl TypeParameterOwnerScope {
    fn publish(
        &mut self,
        owners: &mut HashMap<&'static str, String>,
        uninferred: &mut HashMap<&'static str, bool>,
        identity: &'static str,
        owner: String,
        platform: Option<bool>,
    ) {
        self.prior.push((
            identity,
            owners.insert(identity, owner),
            uninferred.get(identity).copied(),
        ));
        match platform {
            Some(platform) => {
                uninferred.insert(identity, platform);
            }
            None => {
                uninferred.remove(identity);
            }
        }
    }

    fn restore(
        self,
        owners: &mut HashMap<&'static str, String>,
        uninferred: &mut HashMap<&'static str, bool>,
    ) {
        for (parameter, owner, unresolved) in self.prior.into_iter().rev() {
            match owner {
                Some(owner) => {
                    owners.insert(parameter, owner);
                }
                None => {
                    owners.remove(parameter);
                }
            }
            match unresolved {
                Some(platform) => {
                    uninferred.insert(parameter, platform);
                }
                None => {
                    uninferred.remove(parameter);
                }
            }
        }
    }
}

impl TypeParameterNumbering {
    /// Number the parameters of `mentioned`, a diagnostic's parameter identities in mention order.
    pub(super) fn new(mentioned: impl IntoIterator<Item = &'static str>) -> Self {
        let mut by_name: HashMap<&str, Vec<&'static str>> = HashMap::default();
        for identity in mentioned {
            let same_name = by_name
                .entry(crate::types::type_parameter_source_name(identity))
                .or_default();
            if !same_name.contains(&identity) {
                same_name.push(identity);
            }
        }
        let numbers = by_name
            .into_values()
            .filter(|same_name| same_name.len() > 1)
            .flat_map(|same_name| {
                same_name
                    .into_iter()
                    .enumerate()
                    .map(|(index, identity)| (identity, index + 1))
            })
            .collect();
        Self { numbers }
    }

    /// The source name of `parameter`, with its number when another parameter shares it.
    pub(super) fn name(&self, parameter: &str) -> String {
        let source = crate::types::type_parameter_source_name(parameter);
        match self.numbers.get(parameter) {
            Some(number) => format!("{source}#{number}"),
            None => source.to_string(),
        }
    }
}

/// Every type-parameter identity `ty` mentions, in the order a rendering of it writes them.
pub(super) fn mentioned_type_parameters(ty: Ty, mentioned: &mut Vec<&'static str>) {
    match ty.kind() {
        TyKind::TyParam(parameter, _) => mentioned.push(parameter.as_str()),
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::DefinitelyNotNull(inner) => mentioned_type_parameters(inner, mentioned),
        TyKind::Obj(_, arguments) | TyKind::Intersection(arguments) => {
            for argument in arguments {
                mentioned_type_parameters(*argument, mentioned);
            }
        }
        TyKind::Fun(signature) => {
            for parameter in signature.params.iter() {
                mentioned_type_parameters(*parameter, mentioned);
            }
            mentioned_type_parameters(signature.ret, mentioned);
        }
        _ => {}
    }
}

fn nullable_type_parameter(ty: Ty) -> Option<&'static str> {
    let TyKind::Nullable(inner) = ty.kind() else {
        return None;
    };
    let TyKind::TyParam(parameter, _) = inner.kind() else {
        return None;
    };
    Some(parameter.as_str())
}

/// The owner wording of a classifier's own formals from its provider-normalized declaration,
/// whichever module or library declares it: `class B<T : Number>`, `interface I<in T>`.
pub(super) fn classifier_type_parameter_owner(
    source: &dyn crate::symbol_source::SymbolSource,
    classifier: TypeName,
    declaration: &LibraryType,
) -> String {
    let keyword = if declaration.is_interface() {
        "interface"
    } else {
        "class"
    };
    let own = declaration.own_type_parameter_count;
    assert!(
        own <= declaration.type_params().len() && own <= declaration.type_param_bounds().len(),
        "classifier-owned formals and bounds must contain the declared own prefix"
    );
    let formals = super::declaration_rendering::type_parameter_owner_formals(
        source,
        &declaration.type_params()[..own],
        &declaration.type_param_bounds()[..own],
        declaration.type_param_variances(),
    );
    format!("{keyword} {}<{formals}>", classifier.nested_segment_ref())
}

impl Checker<'_> {
    /// Classifiers contributed by stable enclosing `inner` ownership edges, nearest first.
    ///
    /// A retained inner classifier can be checked as a bounded root, without its source owner on
    /// the live lexical-context stack. The stable declaration graph is authoritative for that
    /// missing ancestry; generated classifier-name segments are never interpreted as owners.
    fn active_inner_owner_classifiers(&self, parser: DeclId) -> Vec<TypeName> {
        let Some((active, index)) = self.active_declarations.zip(self.resolved_index) else {
            return Vec::new();
        };
        let Some(mut current) = active.canonical_classifier_declaration(parser, index) else {
            return Vec::new();
        };
        let mut owners = Vec::new();
        loop {
            let Some(header) = index.declaration_header(current) else {
                break;
            };
            if !header.flags.has(crate::fir::DeclarationFlags::INNER) {
                break;
            }
            let Some(owner) = header.owner else {
                break;
            };
            let Some(classifier) = index.classifier_identity(owner) else {
                break;
            };
            if !owners.contains(&classifier) {
                owners.push(classifier);
            }
            current = owner;
        }
        owners
    }

    /// Record the owner wording of a function's formals, bound in `scope`, returning the scoped
    /// publication token that restores any enclosing diagnostic state when the check ends.
    pub(super) fn publish_function_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) -> TypeParameterOwnerScope {
        if function.type_params.is_empty() {
            return TypeParameterOwnerScope::default();
        }
        let formals = self.type_parameter_owner_formals(
            scope,
            &function.type_params,
            &function.type_param_bounds,
            &[],
        );
        self.publish_type_parameter_owner(
            scope,
            &function.type_params,
            format!("fun <{formals}> {}", function.name),
        )
    }

    /// Record the owner wording of a property's formals, bound in `scope` (`val <T> List<T>.pp`).
    /// The receiver is written as the declaration spells it.
    pub(super) fn publish_property_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
    ) -> TypeParameterOwnerScope {
        if property.type_params.is_empty() {
            return TypeParameterOwnerScope::default();
        }
        let formals = self.type_parameter_owner_formals(
            scope,
            &property.type_params,
            &property.type_param_bounds,
            &[],
        );
        let receiver = property
            .receiver
            .as_ref()
            .map_or_else(String::new, |receiver| {
                let mut display = super::BoundedSourceDisplay::new(usize::MAX);
                super::write_source_type_display_with(
                    &mut display,
                    receiver,
                    &mut super::write_source_type_leaf,
                )
                .expect("unbounded type display");
                format!("{}.", display.finish())
            });
        let keyword = if property.is_var { "var" } else { "val" };
        self.publish_type_parameter_owner(
            scope,
            &property.type_params,
            format!("{keyword} <{formals}> {receiver}{}", property.name),
        )
    }

    pub(super) fn restore_type_parameter_owners(&mut self, scope: TypeParameterOwnerScope) {
        scope.restore(
            &mut self.type_parameter_owners,
            &mut self.uninferred_type_parameters,
        );
    }

    /// Publish the owner of provider-normalized callable formals while its selected arguments are
    /// being committed. The formals already are stable declaration identities, so no scope lookup
    /// or source-spelling recovery is involved.
    pub(super) fn publish_generic_signature_owner(
        &mut self,
        signature: &crate::libraries::GenericSig,
        owner: String,
        unresolved: &[String],
    ) -> TypeParameterOwnerScope {
        let mut scope = TypeParameterOwnerScope::default();
        for (index, formal) in signature.formals.iter().enumerate() {
            // `GenericSig::formals` stores the opaque semantic identity as owned text; this
            // restores that exact identity to the interner rather than interning source spelling
            // or deriving identity from it.
            let identity = crate::types::intern(formal);
            let platform = unresolved.contains(formal).then(|| {
                signature
                    .formal_bounds
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .is_some_and(|bound| bound.is_platform_nullable())
            });
            scope.publish(
                &mut self.type_parameter_owners,
                &mut self.uninferred_type_parameters,
                identity,
                owner.clone(),
                platform,
            );
        }
        scope
    }

    /// Mark the published callable formals that `ty` still carries as kotlinc's `uninferred` while
    /// a postponed lambda result is checked against it. A formal lexically in scope is a fixed
    /// solution and keeps its plain owner wording.
    pub(super) fn publish_open_result_formals(
        &mut self,
        scope: &CheckerScope<'_>,
        ty: Ty,
    ) -> TypeParameterOwnerScope {
        let lexical = scope.lexical_tparam_identities();
        let mut formals = Vec::new();
        super::ty_param_names_into(ty, &mut formals);
        let mut published = TypeParameterOwnerScope::default();
        for formal in formals {
            if lexical.iter().any(|fixed| fixed == formal)
                || self.uninferred_type_parameters.contains_key(formal)
            {
                continue;
            }
            let Some(owner) = self.type_parameter_owners.get(formal).cloned() else {
                continue;
            };
            let platform = ty
                .type_parameter_occurrence_bound(formal)
                .is_some_and(|bound| bound.is_platform_nullable());
            published.publish(
                &mut self.type_parameter_owners,
                &mut self.uninferred_type_parameters,
                formal,
                owner,
                Some(platform),
            );
        }
        published
    }

    /// Publish the provider-normalized generic member selected at `call` while its arguments are
    /// committed. Only formals retained as open by postponed inference receive kotlinc's
    /// `uninferred` marker; a stable symbolic solution is still rendered with its declaration owner.
    pub(super) fn publish_selected_member_type_parameter_owner(
        &mut self,
        call: crate::ast::ExprId,
        member: &crate::libraries::LibraryMember,
    ) -> TypeParameterOwnerScope {
        let Some((owner, signature)) = Self::generic_library_member_header(&member.name, member)
            .zip(member.generic_sig.as_ref())
        else {
            return TypeParameterOwnerScope::default();
        };
        let unresolved = self
            .unbound_value_lambda_call_formals
            .get(&call)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .to_vec();
        self.publish_generic_signature_owner(signature, owner, &unresolved)
    }

    /// Record the owner wording of a classifier's formals, bound in `scope`. An inner or local
    /// classifier `owner` also lists the enclosing formals it captures, innermost declaration first,
    /// as `Outer(T)` with their declared variance and bounds: `class E<out S, out Outer(T) : Number>`.
    pub(super) fn publish_class_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        if class.type_params.is_empty() {
            return;
        }
        let keyword = match class.kind {
            ClassKind::Interface => "interface",
            ClassKind::Class | ClassKind::Enum | ClassKind::Annotation => "class",
        };
        let mut formals = self.type_parameter_owner_formals(
            scope,
            &class.type_params,
            class.type_param_bounds(),
            class.type_param_variances(),
        );
        if let Some(captured) = owner.and_then(|owner| self.captured_owner_formals(owner)) {
            formals.push_str(&captured);
        }
        self.publish_type_parameter_owner(
            scope,
            &class.type_params,
            // A nested or local classifier's declaration name carries its parser path (`A.E`);
            // kotlinc names the classifier itself.
            format!(
                "{keyword} {}<{formals}>",
                class.name.rsplit('.').next().unwrap_or(&class.name)
            ),
        );
    }

    /// Record the owner wording of formals declared by the classifiers enclosing parser
    /// declaration `d`, from their provider-normalized classifier records. The live lexical stack
    /// supplies ordinary source ancestry; stable declaration-owner edges supply an inner
    /// classifier checked as a retained root. Generated classifier-name segments are not ancestry.
    pub(super) fn publish_enclosing_type_parameter_owners(
        &mut self,
        d: crate::ast::DeclId,
        owner: TypeName,
    ) {
        let source = self.fed_source();
        let enclosing = self
            .lexical_class_context
            .iter()
            .copied()
            .filter(|classifier| *classifier != owner)
            .chain(self.active_inner_owner_classifiers(d))
            .fold(Vec::new(), |mut classifiers, classifier| {
                if !classifiers.contains(&classifier) {
                    classifiers.push(classifier);
                }
                classifiers
            });
        let mut published = Vec::new();
        for classifier in enclosing {
            let Some(record) = source.classifier(classifier) else {
                continue;
            };
            let parameters = &record.type_parameters;
            let own = &parameters.type_params()[..record.own_type_parameter_count];
            if own.is_empty()
                || own
                    .iter()
                    .all(|parameter| self.type_parameter_owners.contains_key(parameter.as_str()))
            {
                continue;
            }
            let formals = own
                .iter()
                .zip(parameters.type_param_variances())
                .zip(parameters.type_param_bounds())
                .map(|((parameter, variance), bounds)| {
                    owner_formal(
                        crate::types::type_parameter_source_name(parameter),
                        *variance,
                        bounds,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let captured = self.captured_owner_formals(classifier).unwrap_or_default();
            let keyword = if record.is_interface() {
                "interface"
            } else {
                "class"
            };
            // The classifier's own name: a nested classifier's last segment is `Outer$Inner`.
            let segment = classifier.segment_ref();
            let name = segment.rsplit('$').next().unwrap_or(segment);
            let wording = format!("{keyword} {name}<{formals}{captured}>");
            for parameter in own {
                published.push((parameter.clone(), wording.clone()));
            }
        }
        for (parameter, wording) in published {
            self.type_parameter_owners
                .entry(crate::types::intern(&parameter))
                .or_insert(wording);
        }
    }

    /// The `, Outer(T)` wording of the formals classifier `owner` captures from enclosing
    /// declarations, which its classifier record lists after its own.
    fn captured_owner_formals(&self, owner: TypeName) -> Option<String> {
        let record = self.fed_source().classifier(owner)?;
        let parameters = &record.type_parameters;
        let captured = parameters
            .type_params()
            .iter()
            .zip(parameters.type_param_variances())
            .zip(parameters.type_param_bounds())
            .skip(record.own_type_parameter_count)
            .map(|((parameter, variance), bounds)| {
                let source = crate::types::type_parameter_source_name(parameter);
                format!(
                    ", {}",
                    owner_formal(&format!("Outer({source})"), *variance, bounds)
                )
            })
            .collect::<String>();
        Some(captured)
    }

    /// `ty` as a diagnostic names it: a declaration-owned type parameter with its owner
    /// (`T (of fun <T : B> f)`), and classifiers qualified only where `context` makes them clash.
    pub(super) fn diagnostic_type_name(&self, ty: Ty, context: &[Ty]) -> String {
        self.numbered_diagnostic_type_name(ty, context, &TypeParameterNumbering::default())
    }

    /// [`Self::diagnostic_type_name`] inside a diagnostic whose same-named type parameters
    /// `numbering` tells apart (`T#1 (of fun <T> p)`).
    pub(super) fn numbered_diagnostic_type_name(
        &self,
        ty: Ty,
        context: &[Ty],
        numbering: &TypeParameterNumbering,
    ) -> String {
        if let Some(parameter) = nullable_type_parameter(ty) {
            if self.type_parameter_owners.contains_key(parameter) {
                return self.diagnostic_type_parameter_name(parameter, numbering, true);
            }
        }
        ty.source_name_with_classifier_in(
            context,
            &|parameter| self.diagnostic_type_parameter_name(parameter, numbering, false),
            &|classifier| {
                let index = self.resolved_index?;
                let declaration = index.classifier_declaration(classifier)?;
                index
                    .local_class_name_provenance(declaration)?
                    .segments
                    .last()
                    .cloned()
            },
            &|lower| {
                crate::assignable::platform_shape_upper_bound(
                    crate::assignable::TypeOracle::platform_flexible_upper_bound(self, lower),
                )
            },
        )
    }

    fn diagnostic_type_parameter_name(
        &self,
        parameter: &str,
        numbering: &TypeParameterNumbering,
        nullable: bool,
    ) -> String {
        let source = numbering.name(parameter);
        let source = match self.uninferred_type_parameters.get(parameter) {
            Some(platform) => format!("uninferred {source}{}", if *platform { "!" } else { "" }),
            None => source,
        };
        let source = if nullable {
            format!("{source}?")
        } else {
            source
        };
        match self.type_parameter_owners.get(parameter) {
            Some(owner) => format!("{source} (of {owner})"),
            None => source,
        }
    }

    fn publish_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        names: &[String],
        owner: String,
    ) -> TypeParameterOwnerScope {
        let identities = names
            .iter()
            .filter_map(|name| scope.tparam_bound(name).ty_param_name())
            .collect::<Vec<_>>();
        let mut published = TypeParameterOwnerScope::default();
        for identity in identities {
            published.publish(
                &mut self.type_parameter_owners,
                &mut self.uninferred_type_parameters,
                identity,
                owner.clone(),
                None,
            );
        }
        published
    }

    fn type_parameter_owner_formals(
        &self,
        scope: &CheckerScope<'_>,
        names: &[String],
        declared_bounds: &[(String, TypeRef)],
        variances: &[TypeVariance],
    ) -> String {
        let bounds = names
            .iter()
            .map(|name| {
                // Only the bounds written on this parameter: the lexical binding also carries the
                // ones inherited through a bare parameter bound (`T : U`), after the written ones.
                let written = declared_bounds
                    .iter()
                    .filter(|(owner, _)| owner == name)
                    .count();
                let parameter = scope.tparam_bound(name);
                parameter
                    .ty_param_bound()
                    .into_iter()
                    .chain(self.semantic_tparam_extra_bounds(scope, parameter))
                    .take(written)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        super::declaration_rendering::type_parameter_owner_formals(
            &self.fed_source(),
            names,
            &bounds,
            variances,
        )
    }
}

/// One formal of an owner wording: its variance, name and declared bounds (the default `Any?`
/// omitted), as `out T : Number`.
fn owner_formal(name: &str, variance: TypeVariance, bounds: &[Ty]) -> String {
    let variance = match variance {
        TypeVariance::In => "in ",
        TypeVariance::Out => "out ",
        TypeVariance::Invariant => "",
    };
    let bounds = bounds
        .iter()
        .filter(|bound| **bound != Ty::nullable(Ty::obj("kotlin/Any")))
        .map(|bound| bound.source_name())
        .collect::<Vec<_>>();
    if bounds.is_empty() {
        format!("{variance}{name}")
    } else {
        format!("{variance}{name} : {}", bounds.join(", "))
    }
}
