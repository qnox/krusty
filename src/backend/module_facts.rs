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
use crate::types::{Ty, TypeName, TypeVariance};

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
    pub value_declaration: Option<crate::types::DeclaredValueClass>,
    /// The role its provider published for the declaration in type checks and casts.
    pub role: Option<crate::types::ClassifierRole>,
    /// The declared companion object: the name of the static field holding it and its classifier.
    /// Kotlin metadata publishes it for a dependency and the source declaration for this module.
    pub companion: Option<(Box<str>, TypeName)>,
}

/// Declaration facts of a current-module classifier that a dependency's classifier record carries
/// in its own form (modality, companion, and qualified name).
#[derive(Clone, Debug, PartialEq)]
struct SourceDeclaration {
    is_sealed: bool,
    /// The declared companion object: its static field's name and its classifier.
    companion: Option<(Box<str>, TypeName)>,
    /// The Kotlin qualified name with every boundary dotted (`pkg.Outer.Nested`).
    qualified_name: Option<Box<str>>,
    /// Exact accessor owner and arity published by the serialization frontend plugin.
    serialization_companion_accessor: Option<(Box<str>, TypeName, usize)>,
}

impl BackendClassifierFact {
    fn from_library(shape: &LibraryType) -> Self {
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
            annotations: shape.annotations.clone().into_boxed_slice(),
            own_type_parameter_count: shape.own_type_parameter_count,
            type_param_variances: shape.type_param_variances().to_vec().into_boxed_slice(),
            value_underlying: shape.value_underlying,
            value_declaration: shape.value_declaration.clone(),
            role: shape.classifier_role(),
            companion: shape
                .companion_object
                .as_ref()
                .map(|(field, companion)| (Box::from(field.as_str()), *companion)),
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
                .declared_supertypes()
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
            let mut value_underlying = None;
            let mut value_declaration = None;
            if flags.has(crate::fir::DeclarationFlags::VALUE) {
                for child in index.owned_declarations(declaration).iter().copied() {
                    let Some(child_header) = index.declaration_header(child) else {
                        continue;
                    };
                    if child_header.kind != crate::fir::DeclarationKind::Property
                        || !child_header
                            .flags
                            .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
                    {
                        continue;
                    }
                    let signature =
                        index
                            .signature(child)
                            .ok_or(BackendFactError::IncompleteClassifier(
                                classifier.classifier,
                            ))?;
                    let name = index.declaration_name(child).ok_or(
                        BackendFactError::IncompleteClassifier(classifier.classifier),
                    )?;
                    value_underlying = Some(signature.result.get());
                    value_declaration = Some(crate::types::DeclaredValueClass {
                        property: name.into(),
                        underlying: signature.result.get(),
                        type_parameters: index.own_type_parameter_types(declaration).ok_or(
                            BackendFactError::IncompleteClassifier(classifier.classifier),
                        )?,
                    });
                }
            }
            let companion = source_companion(index, declaration);
            let qualified_name = source_qualified_name(index, declaration);
            let fact = BackendClassifierFact {
                access: declaration_header.visibility.into(),
                is_kotlin: true,
                source: true,
                outer_instance,
                kind,
                is_abstract: flags.has(crate::fir::DeclarationFlags::ABSTRACT) || is_interface,
                is_extensible: !is_interface && !flags.has(crate::fir::DeclarationFlags::FINAL),
                supertypes: direct_supertypes.into_boxed_slice(),
                annotations: index
                    .declaration_applied_annotations(declaration)
                    .to_vec()
                    .into_boxed_slice(),
                own_type_parameter_count,
                type_param_variances,
                value_underlying,
                value_declaration,
                // A source declaration is neither a mapped collection builtin nor a `FunctionN`.
                role: None,
                companion: companion.clone(),
            };
            source_declarations.insert(
                classifier.classifier,
                SourceDeclaration {
                    is_sealed: flags.has(crate::fir::DeclarationFlags::SEALED),
                    companion,
                    qualified_name,
                    serialization_companion_accessor: index
                        .serialization_companion_accessor(declaration)
                        .map(|(field, companion, arity)| (Box::from(field), companion, arity)),
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

/// A declared source companion's exact field spelling and classifier identity. The stable name-tree
/// edge from its enclosing classifier is authoritative even when the segment contains `$`.
fn source_companion(
    index: &crate::fir::ResolvedModuleIndex,
    owner: crate::fir::DeclarationId,
) -> Option<(Box<str>, TypeName)> {
    let declaration = index.companion_declaration(owner)?;
    let classifier = index.classifier_header(declaration)?.classifier;
    let owner = index.classifier_header(owner)?.classifier;
    Some((
        Box::from(classifier.nested_segment_within(owner)?),
        classifier,
    ))
}

/// The dotted Kotlin declaration name of a source classifier. The stable declaration name already
/// contains its lexical classifier path, so no JVM/internal name needs to be reinterpreted.
fn source_qualified_name(
    index: &crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
) -> Option<Box<str>> {
    let source = index.declaration_anchor(declaration)?.source;
    let declaration = index.declaration_name(declaration)?;
    let package = index.source_package(source)?.render().replace('/', ".");
    Some(if package.is_empty() {
        Box::from(declaration)
    } else {
        format!("{package}.{declaration}").into_boxed_str()
    })
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
        BackendClassifierSource::classifier(self, classifier).map(|fact| fact.annotations.to_vec())
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

    fn generated_serializer_singleton(&self, classifier: TypeName) -> Option<TypeName> {
        if let Some(generated) = self
            .module
            .generated_classifiers()
            .iter()
            .find(|generated| {
                generated.lexical_owner == classifier
                    && generated.purpose
                        == crate::types::GeneratedClassifierPurpose::SerializationSerializer
            })
        {
            return (BackendClassifierSource::classifier(self, classifier)?
                .own_type_parameter_count
                == 0)
                .then_some(generated.classifier);
        }
        self.dependencies.generated_serializer_singleton(classifier)
    }

    fn has_serialization_serializer_accessor(
        &self,
        companion: TypeName,
        type_parameters: usize,
    ) -> bool {
        if self.module.source_declarations.values().any(|source| {
            source
                .serialization_companion_accessor
                .as_ref()
                .is_some_and(|(_, owner, arity)| *owner == companion && *arity == type_parameters)
        }) {
            return true;
        }
        if let Some(generated) = self
            .module
            .generated_classifiers()
            .iter()
            .find(|generated| {
                generated.classifier == companion
                    && generated.purpose
                        == crate::types::GeneratedClassifierPurpose::SerializationCompanion
            })
        {
            return self
                .module
                .classifier(generated.lexical_owner)
                .is_some_and(|owner| owner.own_type_parameter_count == type_parameters);
        }
        self.dependencies
            .has_serialization_serializer_accessor(companion, type_parameters)
    }

    fn serialization_companion(&self, classifier: TypeName) -> Option<(Box<str>, TypeName)> {
        self.module
            .source_declarations
            .get(&classifier)
            .and_then(|source| {
                source
                    .serialization_companion_accessor
                    .as_ref()
                    .map(|(field, companion, _)| (field.clone(), *companion))
                    .or_else(|| source.companion.clone())
            })
            .or_else(|| {
                self.module
                    .generated_classifiers()
                    .iter()
                    .find(|generated| {
                        generated.lexical_owner == classifier
                            && generated.purpose
                                == crate::types::GeneratedClassifierPurpose::SerializationCompanion
                    })
                    .map(|generated| (generated.source_name.clone(), generated.classifier))
            })
            .or_else(|| {
                self.dependencies
                    .classifier(classifier)?
                    .companion_object
                    .as_ref()
                    .map(|(field, companion)| (Box::from(field.as_str()), *companion))
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
            .and_then(|fact| Some(fact.value_declaration.as_ref()?.property.to_string()))
    }

    fn classifier_value_declaration(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::DeclaredValueClass> {
        BackendClassifierSource::classifier(self, classifier)?
            .value_declaration
            .clone()
    }

    fn classifier_role(&self, classifier: TypeName) -> Option<crate::types::ClassifierRole> {
        BackendClassifierSource::classifier(self, classifier)?.role
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

    #[test]
    fn source_qualified_name_retains_each_lexical_classifier_segment() {
        let source = r#"
            package demo
            class Outer { object Inner }
        "#;
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::frontend::SourceInput::kotlin(source).with_file_stem("Nested")],
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
        let inner = index
            .classifier_declaration(crate::types::type_name("demo/Outer.Inner"))
            .expect("nested classifier declaration");

        assert_eq!(
            source_qualified_name(index, inner).as_deref(),
            Some("demo.Outer.Inner")
        );
    }

    #[test]
    fn source_companion_retains_only_its_exact_field_segment() {
        let source = r#"
            package demo
            class Outer { companion object Named }
        "#;
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::frontend::SourceInput::kotlin(source).with_file_stem("Companion")],
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
        let outer = index
            .classifier_declaration(crate::types::type_name("demo/Outer"))
            .expect("enclosing classifier declaration");

        assert_eq!(
            source_companion(index, outer),
            Some((
                Box::from("Named"),
                crate::types::type_name("demo/Outer.Named")
            ))
        );
    }
}
