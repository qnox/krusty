//! Stable declaration metadata publication and readback.

use super::*;

/// Stable lexical ownership and the later checked-completion state for body-local classifiers.
#[derive(Debug, Default, PartialEq)]
pub(super) struct BodyLocalOwnership {
    roots: HashMap<DeclarationId, DeclarationId>,
    completed: crate::fx_hash::HashSet<DeclarationId>,
}

impl BodyLocalOwnership {
    pub(super) fn root(&self, declaration: DeclarationId) -> Option<DeclarationId> {
        self.roots.get(&declaration).copied()
    }

    pub(super) fn publish_roots(&mut self, roots: HashMap<DeclarationId, DeclarationId>) {
        assert!(
            self.roots.is_empty(),
            "local-classifier lexical ownership may be published only once"
        );
        self.roots = roots;
    }

    fn complete(&mut self, declaration: DeclarationId) {
        self.completed.insert(declaration);
    }

    fn is_complete(&self, declaration: DeclarationId) -> bool {
        self.completed.contains(&declaration)
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.roots.len() * (std::mem::size_of::<DeclarationId>() * 2)
            + self.completed.len() * std::mem::size_of::<DeclarationId>()
    }
}

/// One accessor of a property, as a key for the annotations that apply to it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PropertyAccessor {
    Getter,
    Setter,
}

/// The declaration site a set of checked annotation applications lands on. A compiler-default
/// accessor has no declaration of its own, so an accessor site is named by its property.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CheckedApplicationSite {
    Declaration(DeclarationId),
    Accessor(DeclarationId, PropertyAccessor),
    /// A value parameter of a function or constructor, by its position among the written value
    /// parameters. A primary constructor's `val`/`var` parameter is one too: its applications
    /// are published here as written, whatever site each lands on.
    Parameter(DeclarationId, u32),
}

/// Fully checked annotation applications. Values are folded against the selected annotation
/// constructor and keyed by stable site; consumers never join them back to the identity-only
/// header list by ordinal.
#[derive(Debug, Default, PartialEq)]
pub(super) struct CheckedAnnotationApplications {
    sites: HashMap<CheckedApplicationSite, Box<[crate::types::ResolvedAnnotation]>>,
    /// The folded defaults of a source annotation class's elements, by class declaration, in
    /// declaration order. An element without a default has no entry.
    element_defaults: HashMap<DeclarationId, Box<[(String, crate::types::AnnotationValue)]>>,
}

impl CheckedAnnotationApplications {
    fn get(&self, site: CheckedApplicationSite) -> &[crate::types::ResolvedAnnotation] {
        self.sites.get(&site).map(Box::as_ref).unwrap_or_default()
    }

    fn publish(
        &mut self,
        site: CheckedApplicationSite,
        annotations: impl IntoIterator<Item = crate::types::ResolvedAnnotation>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if annotations.is_empty() {
            return;
        }
        assert!(
            self.sites.insert(site, annotations).is_none(),
            "a declaration site may publish checked annotation applications only once"
        );
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sites.is_empty() && self.element_defaults.is_empty()
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        let sites = self
            .sites
            .values()
            .map(|annotations| {
                std::mem::size_of::<CheckedApplicationSite>()
                    + std::mem::size_of::<Box<[crate::types::ResolvedAnnotation]>>()
                    + annotations.len() * std::mem::size_of::<crate::types::ResolvedAnnotation>()
            })
            .sum::<usize>();
        let defaults = self
            .element_defaults
            .values()
            .map(|defaults| {
                std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<Box<[(String, crate::types::AnnotationValue)]>>()
                    + std::mem::size_of_val::<[(String, crate::types::AnnotationValue)]>(defaults)
            })
            .sum::<usize>();
        sites + defaults
    }
}

/// The superclass constructor a generated no-arg constructor (kotlinc's no-arg plugin) delegates
/// to, selected by the frontend.
///
/// A selected constructor with parameters has a default for every one of them; the generated
/// constructor calls it with all of them omitted, as kotlinc does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoArgSuperConstructor {
    /// A source constructor of the superclass, by declaration.
    Declared(DeclarationId),
    /// A dependency superclass's constructor, with its parameter types.
    External {
        declaration: ExternalCallableId,
        parameters: Vec<ResolvedTy>,
    },
    /// A constructor any subclass may call as `<init>()`: `Any`'s, or the no-arg constructor the
    /// plugin generates for a matched superclass.
    Unrestricted,
}

