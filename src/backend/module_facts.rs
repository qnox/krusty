//! Frozen semantic classifier facts exposed to representation backends.
//!
//! Pass 2 still owns a migration-era frontend symbol table while it checks one active source unit.
//! A backend must never receive that table: it contains lookup APIs, AST-backed member keys, and can
//! contain provisional body-local entries for a source that has not streamed yet. This module copies
//! only finalized classifier records before Pass 2 starts and rejects every nested pending/error type.

use crate::fx_hash::{HashMap, HashSet};
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

/// Frozen classifier and declaration-name facts a backend may inspect after common lowering.
///
/// Callable/property lookup, overload sets, and source names are absent deliberately: checked IR
/// already owns every selected declaration identity and realization. A finalized module contract
/// may retain those opaque identities, but exposes no way to reopen their declarations. The
/// returned classifier is guaranteed not to contain `Ty::Pending`, `Ty::Error`, source type
/// references, or ordinary source-body payloads.
pub trait BackendClassifierSource {
    fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>>;

    /// Own name evidence for an already-bound classifier or alias declaration. An outer `Some`
    /// preserves selected unknown evidence, rather than consulting another declaration.
    fn classifier_declaration_name(
        &self,
        declaration: crate::libraries::ClassifierDeclarationKey,
    ) -> Option<Option<crate::types::ClassifierDeclarationName>> {
        match declaration {
            crate::libraries::ClassifierDeclarationKey::Ordinary(identity) => self
                .classifier(identity)
                .map(|record| record.declaration_name.clone()),
            crate::libraries::ClassifierDeclarationKey::TypeAlias(_) => None,
        }
    }
}

/// The exact classifier information representation backends may inspect after semantic checking.
/// Resolver candidate maps, constructors, constants, source keys, inline plans, contracts, and
/// parser-backed member identities cannot be represented here.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendClassifierFact {
    pub declaration_name: Option<crate::types::ClassifierDeclarationName>,
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
    /// The static accessor realizing an enum's synthetic `entries` property, as its provider
    /// published it: present for an enum of this module and for a dependency enum that declares
    /// one, absent for an enum without (a Java enum, or a Kotlin enum compiled before `entries`).
    pub enum_entries_accessor: Option<EnumEntriesAccessor>,
}

/// The exact static call target of an enum's `entries` accessor: its owner, physical name and
/// descriptor, carried from the provider's [`LibraryMember`](crate::libraries::LibraryMember).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumEntriesAccessor {
    pub owner: TypeName,
    pub name: Box<str>,
    pub descriptor: Box<str>,
}

impl EnumEntriesAccessor {
    fn from_member(owner: TypeName, member: &crate::libraries::LibraryMember) -> Self {
        Self {
            owner: member.owner.unwrap_or(owner),
            name: member
                .physical_name
                .as_deref()
                .unwrap_or(&member.name)
                .into(),
            descriptor: member.descriptor.as_str().into(),
        }
    }
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
    /// The serial elements the serialization plugin committed to for the class.
    serializable_elements: Option<crate::types::SerializableClassElements>,
}

