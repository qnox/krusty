//! Target representation shapes selected from exact declaration identities and frozen hierarchy
//! facts. Standard-library identities are native-backend implementation keys; they do not enter
//! provider records or common IR as a parallel semantic-role taxonomy.

use crate::backend::BackendClassifierSource;
use crate::ir::TypeCheckRole;
use crate::ir::{ClassId, IrFile};
use crate::types::{ClassifierRole, CollectionKind, MappedCollection, Ty, TypeName};

use super::super::super::intrinsics::CollectionShape;
use super::FileLowering;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StandardCollectionImplementation {
    List,
    Map,
    Set,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IntegralRangeElement {
    Int,
    Long,
    Char,
    UInt,
    ULong,
}

fn is_identity(internal: TypeName, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| super::super::super::intrinsics::classifier_matches(internal, name))
}

pub(super) fn standard_collection_implementation(
    internal: TypeName,
) -> Option<StandardCollectionImplementation> {
    Some(
        if is_identity(internal, &["kotlin/collections/ArrayList"]) {
            StandardCollectionImplementation::List
        } else if is_identity(
            internal,
            &[
                "kotlin/collections/HashMap",
                "kotlin/collections/LinkedHashMap",
            ],
        ) {
            StandardCollectionImplementation::Map
        } else if is_identity(
            internal,
            &[
                "kotlin/collections/HashSet",
                "kotlin/collections/LinkedHashSet",
            ],
        ) {
            StandardCollectionImplementation::Set
        } else {
            return None;
        },
    )
}

pub(super) fn is_lazy(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/Lazy"])
}

pub(super) fn is_read_write_property(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/properties/ReadWriteProperty"])
}

pub(super) fn is_read_only_property(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/properties/ReadOnlyProperty"])
}

pub(super) fn is_pair(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/Pair"])
}

pub(super) fn is_indexed_value(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/collections/IndexedValue"])
}

pub(super) fn is_closed_range(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/ranges/ClosedRange"])
}

pub(super) fn is_closed_floating_range(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/ranges/ClosedFloatingPointRange"])
}

pub(super) fn integral_range_element(internal: TypeName) -> Option<IntegralRangeElement> {
    Some(
        if is_identity(
            internal,
            &["kotlin/ranges/IntRange", "kotlin/ranges/IntProgression"],
        ) {
            IntegralRangeElement::Int
        } else if is_identity(
            internal,
            &["kotlin/ranges/LongRange", "kotlin/ranges/LongProgression"],
        ) {
            IntegralRangeElement::Long
        } else if is_identity(
            internal,
            &["kotlin/ranges/CharRange", "kotlin/ranges/CharProgression"],
        ) {
            IntegralRangeElement::Char
        } else if is_identity(
            internal,
            &["kotlin/ranges/UIntRange", "kotlin/ranges/UIntProgression"],
        ) {
            IntegralRangeElement::UInt
        } else if is_identity(
            internal,
            &["kotlin/ranges/ULongRange", "kotlin/ranges/ULongProgression"],
        ) {
            IntegralRangeElement::ULong
        } else {
            return None;
        },
    )
}

pub(super) fn is_integral_range_iterator(internal: TypeName) -> bool {
    is_identity(
        internal,
        &[
            "kotlin/collections/IntIterator",
            "kotlin/collections/LongIterator",
            "kotlin/collections/CharIterator",
            "kotlin/collections/UIntIterator",
            "kotlin/collections/ULongIterator",
        ],
    )
}

pub(super) fn is_number(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/Number"])
}

pub(super) fn is_comparable(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/Comparable"])
}

pub(super) fn is_char_sequence(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/CharSequence"])
}

pub(super) fn is_string_builder(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/text/StringBuilder"])
}

pub(super) fn is_sequence(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/sequences/Sequence"])
}

/// `kotlin.enums.EnumEntries`, which the runtime's `EnumEntriesList` implements.
pub(super) fn is_enum_entries(internal: TypeName) -> bool {
    is_identity(internal, &["kotlin/enums/EnumEntries"])
}

