//! The call-site values of an inline body's reified type parameters.
//!
//! This is the contract between the emitter, which knows each call's reified arguments, and the
//! symbolic inliner, the only code that specializes an inline body for them.

use crate::fx_hash::HashMap;

/// Reified arguments at one inline call site, in the forms the body's markers consume.
#[derive(Clone, Debug, Default)]
pub(in crate::jvm) struct ReifiedArguments {
    pub(in crate::jvm) classes: HashMap<String, ReifiedArgument>,
    pub(in crate::jvm) type_of: HashMap<String, Vec<crate::jvm::type_of::TypeOfInsn>>,
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

/// How a call site reads the entries of one concrete enum, as kotlinc's
/// `ReifiedTypeInliner.processSpecialEnumFunction` decides it from the enum's declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::jvm) enum EnumEntriesRead {
    /// The enum declares the static `getEntries()`.
    Getter,
    /// The enum does not (a Java enum, or a Kotlin enum compiled before `entries`): the call site's
    /// class reads the static `field` of its synthetic mapping class `class`, which builds the
    /// entries from `values()` once.
    Mapping { class: String, field: String },
}

/// The call site's answer to [`EnumEntriesRead`] for each enum an inlined body reads the entries
/// of. A read that claims a mapping field claims it for the class the body is inlined into.
pub(in crate::jvm) trait EnumEntriesReads {
    /// `None` when the enum's declaration cannot be read, which the inliner refuses.
    fn entries(&mut self, enum_class: &str) -> Option<EnumEntriesRead>;
}

/// No declaration is readable: every entries read is refused.
pub(in crate::jvm) struct NoEnumEntriesReads;

impl EnumEntriesReads for NoEnumEntriesReads {
    fn entries(&mut self, _enum_class: &str) -> Option<EnumEntriesRead> {
        None
    }
}
