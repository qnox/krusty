//! The container that names the local classifiers a serialized body declares.

use crate::ir::IrSiteClassContainer;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};

/// The package of the top-level declaration `identity` names, or the classifier its member
/// belongs to. `None` when its file's package is unknown or its owner has no member name.
pub(super) fn site_container(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Option<IrSiteClassContainer> {
    let declared = match &identity.declaration {
        KlibDeclarationSignature::Public(signature) => signature,
        KlibDeclarationSignature::Accessor(accessor) => accessor.property(),
        // A file-local identity is a top-level function of its file.
        KlibDeclarationSignature::FileLocal { file, .. } => {
            let package = bodies.module_file_package(&identity.module, *file)?;
            return Some(IrSiteClassContainer::Package(
                super::klib_types::package_name(package),
            ));
        }
    };
    // A public identity's declaration path is its enclosing classifiers, then its own name.
    if declared.declaration().segments().len() > 1 {
        super::klib_types::member_owner(declared)
            .ok()
            .map(IrSiteClassContainer::Classifier)
    } else {
        Some(IrSiteClassContainer::Package(
            super::klib_types::package_name(declared.package().segments()),
        ))
    }
}