impl ResolvedModuleIndex {
    pub fn local_classifier_lexical_root(
        &self,
        declaration: DeclarationId,
    ) -> Option<DeclarationId> {
        self.body_local_ownership.root(declaration)
    }

    pub(in crate::fir) fn publish_local_classifier_lexical_roots(
        &mut self,
        roots: HashMap<DeclarationId, DeclarationId>,
    ) {
        self.body_local_ownership.publish_roots(roots);
    }

    pub(crate) fn is_actualized_away(&self, declaration: DeclarationId) -> bool {
        self.actualized_away_declarations.contains(&declaration)
    }

    pub(crate) fn publish_actualized_away_declarations(
        &mut self,
        declarations: &crate::fx_hash::HashSet<DeclarationId>,
    ) {
        assert!(
            self.actualized_away_declarations.is_empty(),
            "actualization exclusions may be published only once",
        );
        assert!(
            declarations
                .iter()
                .all(|declaration| self.declaration_anchor(*declaration).is_some()),
            "an actualized-away declaration must retain a stable anchor",
        );
        self.actualized_away_declarations
            .extend(declarations.iter().copied());
    }

    /// Mark exact body-local classifier groups whose checked semantic publication completed.
    pub(crate) fn publish_body_local_owner_completions(
        &mut self,
        declarations: impl IntoIterator<Item = DeclarationId>,
    ) {
        for declaration in declarations {
            assert!(
                self.declaration_header(declaration).is_some_and(|header| {
                    header.kind == DeclarationKind::Classifier
                        && header.flags.has(DeclarationFlags::LOCAL_CLASS)
                }),
                "body-local owner completion requires a stable local classifier"
            );
            assert!(
                self.body_local_owner_semantically_complete(declaration),
                "body-local owner completion requires a complete semantic declaration group"
            );
            self.body_local_ownership.complete(declaration);
        }
    }

    /// Whether the active lexical group for this exact local classifier completed publication.
    pub(crate) fn body_local_owner_complete(&self, declaration: DeclarationId) -> bool {
        self.body_local_ownership.is_complete(declaration)
    }

    /// Whether a body-local classifier and every callable in its exact lexical member group have
    /// complete semantic headers. Generated capture storage is published after checked capture
    /// discovery and is not part of the source declaration group.
    pub(crate) fn body_local_owner_semantically_complete(
        &self,
        declaration: DeclarationId,
    ) -> bool {
        self.declaration_header(declaration).is_some_and(|header| {
            header.kind == DeclarationKind::Classifier
                && header.flags.has(DeclarationFlags::LOCAL_CLASS)
        }) && self.declaration_semantic_header_complete(declaration)
            && self
                .owned_declarations(declaration)
                .iter()
                .copied()
                .filter(|candidate| {
                    self.declaration_anchor(*candidate).is_some_and(|anchor| {
                        matches!(
                            anchor.kind,
                            DeclarationKind::Function
                                | DeclarationKind::Constructor
                                | DeclarationKind::Property
                                | DeclarationKind::Accessor
                        )
                    })
                })
                .filter(|candidate| !self.is_suppressed_generated_callable(*candidate))
                .filter(|candidate| {
                    !self.declaration_header(*candidate).is_some_and(|header| {
                        header.flags.has(DeclarationFlags::COMPILER_GENERATED)
                            && header.kind == DeclarationKind::Property
                    })
                })
                .all(|candidate| self.declaration_semantic_header_complete(candidate))
    }

