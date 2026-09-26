//! Backend-populated side tables recording how the JVM realized static storage after common
//! lowering: companion storage hoisted onto its outer class, `@JvmField` statics, and a field name
//! disambiguated from another static of the same owner. Keeping these here, rather than on
//! [`IrStatic`](super::IrStatic), prevents a physical JVM layout fact from becoming part of an
//! ordinary common-IR static declaration.

use super::{IrFile, TypeName};

impl IrFile {
    /// Record/query the JVM-only companion backing-storage realization selected after common
    /// lowering. Keeping this in a backend-populated side table prevents a physical JVM layout bit
    /// from becoming part of an ordinary common-IR static declaration.
    pub(crate) fn mark_jvm_companion_hoisted_static(&mut self, index: u32) {
        self.jvm_companion_hoisted_statics.insert(index);
    }

    pub(crate) fn mark_jvm_companion_property_static(
        &mut self,
        companion: TypeName,
        property: u32,
        index: u32,
    ) {
        self.jvm_companion_property_statics
            .insert((companion, property), index);
    }

    pub(crate) fn jvm_companion_property_static(
        &self,
        companion: TypeName,
        property: u32,
    ) -> Option<u32> {
        self.jvm_companion_property_statics
            .get(&(companion, property))
            .copied()
    }

    pub(crate) fn set_jvm_static_field_name(&mut self, index: u32, name: String) {
        self.jvm_static_field_names.insert(index, name);
    }

    /// The physical JVM field name of static `index`: its source name unless the JVM realization
    /// had to disambiguate it from another static field of the same owner.
    pub(crate) fn static_field_jvm_name(&self, index: u32) -> &str {
        self.jvm_static_field_names
            .get(&index)
            .map_or(self.statics[index as usize].name.as_str(), String::as_str)
    }

    pub(crate) fn is_jvm_companion_hoisted_static(&self, index: u32) -> bool {
        self.jvm_companion_hoisted_statics.contains(&index)
    }

    /// Record/query the `@JvmField` realization of a hoisted companion property: the static IS the
    /// property's public JVM surface — no accessors, no `access$…$cp` bridges — so every reader and
    /// writer goes `getstatic`/`putstatic` on the owner directly (kotlinc's shape).
    pub(crate) fn mark_jvm_field_static(&mut self, index: u32) {
        self.jvm_field_statics.insert(index);
    }

    pub(crate) fn is_jvm_field_static(&self, index: u32) -> bool {
        self.jvm_field_statics.contains(&index)
    }
}
