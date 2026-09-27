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
}

/// The compiled classes the call site reads bodies from declare the classes those bodies name.
impl ClassRoles for &(dyn MethodBodies + '_) {
    fn regenerated_class(&self, internal: &str) -> Option<RegeneratedClass> {
        MethodBodies::regenerated_class(*self, internal)
    }
}
