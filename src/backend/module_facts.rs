//! Frozen semantic classifier facts exposed to representation backends.
//!
//! Pass 2 still owns a migration-era frontend symbol table while it checks one active source unit.
//! A backend must never receive that table: it contains lookup APIs, AST-backed member keys, and can
//! contain provisional body-local entries for a source that has not streamed yet. This module copies
//! only finalized classifier records before Pass 2 starts and rejects every nested pending/error type.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::libraries::{
    CallSig, DefaultCallRealization, GenericSig, InlineBodyPlan, LibraryCallable, LibraryMember,
    LibraryType,
};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, TypeVariance, Visibility};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendFactError {
    UnpublishableClassifier(TypeName, UndeterminedType),
    IncompleteClassifier(TypeName),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UndeterminedType {
    Pending,
    Error,
}

/// The only semantic query a representation backend may make after common lowering.
///
/// Calls, properties, overloads, and source names are absent deliberately: checked IR already owns
/// every selected declaration identity and realization. The returned classifier is guaranteed not
/// to contain `Ty::Pending`, `Ty::Error`, source type references, or ordinary source-body payloads.
pub trait BackendClassifierSource {
    fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>>;
}

/// The exact classifier information representation backends may inspect after semantic checking.
/// Resolver candidate maps, constructors, constants, source keys, inline plans, contracts, and
/// parser-backed member identities cannot be represented here.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendClassifierFact {
    pub access: crate::libraries::ClassifierAccess,
    pub is_kotlin: bool,
    pub source: bool,
    pub outer_instance: Option<TypeName>,
    pub kind: crate::libraries::TypeKind,
    pub is_abstract: bool,
    pub is_extensible: bool,
    pub supertypes: Box<[TypeName]>,
    /// Functions and property accessors in semantic declaration order.
    pub surface: Box<[BackendMemberFact]>,
    /// Resolved declaration annotations. A backend reads these to answer questions a plugin cannot
    /// answer for itself — whether ANOTHER file of this module carries an annotation whose generated
    /// declarations this file's emission must name.
    pub annotations: Box<[crate::types::ResolvedAnnotation]>,
    /// Number of leading semantic type parameters declared by this classifier itself. Remaining
    /// parameters are lexical captures used by common checking, not parameters of its backend
    /// declaration.
    pub own_type_parameter_count: usize,
    pub type_param_variances: Box<[TypeVariance]>,
    pub value_underlying: Option<Ty>,
    pub value_underlying_property: Option<Box<str>>,
}