    /// Whether every stable semantic record required by a declaration header is published.
    /// Source-spelling sidecars are independent metadata and deliberately do not participate in
    /// this lifecycle decision.
    pub(crate) fn declaration_semantic_header_complete(&self, declaration: DeclarationId) -> bool {
        let Some(anchor) = self.declaration_anchor(declaration) else {
            return false;
        };
        match anchor.kind {
            DeclarationKind::Classifier => {
                self.classifier_header(declaration).is_some()
                    && self.classifier_type_arguments(declaration).is_some()
                    && self.classifier_hierarchy(declaration).is_some()
                    && self.declaration_type_parameters_complete(declaration)
            }
            DeclarationKind::Function
            | DeclarationKind::Constructor
            | DeclarationKind::Accessor => {
                self.signature(declaration).is_some()
                    && self.callable_for_declaration(declaration).is_some()
                    && self.declaration_type_parameters_complete(declaration)
            }
            DeclarationKind::Property => {
                self.signature(declaration).is_some()
                    && self.property_for_declaration(declaration).is_some()
                    && self.declaration_type_parameters_complete(declaration)
            }
            DeclarationKind::EnumEntry
            | DeclarationKind::TypeAlias
            | DeclarationKind::Initializer
            | DeclarationKind::Script => false,
        }
    }

    /// Complete the primary-constructor parameter role of an already-published classifier
    /// spelling record. Classifier parents, bounds, and context parameters can be demanded in
    /// Pass 1 while the primary constructor remains body-local Pass-2 work (for example, a vararg
    /// constructor). The empty parameter vector is the deliberate partial-state marker; no other
    /// spelling role may be replaced here.
    pub(crate) fn complete_classifier_primary_parameter_spellings(
        &mut self,
        declaration: DeclarationId,
        parameters: Vec<crate::spelling::Spelled>,
    ) {
        assert_eq!(
            self.declaration_anchor(declaration)
                .map(|anchor| anchor.kind),
            Some(DeclarationKind::Classifier),
            "primary-parameter spelling completion belongs to a classifier"
        );
        let record = self
            .declaration_spellings
            .get_mut(&declaration)
            .expect("classifier spelling completion requires its stable Pass-1 record");
        assert!(
            record.params.is_empty(),
            "classifier primary-parameter spellings may be completed only once"
        );
        record.params = parameters;
    }

    pub(crate) fn publish_type_alias_spelling(
        &mut self,
        declaration: DeclarationId,
        spelling: crate::spelling::Spelled,
    ) {
        self.type_aliases
            .get_mut(&declaration)
            .expect("a type-alias spelling requires its published semantic header")
            .expansion_spelling = spelling;
    }

    /// The position `declaration`'s continuation class takes in its scope's generated-class
    /// sequence, or `None` when the declaration is not a source `suspend` function.
    pub fn continuation_ordinal(&self, declaration: DeclarationId) -> Option<u32> {
        self.continuation_ordinals.get(&declaration).copied()
    }

    pub fn publish_continuation_ordinal(&mut self, declaration: DeclarationId, ordinal: u32) {
        assert!(
            self.continuation_ordinals
                .insert(declaration, ordinal)
                .is_none_or(|existing| existing == ordinal),
            "a suspend declaration holds exactly one continuation ordinal"
        );
    }

    /// The superclass constructor `classifier`'s generated no-arg constructor delegates to, or
    /// `None` when the no-arg plugin gives it none.
    pub fn no_arg_constructor(&self, classifier: DeclarationId) -> Option<&NoArgSuperConstructor> {
        self.no_arg_constructors.get(&classifier)
    }

    /// The annotations kotlinc's sam-with-receiver plugin is configured with
    /// ([`crate::symbol_source::SymbolSource::sam_with_receiver_annotations`]).
    pub fn sam_with_receiver_annotations(&self) -> &[TypeName] {
        &self.sam_with_receiver_annotations
    }

    pub fn publish_sam_with_receiver_annotations(&mut self, annotations: &[TypeName]) {
        self.sam_with_receiver_annotations = annotations.into();
    }

    pub fn publish_no_arg_constructor(
        &mut self,
        classifier: DeclarationId,
        superclass_constructor: NoArgSuperConstructor,
    ) {
        assert!(
            self.no_arg_constructors
                .insert(classifier, superclass_constructor)
                .is_none(),
            "a classifier holds at most one generated no-arg constructor"
        );
    }

