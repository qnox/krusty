//! The classpath as the inliner's source of compiled bodies and class files.

use super::*;

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
    fn owner_is_interface(&self, owner: &str) -> bool {
        // Prefer the real class flag; otherwise the mapped builtin's own `.kotlin_builtins`
        // `CLASS_KIND`. A Kotlin builtin and the JVM class it maps to always agree on interface-ness
        // (`List`/`java.util.List`, `Number`/`java.lang.Number`), so no curated per-name table is
        // needed — the one this replaced omitted every `java/util/*` and answered "class" for them,
        // which emitted `invokevirtual` on an interface whenever no JDK supplied the class file.
        self.find(owner)
            .map(|ci| ci.is_interface())
            .or_else(|| {
                let owner_id = type_name(owner);
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
    fn static_array_member_realization(
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
        owner: &str,
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
        owner: &str,
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
            owner: callable.owner.render(),
            name: callable.name,
            descriptor: callable.descriptor,
            is_static,
        })
    }
}
