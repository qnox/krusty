//! Backend-populated side tables recording how the JVM realized static storage after common
//! lowering: companion storage hoisted onto its outer class, `@JvmField` statics, and a field name
//! disambiguated from another static of the same owner. Keeping these here, rather than on
//! [`IrStatic`](super::IrStatic), prevents a physical JVM layout fact from becoming part of an
//! ordinary common-IR static declaration.

use super::{IrFile, IrStaticAccessor, TypeName};

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

    /// Record the companion initializer that `outer`'s `<clinit>` runs after storing the instance.
    pub(crate) fn set_companion_clinit_body(&mut self, outer: TypeName, body: super::ExprId) {
        self.companion_clinit_bodies.insert(outer, body);
    }

    /// The companion initializer scheduled for `outer`'s `<clinit>`, if that class has one.
    pub(crate) fn companion_clinit_body(&self, outer: TypeName) -> Option<super::ExprId> {
        self.companion_clinit_bodies.get(&outer).copied()
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

    /// Whether static `index` is published through a compiler-default `getX`. A `const val`
    /// inlines, a `@JvmField` is its own surface, and a private property is reached only through
    /// its field or `access$…$p` bridges, so none of those has default accessors; a declared getter
    /// is an ordinary function.
    /// The annotations on the accessors of the source property `index` stores.
    pub(crate) fn static_accessor_annotations(
        &self,
        index: u32,
    ) -> Option<&crate::ir::AccessorAnnotations> {
        self.local_property_layouts
            .iter()
            .find_map(|(property, layout)| match layout {
                crate::ir::IrLocalPropertyLayout::TopLevelStorage { storage, .. }
                    if *storage == index =>
                {
                    self.accessor_annotations.get(property)
                }
                _ => None,
            })
    }

    pub(crate) fn has_jvm_default_static_getter(&self, index: u32) -> bool {
        self.publishes_jvm_default_static_accessors(index)
            && self.statics[index as usize].accessors.getter == IrStaticAccessor::Default
    }

    /// [`Self::has_jvm_default_static_getter`] for a `var`'s `setX`.
    pub(crate) fn has_jvm_default_static_setter(&self, index: u32) -> bool {
        let storage = &self.statics[index as usize];
        storage.is_var
            && self.publishes_jvm_default_static_accessors(index)
            && storage.accessors.setter == IrStaticAccessor::Default
    }

    fn publishes_jvm_default_static_accessors(&self, index: u32) -> bool {
        let storage = &self.statics[index as usize];
        !(storage.is_const || self.is_jvm_field_static(index) || storage.visibility.is_private())
    }
}
