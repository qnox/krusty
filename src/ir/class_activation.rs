//! What initializing a class initializes, for targets with no class initialization of their own.
//!
//! A `companion { … }` block property is stored with its class, and Kotlin runs its initializer
//! as part of initializing that class. A superclass, and its companion, is initialized before its
//! subclass, and so is each superinterface with statics and a non-abstract member (JLS 12.4.2).
//! Common lowering decides that sequence once, from the module's declarations, and records it on
//! the class; a backend maps each step onto its own representation.

use super::TypeName;

/// One step of initializing a class, keyed by the resolved identity of the classifier it
/// initializes, which may be declared in another file of the module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IrClassActivationStep {
    /// Run the classifier's own deferred statics, in declaration order, at most once.
    OwnStatics(TypeName),
    /// Initialize the classifier's companion object.
    Companion(TypeName),
    /// Initialize a superclass declared in another module, through the activation entry that
    /// module publishes for it. It comes first: the module chain extends it.
    Dependency(TypeName),
}