/// Declaration facts of a current-module classifier that a dependency's classifier record carries
/// in its own form (modality, companion, qualified name) or that its annotation record keeps apart
/// from the class-valued arguments (string-valued arguments).
#[derive(Clone, Debug, PartialEq)]
struct SourceDeclaration {
    is_sealed: bool,
    /// The declared companion object: its static field's name and its classifier.
    companion: Option<(Box<str>, TypeName)>,
    /// The Kotlin qualified name with every boundary dotted (`pkg.Outer.Nested`).
    qualified_name: Option<Box<str>>,
    /// String-valued annotation arguments, by the annotation's ordinal among the declaration's.
    annotation_strings: Box<[Box<[Box<str>]>]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendMemberFact {
    pub name: BackendMemberName,
    pub physical_name: Option<Box<str>>,
    pub owner: Option<TypeName>,
    pub physical_params: Box<[Ty]>,
    pub params: Box<[Ty]>,
    pub ret: Ty,
    pub physical_ret: Ty,
    pub descriptor: Box<str>,
    pub realization: crate::libraries::MemberRealization,
    pub suspend: bool,
    pub abstract_member: bool,
    pub visibility: Visibility,
    pub parameter_identities: Box<[crate::fir::ResolvedParameterIdentity]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendMemberName {
    Declared(Box<str>),
    PropertyGetter(Box<str>),
    PropertySetter(Box<str>),
}

fn erase_backend_parameter(ty: Ty) -> Ty {
    ty.ty_param_bound().map_or(ty, Ty::non_null)
}

impl BackendClassifierFact {
    fn from_library(shape: &LibraryType) -> Self {
        let mut surface = Vec::new();
        for name in &shape.declared_callable_order {
            let Some(callables) = shape.declared_callables.get(name) else {
                continue;
            };
            surface.extend(
                callables
                    .functions()
                    .iter()
                    .map(BackendMemberFact::from_function),
            );
            for property in callables.properties() {
                surface.push(BackendMemberFact::from_callable_named(
                    &property.getter,
                    property.visibility,
                    &property.context_parameter_identities,
                    None,
                    property.receiver.is_some(),
                    BackendMemberName::PropertyGetter(property.name.as_str().into()),
                ));
                if let Some(setter) = &property.setter {
                    surface.push(BackendMemberFact::from_callable_named(
                        setter,
                        property.setter_visibility,
                        &property.context_parameter_identities,
                        property.setter_parameter_name.as_deref(),
                        property.receiver.is_some(),
                        BackendMemberName::PropertySetter(property.name.as_str().into()),
                    ));
                }
            }
        }
        if surface.is_empty() {
            surface.extend(shape.members.iter().map(BackendMemberFact::from_member));
        }
        Self {
            access: shape.access,
            is_kotlin: shape.is_kotlin,
            source: shape.source_file.is_some(),
            outer_instance: shape.outer_instance,
            kind: shape.kind,
            is_abstract: shape.inheritance.is_abstract,
            is_extensible: shape.inheritance.is_extensible,
            supertypes: shape
                .supertypes
                .iter_ids()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            surface: surface.into_boxed_slice(),
            annotations: shape.annotations.clone().into_boxed_slice(),
            own_type_parameter_count: shape.own_type_parameter_count,
            type_param_variances: shape.type_param_variances().to_vec().into_boxed_slice(),
            value_underlying: shape.value_underlying,
            value_underlying_property: shape
                .value_underlying_property
                .as_deref()
                .map(Box::<str>::from),
        }
    }

    pub fn is_interface(&self) -> bool {
        matches!(
            self.kind,
            crate::libraries::TypeKind::Interface | crate::libraries::TypeKind::Annotation
        )
    }

    pub fn is_annotation(&self) -> bool {
        self.kind == crate::libraries::TypeKind::Annotation
    }

    pub fn is_enum(&self) -> bool {
        self.kind == crate::libraries::TypeKind::Enum
    }
}

impl BackendMemberFact {
    fn from_function(function: &crate::libraries::FunctionInfo) -> Self {
        let callable = &function.callable;
        Self {
            name: BackendMemberName::Declared(callable.name.as_str().into()),
            physical_name: None,
            owner: Some(callable.owner),
            physical_params: callable.physical_params.clone().into_boxed_slice(),
            params: callable.params.clone().into_boxed_slice(),
            ret: callable.ret,
            physical_ret: callable.physical_ret,
            descriptor: callable.descriptor.as_str().into(),
            realization: callable.member_realization,
            suspend: function.flags.suspend,
            abstract_member: function.flags.is_abstract,
            visibility: function.visibility,
            parameter_identities: function
                .call_sig
                .physical_parameter_identities(
                    callable.params.len(),
                    function.context_count,
                    (function.is_extension()
                        && callable.params.len()
                            == function.call_sig.parameter_identities.len() + 1)
                        .then_some(function.context_count),
                )
                .expect("a normalized function publishes every typed parameter identity"),
        }
    }

    fn from_member(member: &LibraryMember) -> Self {
        Self {
            name: BackendMemberName::Declared(member.name.as_str().into()),
            physical_name: member.physical_name.as_deref().map(Into::into),
            owner: member.owner,
            physical_params: member.physical_params.clone().into_boxed_slice(),
            params: member.params.clone().into_boxed_slice(),
            ret: member.ret,
            physical_ret: member.physical_ret,
            descriptor: member.descriptor.as_str().into(),
            realization: member.realization,
            suspend: member.suspend(),
            abstract_member: member.is_abstract(),
            visibility: member.visibility,
            parameter_identities: member
                .call_sig
                .physical_parameter_identities(
                    member.params.len(),
                    member.context_count,
                    (member.is_member_extension()
                        && member.params.len() == member.call_sig.parameter_identities.len() + 1)
                        .then_some(member.context_count),
                )
                .expect("a normalized member publishes every typed parameter identity"),
        }
    }

    fn from_callable_named(
        callable: &LibraryCallable,
        visibility: Visibility,
        context_parameter_identities: &[crate::fir::ResolvedParameterIdentity],
        setter_parameter_name: Option<&str>,
        extension_receiver: bool,
        name: BackendMemberName,
    ) -> Self {
        let setter = matches!(name, BackendMemberName::PropertySetter(_));
        let mut logical_identities = context_parameter_identities.to_vec();
        if setter {
            logical_identities.push(setter_parameter_name.map_or(
                crate::fir::ResolvedParameterIdentity::PropertySetterValue,
                |name| crate::fir::ResolvedParameterIdentity::Source(name.into()),
            ));
        }
        // An associated/companion extension participates in source lookup through a receiver but
        // its accessor has no physical receiver parameter. Preserve only receivers actually present
        // in this target-facing parameter list.
        let extension_receiver =
            extension_receiver && callable.params.len() == logical_identities.len() + 1;
        if extension_receiver {
            logical_identities.insert(
                callable.context_count,
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
            );
        }
        assert_eq!(
            callable.params.len(),
            logical_identities.len(),
            "a normalized property accessor publishes every typed parameter identity"
        );
        Self {
            name,
            physical_name: None,
            owner: Some(callable.owner),
            physical_params: callable.physical_params.clone().into_boxed_slice(),
            params: callable.params.clone().into_boxed_slice(),
            ret: callable.ret,
            physical_ret: callable.physical_ret,
            descriptor: callable.descriptor.as_str().into(),
            realization: callable.member_realization,
            suspend: callable.suspend,
            abstract_member: callable.is_abstract,
            visibility,
            parameter_identities: logical_identities.into_boxed_slice(),
        }
    }

    pub fn suspend(&self) -> bool {
        self.suspend
    }

    pub fn is_abstract(&self) -> bool {
        self.abstract_member
    }
}

/// Finalized current-module classifier records, frozen before Pass 2 starts.
pub struct BackendModuleFacts {
    classifiers: HashMap<TypeName, Arc<BackendClassifierFact>>,
    generated_classifiers: Box<[crate::types::GeneratedClassifierFact]>,
    /// Stable identities of classifiers declared inside ordinary bodies. Their completed semantic
    /// shape belongs to the active Pass-2 IR file, never to the pre-Pass-2 module snapshot. Keeping
    /// only the identity prevents an accidental lookup of a same-named dependency classifier.
    body_local_classifiers: HashSet<TypeName>,
    source_value_classes: HashMap<TypeName, Ty>,
    metadata_readable_value_classes: HashSet<TypeName>,
    source_declarations: HashMap<TypeName, SourceDeclaration>,
}

impl BackendModuleFacts {
    /// Build the complete current-module backend view from stable, pending-free declaration
    /// headers. No resolver/provider record or Pass-1 parser coordinate participates.
    pub(crate) fn from_resolved_index(
        index: &crate::fir::ResolvedModuleIndex,
    ) -> Result<Self, BackendFactError> {
        let mut classifiers = HashMap::new();
        let mut body_local_classifiers = HashSet::new();
        let mut source_value_classes = HashMap::new();
        let mut metadata_readable_value_classes = HashSet::new();
        let mut source_declarations = HashMap::new();
        let mut generated_classifiers = Vec::new();
        for raw in 0..index.declaration_count() {
            let declaration = crate::fir::DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            let Some(classifier) = index.classifier_header(declaration) else {
                continue;
            };
            if index.is_body_local_declaration(declaration) {
                body_local_classifiers.insert(classifier.classifier);
                continue;
            }
            let declaration_header = index.declaration_header(declaration).ok_or(
                BackendFactError::IncompleteClassifier(classifier.classifier),
            )?;
            for generated in index.generated_classifiers(declaration) {
                assert_eq!(
                    generated.lexical_owner, classifier.classifier,
                    "a generated classifier must name its exact source owner"
                );
                assert!(
                    generated_classifiers.iter().all(
                        |existing: &crate::types::GeneratedClassifierFact| {
                            existing.classifier != generated.classifier
                        }
                    ),
                    "a generated classifier identity may be published only once"
                );
                generated_classifiers.push(generated.clone());
            }
            let flags = declaration_header.flags;
            let kind = if flags.has(crate::fir::DeclarationFlags::ANNOTATION_CLASS) {
                crate::libraries::TypeKind::Annotation
            } else if flags.has(crate::fir::DeclarationFlags::ENUM) {
                crate::libraries::TypeKind::Enum
            } else if flags.has(crate::fir::DeclarationFlags::SINGLETON) {
                crate::libraries::TypeKind::Object
            } else if flags.has(crate::fir::DeclarationFlags::INTERFACE) {
                crate::libraries::TypeKind::Interface
            } else {
                crate::libraries::TypeKind::Class
            };
            let is_interface = matches!(
                kind,
                crate::libraries::TypeKind::Interface | crate::libraries::TypeKind::Annotation
            );
            let mut direct_supertypes = classifier
                .superclass
                .into_iter()
                .chain(classifier.interfaces.iter().copied())
                .filter_map(|ty| ty.get().non_null().obj_internal())
                .collect::<Vec<_>>();
            if direct_supertypes.is_empty()
                && classifier.classifier != crate::types::type_name("kotlin/Any")
            {
                direct_supertypes.push(crate::types::type_name("kotlin/Any"));
            }
            let outer_instance = flags
                .has(crate::fir::DeclarationFlags::INNER)
                .then(|| {
                    declaration_header
                        .owner
                        .and_then(|owner| index.classifier_header(owner))
                        .map(|owner| owner.classifier)
                })
                .flatten();
            let type_param_variances = index
                .classifier_type_arguments(declaration)
                .unwrap_or_default()
                .iter()
                .map(|parameter| {
                    index
                        .type_parameter_header(*parameter)
                        .ok_or(BackendFactError::IncompleteClassifier(
                            classifier.classifier,
                        ))
                        .map(|parameter| parameter.flags.variance())
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            let own_type_parameter_count = index
                .classifier_own_type_parameter_count(declaration)
                .ok_or(BackendFactError::IncompleteClassifier(
                    classifier.classifier,
                ))? as usize;
            let mut surface = Vec::new();
            let mut value_underlying = None;
            let mut value_underlying_property = None;
            for child_raw in 0..index.declaration_count() {
                let child = crate::fir::DeclarationId::from_raw(
                    u32::try_from(child_raw).expect("too many stable declarations for a packed id"),
                );
                let Some(anchor) = index.declaration_anchor(child) else {
                    continue;
                };
                if anchor.owner != Some(declaration) {
                    continue;
                }
                let Some(child_header) = index.declaration_header(child) else {
                    continue;
                };
                match anchor.kind {
                    crate::fir::DeclarationKind::Function => {
                        if index.is_suppressed_generated_callable(child) {
                            continue;
                        }
                        let signature = index.signature(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let callable = index.callable_for_declaration(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let mut parameters = signature
                            .parameters
                            .iter()
                            .map(|parameter| erase_backend_parameter(parameter.get()))
                            .collect::<Vec<_>>();
                        if let Some(receiver) = callable.shape.extension_receiver {
                            parameters.insert(
                                (callable.shape.context_parameter_count as usize)
                                    .min(parameters.len()),
                                receiver.get(),
                            );
                        }
                        let parameter_identities = index
                            .callable_parameter_identities(callable.id, parameters.len())
                            .ok_or(BackendFactError::IncompleteClassifier(
                                classifier.classifier,
                            ))?;
                        surface.push((
                            index.source_order(child).unwrap_or(u32::MAX),
                            BackendMemberFact {
                                name: BackendMemberName::Declared(
                                    index
                                        .declaration_name(child)
                                        .ok_or(BackendFactError::IncompleteClassifier(
                                            classifier.classifier,
                                        ))?
                                        .into(),
                                ),
                                physical_name: None,
                                owner: Some(classifier.classifier),
                                physical_params: parameters.clone().into_boxed_slice(),
                                params: parameters.into_boxed_slice(),
                                ret: signature.result.get(),
                                physical_ret: signature.result.get(),
                                descriptor: "".into(),
                                realization: crate::libraries::MemberRealization::Dispatch,
                                suspend: child_header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::SUSPEND),
                                abstract_member: child_header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::ABSTRACT),
                                visibility: child_header.visibility,
                                parameter_identities,
                            },
                        ));
                    }
                    crate::fir::DeclarationKind::Property => {
                        let signature = index.signature(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let property_id = index.property_for_declaration(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let property = index.property(property_id).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        let name = index.declaration_name(child).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?;
                        if flags.has(crate::fir::DeclarationFlags::VALUE)
                            && child_header
                                .flags
                                .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
                        {
                            value_underlying = Some(signature.result.get());
                            value_underlying_property = Some(Box::<str>::from(name));
                        }
                        let mut parameters = signature
                            .parameters
                            .iter()
                            .take(property.context_parameter_count as usize)
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>();
                        if let Some(receiver) = property.extension_receiver {
                            parameters.push(receiver.get());
                        }
                        let abstract_member = child_header
                            .flags
                            .has(crate::fir::DeclarationFlags::ABSTRACT);
                        let mut getter_parameter_identities = index
                            .property_context_parameter_identities(property_id)
                            .ok_or(BackendFactError::IncompleteClassifier(
                                classifier.classifier,
                            ))?
                            .into_vec();
                        if property.extension_receiver.is_some() {
                            getter_parameter_identities
                                .push(crate::fir::ResolvedParameterIdentity::ExtensionReceiver);
                        }
                        let getter_parameter_identities =
                            getter_parameter_identities.into_boxed_slice();
                        surface.push((
                            index.source_order(child).unwrap_or(u32::MAX),
                            BackendMemberFact {
                                name: BackendMemberName::PropertyGetter(name.into()),
                                physical_name: None,
                                owner: Some(classifier.classifier),
                                physical_params: parameters.clone().into_boxed_slice(),
                                params: parameters.clone().into_boxed_slice(),
                                ret: signature.result.get(),
                                physical_ret: signature.result.get(),
                                descriptor: "".into(),
                                realization: crate::libraries::MemberRealization::Dispatch,
                                suspend: false,
                                abstract_member,
                                visibility: child_header.visibility,
                                parameter_identities: getter_parameter_identities.clone(),
                            },
                        ));
                        if property.mutable {
                            parameters.push(signature.result.get());
                            let mut setter_parameter_identities =
                                getter_parameter_identities.to_vec();
                            setter_parameter_identities.push(
                                index
                                    .property_setter_parameter_identity(property_id)
                                    .ok_or(BackendFactError::IncompleteClassifier(
                                        classifier.classifier,
                                    ))?
                                    .clone(),
                            );
                            let setter_parameter_identities =
                                setter_parameter_identities.into_boxed_slice();
                            let setter_visibility = index
                                .owned_declaration(child, crate::fir::DeclarationKind::Accessor, 1)
                                .and_then(|setter| index.declaration_header(setter))
                                .map_or(child_header.visibility, |setter| setter.visibility);
                            surface.push((
                                index.source_order(child).unwrap_or(u32::MAX),
                                BackendMemberFact {
                                    name: BackendMemberName::PropertySetter(name.into()),
                                    physical_name: None,
                                    owner: Some(classifier.classifier),
                                    physical_params: parameters.clone().into_boxed_slice(),
                                    params: parameters.into_boxed_slice(),
                                    ret: Ty::Unit,
                                    physical_ret: Ty::Unit,
                                    descriptor: "".into(),
                                    realization: crate::libraries::MemberRealization::Dispatch,
                                    suspend: false,
                                    abstract_member,
                                    visibility: setter_visibility,
                                    parameter_identities: setter_parameter_identities,
                                },
                            ));
                        }
                    }
                    _ => {}
                }
            }
            surface.sort_by_key(|(order, _)| *order);
            // The companion's static field is named after the companion itself: its declared
            // name below the owner's, which a nested declaration's name extends with a dot.
            let companion = index
                .companion_declaration(declaration)
                .and_then(|companion| {
                    let owner = index.declaration_name(declaration)?;
                    let field = index
                        .declaration_name(companion)?
                        .strip_prefix(owner)?
                        .strip_prefix('.')?;
                    let classifier = index.classifier_header(companion)?.classifier;
                    Some((Box::from(field), classifier))
                });
            // Package and lexical-class boundaries are both dots, the spelling lowering records
            // for this file's own classes.
            let qualified_name = match (
                index.declaration_name(declaration),
                index
                    .declaration_anchor(declaration)
                    .and_then(|anchor| index.source_package(anchor.source)),
            ) {
                (Some(name), Some(package)) => {
                    let package = package.render().replace('/', ".");
                    Some(if package.is_empty() {
                        Box::from(name)
                    } else {
                        format!("{package}.{name}").into_boxed_str()
                    })
                }
                _ => None,
            };
            let fact = BackendClassifierFact {
                access: declaration_header.visibility.into(),
                is_kotlin: true,
                source: true,
                outer_instance,
                kind,
                is_abstract: flags.has(crate::fir::DeclarationFlags::ABSTRACT) || is_interface,
                is_extensible: !is_interface && !flags.has(crate::fir::DeclarationFlags::FINAL),
                supertypes: direct_supertypes.into_boxed_slice(),
                surface: surface
                    .into_iter()
                    .map(|(_, member)| member)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                annotations: index
                    .declaration_annotations(declaration)
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(ordinal, annotation)| crate::types::ResolvedAnnotation {
                        annotation,
                        // The class-valued arguments the frontend resolved for this occurrence.
                        // A dependency's annotations reach a backend with their arguments intact;
                        // one declared in ANOTHER FILE OF THIS MODULE must too, or the two sides of
                        // the same boundary answer the same question differently.
                        arguments: index
                            .declaration_annotation_class_arguments(declaration, ordinal as u32)
                            .iter()
                            .map(|&classifier| {
                                (
                                    String::new(),
                                    crate::types::AnnotationValue::Class(classifier),
                                )
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                own_type_parameter_count,
                type_param_variances,
                value_underlying,
                value_underlying_property,
            };
            source_declarations.insert(
                classifier.classifier,
                SourceDeclaration {
                    is_sealed: flags.has(crate::fir::DeclarationFlags::SEALED),
                    companion,
                    qualified_name,
                    annotation_strings: (0..index.declaration_annotations(declaration).len())
                        .map(|ordinal| {
                            index
                                .declaration_annotation_string_arguments(
                                    declaration,
                                    ordinal as u32,
                                )
                                .to_vec()
                                .into_boxed_slice()
                        })
                        .collect(),
                },
            );
            if let Some(underlying) = value_underlying {
                source_value_classes.insert(classifier.classifier, underlying);
                let has_secondary_constructor = (0..index.declaration_count()).any(|raw| {
                    let child = crate::fir::DeclarationId::from_raw(raw as u32);
                    index.declaration_anchor(child).is_some_and(|anchor| {
                        anchor.owner == Some(declaration)
                            && anchor.kind == crate::fir::DeclarationKind::Constructor
                            && anchor.sibling > 0
                    })
                });
                if !has_secondary_constructor {
                    metadata_readable_value_classes.insert(classifier.classifier);
                }
            }
            classifiers.insert(classifier.classifier, Arc::new(fact));
        }
        Ok(Self {
            classifiers,
            generated_classifiers: generated_classifiers.into_boxed_slice(),
            body_local_classifiers,
            source_value_classes,
            metadata_readable_value_classes,
            source_declarations,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_classifiers(
        source: impl IntoIterator<Item = (TypeName, LibraryType)>,
        body_local_classifiers: impl IntoIterator<Item = TypeName>,
    ) -> Result<Self, BackendFactError> {
        let mut classifiers = HashMap::new();
        let mut source_value_classes = HashMap::new();
        let mut metadata_readable_value_classes = HashSet::new();
        for (classifier, shape) in source {
            if classifiers.contains_key(&classifier) {
                continue;
            }
            validate_classifier(classifier, &shape)?;
            if let Some(underlying) = shape.value_underlying {
                source_value_classes.insert(classifier, underlying);
                metadata_readable_value_classes.insert(classifier);
            }
            classifiers.insert(
                classifier,
                Arc::new(BackendClassifierFact::from_library(&shape)),
            );
        }
        Ok(Self {
            classifiers,
            generated_classifiers: Box::default(),
            body_local_classifiers: body_local_classifiers.into_iter().collect(),
            source_value_classes,
            metadata_readable_value_classes,
            source_declarations: HashMap::new(),
        })
    }

    pub fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
        self.classifiers.get(&classifier).cloned()
    }

    pub fn classifiers(&self) -> impl Iterator<Item = (TypeName, &BackendClassifierFact)> {
        self.classifiers
            .iter()
            .map(|(classifier, shape)| (*classifier, shape.as_ref()))
    }

    pub fn generated_classifiers(&self) -> &[crate::types::GeneratedClassifierFact] {
        &self.generated_classifiers
    }

    pub fn is_body_local(&self, classifier: TypeName) -> bool {
        self.body_local_classifiers.contains(&classifier)
    }

    pub fn source_value_classes(&self) -> &HashMap<TypeName, Ty> {
        &self.source_value_classes
    }

    pub fn metadata_readable_value_classes(&self) -> &HashSet<TypeName> {
        &self.metadata_readable_value_classes
    }
}

/// A per-call view federating the frozen current module with dependency classifier metadata.
/// Dependency records are validated before they are returned, so even a malformed provider cannot
/// put an undetermined semantic type in a backend.
pub struct CheckedBackendClassifiers<'a> {
    module: &'a BackendModuleFacts,
    dependencies: &'a dyn SymbolSource,
}

impl<'a> CheckedBackendClassifiers<'a> {
    pub(crate) fn new(module: &'a BackendModuleFacts, dependencies: &'a dyn SymbolSource) -> Self {
        Self {
            module,
            dependencies,
        }
    }

    pub fn module(&self) -> &BackendModuleFacts {
        self.module
    }
}

impl BackendClassifierSource for CheckedBackendClassifiers<'_> {
    fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
        if let Some(shape) = self.module.classifier(classifier) {
            return Some(shape);
        }
        if self.module.is_body_local(classifier) {
            return None;
        }
        let shape = self.dependencies.classifier(classifier)?;
        validate_classifier(classifier, &shape).unwrap_or_else(|error| {
            panic!(
                "dependency classifier crossed the backend boundary with invalid types: {error:?}"
            )
        });
        Some(Arc::new(BackendClassifierFact::from_library(&shape)))
    }
}

impl crate::types::ClassifierFactSource for CheckedBackendClassifiers<'_> {
    fn classifier_annotations(
        &self,
        classifier: TypeName,
    ) -> Option<Vec<crate::types::ResolvedAnnotation>> {
        let mut annotations = BackendClassifierSource::classifier(self, classifier)?
            .annotations
            .to_vec();
        // A dependency's annotations carry every argument. A source declaration's backend record
        // carries the class-valued ones; its string-valued ones are kept beside it, and join here
        // so the two answer the same question the same way.
        if let Some(source) = self.module.source_declarations.get(&classifier) {
            for (annotation, strings) in
                annotations.iter_mut().zip(source.annotation_strings.iter())
            {
                annotation.arguments.extend(
                    strings
                        .iter()
                        .map(|value| (String::new(), crate::types::AnnotationValue::string(value))),
                );
            }
        }
        Some(annotations)
    }

    fn classifier_is_object(&self, classifier: TypeName) -> Option<bool> {
        BackendClassifierSource::classifier(self, classifier)
            .map(|fact| fact.kind == crate::libraries::TypeKind::Object)
    }

    fn classifier_declaration(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::ClassifierDeclarationFacts> {
        let fact = BackendClassifierSource::classifier(self, classifier)?;
        let (is_sealed, companion, qualified_name) =
            match self.module.source_declarations.get(&classifier) {
                Some(source) => (
                    source.is_sealed,
                    source.companion.clone(),
                    source.qualified_name.clone(),
                ),
                None => {
                    let shape = self.dependencies.classifier(classifier)?;
                    (
                        // Only a sealed class records its subclasses; one without any is still
                        // abstract in its class file.
                        !shape.sealed_subclasses.is_empty(),
                        shape
                            .companion_object
                            .as_ref()
                            .map(|(field, companion)| (Box::from(field.as_str()), *companion)),
                        shape.qualified_name.clone(),
                    )
                }
            };
        Some(crate::types::ClassifierDeclarationFacts {
            kind: fact.kind.into(),
            is_abstract: fact.is_abstract || is_sealed,
            own_type_parameter_count: fact.own_type_parameter_count,
            companion,
            qualified_name,
            source: fact.source,
        })
    }

    fn classifier_value_underlying(&self, classifier: TypeName) -> Option<Ty> {
        // A source value class of this module answers from the module's own facts — the same map
        // the JVM value-class pass erases by. A dependency's answers from its decoded metadata.
        self.module
            .source_value_classes()
            .get(&classifier)
            .copied()
            .or_else(|| {
                self.dependencies
                    .classifier(classifier)
                    .and_then(|shape| shape.value_underlying)
            })
    }

    fn classifier_value_property(&self, classifier: TypeName) -> Option<String> {
        BackendClassifierSource::classifier(self, classifier)
            .and_then(|fact| fact.value_underlying_property.as_deref().map(str::to_owned))
    }
}

fn validate_classifier(classifier: TypeName, shape: &LibraryType) -> Result<(), BackendFactError> {
    let mut saw_pending = false;
    let mut saw_error = false;
    let mut visit = |ty: Ty| {
        saw_pending |= ty.mentions_pending();
        saw_error |= ty.mentions_error();
    };

    shape
        .supertype_templates
        .iter()
        .copied()
        .for_each(&mut visit);
    shape
        .constructors
        .iter()
        .chain(&shape.members)
        .chain(&shape.companion)
        .chain(shape.enum_entries_accessor.iter())
        .for_each(|member| visit_member(member, &mut visit));
    for callables in shape.declared_callables.values() {
        for function in callables.functions() {
            function.receiver.into_iter().for_each(&mut visit);
            function.ret.class.into_iter().for_each(&mut visit);
            visit_callable(&function.callable, &mut visit);
            function
                .generic_sig
                .iter()
                .for_each(|generic| visit_generic(generic, &mut visit));
            visit_call_sig(&function.call_sig, &mut visit);
        }
        for property in callables.properties() {
            property.receiver.into_iter().for_each(&mut visit);
            visit(property.ty);
            visit_callable(&property.getter, &mut visit);
            property
                .setter
                .iter()
                .for_each(|setter| visit_callable(setter, &mut visit));
            property
                .compile_time_constant
                .iter()
                .for_each(|constant| visit(constant.ty));
        }
    }
    shape
        .constants
        .values()
        .for_each(|constant| visit(constant.ty));
    shape.callable_signature.into_iter().for_each(&mut visit);
    shape
        .callable_signatures
        .iter()
        .copied()
        .for_each(&mut visit);
    shape.value_underlying.into_iter().for_each(&mut visit);
    shape
        .type_param_bounds()
        .iter()
        .flatten()
        .copied()
        .for_each(&mut visit);
    shape
        .named_parameter_lists
        .iter()
        .flat_map(|parameters| parameters.types.iter().copied())
        .for_each(&mut visit);

    if saw_pending {
        Err(BackendFactError::UnpublishableClassifier(
            classifier,
            UndeterminedType::Pending,
        ))
    } else if saw_error {
        Err(BackendFactError::UnpublishableClassifier(
            classifier,
            UndeterminedType::Error,
        ))
    } else {
        Ok(())
    }
}

fn visit_member(member: &LibraryMember, visit: &mut impl FnMut(Ty)) {
    member.physical_params.iter().copied().for_each(&mut *visit);
    member.params.iter().copied().for_each(&mut *visit);
    visit(member.ret);
    visit(member.physical_ret);
    member
        .generic_sig
        .iter()
        .for_each(|generic| visit_generic(generic, visit));
    visit_call_sig(&member.call_sig, visit);
    member.equality_bound.into_iter().for_each(&mut *visit);
    member.declared_ret.into_iter().for_each(&mut *visit);
    member
        .default_realization
        .iter()
        .for_each(|realization| visit_default_realization(realization, visit));
    member
        .inline_body_plan
        .iter()
        .for_each(|plan| visit_inline_plan(plan, visit));
}

fn visit_callable(callable: &LibraryCallable, visit: &mut impl FnMut(Ty)) {
    callable.params.iter().copied().for_each(&mut *visit);
    callable
        .physical_params
        .iter()
        .copied()
        .for_each(&mut *visit);
    visit(callable.ret);
    visit(callable.physical_ret);
    callable.vararg_elem.into_iter().for_each(&mut *visit);
    callable.source_receiver.into_iter().for_each(&mut *visit);
    callable
        .declared_params
        .iter()
        .flat_map(|parameters| parameters.iter().copied())
        .for_each(&mut *visit);
    callable.declared_ret.into_iter().for_each(&mut *visit);
    callable.equality_bound.into_iter().for_each(&mut *visit);
    callable
        .generic_sig
        .iter()
        .for_each(|generic| visit_generic(generic, visit));
    callable
        .default_realization
        .iter()
        .for_each(|realization| visit_default_realization(realization, visit));
    callable
        .inline_body_plan
        .iter()
        .for_each(|plan| visit_inline_plan(plan, visit));
}

fn visit_generic(generic: &GenericSig, visit: &mut impl FnMut(Ty)) {
    generic.receiver.into_iter().for_each(&mut *visit);
    generic.params.iter().copied().for_each(&mut *visit);
    visit(generic.ret);
    generic
        .formal_bounds
        .iter()
        .flatten()
        .copied()
        .for_each(&mut *visit);
}

fn visit_call_sig(call: &CallSig, visit: &mut impl FnMut(Ty)) {
    call.lambda_param_types
        .iter()
        .flatten()
        .copied()
        .for_each(&mut *visit);
    call.lambda_receivers
        .iter()
        .flatten()
        .copied()
        .for_each(&mut *visit);
}

fn visit_default_realization(realization: &DefaultCallRealization, visit: &mut impl FnMut(Ty)) {
    realization
        .real_params
        .iter()
        .copied()
        .for_each(&mut *visit);
    visit(realization.ret);
}

fn visit_inline_plan(plan: &InlineBodyPlan, visit: &mut impl FnMut(Ty)) {
    if let InlineBodyPlan::InvokeLambda {
        prologue,
        cleanup,
        recovery,
        ..
    } = plan
    {
        for call in prologue.iter().chain(cleanup) {
            visit_callable(&call.callable, &mut *visit);
        }
        if let Some(recovery) = recovery {
            visit(recovery.caught);
            visit_member(&recovery.constructor, &mut *visit);
            visit_callable(&recovery.failure.callable, &mut *visit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::SourceMember;
    use crate::symbol_source::SymbolSource;

    #[test]
    fn backend_facts_reject_pending_inside_a_nested_member_type() {
        let classifier = crate::types::type_name("sample/Box");
        let mut shape = LibraryType::declaration_header();
        shape.members.push(LibraryMember::new(
            "read".to_owned(),
            Vec::new(),
            Ty::obj_args("sample/Result", &[Ty::nullable(Ty::Pending)]),
            String::new(),
        ));

        assert_eq!(
            BackendModuleFacts::from_classifiers([(classifier, shape)], []).err(),
            Some(BackendFactError::UnpublishableClassifier(
                classifier,
                UndeterminedType::Pending,
            ))
        );
    }

    #[test]
    fn backend_facts_reject_error_inside_value_class_metadata() {
        let classifier = crate::types::type_name("sample/Value");
        let mut shape = LibraryType::declaration_header();
        shape.value_underlying = Some(Ty::Error);

        assert_eq!(
            BackendModuleFacts::from_classifiers([(classifier, shape)], []).err(),
            Some(BackendFactError::UnpublishableClassifier(
                classifier,
                UndeterminedType::Error,
            ))
        );
    }

    #[test]
    fn backend_facts_project_only_the_exact_member_surface() {
        let classifier = crate::types::type_name("sample/Owner");
        let mut shape = LibraryType::declaration_header();
        let mut member = LibraryMember::new("run".to_owned(), Vec::new(), Ty::Unit, String::new());
        member.source_member = Some(SourceMember::Class {
            file: 4,
            owner: 8,
            method: 15,
        });
        member.inline_body_plan = Some(Box::new(InlineBodyPlan::InvokeLambda {
            lambda_parameter: 0,
            arguments: Vec::new(),
            prologue: Vec::new(),
            cleanup: Vec::new(),
            cause: None,
            recovery: None,
            defaults: Vec::new(),
            result: None,
        }));
        shape.members.push(member);

        let facts = BackendModuleFacts::from_classifiers([(classifier, shape)], []).unwrap();
        let frozen = facts.classifier(classifier).unwrap();
        assert_eq!(frozen.surface.len(), 1);
        assert_eq!(
            frozen.surface[0].name,
            BackendMemberName::Declared("run".into())
        );
        assert!(frozen.surface[0].params.is_empty());
        assert_eq!(frozen.surface[0].ret, Ty::Unit);
    }

    #[test]
    fn stable_index_projects_the_same_interface_backend_surface_as_module_symbols() {
        let source = r#"
            interface AuditI<T> {
                fun echo(value: T): T = value
                fun text(value: String): String = value
                val answer: Int get() = 42
                var item: String
                    get() = ""
                    set(replacement) {}
            }
        "#;
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::frontend::SourceInput::kotlin(source).with_file_stem("Audit")],
            Box::new(crate::libraries::EmptySymbolSource),
            &crate::features::LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let index = analysis
            .streamed
            .as_ref()
            .expect("streamed module")
            .module
            .index();
        let classifier = crate::types::type_name("AuditI");
        let stable = BackendModuleFacts::from_resolved_index(index)
            .unwrap()
            .classifier(classifier)
            .expect("stable classifier");
        let provider = crate::module_symbols::ModuleSymbols::new(&analysis.symbols);
        let legacy = provider.classifier(classifier).expect("module classifier");
        let legacy = BackendClassifierFact::from_library(&legacy);
        assert_eq!(*stable, legacy);
    }
}
