use super::TypeName;

/// Qualified semantic namespace that declares one selected dependency callable.
///
/// A target may realize a package declaration on a file facade or a Kotlin classifier member on a
/// mapped platform class. Those physical containers are emission facts, not the callable's source
/// identity, so providers retain the semantic namespace independently.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SemanticCallableOwner {
    Package(TypeName),
    Classifier(TypeName),
}

impl SemanticCallableOwner {
    pub fn name(self) -> TypeName {
        match self {
            Self::Package(name) | Self::Classifier(name) => name,
        }
    }
}
