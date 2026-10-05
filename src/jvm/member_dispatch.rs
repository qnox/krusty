//! The class a virtual call names.
//!
//! kotlinc calls an inherited member through the fake override FIR2IR selects in the dispatch
//! receiver's own class (`Leaf.m`, `Leaf.hashCode`, `IntRange.getFirst`). The receiver's class and
//! its supertypes are read through the one classifier model both this module's and the
//! dependencies' classes normalize into, so a module class and a library class take one path.

use crate::backend::BackendClassifierSource;
use crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal;
use crate::libraries::ClassifierAccess;
use crate::types::TypeName;

/// The semantic classifier facts needed to choose one virtual invocation owner.
///
/// The module snapshot deliberately excludes classifiers declared in executable bodies: their
/// completed shape belongs to the active common-IR file. Backend-generated classes likewise exist
/// only in that file. This view freezes those exact IR declarations beside the already-frozen
/// module/dependency facts, so owner selection has one identity-keyed source without reopening a
/// resolver or interpreting an emitted name.
pub(super) struct CheckedDispatchClassifiers<'a> {
    stable: &'a dyn BackendClassifierSource,
    file: std::collections::HashMap<TypeName, DispatchClassifierFact>,
}

#[derive(Clone)]
struct DispatchClassifierFact {
    /// Whether a call emitted in this file may name the classifier as its invocation owner.
    /// File-local/generated classes have no semantic visibility publication; absence there means
    /// nameable inside this emitting package, not an invented public declaration.
    nameable: bool,
    interface: bool,
    value_class: bool,
    supertypes: Box<[TypeName]>,
}

impl<'a> CheckedDispatchClassifiers<'a> {
    pub(super) fn new(ir: &crate::ir::IrFile, stable: &'a dyn BackendClassifierSource) -> Self {
        let mut file = std::collections::HashMap::new();
        for class in ir.classes.iter().filter(|class| {
            !class.is_source_declared || crate::jvm::local_classifiers::is_local(ir, class)
        }) {
            let identity = class.fq_name_id();
            let mut supertypes = Vec::new();
            let mut publish = |supertype: TypeName| {
                if supertype != identity && !supertypes.contains(&supertype) {
                    supertypes.push(supertype);
                }
            };
            publish(class.superclass);
            for supertype in class
                .supertypes
                .iter()
                .filter_map(|ty| ty.non_null().obj_internal())
            {
                publish(supertype);
            }
            for interface in class.interfaces.iter_ids() {
                publish(interface);
            }
            let fact = DispatchClassifierFact {
                nameable: !matches!(
                    ir.class_visibilities.get(&identity),
                    Some(crate::types::Visibility::PackagePrivate)
                ),
                interface: class.is_interface || class.is_annotation,
                value_class: class.is_value,
                supertypes: supertypes.into_boxed_slice(),
            };
            assert!(
                file.insert(identity, fact).is_none(),
                "one common-IR file may publish one classifier per semantic identity"
            );
        }
        Self { stable, file }
    }