fn standard_interface_descriptor(internal: TypeName) -> Option<&'static str> {
    Some(if internal == crate::types::wk::function_root() {
        "kt_type_function"
    } else if is_comparable(internal) {
        "kt_type_comparable"
    } else if is_char_sequence(internal) {
        "kt_type_char_sequence"
    } else if is_identity(internal, &["kotlin/reflect/KCallable"]) {
        "kt_type_kcallable"
    } else if is_identity(internal, &["kotlin/reflect/KProperty"]) {
        "kt_type_kproperty"
    } else if is_identity(internal, &["kotlin/reflect/KProperty0"]) {
        "kt_type_kproperty0"
    } else if is_identity(internal, &["kotlin/reflect/KProperty1"]) {
        "kt_type_kproperty1"
    } else if is_identity(internal, &["kotlin/reflect/KProperty2"]) {
        "kt_type_kproperty2"
    } else if is_identity(internal, &["kotlin/reflect/KMutableProperty"]) {
        "kt_type_kmutable_property"
    } else if is_identity(internal, &["kotlin/reflect/KMutableProperty0"]) {
        "kt_type_kmutable_property0"
    } else if is_identity(internal, &["kotlin/reflect/KMutableProperty1"]) {
        "kt_type_kmutable_property1"
    } else if is_identity(internal, &["kotlin/reflect/KMutableProperty2"]) {
        "kt_type_kmutable_property2"
    } else {
        return None;
    })
}

/// The most specific mapped-collection role in a classifier's declared supertype graph.
///
/// Concrete JVM implementations do not carry Kotlin collection spellings of their own. Their
/// provider-normalized supertypes do carry these roles, so representation lowering follows that
/// graph instead of maintaining a second table of `ArrayList`/`HashMap`/… names.
pub(super) fn mapped_collection(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> Option<MappedCollection> {
    fn rank(kind: CollectionKind) -> u8 {
        match kind {
            CollectionKind::MapEntry => 8,
            CollectionKind::Map => 7,
            CollectionKind::ListIterator => 6,
            CollectionKind::List | CollectionKind::Set => 5,
            CollectionKind::Collection => 4,
            CollectionKind::Iterator => 3,
            CollectionKind::Iterable => 2,
        }
    }

    let mut pending = vec![internal];
    let mut visited = crate::fx_hash::HashSet::default();
    let mut selected = None;
    while let Some(classifier) = pending.pop() {
        if !visited.insert(classifier) {
            continue;
        }
        let Some(fact) = classifiers.classifier(classifier) else {
            continue;
        };
        if let Some(ClassifierRole::MappedCollection(collection)) = fact.role {
            if selected
                .is_none_or(|current: MappedCollection| rank(collection.kind) > rank(current.kind))
            {
                selected = Some(collection);
            }
        }
        pending.extend(fact.supertypes.iter().copied());
    }
    selected
}

/// What declares a selected collection member, by the declaration owner's OWN identity: a face of
/// a mapped collection interface its provider published a role on, or one of the standard
/// implementations the runtime realizes.
///
/// Unlike [`mapped_collection`], no supertype is consulted. A dependency subtype's own `size` or
/// `clear()` is ITS declaration, not the interface's, and the runtime's representation is not
/// behind it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CollectionDeclaration {
    Interface(MappedCollection),
    Implementation(StandardCollectionImplementation),
}

impl CollectionDeclaration {
    fn kind(self) -> CollectionKind {
        match self {
            Self::Interface(collection) => collection.kind,
            Self::Implementation(StandardCollectionImplementation::List) => CollectionKind::List,
            Self::Implementation(StandardCollectionImplementation::Set) => CollectionKind::Set,
            Self::Implementation(StandardCollectionImplementation::Map) => CollectionKind::Map,
        }
    }

