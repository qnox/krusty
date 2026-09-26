//! The call-site values of an inline body's reified type parameters.
//!
//! This is the contract between the emitter, which knows each call's reified arguments, and the
//! code that specializes an inline body for them: the symbolic inliner, and the raw-byte splicer's
//! temporary adapter while it remains.

use std::collections::HashMap;

/// Reified arguments at one inline call site, in the forms the body's markers consume.
#[derive(Clone, Debug, Default)]
pub(in crate::jvm) struct ReifiedArguments {
    pub(in crate::jvm) classes: HashMap<String, ReifiedArgument>,
    pub(in crate::jvm) type_of: HashMap<String, Vec<crate::jvm::type_of::TypeOfInsn>>,
}

impl ReifiedArguments {
    pub(in crate::jvm) fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.type_of.is_empty()
    }
}

/// The call-site value of one reified type parameter of an inline body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::jvm) enum ReifiedArgument {
    /// A concrete JVM class (internal name): the marker is erased and its type-bearing op
    /// repointed. `nullable`, `intrinsic` and `rendered` (the type as kotlinc spells it in a failed
    /// cast's message) decide the code an `is`, `as` or `as?` needs around that op.
    Class {
        internal: String,
        nullable: bool,
        intrinsic: Option<crate::ir::TypeCheckRole>,
        rendered: String,
    },
    /// A reified type parameter of the HOST (its source name, and whether the argument is `T?`). The
    /// host is itself a reified inline body, so the marker stays, renamed to the host's parameter,
    /// and the type-bearing op keeps its erased placeholder until the host's own caller reifies it.
    Forwarded { name: String, nullable: bool },
}
