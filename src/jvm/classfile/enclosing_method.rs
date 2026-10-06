//! The `EnclosingMethod` attribute of a local or anonymous class: its owner class and, when the
//! class is immediately enclosed by one, the method's NameAndType.

use super::{u2, ClassWriter};

impl ClassWriter {
    /// Set the enclosing class and method for a local class.
    pub fn set_enclosing_method(&mut self, owner: &str, method: &str, descriptor: &str) {
        self.enclosing_method = Some((
            owner.to_string(),
            method.to_string(),
            descriptor.to_string(),
        ));
    }

    /// Set only the enclosing CLASS. The JVM spec allows `method_index = 0` — "not immediately
    /// enclosed by a method or constructor" — and the attribute's presence is what makes reflection
    /// treat the class as local rather than top-level, which is what decides `simpleName`. Used
    /// where the enclosing method's descriptor is not reconstructable; a wrong one would make
    /// `Class.getEnclosingMethod()` throw, while absent is well-defined.
    pub fn set_enclosing_class(&mut self, owner: &str) {
        self.enclosing_method = Some((owner.to_string(), String::new(), String::new()));
    }

    /// Intern the `EnclosingMethod` refs (owner class, method NameAndType). kotlinc visits them
    /// before the `InnerClasses` rows, although the attribute is serialized after that table; its
    /// NAME interns later, with the other attribute names.
    pub(super) fn intern_enclosing_method_refs(&mut self) {
        if let Some((owner, method, desc)) = self.enclosing_method.clone() {
            self.cp.class(&owner);
            if !method.is_empty() {
                self.cp.name_and_type(&method, &desc);
            }
        }
    }

    /// Build the attribute (name index, body), consuming the recorded enclosing refs.
    pub(super) fn enclosing_method_attribute(&mut self) -> Option<(u16, Vec<u8>)> {
        let (owner, method, desc) = self.enclosing_method.take()?;
        let name = self.cp.utf8("EnclosingMethod");
        let class_idx = self.cp.class(&owner);
        // An empty method name is the class-only form: `method_index = 0`.
        let nat_idx = if method.is_empty() {
            0
        } else {
            self.cp.name_and_type(&method, &desc)
        };
        let mut body = Vec::new();
        u2(&mut body, class_idx);
        u2(&mut body, nat_idx);
        Some((name, body))
    }

    /// kotlinc's class-attribute visit after `@Metadata`: the `EnclosingMethod` refs, then the
    /// retained `InnerClasses` rows. This fixes their pool order only; the attributes themselves
    /// are serialized as `InnerClasses`, then `EnclosingMethod`.
    pub(in crate::jvm) fn intern_post_metadata_attribute_refs(&mut self) {
        // Before these rows intern their class constants: a `Ref` the captured-vars pass is about
        // to delete must be met here, not at the `new` that emission already interned.
        self.release_refs_removed_by_unboxing();
        self.intern_enclosing_method_refs();
        self.seed_inner_class_names();
    }
}