    /// A query member of one of `kinds`: declared on its read-only face, or by an implementation.
    pub(super) fn reads(self, kinds: &[CollectionKind]) -> bool {
        kinds.contains(&self.kind())
            && !matches!(
                self,
                Self::Interface(MappedCollection { mutable: true, .. })
            )
    }

    /// A mutating member of one of `kinds`: declared on its mutable face, or by an implementation.
    pub(super) fn mutates(self, kinds: &[CollectionKind]) -> bool {
        kinds.contains(&self.kind())
            && !matches!(
                self,
                Self::Interface(MappedCollection { mutable: false, .. })
            )
    }
}

pub(super) fn collection_declaration(
    classifiers: &dyn BackendClassifierSource,
    owner: TypeName,
) -> Option<CollectionDeclaration> {
    match classifiers.classifier(owner).and_then(|fact| fact.role) {
        Some(ClassifierRole::MappedCollection(collection)) => {
            Some(CollectionDeclaration::Interface(collection))
        }
        _ => standard_collection_implementation(owner).map(CollectionDeclaration::Implementation),
    }
}

/// The runtime collection shape reached by this classifier's declared supertype graph.
///
/// Looking through supertypes is necessary for concrete implementations, ranges, and primitive
/// iterators: providers publish the mapping on the collection interface declaration rather than
/// copying it onto every subtype.
pub(super) fn collection_shape(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> Option<CollectionShape> {
    if is_sequence(internal) {
        return Some(CollectionShape::Sequence);
    }
    if internal == crate::types::wk::string()
        || is_char_sequence(internal)
        || is_string_builder(internal)
    {
        return Some(CollectionShape::Text);
    }

    let collection = mapped_collection(classifiers, internal)?;
    Some(match collection.kind {
        CollectionKind::Iterator | CollectionKind::ListIterator => CollectionShape::Iterator,
        CollectionKind::Map => CollectionShape::Map,
        CollectionKind::MapEntry => CollectionShape::MapEntry,
        CollectionKind::Iterable
        | CollectionKind::Collection
        | CollectionKind::List
        | CollectionKind::Set => CollectionShape::Iterable,
    })
}

/// Whether a type check names the read-only List face represented by the runtime's List marker.
pub(super) fn is_list_check_type(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> bool {
    matches!(
        classifiers.classifier(internal).and_then(|fact| fact.role),
        Some(ClassifierRole::MappedCollection(MappedCollection {
            kind: CollectionKind::List,
            mutable: false,
        }))
    )
}

/// The native runtime marker for a checked function role.
///
/// Native function values publish one marker per ordinary source arity through 22. Suspend
/// functions have a distinct semantic role, but this runtime does not yet construct suspend
/// function values or publish suspend markers. Wider ordinary functions likewise have no marker,
/// so both remain an explicit decline instead of being mistaken for another function family.
pub(super) fn function_check_descriptor(role: TypeCheckRole) -> Option<&'static str> {
    const ARITIES: [&str; 23] = [
        "kt_type_function0",
        "kt_type_function1",
        "kt_type_function2",
        "kt_type_function3",
        "kt_type_function4",
        "kt_type_function5",
        "kt_type_function6",
        "kt_type_function7",
        "kt_type_function8",
        "kt_type_function9",
        "kt_type_function10",
        "kt_type_function11",
        "kt_type_function12",
        "kt_type_function13",
        "kt_type_function14",
        "kt_type_function15",
        "kt_type_function16",
        "kt_type_function17",
        "kt_type_function18",
        "kt_type_function19",
        "kt_type_function20",
        "kt_type_function21",
        "kt_type_function22",
    ];
    match role {
        TypeCheckRole::FunctionOfArity(arity) => ARITIES.get(usize::from(arity)).copied(),
        TypeCheckRole::SuspendFunctionOfArity(_) | TypeCheckRole::MutableCollection(_) => None,
    }
}

/// Runtime markers a class's checked function supertypes contribute to its descriptor.
///
/// [`IrClass::interfaces`](crate::ir::IrClass::interfaces) contains nominal classifier identities,
/// while an arrow-written supertype remains a semantic [`Ty::Fun`]. Its checked role therefore
/// travels through [`IrClass::supertypes`](crate::ir::IrClass::supertypes) instead. Native attaches
/// both the arity marker used by `is FunctionN` and the common marker used by `is Function`; it
/// never reconstructs either role from a classifier spelling.
pub(super) fn function_supertype_descriptors(
    roles: impl IntoIterator<Item = TypeCheckRole>,
) -> Vec<&'static str> {
    let mut descriptors = Vec::new();
    for role in roles {
        let physical_arity = match role {
            TypeCheckRole::FunctionOfArity(arity) => Some(arity),
            TypeCheckRole::SuspendFunctionOfArity(arity) => arity.checked_add(1),
            TypeCheckRole::MutableCollection(_) => continue,
        };
        if let Some(arity) = physical_arity
            .and_then(|arity| function_check_descriptor(TypeCheckRole::FunctionOfArity(arity)))
        {
            if !descriptors.contains(&arity) {
                descriptors.push(arity);
            }
        }
        if !descriptors.contains(&"kt_type_function") {
            descriptors.push("kt_type_function");
        }
    }
    descriptors
}

