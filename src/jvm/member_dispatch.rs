//! The class a virtual call names.
//!
//! kotlinc calls an inherited member through the fake override FIR2IR selects in the dispatch
//! receiver's own class (`Leaf.m`, `Leaf.hashCode`, `IntRange.getFirst`). The receiver's class and
//! its supertypes are read through the one classifier model both this module's and the
//! dependencies' classes normalize into, so a module class and a library class take one path.

pub(super) use super::checked_classifiers::{
    CheckedClassifiers as CheckedDispatchClassifiers, MissingClassifier,
};
use crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal;
use crate::types::TypeName;

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
/// An array has no `kotlin/Array` or `kotlin/CharArray` class file. A member it inherits from
/// `Any` is called, as kotlinc calls it, through the array classifier's fake override, whose class
/// is the erased JVM array type: `[C` for `CharArray`, and `[Ljava/lang/Object;` for every
/// `Array<T>`. The array's own `clone()` is realized where its provider declares it
/// (`Object.clone`, kotlinc's `Clone` intrinsic). Naming the Kotlin classifier makes the runtime
/// look up a class that does not exist.
pub(super) fn virtual_owner(
    classifiers: &CheckedDispatchClassifiers<'_>,
    declared: TypeName,
    declared_interface: bool,
    dispatch: Option<TypeName>,
    receiver: Option<crate::types::Ty>,
    declaration_owner: Option<crate::types::SemanticCallableOwner>,
) -> Result<(TypeName, bool), MissingClassifier> {
    let array_class = receiver
        .and_then(|ty| ty.non_null().obj_internal())
        .filter(|&array| {
            crate::types::Ty::obj_name(array).is_array()
                && !is_value_class(classifiers, array)
                && declaration_owner != Some(crate::types::SemanticCallableOwner::Classifier(array))
        })
        .and_then(crate::jvm::names::array_class_descriptor);
    if let Some(internal) = array_class {
        return Ok((crate::types::type_name(&internal), false));
    }
    call_owner(classifiers, declared, declared_interface, dispatch)
}

/// Whether `classifier` is declared a value class. An unsigned array is one over a primitive
/// array: it has its own class file, and its members are that class's `-impl` statics rather than
/// operations of the JVM array type that carries it.
pub(super) fn is_value_class(
    classifiers: &CheckedDispatchClassifiers<'_>,
    classifier: TypeName,
) -> bool {
    classifiers
        .classifier(classifier)
        .is_some_and(|class| class.value_class)
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
    let mut seen = crate::fx_hash::HashSet::default();
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
    use crate::backend::{BackendClassifierFact, BackendClassifierSource};
    use crate::fx_hash::HashMap;
    use crate::libraries::ClassifierAccess;
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
            runtime_exception: None,
            declaration_name: None,
            access: ClassifierAccess::Public,
            is_kotlin: true,
            source: true,
            outer_instance: None,
            kind: crate::libraries::TypeKind::Class,
            is_abstract: false,
            is_extensible: true,
            supertypes: supertypes.into(),
            annotations: Box::new([]),
            own_type_parameter_count: 0,
            type_param_variances: Box::new([]),
            value_underlying: None,
            value_declaration: None,
            role: None,
            companion: None,
            enum_entries_accessor: None,
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
