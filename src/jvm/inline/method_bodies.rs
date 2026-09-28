//! JVM realization facts consumed while emitting an already-checked call or property access.
//!
//! This is the narrow provider boundary between the emitter and classpath-backed bytecode. It
//! exposes physical owners, descriptors, fields, and method bodies only after frontend resolution
//! has selected the semantic declaration; none of these facts participate in source resolution.

use crate::jvm::classreader::MethodCode;
use crate::types::TypeName;

/// A platform realization selected only while emitting an already-resolved semantic member call.
/// Nothing in checking or common lowering sees this owner/descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticMemberRealization {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

/// How a compiled class realizes a property read or write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyAccess {
    /// `getfield` / `getstatic <owner>.<name>:<descriptor>`.
    Field {
        owner: TypeName,
        name: String,
        descriptor: String,
        is_static: bool,
    },
    /// `invokevirtual` / `invokeinterface` / `invokestatic <owner>.<name><descriptor>`.
    Accessor {
        owner: TypeName,
        name: String,
        descriptor: String,
        is_static: bool,
        is_interface: bool,
    },
    /// `invokestatic <owner>.<name>…` — a synthetic static access bridge. An instance member's
    /// bridge takes the receiver as its first argument; a named object's static field does not.
    AccessBridge {
        owner: TypeName,
        name: String,
        descriptor: String,
        takes_receiver: bool,
        /// Source property named in the inline `lateinit` guard when this bridge performs a raw
        /// backing-field read. A bridge that dispatches to a getter leaves the guard in that body.
        inline_uninitialized_guard: Option<String>,
    },
}

/// How a class an inlined body constructs or loads is declared, for the classes the call site
/// regenerates (kotlinc's `isAnonymousClass || isSamWrapper`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegeneratedClass {
    /// An anonymous object or lambda class: its `InnerClasses` self entry has no simple name.
    AnonymousObject,
    /// A SAM wrapper: a synthetic class that is not nested in another.
    SamWrapper,
}

/// The bytecode and physical-realization capabilities needed by JVM emission.
///
/// Whether a declaration is inline and which source declaration was selected are checked facts
/// carried in FIR/IR. Implementations answer only how that declaration is represented on the JVM.
pub trait MethodBodies {
    /// The compiled `Code` body of `owner.name descriptor`, or `None` if absent/abstract/native.
    fn body(&self, owner: &str, name: &str, descriptor: &str) -> Option<MethodCode>;

    /// [`Self::body`] for a classifier that is already interned.
    ///
    /// The default has no classpath, so it answers `None` without rendering the owner. An
    /// implementation that can read a body overrides this method directly.
    fn body_name(&self, _owner: TypeName, _name: &str, _descriptor: &str) -> Option<MethodCode> {
        None
    }

    /// The class file of `internal`, for a class an inlined body's objects are copied from.
    fn class_file(&self, _internal: &str) -> Option<Vec<u8>> {
        None
    }

    /// How the class file of `internal` declares it, when the call site regenerates it; `None` for
    /// any other class, and for one whose class file is not available.
    fn regenerated_class(&self, _internal: &str) -> Option<RegeneratedClass> {
        None
    }

    /// Whether `owner` is an interface, which determines the constant-pool reference kind.
    fn owner_is_interface(&self, _owner: &str) -> bool {
        false
    }

    /// [`Self::owner_is_interface`] for a classifier that is already interned.
    ///
    /// The string method's default ignores its argument, so this default answers the same `false`
    /// without rendering the identity. An implementation that can tell an interface from a class
    /// overrides this method directly.
    fn owner_is_interface_name(&self, _owner: TypeName) -> bool {
        false
    }

    /// Whether an instruction-level method or field reference is private to its defining class.
    fn member_is_private(&self, _owner: &str, _name: &str, _descriptor: &str) -> bool {
        false
    }

    /// Whether a bootstrap dependency is provably reachable from any destination class.
    ///
    /// This is intentionally stronger than “not private”: unknown, protected, package-private, or
    /// public members on non-public owners all return false. Bootstrap dependencies receive no
    /// verifier access check before linkage, so uncertainty must decline the splice.
    fn member_is_publicly_reachable(&self, _owner: &str, _name: &str, _descriptor: &str) -> bool {
        false
    }

    /// Whether a class named by a relocated bootstrap dependency is provably reachable from any
    /// destination class. Unknown and non-public classes return false.
    fn class_is_publicly_reachable(&self, _class: &str) -> bool {
        false
    }

    /// Whether a selected JVM method realization is static.
    fn method_is_static(&self, _owner: &str, _name: &str, _descriptor: &str) -> bool {
        false
    }

    /// Find a unique metadata-declared static realization for a selected array member.
    fn static_array_member_realization(
        &self,
        _name: &str,
        _descriptor: &str,
    ) -> Option<StaticMemberRealization> {
        None
    }

    /// Decode a selected property read into its exact accessor or field realization.
    fn property_read_access(&self, _owner: TypeName, _property: &str) -> Option<PropertyAccess> {
        None
    }

    /// Decode a selected mutable-property write into its exact realization.
    fn property_write_access(&self, _owner: TypeName, _property: &str) -> Option<PropertyAccess> {
        None
    }

    /// Decode one checked external accessor identity into its exact JVM realization.
    fn external_property_access(
        &self,
        _accessor: crate::fir::ExternalCallableId,
    ) -> Option<PropertyAccess> {
        None
    }

    /// JVM storage for an already-resolved semantic singleton classifier.
    fn singleton_storage(&self, _classifier: TypeName) -> Option<(TypeName, String)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::MethodBodies;
    use crate::jvm::classreader::MethodCode;
    use crate::types::type_name;

    struct StringProbe {
        called: Cell<bool>,
    }

    impl MethodBodies for StringProbe {
        fn body(&self, _owner: &str, _name: &str, _descriptor: &str) -> Option<MethodCode> {
            self.called.set(true);
            None
        }

        fn owner_is_interface(&self, _owner: &str) -> bool {
            self.called.set(true);
            true
        }
    }

    #[test]
    fn default_interface_probe_ignores_the_string_method() {
        let probe = StringProbe {
            called: Cell::new(false),
        };
        assert!(!probe.owner_is_interface_name(type_name("java/util/List")));
        assert!(!probe.called.get());
    }

    #[test]
    fn default_body_probe_ignores_the_string_method() {
        let probe = StringProbe {
            called: Cell::new(false),
        };
        assert!(probe
            .body_name(type_name("shared/Pool"), "answer", "()I")
            .is_none());
        assert!(!probe.called.get());
    }
}
