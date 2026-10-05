//! Declaration facts a provider publishes for a classifier, as its declaring compilation fixed them.

use super::{Ty, TypeName, Visibility};

/// A plugin-generated classifier published with the source header that owns it. The common
/// frontend records semantic declaration facts; a representation backend maps them to its own
/// class flags without recognizing generated JVM spellings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedClassifierFact {
    pub classifier: TypeName,
    pub lexical_owner: TypeName,
    pub purpose: GeneratedClassifierPurpose,
    pub source_name: Box<str>,
    pub visibility: Visibility,
    pub kind: GeneratedClassifierKind,
    pub is_abstract: bool,
    pub is_final: bool,
    pub captures_outer: bool,
    pub compiler_generated: bool,
}

/// Semantic reason a frontend plugin contributed a classifier. Consumers select this contract,
/// never a generated source/JVM spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedClassifierPurpose {
    SerializationSerializer,
    SerializationCompanion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedClassifierKind {
    Class,
    Interface,
    Annotation,
    Enum,
}

/// Declaration facts of a classifier, as its declaring compilation fixed them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifierDeclarationFacts {
    pub kind: ClassifierDeclarationKind,
    /// `abstract` or `sealed`: no instance has exactly this class. An interface is always abstract.
    pub is_abstract: bool,
    /// Type parameters the classifier declares itself, not the ones it captures lexically.
    pub own_type_parameter_count: usize,
    /// The companion object: the name of the static field holding it and its classifier. A source
    /// classifier of this module reports only a DECLARED companion; one a compiler plugin adds is
    /// that plugin's to name.
    pub companion: Option<(Box<str>, TypeName)>,
    /// The Kotlin qualified name with every boundary dotted (`lib.Outer.Nested`), as metadata and
    /// source declare it. It is not derivable from the internal name, where `$` is also a legal
    /// identifier character.
    pub qualified_name: Option<Box<str>>,
    /// Declared in a source file of this module rather than read from a dependency.
    pub source: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassifierDeclarationKind {
    Class,
    Interface,
    Annotation,
    Enum,
    Object,
}

/// A value class's sole property and its underlying type as the declaration states it, over the
/// declaration's own type parameters (`Wrapped<T>(val sink: Sink<T>)` declares `sink: Sink<T>` over
/// `[T]`), so an application `Wrapped<Label>` can substitute its arguments into it wherever the
/// declaration lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredValueClass {
    pub property: Box<str>,
    pub underlying: Ty,
    /// The declaration's own type parameters, in order, each as the type-parameter type its
    /// underlying type refers to it by.
    pub type_parameters: Box<[Ty]>,
}
