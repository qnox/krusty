use crate::{PluginOption, TypeName};

/// What the frontend does with a convention plugin's annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convention {
    /// Classes the annotations match, and their members, are `open` by default (all-open).
    OpenByDefault,
    /// Classes the annotations match get a generated zero-argument constructor (no-arg).
    NoArgConstructor,
    /// An interface the annotations mark takes a SAM conversion's first parameter as the converted
    /// function's receiver (sam-with-receiver).
    SamWithReceiver,
    /// A class the annotations mark takes part in `target.p = v` becoming `target.p.assign(v)`
    /// (assignment).
    Assignment,
}

/// A kotlinc plugin that only configures a [`Convention`] with annotations.
pub struct ConventionPlugin {
    /// kotlinc's plugin id, the `<id>` of `-P plugin:<id>:<key>=<value>`.
    pub plugin_id: &'static str,
    pub name: &'static str,
    pub convention: Convention,
    /// The annotations the compilation's options for this plugin configure, as resolved
    /// classifier identities.
    pub annotations: fn(&[PluginOption]) -> Vec<TypeName>,
}
