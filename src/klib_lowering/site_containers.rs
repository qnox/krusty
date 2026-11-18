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

/// The name Kotlin's IR gives a class `module` itself declares: its container, then its serialized
/// name. The container comes from the class's own identity, never from its constructor's, which
/// ends one segment deeper.
pub(super) fn class_site_name(
    bodies: &KlibDeclarationBodies,
    module: &str,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
) -> Option<crate::ir::IrSiteClassName> {
    Some(crate::ir::IrSiteClassName {
        container: class_container(bodies, module, &class.base.symbol.signature)?,
        name: class.name.as_str().into(),
    })
}

/// The container that names a class `module` itself declares under `signature`: its file's
/// package for a file-local class, otherwise the package or classifier its public path nests in.
/// `None` for an accessor, which names no class, or when the file's package is unknown.
pub(super) fn class_container(
    bodies: &KlibDeclarationBodies,
    module: &str,
    signature: &crate::metadata::klib_ir::KlibIrSignature,
) -> Option<IrSiteClassContainer> {
    use crate::metadata::klib_ir::KlibIrSignature;
    match signature {
        KlibIrSignature::FileLocal { file, .. } => {
            let package = bodies.module_file_package(module, *file)?;
            Some(IrSiteClassContainer::Package(
                super::klib_types::package_name(package),
            ))
        }
        KlibIrSignature::Public(public) => {
            if public.declaration().segments().len() > 1 {
                super::klib_types::member_owner(public)
                    .ok()
                    .map(IrSiteClassContainer::Classifier)
            } else {
                Some(IrSiteClassContainer::Package(
                    super::klib_types::package_name(public.package().segments()),
                ))
            }
        }
        KlibIrSignature::Accessor(_) => None,
    }
}