/// Runtime marker descriptors contributed by one source class's checked interface graph.
///
/// Nominal dependency interfaces are flattened through backend classifier facts. Arrow-written
/// function supertypes have no nominal entry in [`crate::ir::IrClass::interfaces`], so their
/// already-checked semantic roles are consumed from [`crate::ir::IrClass::supertypes`].
pub(super) fn class_runtime_interface_descriptors(
    ir: &IrFile,
    classifiers: &dyn BackendClassifierSource,
    class: ClassId,
    local_interfaces: &[ClassId],
) -> Vec<&'static str> {
    let external_roots = ir.classes[class as usize]
        .interfaces
        .iter()
        .chain(
            local_interfaces
                .iter()
                .flat_map(|interface| ir.classes[*interface as usize].interfaces.iter()),
        )
        .filter(|interface| ir.class_id_by_name(*interface).is_none());
    let mut descriptors = runtime_interface_descriptors(classifiers, external_roots);
    let function_roles = std::iter::once(class)
        .chain(local_interfaces.iter().copied())
        .flat_map(|classifier| ir.classes[classifier as usize].supertypes.iter())
        .filter_map(|supertype| ir.type_check_role(*supertype));
    for descriptor in function_supertype_descriptors(function_roles) {
        if !descriptors.contains(&descriptor) {
            descriptors.push(descriptor);
        }
    }
    descriptors
}

/// The native runtime marker for a published non-suspend function classifier.
pub(super) fn function_type_descriptor(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> Option<&'static str> {
    if internal == crate::types::wk::function_root() {
        return Some("kt_type_function");
    }
    let ClassifierRole::FunctionOfArity(arity) = classifiers.classifier(internal)?.role? else {
        return None;
    };
    function_check_descriptor(TypeCheckRole::FunctionOfArity(arity))
}

/// The runtime marker of one of Kotlin's mapped collection interfaces, or `None` where the runtime
/// has none of its own.
pub(super) fn mapped_collection_descriptor(collection: MappedCollection) -> Option<&'static str> {
    match collection.kind {
        CollectionKind::Iterator => Some("kt_type_iterator_interface"),
        CollectionKind::Iterable if collection.mutable => {
            Some("kt_type_mutable_iterable_interface")
        }
        CollectionKind::Iterable => Some("kt_type_iterable_interface"),
        CollectionKind::Collection if collection.mutable => {
            Some("kt_type_mutable_collection_interface")
        }
        CollectionKind::Collection => Some("kt_type_collection_interface"),
        CollectionKind::List if collection.mutable => Some("kt_type_mutable_list_interface"),
        CollectionKind::List => Some("kt_type_list_interface"),
        CollectionKind::Set if collection.mutable => Some("kt_type_mutable_set_interface"),
        CollectionKind::Set => Some("kt_type_set_interface"),
        CollectionKind::Map if collection.mutable => Some("kt_type_mutable_map_interface"),
        CollectionKind::Map => Some("kt_type_map_interface"),
        CollectionKind::MapEntry if collection.mutable => Some("kt_type_mutable_map_entry"),
        CollectionKind::MapEntry => Some("kt_type_map_entry"),
        // The runtime has no distinct ListIterator interface marker. Its provider-published
        // Iterator supertype is visited separately and supplies the marker this target has.
        CollectionKind::ListIterator => None,
    }
}

