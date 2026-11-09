//! Backend-populated side tables recording the JVM names realized for compiler-default property
//! accessors: a written `@JvmName`, an internal member's `$<module>` suffix, and a value-class
//! signature's mangled spelling. These are JVM ABI facts decided after common lowering, so they
//! live beside, rather than on, the common [`IrProperty`](super::IrProperty) and
//! [`IrStatic`](super::IrStatic) declarations. A declaration without an entry keeps the ordinary
//! `get<X>`/`set<X>` spelling.

use super::{IrFile, TypeName};

/// The realized JVM names of one declaration's default accessors. `None` keeps the convention.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JvmAccessorNames {
    pub getter: Option<String>,
    pub setter: Option<String>,
}

/// Which default accessor of a declaration a realized name belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JvmAccessorSide {
    Getter,
    Setter,
}

impl JvmAccessorNames {
    fn side(&self, side: JvmAccessorSide) -> Option<&str> {
        match side {
            JvmAccessorSide::Getter => self.getter.as_deref(),
            JvmAccessorSide::Setter => self.setter.as_deref(),
        }
    }

    fn set(&mut self, side: JvmAccessorSide, name: String) {
        match side {
            JvmAccessorSide::Getter => self.getter = Some(name),
            JvmAccessorSide::Setter => self.setter = Some(name),
        }
    }
}

impl IrFile {
    /// The JVM name realized for a class property's default accessor, keyed by the property's
    /// stable index within its declaring class. `None` keeps the ordinary spelling.
    pub fn jvm_property_accessor_name(
        &self,
        class: TypeName,
        property: u32,
        side: JvmAccessorSide,
    ) -> Option<&str> {
        self.jvm_property_accessor_names
            .get(&(class, property))
            .and_then(|names| names.side(side))
    }

    /// Both realized accessor names of a class property.
    pub fn jvm_property_accessor_names(&self, class: TypeName, property: u32) -> JvmAccessorNames {
        self.jvm_property_accessor_names
            .get(&(class, property))
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_jvm_property_accessor_name(
        &mut self,
        class: TypeName,
        property: u32,
        side: JvmAccessorSide,
        name: String,
    ) {
        self.jvm_property_accessor_names
            .entry((class, property))
            .or_default()
            .set(side, name);
    }

    /// The JVM name realized for a static's default accessor. `None` keeps the ordinary spelling.
    pub fn jvm_static_accessor_name(&self, storage: u32, side: JvmAccessorSide) -> Option<&str> {
        self.jvm_static_accessor_names
            .get(&storage)
            .and_then(|names| names.side(side))
    }

    pub fn set_jvm_static_accessor_name(
        &mut self,
        storage: u32,
        side: JvmAccessorSide,
        name: String,
    ) {
        self.jvm_static_accessor_names
            .entry(storage)
            .or_default()
            .set(side, name);
    }

    /// Give a new static the names a declaration's accessors were realized with.
    pub fn set_jvm_static_accessor_names(&mut self, storage: u32, names: JvmAccessorNames) {
        if names != JvmAccessorNames::default() {
            self.jvm_static_accessor_names.insert(storage, names);
        }
    }

    /// The JVM name of a static's compiler-default getter.
    pub fn jvm_static_getter_name(&self, storage: u32) -> String {
        self.jvm_static_accessor_name(storage, JvmAccessorSide::Getter)
            .map_or_else(
                || crate::names::property_getter_name(&self.statics[storage as usize].name),
                str::to_owned,
            )
    }

    /// The JVM name of a static's compiler-default setter.
    pub fn jvm_static_setter_name(&self, storage: u32) -> String {
        self.jvm_static_accessor_name(storage, JvmAccessorSide::Setter)
            .map_or_else(
                || crate::names::property_setter_name(&self.statics[storage as usize].name),
                str::to_owned,
            )
    }
}
