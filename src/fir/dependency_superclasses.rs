//! The dispatch shape of the dependency classes a module class extends, frozen in Pass 1.
//!
//! A module class whose superclass is a dependency class inherits that class's dispatch table, so a
//! target that numbers slots itself numbers the dependency class too. Its shape is a semantic fact
//! of the dependency declaration, the same for every module: its superclass, the members a subclass
//! overrides or reaches by dispatch (open, abstract and overriding ones) in declaration order, and
//! their override edges, selected by the same rule as a module class's. A backend reads it here and
//! never reopens the declaration provider.

use super::{
    ExternalCallableId, ResolvedFunctionOverride, ResolvedModuleIndex, ResolvedPropertyOverride,
    ResolvedTy,
};
use crate::types::TypeName;

/// One dependency class a module class extends, directly or through another dependency class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDependencySuperclass {
    pub classifier: TypeName,
    /// The declared superclass, or `None` for `kotlin.Any`.
    pub superclass: Option<TypeName>,
    /// The members a subclass overrides or reaches by dispatch, in declaration order.
    pub members: Box<[ResolvedDependencyMember]>,
    /// The override edges of those members, whose implementations are this class's.
    pub function_overrides: Box<[ResolvedFunctionOverride]>,
    pub property_overrides: Box<[ResolvedPropertyOverride]>,
}

/// A dispatched member of a dependency class, by its provider identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDependencyMember {
    Function {
        callable: ExternalCallableId,
        /// The declaration's own parameters (extension receiver in place) and result.
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        /// A `suspend` function, which takes its caller's continuation after `parameters`.
        suspend: bool,
    },
    /// A property, by its getter's identity.
    Property {
        getter: ExternalCallableId,
        setter: bool,
        /// What its accessors take after the dispatch receiver, as its getter declares them: its
        /// context parameters, then a member extension's receiver. A setter takes `value` after.
        accessor_parameters: Box<[ResolvedTy]>,
        /// What its getter answers.
        value: ResolvedTy,
    },
}

impl ResolvedModuleIndex {
    /// The dependency classes the module's classes extend, transitively, in a stable order.
    pub fn dependency_superclasses(&self) -> &[ResolvedDependencySuperclass] {
        &self.dependency_superclasses
    }

    pub(crate) fn publish_dependency_superclasses(
        &mut self,
        superclasses: Vec<ResolvedDependencySuperclass>,
    ) {
        assert!(
            self.dependency_superclasses.is_empty(),
            "the dependency superclasses are published once"
        );
        self.dependency_superclasses = superclasses.into_boxed_slice();
    }
}