impl BackendClassifierFact {
    fn from_library(classifier: TypeName, shape: &LibraryType) -> Self {
        Self {
            declaration_name: shape.declaration_name.clone(),
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
            enum_entries_accessor: shape
                .enum_entries_accessor
                .as_ref()
                .map(|accessor| EnumEntriesAccessor::from_member(classifier, accessor)),
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
    declaration_names: HashMap<
        crate::libraries::ClassifierDeclarationKey,
        Option<crate::types::ClassifierDeclarationName>,
    >,
    generated_classifiers: Box<[crate::types::GeneratedClassifierFact]>,
    serialization_generated_member_statuses:
        Option<crate::types::SerializationGeneratedMemberStatuses>,
    /// Stable identities of classifiers declared inside ordinary bodies. Their completed semantic
    /// shape belongs to the active Pass-2 IR file, never to the pre-Pass-2 module snapshot. Keeping
    /// only the identity prevents an accidental lookup of a same-named dependency classifier.
    body_local_classifiers: HashSet<TypeName>,
    source_value_classes: HashMap<TypeName, Ty>,
    metadata_readable_value_classes: HashSet<TypeName>,
    /// Finalized inherited-interface defaults of named source classifiers. These records retain
    /// the exact selected declaration identities and applied types, so every Pass-2 file can make
    /// its target-representation decision without walking providers or recovering a member name.
    inherited_defaults: HashMap<TypeName, Box<[crate::fir::ResolvedInheritedDefault]>>,
    source_declarations: HashMap<TypeName, SourceDeclaration>,
    /// Each source's checked file-annotation applications, for sources that declare any.
    file_annotations: HashMap<crate::fir::SourceFileId, Box<[crate::types::ResolvedAnnotation]>>,
    /// The dispatch shape of every named class, for a target that numbers slots itself.
    classes: super::class_tables::module_classes::ModuleClasses,
    /// Each dependency classifier the module's own classes stand behind, with the file of every
    /// source class that can be an instance of it, body-local ones included.
    dependency_implementors: HashMap<TypeName, Box<[crate::fir::SourceFileId]>>,
    /// Each `expect` classifier an `actual typealias` actualizes, mapped to the alias's target.
    /// Common code types the `expect` as its own classifier; every target realizes the target.
    actualized_expects: HashMap<TypeName, TypeName>,
    /// Each classifier nested in such an `expect` class, mapped to the same-named classifier
    /// nested in the alias's target. No alias names it, so it is never abbreviated.
    actualized_nested_expects: HashMap<TypeName, TypeName>,
}

/// The classifier an `expect` classifier stands for: the target of the `actual typealias` naming
/// it, or, for one nested in such a class, the same-named classifier nested in that target.
fn actualized_expect_target(
    index: &crate::fir::ResolvedModuleIndex,
    classifier: TypeName,
) -> Option<TypeName> {
    if let Some(alias) = index.type_alias_by_identity(classifier) {
        return Some(alias.target);
    }
    let target = actualized_expect_target(index, classifier.nested_owner()?)?;
    Some(crate::types::type_name_nested_child(
        target,
        classifier.nested_segment_ref(),
    ))
}

impl BackendModuleFacts {
    /// Build the complete current-module backend view from stable, pending-free declaration
    /// headers. No resolver/provider record or Pass-1 parser coordinate participates.
    pub(crate) fn from_resolved_index(
        index: &crate::fir::ResolvedModuleIndex,
    ) -> Result<Self, BackendFactError> {
        let mut classifiers = HashMap::default();
        let mut declaration_names = HashMap::default();
        let mut body_local_classifiers = HashSet::default();
        let mut source_value_classes = HashMap::default();
        let mut metadata_readable_value_classes = HashSet::default();
        let mut inherited_defaults = HashMap::default();
        let mut source_declarations = HashMap::default();
        let mut generated_classifiers = Vec::new();
        let mut source_supertypes = HashMap::default();
        let mut actualized_expects = HashMap::default();
        let mut actualized_nested_expects = HashMap::default();
        for raw in 0..index.declaration_count() {
            let declaration = crate::fir::DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            if let Some(alias) = index.type_alias_header(declaration) {
                declaration_names.insert(
                    crate::libraries::ClassifierDeclarationKey::TypeAlias(alias.identity),
                    alias.declaration_name.clone(),
                );
            }
            let Some(classifier) = index.classifier_header(declaration) else {
                continue;
            };
            declaration_names
                .entry(crate::libraries::ClassifierDeclarationKey::Ordinary(
                    classifier.classifier,
                ))
                .or_insert_with(|| classifier.declaration_name.clone());
            let instantiable = index.declaration_header(declaration).is_some_and(|header| {
                !header.flags.has(crate::fir::DeclarationFlags::INTERFACE)
                    && !header
                        .flags
                        .has(crate::fir::DeclarationFlags::ANNOTATION_CLASS)
                    && !header.flags.has(crate::fir::DeclarationFlags::ABSTRACT)
            });
            source_supertypes.insert(
                classifier.classifier,
                (
                    instantiable
                        .then(|| index.declaration_anchor(declaration))
                        .flatten()
                        .map(|anchor| anchor.source),
                    classifier
                        .declared_supertypes()
                        .filter_map(|ty| ty.get().non_null().obj_internal())
                        .collect::<Box<[_]>>(),
                ),
            );
            if index.is_body_local_declaration(declaration) {
                body_local_classifiers.insert(classifier.classifier);
                continue;
            }
            let declaration_header = index.declaration_header(declaration).ok_or(
                BackendFactError::IncompleteClassifier(classifier.classifier),
            )?;
            // An `expect` an `actual typealias` actualized is realized as the alias target. An
            // `@OptionalExpectation` annotation without an actual stays a classifier of its own.
            if let Some(target) = declaration_header
                .flags
                .has(crate::fir::DeclarationFlags::EXPECT)
                .then(|| actualized_expect_target(index, classifier.classifier))
                .flatten()
            {
                match index.type_alias_by_identity(classifier.classifier) {
                    Some(_) => actualized_expects.insert(classifier.classifier, target),
                    None => actualized_nested_expects.insert(classifier.classifier, target),
                };
                continue;
            }
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
                declaration_name: classifier.declaration_name.clone(),
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
                enum_entries_accessor: (kind == crate::libraries::TypeKind::Enum).then(|| {
                    EnumEntriesAccessor::from_member(
                        classifier.classifier,
                        &crate::libraries::LibraryMember::source_enum_entries_accessor(
                            classifier.classifier,
                        ),
                    )
                }),
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
                    serializable_elements: index.serializable_class_elements(declaration).cloned(),
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
            if let Some(defaults) = index.inherited_defaults(declaration) {
                inherited_defaults
                    .insert(classifier.classifier, defaults.to_vec().into_boxed_slice());
            }
            classifiers.insert(classifier.classifier, Arc::new(fact));
        }
        Ok(Self {
            classifiers,
            declaration_names,
            generated_classifiers: generated_classifiers.into_boxed_slice(),
            serialization_generated_member_statuses: index
                .serialization_generated_member_statuses(),
            body_local_classifiers,
            source_value_classes,
            metadata_readable_value_classes,
            inherited_defaults,
            source_declarations,
            file_annotations: index
                .file_annotation_applications()
                .filter(|(_, applications)| !applications.is_empty())
                .map(|(source, applications)| (source, Box::from(applications)))
                .collect(),
            classes: Default::default(),
            dependency_implementors: dependency_implementors(&source_supertypes),
            actualized_expects,
            actualized_nested_expects,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_classifiers(
        source: impl IntoIterator<Item = (TypeName, LibraryType)>,
        body_local_classifiers: impl IntoIterator<Item = TypeName>,
    ) -> Result<Self, BackendFactError> {
        let mut classifiers = HashMap::default();
        let mut source_value_classes = HashMap::default();
        let mut metadata_readable_value_classes = HashSet::default();
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
                Arc::new(BackendClassifierFact::from_library(classifier, &shape)),
            );
        }
        Ok(Self {
            declaration_names: classifiers
                .iter()
                .map(|(identity, record)| {
                    (
                        crate::libraries::ClassifierDeclarationKey::Ordinary(*identity),
                        record.declaration_name.clone(),
                    )
                })
                .collect(),
            classifiers,
            generated_classifiers: Box::default(),
            serialization_generated_member_statuses: None,
            body_local_classifiers: body_local_classifiers.into_iter().collect(),
            source_value_classes,
            metadata_readable_value_classes,
            inherited_defaults: HashMap::default(),
            source_declarations: HashMap::default(),
            file_annotations: HashMap::default(),
            classes: Default::default(),
            dependency_implementors: HashMap::default(),
            actualized_expects: HashMap::default(),
            actualized_nested_expects: HashMap::default(),
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

    pub(crate) fn serialization_generated_member_statuses(
        &self,
    ) -> Option<crate::types::SerializationGeneratedMemberStatuses> {
        self.serialization_generated_member_statuses
    }

    /// Each `expect` classifier an `actual typealias` actualizes, mapped to the alias's target.
    pub(crate) fn actualized_expects(
        &self,
    ) -> (&HashMap<TypeName, TypeName>, &HashMap<TypeName, TypeName>) {
        (&self.actualized_expects, &self.actualized_nested_expects)
    }

    pub fn is_body_local(&self, classifier: TypeName) -> bool {
        self.body_local_classifiers.contains(&classifier)
    }

    /// These facts with the dispatch shape of every named class of `index`, which only a target
    /// that numbers dispatch slots itself reads ([`Backend::numbers_slots`](super::Backend)).
    pub(crate) fn with_classes(mut self, index: &crate::fir::ResolvedModuleIndex) -> Self {
        self.classes =
            super::class_tables::module_classes::ModuleClasses::from_resolved_index(index);
        self
    }

    /// The dispatch shape of every named class of the module, when the target asked for it.
    pub(crate) fn classes(&self) -> &super::class_tables::module_classes::ModuleClasses {
        &self.classes
    }

    pub fn source_value_classes(&self) -> &HashMap<TypeName, Ty> {
        &self.source_value_classes
    }

    pub fn metadata_readable_value_classes(&self) -> &HashSet<TypeName> {
        &self.metadata_readable_value_classes
    }

    /// The checked defaults `classifier` inherits without overriding, independent of which source
    /// file is currently entering Pass 2.
    pub(crate) fn inherited_defaults(
        &self,
        classifier: TypeName,
    ) -> Option<&[crate::fir::ResolvedInheritedDefault]> {
        self.inherited_defaults.get(&classifier).map(Box::as_ref)
    }

    /// The declaring file of each source class of the module, whichever file declares it, that
    /// can be an instance of the dependency classifier `dependency`.
    pub fn dependency_implementors(&self, dependency: TypeName) -> &[crate::fir::SourceFileId] {
        self.dependency_implementors
            .get(&dependency)
            .map_or(&[], |implementors| &implementors[..])
    }

    /// `source`'s checked file-annotation applications, empty when it declares none.
    pub fn file_annotations(
        &self,
        source: crate::fir::SourceFileId,
    ) -> &[crate::types::ResolvedAnnotation] {
        self.file_annotations
            .get(&source)
            .map_or(&[], |applications| &applications[..])
    }
}

/// For each dependency classifier a source class reaches through its own and its source
/// supertypes' declarations, the file of each instantiable source class that reaches it.
fn dependency_implementors(
    source_supertypes: &HashMap<TypeName, (Option<crate::fir::SourceFileId>, Box<[TypeName]>)>,
) -> HashMap<TypeName, Box<[crate::fir::SourceFileId]>> {
    let mut implementors: HashMap<TypeName, Vec<crate::fir::SourceFileId>> = HashMap::default();
    for (&class, (instantiable, _)) in source_supertypes {
        let Some(file) = *instantiable else {
            continue;
        };
        let mut seen = HashSet::default();
        let mut pending = vec![class];
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            match source_supertypes.get(&current) {
                Some((_, supertypes)) => pending.extend(supertypes.iter().copied()),
                None => implementors.entry(current).or_default().push(file),
            }
        }
    }
    implementors
        .into_iter()
        .map(|(dependency, classes)| (dependency, classes.into_boxed_slice()))
        .collect()
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
    fn classifier_declaration_name(
        &self,
        declaration: crate::libraries::ClassifierDeclarationKey,
    ) -> Option<Option<crate::types::ClassifierDeclarationName>> {
        if let Some(name) = self.module.declaration_names.get(&declaration) {
            return Some(name.clone());
        }
        if matches!(declaration, crate::libraries::ClassifierDeclarationKey::Ordinary(identity) if self.module.is_body_local(identity))
        {
            return Some(None);
        }
        self.dependencies.classifier_declaration_name(declaration)
    }

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
        Some(Arc::new(BackendClassifierFact::from_library(
            classifier, &shape,
        )))
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

    fn serializable_class_elements(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::SerializableClassElements> {
        if let Some(source) = self.module.source_declarations.get(&classifier) {
            return source.serializable_elements.clone();
        }
        // A body-local classifier's completed shape belongs to its own file's IR.
        if self.module.classifier(classifier).is_some() || self.module.is_body_local(classifier) {
            return None;
        }
        self.dependencies.serializable_class_elements(classifier)
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

    fn classifier_name_snapshot(source: &str) -> BackendModuleFacts {
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::frontend::SourceInput::kotlin(source).with_file_stem("Ownership")],
            crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
            &crate::features::LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let module = analysis
            .streamed
            .as_ref()
            .expect("streamed module")
            .module
            .index();
        let facts = BackendModuleFacts::from_resolved_index(module).unwrap();
        let source = crate::module_symbols::ModuleSymbols::new(&analysis.symbols);
        for (identity, fact) in facts.classifiers() {
            let shape = source.classifier(identity).expect("source classifier");
            assert_eq!(fact.declaration_name, shape.declaration_name);
            let declaration = module.classifier_declaration(identity).unwrap();
            let header = module.classifier_header(declaration).unwrap();
            let flags = module.declaration_header(declaration).unwrap().flags;
            let class = crate::ir::IrClass::source_skeleton(header, flags);
            assert_eq!(class.declaration_name, fact.declaration_name);
        }
        facts
    }

    #[test]
    fn classifier_declaration_names_retain_companion_segments() {
        use crate::types::{type_name, ClassifierDeclarationName};
        let outer = type_name("ownership/companions/Outer");
        for (source, name) in [
            ("package ownership.companions\nclass Outer { companion object { class Child } }", "Companion"),
            ("package ownership.companions\nclass Outer { companion object `Named$Literal` { class Child } }", "Named$Literal"),
        ] {
            let facts = classifier_name_snapshot(source);
            let companion = outer.nested_child(name);
            assert_eq!(facts.classifier(companion).unwrap().declaration_name,
                Some(ClassifierDeclarationName::Nested { owner: outer, name: name.into() }));
            assert_eq!(facts.classifier(companion.nested_child("Child")).unwrap().declaration_name,
                Some(ClassifierDeclarationName::Nested { owner: companion, name: "Child".into() }));
        }
    }

    #[test]
    fn classifier_declaration_names_belong_to_each_snapshot() {
        use crate::types::{type_name, type_name_child, ClassifierDeclarationName};
        let package = type_name("ownership/snapshot");
        let outer = type_name_child(package, "Outer");
        let literal = type_name_child(package, "Outer$Literal");
        let top = classifier_name_snapshot(
            "package ownership.snapshot\nclass Outer\nclass `Outer$Literal`\n",
        );
        let nested =
            classifier_name_snapshot("package ownership.snapshot\nclass Outer { class Literal }\n");
        let top_again = classifier_name_snapshot(
            "package ownership.snapshot\nclass Outer\nclass `Outer$Literal`\n",
        );
        let top_name = Some(ClassifierDeclarationName::TopLevel {
            package,
            name: "Outer$Literal".into(),
        });
        assert_eq!(top.classifier(literal).unwrap().declaration_name, top_name);
        assert_eq!(
            top_again.classifier(literal).unwrap().declaration_name,
            top_name
        );
        assert_eq!(
            nested.classifier(literal).unwrap().declaration_name,
            Some(ClassifierDeclarationName::Nested {
                owner: outer,
                name: "Literal".into()
            })
        );
    }

    #[test]
    fn classifier_declaration_names_do_not_imply_outer_instance_capture() {
        use crate::types::{
            type_name, type_name_child, type_name_nested_child, ClassifierDeclarationName,
        };
        let package = type_name("ownership/capture");
        let owner = type_name_child(package, "Outer$Literal");
        let facts = classifier_name_snapshot(
            "package ownership.capture\nclass `Outer$Literal` { class `Part$Name`; inner class Captured }\n",
        );
        for (name, capture) in [("Part$Name", None), ("Captured", Some(owner))] {
            let fact = facts
                .classifier(type_name_nested_child(owner, name))
                .unwrap();
            assert_eq!(
                fact.declaration_name,
                Some(ClassifierDeclarationName::Nested {
                    owner,
                    name: name.into()
                })
            );
            assert_eq!(fact.outer_instance, capture);
        }
    }

    #[test]
    fn missing_classifier_declaration_names_stay_unknown() {
        let owner = crate::types::type_name("ownership/Unknown");
        let nested = owner.nested_child("Member");
        let shape = LibraryType::declaration_header();
        assert_eq!(
            BackendClassifierFact::from_library(nested, &shape).declaration_name,
            None
        );
    }

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
            crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
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
        let legacy = BackendClassifierFact::from_library(classifier, &legacy);
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
            crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
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
            crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
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
