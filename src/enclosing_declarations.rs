//! The declarations a class declared in executable code is nested in, below its lexical owner.
//!
//! The frontend records them while it names local classes, and checked FIR and common IR carry
//! them unchanged in a local class's naming provenance. They are source identities and semantic
//! roles only: a target decides how each one is spelled, so no JVM or kotlinc synthetic name
//! crosses this boundary.

/// One declaration enclosing a local class, anonymous object or callable-reference class.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EnclosingDeclaration {
    /// A named function (member, top-level or local), by its source name.
    Function(String),
    /// The getter of the property with this source name.
    Getter(String),
    /// The setter of the property with this source name.
    Setter(String),
    /// A lambda's or anonymous function's body.
    Lambda,
    /// The enclosing class's instance initialization: its constructors, property initializers and
    /// `init` blocks.
    InstanceInitializer,
    /// Static initialization: of the file's top-level properties, of an object, or of an enum's
    /// entries.
    StaticInitializer,
    /// The body of the enum entry with this source name.
    EnumEntry(String),
}
