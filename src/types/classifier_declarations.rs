//! Declaration facts a provider publishes for a classifier, as its declaring compilation fixed them.

use super::{ReturnValueStatus, Ty, TypeName, Visibility};

/// The declaring container and exact source segment of a non-local classifier.
///
/// A top-level declaration explicitly has no classifier owner, even when its name contains `$`.
/// This is independent of the enclosing instance an `inner` class captures. Providers retain this
/// declaration fact in their own snapshot; an absent record means unavailable ownership evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClassifierDeclarationName {
    TopLevel { package: TypeName, name: Box<str> },
    Nested { owner: TypeName, name: Box<str> },
}

impl ClassifierDeclarationName {
    /// The classifier's own declared segment, exactly as written: a `$` in a backticked name is
    /// part of it, never a nesting separator.
    pub fn name(&self) -> &str {
        match self {
            Self::TopLevel { name, .. } | Self::Nested { name, .. } => name,
        }
    }
}

/// Return-value statuses of the four overrides every serialization-plugin generated serializer
/// declares. Pass 1 selects the overridden declarations from their complete semantic shapes; the
/// closed result crosses common lowering without retaining a provider or reopening member lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SerializationGeneratedMemberStatuses {
    pub(crate) serialize: ReturnValueStatus,
    pub(crate) deserialize: ReturnValueStatus,
    pub(crate) child_serializers: ReturnValueStatus,
    pub(crate) type_parameter_serializers: ReturnValueStatus,
}

impl SerializationGeneratedMemberStatuses {
    pub(crate) fn new(
        serialize: ReturnValueStatus,
        deserialize: ReturnValueStatus,
        child_serializers: ReturnValueStatus,
        type_parameter_serializers: ReturnValueStatus,
    ) -> Self {
        Self {
            serialize,
            deserialize,
            child_serializers,
            type_parameter_serializers,
        }
    }
}

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

/// The serial elements of a `@Serializable` class whose serializer kotlinx.serialization's plugin
/// generated, as a subclass compiled apart from the class inherits them: the elements the class's
/// own `write$Self` writes, and the superclass whose `write$Self` writes the ones before them.
///
/// A provider that cannot state every element exactly (an element carries annotations it does not
/// read, or the class lists no program order) publishes no record rather than a partial one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerializableClassElements {
    /// The class's own type parameters, by the identities its element types and superclass name
    /// them with. A generic class publishes them only where its provider knows those identities.
    pub type_parameters: Box<[Box<str>]>,
    /// The class's own elements, in program order.
    pub elements: Box<[SerializableElement]>,
    /// The superclass as the class declares it, type arguments included; `None` for `Any`.
    pub superclass: Option<Ty>,
}

/// One serial element of a [`SerializableClassElements`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerializableElement {
    pub name: Box<str>,
    /// The property's declared type, over the class's own type parameters.
    pub ty: Ty,
    /// The input may omit the element: its property is not a primary-constructor parameter
    /// without a default value.
    pub optional: bool,
    /// The element's name on the wire when the property's `@SerialName` gives one.
    pub serial_name: Option<crate::kt_string::KtString>,
    /// The serializer the property's `@Serializable(with = …)` names.
    pub serializer: Option<TypeName>,
    /// The property is `@Contextual`.
    pub contextual: bool,
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
