//! Declaration-owned class IDs shared by metadata carriers.

use crate::fx_hash::HashSet;
use crate::libraries::ClassifierDeclarationKey;
use crate::types::{ClassifierDeclarationName, TypeName};

#[cfg(test)]
mod tests;

/// A metadata class name whose lexical segments are known, or an explicitly local raw identity.
/// The semantic identity remains available for self types and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedClassId {
    classifier: TypeName,
    declaration: Option<(TypeName, Box<[Box<str>]>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassIdError {
    MissingDeclaration(ClassifierDeclarationKey),
    UnknownName(ClassifierDeclarationKey),
    CyclicOwner(ClassifierDeclarationKey),
    LocalInKlib(TypeName),
}

impl std::fmt::Display for ClassIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDeclaration(key) => write!(f, "missing metadata declaration {key:?}"),
            Self::UnknownName(key) => write!(f, "unknown metadata declaration name {key:?}"),
            Self::CyclicOwner(key) => write!(f, "cyclic metadata declaration owner {key:?}"),
            Self::LocalInKlib(classifier) => write!(
                f,
                "local metadata classifier {classifier} is not a KLIB declaration"
            ),
        }
    }
}

impl std::error::Error for ClassIdError {}

impl ResolvedClassId {
    /// Resolve a selected declaration and every lexical owner from one immutable fact source.
    pub fn resolve(
        key: ClassifierDeclarationKey,
        names: &dyn Fn(ClassifierDeclarationKey) -> Option<Option<ClassifierDeclarationName>>,
    ) -> Result<Self, ClassIdError> {
        let classifier = match key {
            ClassifierDeclarationKey::Ordinary(identity)
            | ClassifierDeclarationKey::TypeAlias(identity) => identity,
        };
        let mut current = key;
        let mut seen = HashSet::default();
        let mut segments = Vec::new();
        loop {
            if !seen.insert(current) {
                return Err(ClassIdError::CyclicOwner(current));
            }
            match names(current)
                .ok_or(ClassIdError::MissingDeclaration(current))?
                .ok_or(ClassIdError::UnknownName(current))?
            {
                ClassifierDeclarationName::TopLevel { package, name } => {
                    segments.push(name);
                    segments.reverse();
                    return Ok(Self {
                        classifier,
                        declaration: Some((package, segments.into_boxed_slice())),
                    });
                }
                ClassifierDeclarationName::Nested { owner, name } => {
                    segments.push(name);
                    current = ClassifierDeclarationKey::Ordinary(owner);
                }
            }
        }
    }

    /// An explicit declaration supplied by a metadata producer, including standalone fixtures.
    /// No part of the name is recovered from the semantic identity.
    pub fn declared(classifier: TypeName, package: TypeName, segments: &[&str]) -> Self {
        assert!(
            !segments.is_empty(),
            "a metadata class declaration has a name"
        );
        Self {
            classifier,
            declaration: Some((
                package,
                segments.iter().map(|name| Box::from(*name)).collect(),
            )),
        }
    }

    /// A classifier proven local by the source/IR provenance inventory.
    pub fn local(classifier: TypeName) -> Self {
        Self {
            classifier,
            declaration: None,
        }
    }

    pub fn classifier(&self) -> TypeName {
        self.classifier
    }

    pub(crate) fn declaration(&self) -> Option<(TypeName, &[Box<str>])> {
        self.declaration
            .as_ref()
            .map(|(package, names)| (*package, names.as_ref()))
    }
}
