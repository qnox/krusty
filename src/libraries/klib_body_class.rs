//! Declaration-owned class evidence for authenticating serialized member headers.

use crate::metadata::id_signature::{
    metadata_class_signature, KlibPublicIdSignature, MetadataClass, MetadataContainer,
};
use crate::metadata::semantic::KotlinModality;
use crate::types::{Ty, TypeName, TypeVariance};

#[cfg(test)]
pub(crate) mod test_fixture;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KlibBodyClassParameters {
    pub(crate) formals: Box<[Ty]>,
    pub(crate) bounds: Box<[Vec<Ty>]>,
    pub(crate) variances: Box<[TypeVariance]>,
    pub(crate) own_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KlibBodyClassContract {
    pub(crate) module: String,
    pub(crate) signature: KlibPublicIdSignature,
    pub(crate) classifier: TypeName,
    pub(crate) parameters: KlibBodyClassParameters,
    pub(crate) kind: super::TypeKind,
    pub(crate) modality: KotlinModality,
    pub(crate) is_value: bool,
    pub(crate) is_inner: bool,
    pub(crate) is_expect: bool,
    /// Exact primary declaration, including a private primary that is not a public candidate.
    pub(crate) primary_constructor: Option<KlibPublicIdSignature>,
}

impl KlibBodyClassContract {
    /// Whether the common type model carries this class's instances as one of its builtin types,
    /// as it does an unsigned integer, rather than as an instance of a class.
    pub(crate) fn is_builtin_type(&self) -> bool {
        crate::types::builtin_semantic(self.classifier).is_some()
    }

    /// A final object named by `classifier` in `module`. A singleton edge carries this when the
    /// provider did not publish the class, so the contract is the class the classifier names.
    pub(crate) fn singleton_object(module: &str, classifier: TypeName) -> Option<Self> {
        let mut segments = Vec::new();
        let mut current = classifier;
        loop {
            if current == TypeName::ROOT {
                break;
            }
            segments.push(current.segment_ref());
            match current.parent() {
                Some(parent) if parent != TypeName::ROOT => current = parent,
                _ => break,
            }
        }
        segments.reverse();
        let (name, package) = segments.split_last()?;
        if name.contains('$') || name.contains('.') {
            return None;
        }
        let package = package
            .iter()
            .map(|&segment| segment.to_owned())
            .collect::<Vec<_>>();
        let signature = metadata_class_signature(
            MetadataContainer {
                package: &package,
                classes: &[],
                native_interop_library: false,
            },
            MetadataClass {
                name,
                type_params: &[],
                expect: false,
            },
        );
        Some(Self {
            module: module.to_owned(),
            signature,
            classifier,
            parameters: KlibBodyClassParameters {
                formals: Box::new([]),
                bounds: Box::new([]),
                variances: Box::new([]),
                own_count: 0,
            },
            kind: super::TypeKind::Object,
            modality: KotlinModality::Final,
            is_value: false,
            is_inner: false,
            is_expect: false,
            primary_constructor: None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KlibStoredField {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) final_: bool,
    pub(crate) private: bool,
}