    fn classifier(&self, classifier: TypeName) -> Option<DispatchClassifierFact> {
        if let Some(fact) = self.file.get(&classifier) {
            return Some(fact.clone());
        }
        self.stable
            .classifier(classifier)
            .map(|fact| DispatchClassifierFact {
                nameable: fact.access != ClassifierAccess::PackagePrivate,
                interface: fact.is_interface(),
                value_class: fact.value_underlying.is_some(),
                supertypes: fact.supertypes.clone(),
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MissingClassifier(pub TypeName);

impl std::fmt::Display for MissingClassifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "missing backend classifier fact for {}", self.0)
    }
}

/// The class and interface-ness a virtual call to a member declared in `declared` names when its
/// dispatch receiver statically has `receiver`. The receiver's class is named when it inherits the
/// member and the call site can name it; the declaration's class otherwise: for a receiver that
/// declares the member itself or is a supertype of the class that does (an enum entry's own member
/// reached through the enum's type), a value class, a package-private base, and an interface
/// receiver reaching a class member (`toString` on an interface stays `Object.toString`).
pub(super) fn call_owner(
    classifiers: &CheckedDispatchClassifiers<'_>,
    declared: TypeName,
    declared_interface: bool,
    receiver: Option<TypeName>,
) -> Result<(TypeName, bool), MissingClassifier> {
    Ok(
        receiver_owner(classifiers, declared, declared_interface, receiver)?
            .unwrap_or((declared, declared_interface)),
    )
}

/// The class a virtual call names when the receiver's full static type is known.
///
/// An array has no `kotlin/Array` or `kotlin/CharArray` class file. Its virtual calls name the JVM
/// array type (`[Ljava/lang/String;`, `[C`), which is where `clone` is public. Naming the Kotlin
/// classifier makes the runtime look up a class that does not exist.
pub(super) fn virtual_owner(
    classifiers: &CheckedDispatchClassifiers<'_>,
    declared: TypeName,
    declared_interface: bool,
    dispatch: Option<TypeName>,
    receiver: Option<crate::types::Ty>,
) -> Result<(TypeName, bool), MissingClassifier> {
    if let Some(array) = receiver.filter(|ty| ty.non_null().is_array()) {
        let internal = crate::jvm::names::type_descriptor(array.non_null());
        return Ok((crate::types::type_name(&internal), false));
    }
    call_owner(classifiers, declared, declared_interface, dispatch)
}

fn receiver_owner(
    classifiers: &CheckedDispatchClassifiers<'_>,
    declared: TypeName,
    declared_interface: bool,
    receiver: Option<TypeName>,
) -> Result<Option<(TypeName, bool)>, MissingClassifier> {
    let Some(receiver) = receiver else {
        return Ok(None);
    };
    if type_names_map_to_same_jvm_internal(receiver, declared) {
        return Ok(None);
    }
    // `kotlin/Array` and `kotlin/IntArray` are classifiers, not class files. A caller that only
    // has that name keeps the declaration; [`virtual_owner`] names the JVM array type when the
    // element is known.
    if crate::types::Ty::obj_name(receiver).is_array() {
        return Ok(None);
    }
    let class = classifiers
        .classifier(receiver)
        .ok_or(MissingClassifier(receiver))?;
    let interface = class.interface;
    if !class.nameable || class.value_class || (!declared_interface && interface) {
        return Ok(None);
    }
    Ok(inherits(classifiers, receiver, declared)?.then_some((receiver, interface)))
}

/// Whether `class` has `ancestor` among its transitive supertypes, a mapped Kotlin builtin
/// standing for the JVM class it maps to (`kotlin/Any` for `java/lang/Object`).
pub(super) fn inherits(
    classifiers: &CheckedDispatchClassifiers<'_>,
    class: TypeName,
    ancestor: TypeName,
) -> Result<bool, MissingClassifier> {
    let mut pending = vec![class];
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        // A mapped Kotlin builtin's JVM class (`java/util/Collection`) can be absent from a
        // JDK-free classpath: its checked facts live under the Kotlin metadata name, and the
        // twins' supertypes correspond modulo the same mapping, so they stand in for the walk.
        let fact = match classifiers.classifier(current) {
            Some(fact) => fact,
            None => {
                if let Some(twin) =
                    crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(current)
                {
                    if seen.contains(&twin) {
                        continue;
                    }
                    match classifiers.classifier(twin) {
                        Some(fact) => {
                            seen.insert(twin);
                            fact
                        }
                        None => return Err(MissingClassifier(current)),
                    }
                } else {
                    return Err(MissingClassifier(current));
                }
            }
        };
        if fact
            .supertypes
            .iter()
            .any(|&supertype| type_names_map_to_same_jvm_internal(supertype, ancestor))
        {
            return Ok(true);
        }
        pending.extend(fact.supertypes.iter().copied());
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendClassifierFact;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Default)]
    struct Facts(HashMap<TypeName, Arc<BackendClassifierFact>>);

    impl BackendClassifierSource for Facts {
        fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
            self.0.get(&classifier).cloned()
        }
    }

    fn class(supertypes: &[TypeName]) -> Arc<BackendClassifierFact> {
        Arc::new(BackendClassifierFact {
            access: ClassifierAccess::Public,
            is_kotlin: true,
            source: true,
            outer_instance: None,
            kind: crate::libraries::TypeKind::Class,
            is_abstract: false,
            is_extensible: true,
            supertypes: supertypes.into(),
            surface: Box::new([]),
            annotations: Box::new([]),
            own_type_parameter_count: 0,
            type_param_variances: Box::new([]),
            value_underlying: None,
            value_declaration: None,
            role: None,
            companion: None,
        })
    }

    #[test]
    fn missing_receiver_and_intermediate_facts_fail_closed() {
        let declared = crate::types::type_name("review/OwnedBase");
        let receiver = crate::types::type_name("review/OwnedLeaf");
        let ir = crate::ir::IrFile::default();
        let empty = Facts::default();
        let classifiers = CheckedDispatchClassifiers::new(&ir, &empty);
        assert_eq!(
            call_owner(&classifiers, declared, false, Some(receiver)),
            Err(MissingClassifier(receiver))
        );

        let intermediate = crate::types::type_name("review/OwnedMiddle");
        let mut facts = Facts::default();
        facts.0.insert(receiver, class(&[intermediate]));
        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);
        assert_eq!(
            call_owner(&classifiers, declared, false, Some(receiver)),
            Err(MissingClassifier(intermediate))
        );
    }

    #[test]
    fn inherits_uses_kotlin_metadata_twin_when_jvm_builtin_facts_are_absent() {
        let receiver = crate::types::type_name("review/UserCollection");
        let jvm_collection = crate::types::type_name("java/util/Collection");
        let kotlin_collection = crate::types::type_name("kotlin/collections/Collection");
        let kotlin_iterable = crate::types::type_name("kotlin/collections/Iterable");
        let kotlin_any = crate::types::type_name("kotlin/Any");

        let mut facts = Facts::default();
        facts.0.insert(receiver, class(&[jvm_collection]));
        facts.0.insert(kotlin_collection, class(&[kotlin_iterable]));
        facts.0.insert(kotlin_iterable, class(&[]));
        let ir = crate::ir::IrFile::default();
        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);

        assert_eq!(inherits(&classifiers, receiver, jvm_collection), Ok(true));
        assert_eq!(inherits(&classifiers, receiver, kotlin_any), Ok(false));
    }

    #[test]
    fn inherits_still_fails_closed_without_jvm_class_or_twin_facts() {
        let receiver = crate::types::type_name("review/UserCollection");
        let jvm_collection = crate::types::type_name("java/util/Collection");

        let mut facts = Facts::default();
        facts.0.insert(receiver, class(&[jvm_collection]));
        let ir = crate::ir::IrFile::default();
        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);

        assert_eq!(
            inherits(
                &classifiers,
                receiver,
                crate::types::type_name("java/util/List")
            ),
            Err(MissingClassifier(jvm_collection))
        );
    }

    #[test]
    fn intentional_keep_declaration_cases_need_no_classifier_fact() {
        let declared = crate::types::type_name("review/OwnedBase");
        let ir = crate::ir::IrFile::default();
        let facts = Facts::default();
        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);
        assert_eq!(
            call_owner(&classifiers, declared, false, None),
            Ok((declared, false))
        );
        assert_eq!(
            call_owner(&classifiers, declared, false, Some(declared)),
            Ok((declared, false))
        );
        assert_eq!(
            call_owner(
                &classifiers,
                declared,
                false,
                Some(crate::types::type_name("kotlin/Array")),
            ),
            Ok((declared, false))
        );
    }

    #[test]
    fn file_local_anonymous_and_generated_classifiers_form_one_checked_hierarchy() {
        let declared = crate::types::type_name("review/RootInterface");
        let inherited = crate::types::type_name("review/InheritedInterface");
        let local_interface = crate::types::type_name("review/box$LocalInterface");
        let anonymous = crate::types::type_name("review/box$1");
        let generated = crate::types::type_name("review/box$generated");

        let mut facts = Facts::default();
        let mut root = (*class(&[])).clone();
        root.kind = crate::libraries::TypeKind::Interface;
        facts.0.insert(declared, Arc::new(root));
        let mut inherited_fact = (*class(&[declared])).clone();
        inherited_fact.kind = crate::libraries::TypeKind::Interface;
        facts.0.insert(inherited, Arc::new(inherited_fact));

        let mut ir = crate::ir::IrFile::default();
        let mut interface = crate::ir::IrClass::synthetic(local_interface);
        interface.is_source_declared = true;
        interface.is_local_class = true;
        interface.is_interface = true;
        interface.interfaces = vec![inherited].into();
        ir.add_class(interface);

        let mut object = crate::ir::IrClass::synthetic(anonymous);
        object.is_source_declared = true;
        object.is_anonymous_object = true;
        object.interfaces = vec![local_interface].into();
        ir.add_class(object);

        let mut adapter = crate::ir::IrClass::synthetic(generated);
        adapter.superclass = anonymous;
        ir.add_class(adapter);

        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);
        assert_eq!(
            call_owner(&classifiers, declared, true, Some(anonymous)),
            Ok((anonymous, false))
        );
        assert_eq!(
            call_owner(&classifiers, declared, true, Some(generated)),
            Ok((generated, false))
        );
    }
}