    /// Every published type alias's expansion spelling, for the one stable-metadata step that
    /// seals its type-use annotations (see [`crate::spelling::TypeUseAnnotation`]).
    pub(crate) fn type_alias_spellings_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut crate::spelling::Spelled> + '_ {
        self.type_aliases
            .values_mut()
            .map(|header| &mut header.expansion_spelling)
    }

    pub fn declaration_annotations(&self, declaration: DeclarationId) -> &[TypeName] {
        self.declaration_annotations
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn declaration_applied_annotations(
        &self,
        declaration: DeclarationId,
    ) -> &[crate::types::ResolvedAnnotation] {
        self.checked_annotation_applications
            .get(CheckedApplicationSite::Declaration(declaration))
    }

    /// The checked applications written on the value parameter at `position` of the function or
    /// constructor `callable`, in source order, each with the use-site target it was written with.
    pub fn parameter_applied_annotations(
        &self,
        callable: DeclarationId,
        position: u32,
    ) -> &[crate::types::ResolvedAnnotation] {
        self.checked_annotation_applications
            .get(CheckedApplicationSite::Parameter(callable, position))
    }

    /// The folded defaults of the elements the source annotation class `class` declares, in
    /// declaration order; an element without a default has none. `None` when the class's defaults
    /// were not published: it is no annotation class, or one of its defaults did not fold.
    pub fn annotation_element_defaults(
        &self,
        class: DeclarationId,
    ) -> Option<&[(String, crate::types::AnnotationValue)]> {
        self.checked_annotation_applications
            .element_defaults
            .get(&class)
            .map(Box::as_ref)
    }

    /// The checked applications on `property`'s `accessor`, whether written on the accessor or
    /// targeted at it from the property declaration (`@get:A`, `@set:A`), in source order with the
    /// targeted ones first.
    pub fn property_accessor_applied_annotations(
        &self,
        property: DeclarationId,
        accessor: PropertyAccessor,
    ) -> &[crate::types::ResolvedAnnotation] {
        self.checked_annotation_applications
            .get(CheckedApplicationSite::Accessor(property, accessor))
    }

    pub fn declaration_annotation_string_arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
    ) -> &[Box<str>] {
        self.declaration_annotation_string_arguments
            .get(&(declaration, annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn declaration_annotation_class_arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
    ) -> &[TypeName] {
        self.declaration_annotation_class_arguments
            .get(&(declaration, annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn generated_classifiers(
        &self,
        declaration: DeclarationId,
    ) -> &[crate::types::GeneratedClassifierFact] {
        self.generated_classifiers
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn has_generated_classifier_purpose(
        &self,
        purpose: crate::types::GeneratedClassifierPurpose,
    ) -> bool {
        self.generated_classifiers
            .values()
            .flatten()
            .any(|classifier| classifier.purpose == purpose)
    }

    pub fn serialization_generated_member_statuses(
        &self,
    ) -> Option<crate::types::SerializationGeneratedMemberStatuses> {
        self.serialization_generated_member_statuses
    }

    pub(crate) fn publish_serialization_generated_member_statuses(
        &mut self,
        statuses: crate::types::SerializationGeneratedMemberStatuses,
    ) {
        assert!(
            self.serialization_generated_member_statuses
                .replace(statuses)
                .is_none(),
            "serialization generated-member statuses may be published only once"
        );
    }

    pub fn serialization_companion_accessor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(&str, TypeName, usize)> {
        self.serialization_companion_accessors
            .get(&declaration)
            .map(|(field, companion, arity)| (field.as_ref(), *companion, *arity))
    }

    /// The custom serializer class's primary constructor selected for `declaration`'s
    /// `@Serializable(with = …)`, with the accessor operand ordinal for each of its parameters.
    pub fn serialization_custom_serializer_constructor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(DeclarationId, &[u32])> {
        self.serialization_custom_serializer_constructors
            .get(&declaration)
            .map(|(constructor, operands)| (*constructor, operands.as_ref()))
    }

    pub fn serialization_type_use_serializer_constructors(
        &self,
    ) -> impl Iterator<Item = (&(TypeName, u32), &ResolvedTypeUseSerializerConstruction)> {
        self.serialization_type_use_serializer_constructors.iter()
    }

    pub fn publish_declaration_annotations(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = TypeName>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if annotations.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotations
                .insert(declaration, annotations)
                .is_none(),
            "a stable declaration may publish its annotation identities only once"
        );
    }

    pub fn publish_declaration_applied_annotations(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = crate::types::ResolvedAnnotation>,
    ) {
        self.checked_annotation_applications.publish(
            CheckedApplicationSite::Declaration(declaration),
            annotations,
        );
    }

    pub fn publish_parameter_applied_annotations(
        &mut self,
        callable: DeclarationId,
        position: u32,
        annotations: impl IntoIterator<Item = crate::types::ResolvedAnnotation>,
    ) {
        self.checked_annotation_applications.publish(
            CheckedApplicationSite::Parameter(callable, position),
            annotations,
        );
    }

    pub fn publish_annotation_element_defaults(
        &mut self,
        class: DeclarationId,
        defaults: Vec<(String, crate::types::AnnotationValue)>,
    ) {
        assert!(
            self.checked_annotation_applications
                .element_defaults
                .insert(class, defaults.into_boxed_slice())
                .is_none(),
            "an annotation class may publish its element defaults only once"
        );
    }

    pub fn publish_property_accessor_applied_annotations(
        &mut self,
        property: DeclarationId,
        accessor: PropertyAccessor,
        annotations: impl IntoIterator<Item = crate::types::ResolvedAnnotation>,
    ) {
        self.checked_annotation_applications.publish(
            CheckedApplicationSite::Accessor(property, accessor),
            annotations,
        );
    }

    pub fn publish_declaration_annotation_string_arguments(
        &mut self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
        arguments: impl IntoIterator<Item = Box<str>>,
    ) {
        let arguments = arguments.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if arguments.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotation_string_arguments
                .insert((declaration, annotation_ordinal), arguments)
                .is_none(),
            "a stable annotation occurrence may publish its string arguments only once"
        );
    }

    pub fn publish_declaration_annotation_class_arguments(
        &mut self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
        arguments: impl IntoIterator<Item = TypeName>,
    ) {
        let arguments = arguments.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if arguments.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotation_class_arguments
                .insert((declaration, annotation_ordinal), arguments)
                .is_none(),
            "a stable annotation occurrence may publish its class arguments only once"
        );
    }

    pub fn publish_generated_classifiers(
        &mut self,
        declaration: DeclarationId,
        classifiers: impl IntoIterator<Item = crate::types::GeneratedClassifierFact>,
    ) {
        let classifiers = classifiers
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if classifiers.is_empty() {
            return;
        }
        assert!(
            self.generated_classifiers
                .insert(declaration, classifiers)
                .is_none(),
            "a stable declaration may publish generated classifiers only once"
        );
    }

    pub fn publish_serialization_companion_accessor(
        &mut self,
        declaration: DeclarationId,
        field: Box<str>,
        companion: TypeName,
        type_parameters: usize,
    ) {
        assert!(
            self.serialization_companion_accessors
                .insert(declaration, (field, companion, type_parameters))
                .is_none(),
            "a source classifier may publish only one serialization companion accessor"
        );
    }

    pub fn publish_serialization_custom_serializer_constructor(
        &mut self,
        declaration: DeclarationId,
        constructor: DeclarationId,
        operands: Box<[u32]>,
    ) {
        assert!(
            self.serialization_custom_serializer_constructors
                .insert(declaration, (constructor, operands))
                .is_none(),
            "a source classifier may publish only one custom serializer constructor"
        );
    }

    pub fn publish_serialization_type_use_serializer_constructor(
        &mut self,
        type_argument_count: u32,
        construction: ResolvedTypeUseSerializerConstruction,
    ) {
        let key = (construction.serializer, type_argument_count);
        if let Some(existing) = self
            .serialization_type_use_serializer_constructors
            .get(&key)
        {
            assert_eq!(
                existing, &construction,
                "equal checked serializer occurrences must select one constructor declaration"
            );
            return;
        }
        self.serialization_type_use_serializer_constructors
            .insert(key, construction);
    }
}
