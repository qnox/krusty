/// What a library type *is*. Mutually exclusive at the source level; at the JVM level an
/// `Annotation` also carries `ACC_INTERFACE`, which classifier queries reflect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Class,
    Interface,
    Annotation,
    Enum,
    /// A Kotlin `object` (singleton) — has a `public static final INSTANCE` field of its own type,
    /// read as `getstatic <Type>.INSTANCE` when the object is referenced as a value.
    Object,
}

impl From<TypeKind> for crate::types::ClassifierDeclarationKind {
    fn from(kind: TypeKind) -> Self {
        match kind {
            TypeKind::Class => Self::Class,
            TypeKind::Interface => Self::Interface,
            TypeKind::Annotation => Self::Annotation,
            TypeKind::Enum => Self::Enum,
            TypeKind::Object => Self::Object,
        }
    }
}
