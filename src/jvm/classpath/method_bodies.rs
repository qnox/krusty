//! The classpath as the inliner's source of compiled bodies and class files.

use super::*;

fn declared_regeneration_role(
    unnamed_self: bool,
    sam_wrapper: bool,
) -> Option<crate::jvm::inline::RegeneratedClass> {
    use crate::jvm::inline::RegeneratedClass;
    if sam_wrapper {
        Some(RegeneratedClass::SamWrapper)
    } else if unnamed_self {
        Some(RegeneratedClass::AnonymousObject)
    } else {
        None
    }
}

/// Whether `class` declares the classfile shape of a shared SAM-conversion wrapper.
///
/// Callable/property-reference carriers also have an unnamed `InnerClasses` self row and
/// `ACC_SYNTHETIC`, so those two flags alone do not establish the role. A wrapper is the synthetic
/// `Object` subclass that captures exactly one function value through its one-argument constructor
/// and implements a declaration that is semantically eligible for SAM conversion.
fn is_sam_wrapper_declaration(classpath: &Classpath, class: &ClassInfo) -> bool {
    use crate::jvm::classreader::{ACC_FINAL, ACC_PRIVATE, ACC_STATIC, ACC_SYNTHETIC};
    const ACC_ANNOTATION: u16 = 0x2000;
    const CAPTURE_ACCESS: u16 = ACC_PRIVATE | ACC_FINAL | ACC_SYNTHETIC;

    if class.access & ACC_SYNTHETIC == 0
        || !class
            .super_class
            .is_some_and(|superclass| superclass.matches("java/lang/Object"))
    {
        return false;
    }
    let [capture] = class.fields.as_slice() else {
        return false;
    };
    if capture.access & CAPTURE_ACCESS != CAPTURE_ACCESS || capture.access & ACC_STATIC != 0 {
        return false;
    }
    let capture_type = crate::jvm::physical_type::class_descriptor_ty(&capture.descriptor);
    if classpath.function_classifier_arity(capture_type).is_none() {
        return false;
    }
    let mut constructors = class
        .methods
        .iter()
        .filter(|method| method.name == "<init>");
    let Some(constructor) = constructors.next() else {
        return false;
    };
    if constructors.next().is_some() {
        return false;
    }
    let Some((parameters, result)) =
        crate::jvm::names::parse_method_descriptor(&constructor.descriptor)
    else {
        return false;
    };
    if result != "V" || parameters.as_slice() != [capture.descriptor.as_str()] {
        return false;
    }
    class.interfaces.iter_ids().any(|interface| {
        classpath.find_name(interface).is_some_and(|target| {
            target.is_interface()
                && target.access & ACC_ANNOTATION == 0
                && (!target.meta.is_present() || target.meta.is_fun_interface)
        })
    })
}

