//! The role a class an inlined body constructs or loads plays in regeneration, as its class file
//! declares it.
//!
//! kotlinc decides by spelling (`isAnonymousClass`: a simple name ending in `$<number>`;
//! `isSamWrapper`: a name containing `$sam$`). The compiler's names are not identities, so the port
//! asks the class's declaration instead; for every class kotlinc writes, both answers agree.

use crate::jvm::inline::MethodBodies;
pub(crate) use crate::jvm::inline::RegeneratedClass;

/// Where the inliner learns the declared role of a class the body names.
pub(crate) trait ClassRoles {
    /// The role of `internal`, or `None` for a class the call site does not regenerate (including
    /// one whose class file is not available).
    fn regenerated_class(&self, internal: &str) -> Option<RegeneratedClass>;

    /// Whether `internal` is an anonymous object (`isAnonymousClass`).
    fn is_anonymous_object(&self, internal: &str) -> bool {
        self.regenerated_class(internal) == Some(RegeneratedClass::AnonymousObject)
    }

    /// Whether `internal` is regenerated at all (`isAnonymousClass || isSamWrapper`).
    fn is_regenerated(&self, internal: &str) -> bool {
        self.regenerated_class(internal).is_some()
    }

    /// Whether a method of `internal` calls `Intrinsics.reifiedOperationMarker`. A class the
    /// inliner cannot read is treated as if it did: a `needClassReification` body must not keep a
    /// reference whose markers were not specialized.
    fn contains_reified_marker(&self, _internal: &str) -> bool {
        false
    }
}

/// The compiled classes the call site reads bodies from declare the classes those bodies name.
impl ClassRoles for &(dyn MethodBodies + '_) {
    fn regenerated_class(&self, internal: &str) -> Option<RegeneratedClass> {
        MethodBodies::regenerated_class(*self, internal)
    }

    fn contains_reified_marker(&self, internal: &str) -> bool {
        let Some(bytes) = self.class_file(internal) else {
            return false;
        };
        // `ClassNode::read` refuses a class whose `Code` carries an attribute a copy would drop
        // (`LocalVariableTypeTable` on `Intrinsics`). This predicate is not a copy: it asks whether
        // a method invokes `reifiedOperationMarker`, and the body index already skips attributes
        // it does not interpret.
        let Some(bodies) = crate::jvm::classreader::ClassBodies::parse(std::sync::Arc::new(bytes))
        else {
            return true;
        };
        for (name, descriptor) in bodies.coded_methods() {
            let Some(code) = bodies.method_code(name, descriptor) else {
                return true;
            };
            let Ok(node) = crate::jvm::method_node::MethodNode::read(0, name, descriptor, &code)
            else {
                return true;
            };
            if super::reified::has_reified_markers(&node) {
                return true;
            }
        }
        false
    }
}
