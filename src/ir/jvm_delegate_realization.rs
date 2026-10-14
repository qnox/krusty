//! Backend-populated record of the delegated properties whose delegate the JVM evaluates in place.
//!
//! A delegate that is a singleton or a constant needs no storage: kotlinc reads it again wherever
//! an accessor needs it and adds a `get<X>$delegate` method that returns it for reflection. The
//! JVM realization pass records here which storage it retired and which method it generated, so
//! emission and `@Metadata` consume one answer instead of re-deciding from the IR's shape.

use super::{ClassId, ExprId, FunId, IrFile};
use crate::fir::PropertyId;
use crate::fx_hash::{HashMap, HashSet};

/// Which delegated properties the JVM realized without delegate storage, and their generated
/// `get<X>$delegate` methods.
#[derive(Clone, Debug, Default)]
pub(crate) struct JvmInPlaceDelegates {
    /// Facade statics that held such a delegate. They stay in [`IrFile::statics`] so no static
    /// index moves, but the JVM declares no field for them.
    retired_statics: HashSet<u32>,
    package_methods: HashMap<PropertyId, FunId>,
    /// `(declaring class, index into its properties)`.
    member_methods: HashMap<(ClassId, u32), FunId>,
    /// `(declaring class, index into its member extension properties)`.
    member_extension_methods: HashMap<(ClassId, u32), FunId>,
    /// Each generated method and the getter it is named after.
    methods: HashMap<FunId, FunId>,
    /// Constructor statements that evaluate such a delegate where its field was initialized.
    evaluations: HashSet<ExprId>,
}

impl IrFile {
    pub(crate) fn retire_jvm_delegate_static(&mut self, index: u32) {
        self.jvm_in_place_delegates.retired_statics.insert(index);
    }

    /// Whether static `index` held a delegate the JVM now evaluates in place, so it has no field.
    pub(crate) fn is_retired_jvm_delegate_static(&self, index: u32) -> bool {
        self.jvm_in_place_delegates.retired_statics.contains(&index)
    }

    pub(crate) fn record_jvm_package_delegate_method(
        &mut self,
        property: PropertyId,
        method: FunId,
        getter: FunId,
    ) {
        let delegates = &mut self.jvm_in_place_delegates;
        delegates.package_methods.insert(property, method);
        delegates.methods.insert(method, getter);
    }

    pub(crate) fn record_jvm_member_delegate_method(
        &mut self,
        class: ClassId,
        property: u32,
        method: FunId,
        getter: FunId,
    ) {
        let delegates = &mut self.jvm_in_place_delegates;
        delegates.member_methods.insert((class, property), method);
        delegates.methods.insert(method, getter);
    }

    pub(crate) fn record_jvm_member_extension_delegate_method(
        &mut self,
        class: ClassId,
        property: u32,
        method: FunId,
        getter: FunId,
    ) {
        let delegates = &mut self.jvm_in_place_delegates;
        delegates
            .member_extension_methods
            .insert((class, property), method);
        delegates.methods.insert(method, getter);
    }

    /// The `get<X>$delegate` method of package property `property`, when its delegate is
    /// evaluated in place.
    pub(crate) fn jvm_package_delegate_method(&self, property: PropertyId) -> Option<FunId> {
        self.jvm_in_place_delegates
            .package_methods
            .get(&property)
            .copied()
    }

    /// The `get<X>$delegate` method of `class`'s property at `property`, when its delegate is
    /// evaluated in place.
    pub(crate) fn jvm_member_delegate_method(
        &self,
        class: ClassId,
        property: u32,
    ) -> Option<FunId> {
        self.jvm_in_place_delegates
            .member_methods
            .get(&(class, property))
            .copied()
    }

    /// The `get<X>$delegate` method of `class`'s member extension property at `property`.
    pub(crate) fn jvm_member_extension_delegate_method(
        &self,
        class: ClassId,
        property: u32,
    ) -> Option<FunId> {
        self.jvm_in_place_delegates
            .member_extension_methods
            .get(&(class, property))
            .copied()
    }

    /// Whether `function` is a generated `get<X>$delegate` method. It is named by its property's
    /// `JvmPropertySignature`, not described as a function of its own.
    pub(crate) fn is_jvm_delegate_method(&self, function: FunId) -> bool {
        self.jvm_in_place_delegates.methods.contains_key(&function)
    }

    /// Every generated `get<X>$delegate` method with the getter whose JVM name it extends.
    pub(crate) fn jvm_delegate_methods_with_getters(
        &self,
    ) -> impl Iterator<Item = (FunId, FunId)> + '_ {
        self.jvm_in_place_delegates
            .methods
            .iter()
            .map(|(&method, &getter)| (method, getter))
    }

    pub(crate) fn record_jvm_delegate_evaluation(&mut self, statement: ExprId) {
        self.jvm_in_place_delegates.evaluations.insert(statement);
    }

    /// Whether `statement` evaluates an in-place delegate at its property's initialization site.
    /// kotlinc keeps that site's line even when the evaluation itself leaves no instruction.
    pub(crate) fn is_jvm_delegate_evaluation(&self, statement: ExprId) -> bool {
        self.jvm_in_place_delegates.evaluations.contains(&statement)
    }
}
