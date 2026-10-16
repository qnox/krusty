//! kotlinc's diagnostic wording for a declaration-owned type parameter: `T (of fun <T : B> f)` or
//! `X (of class Box<out X, in Y : Number>)`. The owner text lists every formal of the declaration
//! with its variance and declared bounds (an explicit `Any?` bound is the default and is omitted,
//! and `reified` is not part of the wording). Owners are keyed by the formal's semantic identity,
//! which is unique per declaration. A function's formals are named with their owner only while that
//! function is checked; a formal leaking out through an unsolved call keeps its bare name.

use super::{Checker, CheckerScope};
use crate::ast::{ClassDecl, ClassKind, FunDecl, PropDecl, TypeRef};
use crate::fx_hash::HashMap;
use crate::libraries::LibraryType;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, TypeVariance};

/// kotlinc's disambiguation of same-named type parameters within one diagnostic: when two distinct
/// parameters share a source name, each is written `T#N`, numbered in the order the diagnostic's
/// arguments first mention them. A name used by one parameter only stays bare.
#[derive(Default)]
pub(super) struct TypeParameterNumbering {
    numbers: HashMap<&'static str, usize>,
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
    match ty {
        Ty::TyParam(parameter, _) => mentioned.push(parameter),
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::DefinitelyNotNull(inner) => mentioned_type_parameters(*inner, mentioned),
        Ty::Obj(_, arguments) | Ty::Intersection(arguments) => {
            for argument in arguments {
                mentioned_type_parameters(*argument, mentioned);
            }
        }
        Ty::Fun(signature) => {
            for parameter in signature.params.iter() {
                mentioned_type_parameters(*parameter, mentioned);
            }
            mentioned_type_parameters(signature.ret, mentioned);
        }
        _ => {}
    }
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
    /// Record the owner wording of a function's formals, bound in `scope`, returning their
    /// identities for [`Self::retire_type_parameter_owners`] when the function's check ends.
    pub(super) fn publish_function_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) -> Vec<&'static str> {
        if function.type_params.is_empty() {
            return Vec::new();
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

    /// Record the owner wording of a property's formals, bound in `scope` (`val <T> List<T>.pp`),
    /// returning their identities for [`Self::retire_type_parameter_owners`]. The receiver is
    /// written as the declaration spells it.
    pub(super) fn publish_property_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
    ) -> Vec<&'static str> {
        if property.type_params.is_empty() {
            return Vec::new();
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

    pub(super) fn retire_type_parameter_owners(&mut self, parameters: &[&'static str]) {
        for parameter in parameters {
            self.type_parameter_owners.remove(parameter);
        }
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

    /// Record the owner wording of the formals declared by the classifiers enclosing `owner`, from
    /// their classifier records, where no check of that classifier published it: an inner
    /// classifier can be checked in a unit of its own, and its members still name the formals it
    /// captures by their declaring classifier (`O (of class A<out O>)`).
    pub(super) fn publish_enclosing_type_parameter_owners(&mut self, owner: TypeName) {
        let source = self.fed_source();
        let mut enclosing = owner.nested_owner();
        let mut published = Vec::new();
        while let Some(classifier) = enclosing {
            enclosing = classifier.nested_owner();
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
        ty.source_name_with_classifier_in(
            context,
            &|parameter| {
                let source = numbering.name(parameter);
                match self.type_parameter_owners.get(parameter) {
                    Some(owner) => format!("{source} (of {owner})"),
                    None => source,
                }
            },
            &|classifier| {
                let index = self.resolved_index?;
                let declaration = index.classifier_declaration(classifier)?;
                index
                    .local_class_name_provenance(declaration)?
                    .segments
                    .last()
                    .cloned()
            },
        )
    }

    fn publish_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        names: &[String],
        owner: String,
    ) -> Vec<&'static str> {
        let identities = names
            .iter()
            .filter_map(|name| scope.tparam_bound(name).ty_param_name())
            .collect::<Vec<_>>();
        for identity in &identities {
            self.type_parameter_owners.insert(identity, owner.clone());
        }
        identities
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
