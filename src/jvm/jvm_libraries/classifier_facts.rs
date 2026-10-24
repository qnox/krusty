//! Provider-normalized classifier facts consumed after semantic checking.

use super::*;

/// Give every metadata-declared constructor the classifier parameters it can infer, independently
/// of how the JVM realizes that declaration. Value classes use `constructor-impl` rather than a
/// directly callable `<init>`, while ordinary and marker-backed constructors do have `<init>`
/// methods; that representation distinction must not change their source-level generic signature.
pub(super) fn classifier_constructor_generic_sig(
    parameters: &crate::types::TypeParameters<Vec<Vec<Ty>>>,
    owner: TypeName,
    value_parameters: &[Ty],
) -> Option<GenericSig> {
    if parameters.type_params.is_empty() {
        return None;
    }
    let arguments = parameters
        .type_params
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let bound = parameters
                .type_param_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::ty_param(formal, bound)
        })
        .collect::<Vec<_>>();
    Some(GenericSig {
        formals: parameters.type_params.clone(),
        formal_bounds: parameters.type_param_bounds.clone(),
        receiver: None,
        params: value_parameters.to_vec(),
        ret: Ty::obj_args_name(owner, &arguments),
        return_policy: GenericReturnPolicy::Exact,
    })
}

/// A mapped Kotlin classifier and its JVM realization may use different source names for the same
/// owner type parameters (`Iterator<T>` in builtins versus `java.util.Iterator<E>` in the class
/// file). Java members admitted into the mapped Kotlin scope must use the Kotlin classifier's
/// parameter identities before they leave this provider. Otherwise core can apply `Iterator<String>`
/// only to declarations written in terms of `T`, while a visible Java default such as
/// `forEachRemaining(Consumer<? super E>)` leaks the unrelated physical `E` into checked signatures.
pub(super) fn align_mapped_owner_type_parameters(
    members: &mut [LibraryMember],
    physical: &[String],
    semantic: &[String],
    semantic_bounds: &[Vec<Ty>],
) {
    if physical.len() != semantic.len() || physical == semantic {
        return;
    }
    let owner_bindings = physical
        .iter()
        .zip(semantic)
        .enumerate()
        .map(|(index, (physical, semantic))| {
            let bound = semantic_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            (physical.clone(), Ty::ty_param(semantic, bound))
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();
    for member in members {
        let Some(signature) = &mut member.generic_sig else {
            continue;
        };
        let mut bindings = owner_bindings.clone();
        for formal in &signature.formals {
            // A method formal shadows an identically named owner formal.
            bindings.remove(formal);
        }
        signature.receiver = signature
            .receiver
            .map(|receiver| ty_subst_keep_unbound(receiver, &bindings));
        for parameter in &mut signature.params {
            *parameter = ty_subst_keep_unbound(*parameter, &bindings);
        }
        signature.ret = ty_subst_keep_unbound(signature.ret, &bindings);
        for bounds in &mut signature.formal_bounds {
            for bound in bounds {
                *bound = ty_subst_keep_unbound(*bound, &bindings);
            }
        }
    }
}

pub(super) fn generated_serializer_singleton(
    libraries: &JvmLibraries,
    classifier: TypeName,
) -> Option<TypeName> {
    let owner = libraries.cp.find_name(classifier)?;
    let owner_identity = owner.this_class;
    if owner_identity != classifier {
        return None;
    }
    let generated_serializer = type_name("kotlinx/serialization/internal/GeneratedSerializer");
    let mut candidates = owner
        .inner_classes
        .iter()
        // `InnerClasses` gives both the exact lexical owner and the source segment. Preserve that
        // relation as a `TypeName`: reparsing `inner` would misread a literal `$` in a segment (the
        // serialization plugin's `$serializer` renders with `$$`) as another nesting delimiter.
        .filter_map(|nested| {
            let outer = nested.outer.as_deref()?;
            let source_name = nested.name.as_deref()?;
            if !owner_identity.matches(outer)
                || nested.inner.strip_prefix(outer)?.strip_prefix('$')? != source_name
            {
                return None;
            }
            // Only a self-consistent external tuple may publish a semantic name-tree edge.
            let identity = crate::types::type_name_nested_child(owner_identity, source_name);
            libraries
                .cp
                .find_name(identity)
                .filter(|class| class.this_class == identity)
        })
        .filter(|nested| {
            nested.interfaces.contains_name(generated_serializer)
                && libraries
                    .cp
                    .singleton_storage(nested.this_class)
                    .is_some_and(|(owner, field)| owner == nested.this_class && field == "INSTANCE")
        });
    let singleton = candidates.next()?.this_class;
    candidates.next().is_none().then_some(singleton)
}

/// The serial elements of `classifier`, a `@Serializable` class whose serializer the plugin
/// generated, read from the class's metadata as kotlinc's plugin reads them for a subclass: the
/// plugin's program-order list names the elements, each property's metadata gives its type, and
/// the primary constructor says which parameters have no default and so must be present.
pub(super) fn serializable_class_elements(
    libraries: &JvmLibraries,
    classifier: TypeName,
) -> Option<crate::types::SerializableClassElements> {
    published_chain(libraries, classifier).map(|chain| chain.record)
}

/// A published class's record with every element on its wire, inherited ones first.
struct PublishedChain {
    record: crate::types::SerializableClassElements,
    wire: Vec<(Box<str>, Ty)>,
}

fn published_chain(libraries: &JvmLibraries, classifier: TypeName) -> Option<PublishedChain> {
    let shape = SymbolSource::classifier(libraries, classifier)?;
    if shape.kind != crate::libraries::TypeKind::Class || !generates_serializer(&shape.annotations)
    {
        return None;
    }
    let class = libraries.cp.find_name(classifier)?;
    if class.this_class != classifier {
        return None;
    }
    let any = type_name("kotlin/Any");
    let superclass = shape
        .supertype_templates
        .iter()
        .copied()
        .find(|supertype| {
            supertype
                .kotlin_class_internal()
                .and_then(|name| SymbolSource::classifier(libraries, name))
                .is_some_and(|supertype| supertype.kind == crate::libraries::TypeKind::Class)
        })
        .filter(|ty| ty.kotlin_class_internal() != Some(any));
    // The elements the class inherits are exactly its serializable superclass's wire; a
    // superclass the plugin did not generate a serializer for contributes none.
    let mut wire = match superclass {
        Some(superclass) => {
            let parent = superclass.kotlin_class_internal()?;
            let parent_generates = SymbolSource::classifier(libraries, parent)
                .is_some_and(|parent| generates_serializer(&parent.annotations));
            if !parent_generates {
                Vec::new()
            } else {
                // A generic parent's elements are typed over its own type parameters; the class
                // inherits them typed by the arguments it gives the parent.
                let formals = &SymbolSource::classifier(libraries, parent)?
                    .type_parameters
                    .type_params;
                let arguments = superclass.type_args();
                if formals.len() != arguments.len() {
                    return None;
                }
                let bindings = formals
                    .iter()
                    .cloned()
                    .zip(arguments.iter().copied())
                    .collect::<crate::fx_hash::HashMap<_, _>>();
                published_chain(libraries, parent)?
                    .wire
                    .into_iter()
                    .map(|(name, ty)| (name, ty_subst_keep_unbound(ty, &bindings)))
                    .collect()
            }
        }
        None => Vec::new(),
    };
    let meta = &class.meta;
    let listed = &meta.serial_element_names;
    let inherited = wire.len();
    if listed.len() < inherited
        || !listed
            .iter()
            .zip(&wire)
            .all(|(element, (name, _))| element.name == *name)
    {
        return None;
    }
    // kotlinc reads a library class's own elements as its listed, declared properties, so a
    // class that redeclares an inherited element (an override) names it twice and kotlinc
    // rejects every subclass with a duplicate serial name.
    if meta.class_properties.iter().any(|property| {
        wire.iter()
            .any(|(name, _)| property.name.as_str() == &**name)
    }) {
        return None;
    }
    // A property the class stores itself: its metadata names a backing field the classfile has.
    let stored = |property: &crate::jvm::metadata::MetaProp| {
        property.field.as_ref().is_some_and(|field| {
            class
                .fields
                .iter()
                .any(|candidate| candidate.name == field.name && candidate.descriptor == field.desc)
        })
    };
    // Without the list a class with stored properties gives no element order to follow.
    if listed.len() == inherited && meta.class_properties.iter().any(stored) {
        return None;
    }
    let primary = meta
        .constructors
        .iter()
        .find(|constructor| constructor.primary);
    let elements = listed[inherited..]
        .iter()
        .map(|element| {
            let name = &element.name;
            let property = meta
                .class_properties
                .iter()
                .find(|property| property.name.as_str() == &**name)
                .filter(|property| stored(property))?;
            // Annotations on the element or inside its type (a serial name, a serializer of its
            // own) change its wire shape, and this record carries none.
            if property.annotation_method.is_some() || element.annotated_type {
                return None;
            }
            let ty = shape
                .declared_callables
                .get(&**name)?
                .properties()
                .first()?
                .ty;
            let optional = primary
                .and_then(|constructor| {
                    let index = constructor
                        .params
                        .names
                        .iter()
                        .position(|parameter| parameter.as_str() == &**name)?;
                    Some(
                        constructor
                            .params
                            .defaults
                            .as_slice()
                            .get(index)
                            .copied()
                            .unwrap_or(false),
                    )
                })
                .unwrap_or(true);
            Some(crate::types::SerializableElement {
                name: name.clone(),
                ty,
                optional,
            })
        })
        .collect::<Option<Box<[_]>>>()?;
    wire.extend(
        elements
            .iter()
            .map(|element| (element.name.clone(), element.ty)),
    );
    // The annotation says the class asked for a generated serializer; only the generated members
    // themselves say the plugin ran and what they take.
    let type_parameters = shape.type_parameters.type_params.len();
    if !has_generated_parent_abi(&class.methods, classifier, type_parameters, &wire) {
        return None;
    }
    Some(PublishedChain {
        record: crate::types::SerializableClassElements {
            type_parameters: shape
                .type_parameters
                .type_params
                .iter()
                .map(|parameter| Box::from(parameter.as_str()))
                .collect(),
            elements,
            superclass,
        },
        wire,
    })
}

/// `@Serializable` without its own `with` serializer: the plugin generates the serializer.
fn generates_serializer(annotations: &[crate::types::ResolvedAnnotation]) -> bool {
    let serializable = type_name("kotlinx/serialization/Serializable");
    annotations.iter().any(|annotation| {
        annotation.annotation == serializable
            && !annotation.arguments.iter().any(|(name, _)| name == "with")
    })
}

/// Whether `methods` hold exactly the members the serialization plugin generates on an
/// inheritable class with `type_parameters` type parameters whose wire is `wire`: a public static
/// `write$Self(self, output, descriptor, typeSerial0, …)` and a public deserialization constructor
/// taking the `seen` masks, each element as the subclass's delegation passes it, and the marker.
fn has_generated_parent_abi(
    methods: &[crate::jvm::classreader::MethodSig],
    classifier: TypeName,
    type_parameters: usize,
    wire: &[(Box<str>, Ty)],
) -> bool {
    const PUBLIC: u16 = 0x0001;
    const STATIC: u16 = 0x0008;
    let write_self = format!(
        "(L{};Lkotlinx/serialization/encoding/CompositeEncoder;Lkotlinx/serialization/descriptors/SerialDescriptor;{})V",
        classifier.render(),
        "Lkotlinx/serialization/KSerializer;".repeat(type_parameters)
    );
    let mut constructor = String::from("(");
    constructor.push_str(&"I".repeat(wire.len() / 32 + 1));
    for (_, ty) in wire {
        constructor.push_str(&crate::jvm::names::type_descriptor(
            crate::plugins::serialization::deserialization_parameter_ty(*ty),
        ));
    }
    constructor.push_str("Lkotlinx/serialization/internal/SerializationConstructorMarker;)V");
    let has = |name: &str, access: u16, descriptor: &str| {
        methods.iter().any(|method| {
            method.name == name
                && method.access & access == access
                && method.descriptor == descriptor
        })
    };
    has("write$Self", PUBLIC | STATIC, &write_self) && has("<init>", PUBLIC, &constructor)
}

/// Whether `internal` is, transitively, a subtype of `target` (a superclass or implemented interface,
/// at any depth). Names are normalized to their JVM spelling so a Kotlin built-in (`kotlin/collections/
/// MutableMap`) and its `java/util/Map` realization compare equal.
pub(super) fn class_implements_name(cp: &Classpath, internal: TypeName, target: TypeName) -> bool {
    let target = crate::jvm::jvm_class_map::to_jvm_type_name(target);
    let mut seen = crate::fx_hash::HashSet::default();
    let mut q = std::collections::VecDeque::new();
    q.push_back(crate::jvm::jvm_class_map::to_jvm_type_name(internal));
    while let Some(name) = q.pop_front() {
        if name == target {
            return true;
        }
        if !seen.insert(name) {
            continue;
        }
        if let Some(ci) = cp.find_name(name) {
            q.extend(ci.interfaces.iter_ids().chain(ci.super_class));
        }
    }
    false
}

impl crate::types::ClassifierFactSource for JvmLibraries {
    fn classifier_annotations(
        &self,
        classifier: TypeName,
    ) -> Option<Vec<crate::types::ResolvedAnnotation>> {
        SymbolSource::classifier(self, classifier).map(|shape| shape.annotations.clone())
    }

    fn classifier_is_object(&self, classifier: TypeName) -> Option<bool> {
        SymbolSource::classifier(self, classifier)
            .map(|shape| shape.kind == crate::libraries::TypeKind::Object)
    }

    fn classifier_declaration(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::ClassifierDeclarationFacts> {
        let shape = SymbolSource::classifier(self, classifier)?;
        Some(crate::types::ClassifierDeclarationFacts {
            kind: shape.kind.into(),
            is_abstract: shape.inheritance.is_abstract || !shape.sealed_subclasses.is_empty(),
            own_type_parameter_count: shape.own_type_parameter_count,
            companion: shape
                .companion_object
                .as_ref()
                .map(|(field, companion)| (Box::from(field.as_str()), *companion)),
            qualified_name: shape.qualified_name.clone(),
            source: shape.source_file.is_some(),
        })
    }

    fn generated_serializer_singleton(&self, classifier: TypeName) -> Option<TypeName> {
        SymbolSource::generated_serializer_singleton(self, classifier)
    }

    fn has_serialization_serializer_accessor(
        &self,
        companion: TypeName,
        type_parameters: usize,
    ) -> bool {
        SymbolSource::has_serialization_serializer_accessor(self, companion, type_parameters)
    }

    fn serializable_class_elements(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::SerializableClassElements> {
        SymbolSource::serializable_class_elements(self, classifier)
    }

    fn serialization_companion(&self, classifier: TypeName) -> Option<(Box<str>, TypeName)> {
        let shape = SymbolSource::classifier(self, classifier)?;
        shape
            .companion_object
            .as_ref()
            .map(|(field, companion)| (Box::from(field.as_str()), *companion))
    }

    fn classifier_value_underlying(&self, classifier: TypeName) -> Option<Ty> {
        SymbolSource::classifier(self, classifier).and_then(|shape| shape.value_underlying)
    }

    fn classifier_value_declaration(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::DeclaredValueClass> {
        SymbolSource::classifier(self, classifier).and_then(|shape| shape.value_declaration.clone())
    }

    fn classifier_value_property(&self, classifier: TypeName) -> Option<String> {
        SymbolSource::classifier(self, classifier)
            .and_then(|shape| Some(shape.value_declaration.as_ref()?.property.to_string()))
    }

    fn classifier_role(&self, classifier: TypeName) -> Option<crate::types::ClassifierRole> {
        SymbolSource::classifier(self, classifier)?.classifier_role()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(access: u16, name: &str, descriptor: &str) -> crate::jvm::classreader::MethodSig {
        crate::jvm::classreader::MethodSig {
            access,
            name: name.to_string(),
            descriptor: descriptor.to_string(),
            signature: None,
            parameter_nullability: Vec::new(),
            return_nullability: None,
            deprecated_hidden: false,
            deprecated_hidden_since: None,
            deprecation: None,
            has_annotation_default: false,
            annotations: Box::default(),
        }
    }

    /// A parent's deserialization constructor counts only when every parameter is what the
    /// subclass's delegation passes: the right count of masks, elements and marker is not enough
    /// when an element's descriptor differs.
    #[test]
    fn generated_parent_abi_requires_each_element_descriptor() {
        let classifier = type_name("lib/Base");
        let write_self = method(
            0x0001 | 0x0008,
            "write$Self",
            "(Llib/Base;Lkotlinx/serialization/encoding/CompositeEncoder;Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
        );
        let wire = [
            (Box::<str>::from("zeta"), Ty::Int),
            (Box::<str>::from("tags"), Ty::obj("kotlin/collections/List")),
        ];
        let constructor = |elements: &str| {
            method(
                0x0001,
                "<init>",
                &format!(
                    "(I{elements}Lkotlinx/serialization/internal/SerializationConstructorMarker;)V"
                ),
            )
        };
        assert!(has_generated_parent_abi(
            &[write_self.clone(), constructor("ILjava/util/List;")],
            classifier,
            0,
            &wire,
        ));
        assert!(!has_generated_parent_abi(
            &[write_self.clone(), constructor("ILjava/lang/String;")],
            classifier,
            0,
            &wire,
        ));
        assert!(!has_generated_parent_abi(
            &[constructor("ILjava/util/List;")],
            classifier,
            0,
            &wire,
        ));
        // A generic class's `write$Self` takes one serializer per type parameter.
        assert!(!has_generated_parent_abi(
            &[write_self, constructor("ILjava/util/List;")],
            classifier,
            1,
            &wire,
        ));
    }

    #[derive(Clone, Copy)]
    enum InnerClassFixture {
        Exact,
        Mismatched,
        Ambiguous,
    }

    struct SerializerFixture {
        directory: std::path::PathBuf,
        owner: String,
        serializer: String,
        second_serializer: Option<String>,
    }

    fn write_serializer(package: &std::path::Path, internal: &str, file: &str) {
        let mut serializer = crate::jvm::classfile::ClassWriter::new(internal, "java/lang/Object");
        serializer.add_interface("kotlinx/serialization/internal/GeneratedSerializer");
        serializer.add_field(
            0x0001 | 0x0008 | 0x0010,
            "INSTANCE",
            &format!("L{internal};"),
        );
        std::fs::write(package.join(file), serializer.finish())
            .expect("write generated serializer");
    }

    fn serializer_fixture(tag: &str, kind: InnerClassFixture) -> SerializerFixture {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "krusty-serializer-identity-{tag}-{}-{unique}",
            std::process::id()
        ));
        let package = directory.join("cold");
        std::fs::create_dir_all(&package).expect("create classpath package");

        let owner_name = format!("cold/Payload{unique}");
        let serializer_name = format!("{owner_name}$$serializer");
        let second_serializer =
            matches!(kind, InnerClassFixture::Ambiguous).then(|| format!("{owner_name}$Second"));
        let mut owner = crate::jvm::classfile::ClassWriter::new(&owner_name, "java/lang/Object");
        owner.add_inner_class(crate::jvm::classfile::InnerClassSpec {
            inner: if matches!(kind, InnerClassFixture::Mismatched) {
                format!("cold/Unrelated{unique}")
            } else {
                serializer_name.clone()
            },
            outer: Some(owner_name.clone()),
            name: Some("$serializer".to_string()),
            access: 0x0001 | 0x0008 | 0x0010,
        });
        if let Some(second) = &second_serializer {
            owner.add_inner_class(crate::jvm::classfile::InnerClassSpec {
                inner: second.clone(),
                outer: Some(owner_name.clone()),
                name: Some("Second".to_string()),
                access: 0x0001 | 0x0008 | 0x0010,
            });
        }
        std::fs::write(
            package.join(format!("Payload{unique}.class")),
            owner.finish(),
        )
        .expect("write serialized classifier");

        write_serializer(
            &package,
            &serializer_name,
            &format!("Payload{unique}$$serializer.class"),
        );
        if let Some(second) = &second_serializer {
            write_serializer(&package, second, &format!("Payload{unique}$Second.class"));
        }

        SerializerFixture {
            directory,
            owner: owner_name,
            serializer: serializer_name,
            second_serializer,
        }
    }

    fn classpath(fixture: &SerializerFixture) -> std::rc::Rc<crate::jvm::classpath::Classpath> {
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![fixture
            .directory
            .clone()]))
    }

    #[test]
    fn generated_serializer_identity_comes_from_the_cold_inner_class_relation() {
        let fixture = serializer_fixture("cold", InnerClassFixture::Exact);
        let libraries = JvmLibraries::new(classpath(&fixture)).expect("initialize JVM libraries");
        let generated = generated_serializer_singleton(&libraries, type_name(&fixture.owner))
            .expect("generated serializer on first provider lookup");
        assert!(generated.matches(&fixture.serializer));

        drop(libraries);
        std::fs::remove_dir_all(fixture.directory).expect("remove classpath directory");
    }

    #[test]
    fn generated_serializer_identity_is_the_same_when_the_nested_class_cache_is_warm_first() {
        let fixture = serializer_fixture("warm", InnerClassFixture::Exact);
        let owner = type_name(&fixture.owner);
        let expected = crate::types::type_name_nested_child(owner, "$serializer");
        let classpath = classpath(&fixture);
        assert_eq!(
            classpath
                .find_name(expected)
                .map(|classifier| classifier.this_class),
            Some(expected)
        );
        let libraries = JvmLibraries::new(classpath).expect("initialize JVM libraries");

        assert_eq!(
            generated_serializer_singleton(&libraries, owner),
            Some(expected)
        );

        drop(libraries);
        std::fs::remove_dir_all(fixture.directory).expect("remove classpath directory");
    }

    #[test]
    fn a_mismatched_inner_class_tuple_does_not_name_a_serializer() {
        let fixture = serializer_fixture("mismatch", InnerClassFixture::Mismatched);
        let libraries = JvmLibraries::new(classpath(&fixture)).expect("initialize JVM libraries");
        let owner = type_name(&fixture.owner);
        // Writing the fixture's `Payload$$serializer` class file names the `$serializer` child;
        // only the `InnerClasses` tuple, which points elsewhere, can make it the serializer.
        let named = owner.existing_nested_child("$serializer");

        assert_eq!(generated_serializer_singleton(&libraries, owner), None);
        assert_eq!(owner.existing_nested_child("$serializer"), named);

        drop(libraries);
        std::fs::remove_dir_all(fixture.directory).expect("remove classpath directory");
    }

    #[test]
    fn multiple_valid_generated_serializer_singletons_are_ambiguous() {
        let fixture = serializer_fixture("ambiguous", InnerClassFixture::Ambiguous);
        assert!(fixture.second_serializer.is_some());
        let libraries = JvmLibraries::new(classpath(&fixture)).expect("initialize JVM libraries");

        assert_eq!(
            generated_serializer_singleton(&libraries, type_name(&fixture.owner)),
            None
        );

        drop(libraries);
        std::fs::remove_dir_all(fixture.directory).expect("remove classpath directory");
    }

    /// The provider publishes a classifier's type-check role on its record: the collection face
    /// from its builtin class map and a function classifier's arity from its declared signature.
    #[test]
    fn classifier_roles_come_from_the_provider_records() {
        let stdlib = crate::toolchain::stdlib_jar().expect("Kotlin stdlib is provisioned");
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![stdlib],
        )))
        .expect("initialize JVM libraries");
        let collection = |kind, mutable| {
            Some(crate::types::ClassifierRole::MappedCollection(
                crate::types::MappedCollection { kind, mutable },
            ))
        };
        let roles = [
            "kotlin/Function2",
            "kotlin/coroutines/SuspendFunction1",
            "kotlin/reflect/KFunction1",
            "kotlin/collections/MutableList",
            "kotlin/collections/List",
            "kotlin/collections/MutableMap.MutableEntry",
            "java/util/List",
        ]
        .map(|name| {
            crate::types::ClassifierFactSource::classifier_role(&libraries, type_name(name))
        });
        assert_eq!(
            roles,
            [
                Some(crate::types::ClassifierRole::FunctionOfArity(2)),
                Some(crate::types::ClassifierRole::SuspendFunctionOfArity(1)),
                None,
                collection(crate::types::CollectionKind::List, true),
                collection(crate::types::CollectionKind::List, false),
                collection(crate::types::CollectionKind::MapEntry, true),
                None,
            ]
        );
    }
}