/// The runtime marker that is exactly one of Kotlin's mapped collection interfaces, or `None` where
/// the marker an `is` check tests stands for another interface too.
pub(super) fn exact_collection_descriptor(collection: MappedCollection) -> Option<&'static str> {
    match collection.kind {
        // `kt_type_iterator_interface` is what `MutableIterator` is tested by as well.
        CollectionKind::Iterator if collection.mutable => None,
        _ => mapped_collection_descriptor(collection),
    }
}

/// Runtime marker descriptors for dependency interfaces in one class's declared interface graph.
///
/// Same-file interfaces have descriptors emitted from their own declarations. Dependency
/// interfaces instead name one of the runtime's representation markers. Follow the provider's
/// direct-supertype graph and flatten those markers here because `KType.interfaces` is deliberately
/// a flat table: `kt_is_instance` must not reopen a provider or recursively interpret interface
/// declarations at run time. The graph's resolved `kotlin.Function` identity contributes the
/// common function marker separately from the arity role on `FunctionN`.
fn runtime_interface_descriptors(
    classifiers: &dyn BackendClassifierSource,
    roots: impl IntoIterator<Item = TypeName>,
) -> Vec<&'static str> {
    fn descriptor(
        internal: TypeName,
        fact: &crate::backend::BackendClassifierFact,
    ) -> Option<&'static str> {
        if !fact.is_interface() {
            return None;
        }
        match fact.role {
            Some(ClassifierRole::MappedCollection(collection)) => {
                mapped_collection_descriptor(collection)
            }
            Some(ClassifierRole::FunctionOfArity(arity)) => {
                function_check_descriptor(TypeCheckRole::FunctionOfArity(arity))
            }
            Some(ClassifierRole::SuspendFunctionOfArity(_)) | None => {
                standard_interface_descriptor(internal)
            }
        }
    }

    let mut pending = roots.into_iter().collect::<Vec<_>>();
    let mut visited = crate::fx_hash::HashSet::default();
    let mut markers = Vec::new();
    while let Some(classifier) = pending.pop() {
        if !visited.insert(classifier) {
            continue;
        }
        let Some(fact) = classifiers.classifier(classifier) else {
            continue;
        };
        if let Some(marker) = descriptor(classifier, &fact) {
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
        pending.extend(fact.supertypes.iter().copied());
    }
    markers
}

impl<'a> FileLowering<'a> {
    pub(super) fn collection_shape(&self, internal: TypeName) -> Option<CollectionShape> {
        collection_shape(self.classifiers, internal)
    }

    pub(super) fn collection_shape_of(&self, ty: Ty) -> Option<CollectionShape> {
        if ty.non_null().is_array() {
            return Some(CollectionShape::Iterable);
        }
        self.collection_shape(ty.non_null().obj_internal()?)
    }

    pub(super) fn mapped_collection(&self, ty: Ty) -> Option<MappedCollection> {
        mapped_collection(self.classifiers, ty.non_null().obj_internal()?)
    }

    pub(super) fn standard_collection_implementation(
        &self,
        internal: TypeName,
    ) -> Option<StandardCollectionImplementation> {
        standard_collection_implementation(internal)
    }