/// The classpath is the JVM realization of the inliner's narrow [`MethodBodies`] capability — the
/// emitter sees only this, not the whole `Classpath`.
impl crate::jvm::inline::MethodBodies for Classpath {
    fn body(&self, owner: &str, name: &str, descriptor: &str) -> Option<MethodCode> {
        self.method_code(owner, name, descriptor)
    }
    fn class_file(&self, internal: &str) -> Option<Vec<u8>> {
        self.class_bytes(internal)
    }
    fn singleton_storage(&self, classifier: TypeName) -> Option<(TypeName, String)> {
        Classpath::singleton_storage(self, classifier)
    }
    fn enum_declares_entries(&self, internal: &str) -> Option<bool> {
        let class = self.find(internal)?;
        Some(class.meta.has_enum_entries)
    }
    fn regenerated_class(&self, internal: &str) -> Option<crate::jvm::inline::RegeneratedClass> {
        let class = self.find(internal)?;
        let unnamed_self = class
            .inner_class_self()
            .is_some_and(|entry| entry.name.is_none());
        declared_regeneration_role(unnamed_self, is_sam_wrapper_declaration(self, &class))
    }
    fn owner_is_interface_name(&self, owner: TypeName) -> bool {
        let owner_id = crate::jvm::jvm_class_map::to_jvm_type_name(owner);
        self.find_name(owner_id)
            .map(|class| class.is_interface())
            .or_else(|| {
                let kotlin =
                    crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(owner_id)
                        .unwrap_or(owner_id);
                self.builtin_is_interface_name(kotlin)
            })
            .unwrap_or(false)
    }
    fn method_is_static(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.find(owner).is_some_and(|ci| {
            ci.methods
                .iter()
                .any(|m| m.name == name && m.descriptor == descriptor && m.is_static())
        })
    }
    fn member_is_private(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.find(owner).is_some_and(|ci| {
            ci.methods
                .iter()
                .any(|m| m.name == name && m.descriptor == descriptor && m.is_private())
                || ci
                    .fields
                    .iter()
                    .any(|f| f.name == name && f.descriptor == descriptor && f.is_private())
        })
    }
    fn member_is_publicly_reachable(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.find(owner).is_some_and(|ci| {
            // The OWNER must be public too. A public member of a package-private class is
            // reachable only from that package, and a relocated bootstrap entry has no package.
            ci.access & crate::jvm::classreader::ACC_PUBLIC != 0
                && (ci
                    .methods
                    .iter()
                    .any(|m| m.name == name && m.descriptor == descriptor && m.is_public())
                    || ci
                        .fields
                        .iter()
                        .any(|f| f.name == name && f.descriptor == descriptor && f.is_public()))
        })
    }
    fn class_is_publicly_reachable(&self, class: &str) -> bool {
        self.find(class)
            .is_some_and(|ci| ci.access & crate::jvm::classreader::ACC_PUBLIC != 0)
    }
    fn property_read_access(
        &self,
        owner: TypeName,
        property: &str,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        // The class file first — it is authoritative whenever the owner has one. A mapped builtin
        // whose JVM owner is absent (no JDK on the classpath) still has a `.kotlin_builtins`
        // declaration carrying the same accessor name, erased descriptor and interface flag; without
        // this fallback the caller invents a JavaBean getter (`getSize`) off the LOGICAL type.
        inherited_property_access(self, owner, property, class_property_read_access)
            .or_else(|| self.builtin_property_read_access(owner, property))
            .or_else(|| companion_owner_field_access(self, owner, property, false))
    }
    /// No builtins fallback, deliberately: `.kotlin_builtins` declares no `var` on a mapped type, so
    /// there is no setter for one to answer with (`MutableMap.MutableEntry` exposes `setValue` as a
    /// FUNCTION, which resolves as an ordinary member call, not a property write).
    fn property_write_access(
        &self,
        owner: TypeName,
        property: &str,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        inherited_property_access(self, owner, property, class_property_write_access)
            .or_else(|| companion_owner_field_access(self, owner, property, true))
    }
    fn external_property_access(
        &self,
        accessor: crate::fir::ExternalCallableId,
    ) -> Option<crate::jvm::inline::PropertyAccess> {
        let realization = self.external_callable(accessor)?;
        let is_static = match realization.kind {
            ExternalCallableKind::InstanceFieldRead | ExternalCallableKind::InstanceFieldWrite => {
                false
            }
            ExternalCallableKind::StaticFieldRead | ExternalCallableKind::StaticFieldWrite => true,
            ExternalCallableKind::TopLevel
            | ExternalCallableKind::Extension
            | ExternalCallableKind::Member
            | ExternalCallableKind::Constructor => return None,
        };
        let callable = realization.callable;
        Some(crate::jvm::inline::PropertyAccess::Field {
            owner: callable.owner,
            name: callable.physical_name().to_string(),
            descriptor: callable.descriptor,
            is_static,
        })
    }
}

impl Classpath {
    /// The unique metadata-declared static function implementing a dispatched member of an array
    /// classifier on the JVM array class (`ArrayIteratorsKt.iterator([I)`).
    pub(crate) fn static_array_member_realization(
        &self,
        name: &str,
        descriptor: &str,
    ) -> Option<crate::jvm::inline::StaticMemberRealization> {
        let candidates = self.ext_by_name(name);
        let mut matches = candidates
            .all
            .iter()
            .filter(|candidate| candidate.public && candidate.descriptor == descriptor)
            .map(|candidate| candidate.render(&candidates.owner_names))
            .filter(|candidate| {
                self.meta_functions_name(candidate.owner)
                    .iter()
                    .any(|function| {
                        !function.is_extension()
                            && function.kotlin_name == name
                            && function.jvm_name == candidate.name
                            && function
                                .jvm_desc
                                .is_none_or(|metadata| metadata == candidate.descriptor)
                    })
            })
            .map(|candidate| crate::jvm::inline::StaticMemberRealization {
                owner: candidate.owner.render(),
                name: candidate.name,
                descriptor: candidate.descriptor,
            });
        let realization = matches.next()?;
        matches
            .all(|other| other == realization)
            .then_some(realization)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Classpath;
    use super::declared_regeneration_role;
    use crate::jvm::inline::MethodBodies;
    use crate::jvm::inline::RegeneratedClass;
    use crate::types::type_name;

    #[test]
    fn a_declared_sam_wrapper_takes_precedence_over_its_unnamed_self_row() {
        assert_eq!(
            declared_regeneration_role(true, true),
            Some(RegeneratedClass::SamWrapper)
        );
        assert_eq!(
            declared_regeneration_role(true, false),
            Some(RegeneratedClass::AnonymousObject)
        );
        assert_eq!(
            declared_regeneration_role(false, true),
            Some(RegeneratedClass::SamWrapper)
        );
        assert_eq!(declared_regeneration_role(false, false), None);
    }

    #[test]
    fn interned_interface_probe_maps_physical_builtin_owners() {
        let Some(jar) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let classpath = Classpath::new(vec![jar]);
        for (spelling, expected) in [
            ("kotlin/collections/List", true),
            ("java/util/List", true),
            ("kotlin/Number", false),
            ("java/lang/Number", false),
            ("kotlin/String", false),
            ("java/lang/String", false),
            ("sample/Class", false),
        ] {
            assert_eq!(
                classpath.owner_is_interface_name(type_name(spelling)),
                expected,
                "{spelling}"
            );
        }
    }
}