    pub(super) fn iteration_shape_of(&self, ty: Ty) -> Option<CollectionShape> {
        if ty.non_null().is_array() {
            return Some(CollectionShape::Iterable);
        }
        let shape = self.collection_shape(ty.non_null().obj_internal()?)?;
        (shape != CollectionShape::MapEntry).then_some(shape)
    }

    pub(super) fn is_list_check_type(&self, internal: TypeName) -> bool {
        is_list_check_type(self.classifiers, internal)
    }

    pub(super) fn function_type_descriptor(&self, internal: TypeName) -> Option<&'static str> {
        function_type_descriptor(self.classifiers, internal)
    }

    /// Every concrete class of this file that can stand behind `internal`.
    pub(super) fn implementors_of(&self, internal: TypeName) -> Vec<ClassId> {
        (0..self.ir.classes.len() as ClassId)
            .filter(|&id| {
                let class = &self.ir.classes[id as usize];
                !class.is_interface
                    && std::iter::once(class.superclass)
                        .chain(class.interfaces.iter())
                        .chain(
                            class
                                .supertypes
                                .iter()
                                .copied()
                                .filter_map(Ty::obj_internal),
                        )
                        .any(|named| named == internal)
            })
            .collect()
    }

    pub(super) fn implements_dependency(&self, internal: TypeName) -> bool {
        self.implemented_dependencies.contains(&internal)
    }

    /// Record the program classes the runtime can walk and the collection shapes it cannot.
    pub(super) fn resolve_walkable_classes(&mut self) {
        for id in 0..self.ir.classes.len() as ClassId {
            let slots = self.walk_slots(id);
            let shape = if slots.length != 0 {
                Some(CollectionShape::Text)
            } else if slots.iterator != 0 {
                Some(CollectionShape::Iterable)
            } else if slots.has_next != 0 && slots.next != 0 {
                Some(CollectionShape::Iterator)
            } else {
                None
            };
            if let Some(shape) = shape {
                let name = self.ir.classes[id as usize].fq_name;
                self.walkable_classes.insert(name, shape);
            }
        }
        let mut record = |owner: &TypeName, overridden| {
            if self.walkable_classes.contains_key(owner) {
                return;
            }
            self.unwalkable_collections
                .extend(collection_shape(self.classifiers, overridden));
        };
        for (owner, edges) in &self.ir.function_overrides {
            for edge in edges {
                if matches!(
                    edge.overridden,
                    crate::fir::ResolvedFunctionOverrideTarget::External(_)
                ) {
                    record(owner, edge.overridden_owner);
                }
            }
        }
        for (owner, edges) in &self.ir.property_overrides {
            for edge in edges {
                if matches!(
                    edge.overridden,
                    crate::fir::ResolvedPropertyOverrideTarget::External(_)
                ) {
                    record(owner, edge.overridden_owner);
                }
            }
        }
    }

    pub(super) fn walkable_shape(&self, ty: Ty) -> Option<CollectionShape> {
        let internal = ty.non_null().obj_internal()?;
        self.walkable_classes.get(&internal).copied()
    }

    pub(super) fn implements_unwalkable_collection_of(&self, ty: Ty) -> bool {
        self.collection_shape_of(ty)
            .is_some_and(|shape| self.unwalkable_collections.contains(&shape))
    }

    pub(super) fn implements_collection_shape(&self, shape: CollectionShape) -> bool {
        self.implemented_collections.contains(&shape)
    }

    pub(super) fn implements_collection_of(&self, ty: Ty) -> bool {
        self.collection_shape_of(ty)
            .is_some_and(|shape| self.implemented_collections.contains(&shape))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;
    use std::sync::Arc;

    #[derive(Default)]
    struct Facts(crate::fx_hash::HashMap<TypeName, Arc<crate::backend::BackendClassifierFact>>);

    impl BackendClassifierSource for Facts {
        fn classifier(
            &self,
            classifier: TypeName,
        ) -> Option<Arc<crate::backend::BackendClassifierFact>> {
            self.0.get(&classifier).cloned()
        }
    }

    fn interface_fact(
        supertypes: &[TypeName],
        role: Option<ClassifierRole>,
    ) -> Arc<crate::backend::BackendClassifierFact> {
        Arc::new(crate::backend::BackendClassifierFact {
            runtime_exception: None,
            declaration_name: None,
            access: crate::libraries::ClassifierAccess::Public,
            is_kotlin: true,
            mapped_builtin: false,
            source: false,
            outer_instance: None,
            kind: crate::libraries::TypeKind::Interface,
            is_abstract: true,
            is_extensible: true,
            supertypes: supertypes.into(),
            annotations: Box::new([]),
            own_type_parameter_count: 0,
            type_param_variances: Box::new([]),
            value_underlying: None,
            value_declaration: None,
            role,
            companion: None,
            enum_entries_accessor: None,
        })
    }

    #[test]
    fn native_standard_library_implementations_require_exact_classifier_identities() {
        assert!(is_pair(type_name("kotlin/Pair")));
        assert!(!is_pair(type_name("sample/Pair")));
        assert_eq!(
            standard_collection_implementation(type_name("kotlin/collections/ArrayList")),
            Some(StandardCollectionImplementation::List)
        );
        assert_eq!(
            standard_collection_implementation(type_name("sample/ArrayList")),
            None
        );
        assert_eq!(
            integral_range_element(type_name("kotlin/ranges/UIntProgression")),
            Some(IntegralRangeElement::UInt)
        );
        assert_eq!(
            integral_range_element(type_name("sample/UIntProgression")),
            None
        );
    }

    #[test]
    fn native_interface_markers_require_exact_classifier_identities() {
        assert_eq!(
            standard_interface_descriptor(crate::types::wk::function_root()),
            Some("kt_type_function")
        );
        assert_eq!(
            standard_interface_descriptor(type_name("sample/Function")),
            None
        );
        assert_eq!(
            standard_interface_descriptor(type_name("kotlin/reflect/KProperty1")),
            Some("kt_type_kproperty1")
        );
        assert_eq!(
            standard_interface_descriptor(type_name("sample/KProperty1")),
            None
        );
        assert!(is_comparable(type_name("kotlin/Comparable")));
        assert!(!is_comparable(type_name("sample/Comparable")));
    }

    #[test]
    fn native_function_check_markers_follow_runtime_role_limits() {
        assert_eq!(
            function_check_descriptor(TypeCheckRole::FunctionOfArity(0)),
            Some("kt_type_function0")
        );
        assert_eq!(
            function_check_descriptor(TypeCheckRole::FunctionOfArity(22)),
            Some("kt_type_function22")
        );
        assert_eq!(
            function_check_descriptor(TypeCheckRole::FunctionOfArity(23)),
            None
        );
        assert_eq!(
            function_check_descriptor(TypeCheckRole::SuspendFunctionOfArity(0)),
            None
        );
    }

    #[test]
    fn dependency_function_graph_contributes_arity_and_common_markers() {
        let external = type_name("sample/ExternalFn");
        let function1 = type_name("kotlin/Function1");
        let function = crate::types::wk::function_root();
        let mut facts = Facts::default();
        facts.0.insert(external, interface_fact(&[function1], None));
        facts.0.insert(
            function1,
            interface_fact(&[function], Some(ClassifierRole::FunctionOfArity(1))),
        );
        facts.0.insert(function, interface_fact(&[], None));

        assert_eq!(
            runtime_interface_descriptors(&facts, [external]),
            vec!["kt_type_function1", "kt_type_function"]
        );
    }

    #[test]
    fn checked_function_supertypes_contribute_physical_arity_and_common_markers() {
        assert_eq!(
            function_supertype_descriptors([
                TypeCheckRole::FunctionOfArity(1),
                TypeCheckRole::FunctionOfArity(1),
                TypeCheckRole::SuspendFunctionOfArity(1),
            ]),
            vec!["kt_type_function1", "kt_type_function", "kt_type_function2"]
        );
    }
}
