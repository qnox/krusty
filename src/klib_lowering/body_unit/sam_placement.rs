//! Where the code of the dependency body being lowered stands, for a SAM conversion in it.

use super::Linker;
use crate::ir::IrSamPlacement;
use crate::metadata::id_signature::{KlibDeclarationSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::KlibIrSignature;

use super::super::decline::KlibBodyDeclineReason;
use super::super::klib_types::{member_owner, package_name};

impl Linker<'_, '_> {
    /// The placement of the body being lowered: a member of the classifier its public identity
    /// names, the level of the file a top-level declaration stands in, or a member of a class
    /// private to its file, which the library does not name.
    pub(super) fn active_sam_placement(&self) -> Result<IrSamPlacement, KlibBodyDeclineReason> {
        let module = self.active_identity.module.as_str();
        match &self.active_identity.declaration {
            KlibDeclarationSignature::Public(signature) => public_placement(signature),
            KlibDeclarationSignature::Accessor(accessor) => public_placement(accessor.property()),
            &KlibDeclarationSignature::FileLocal { file, slot } => {
                let signature = KlibIrSignature::FileLocal { file, slot };
                if self
                    .sources
                    .bodies
                    .module_class_declaring(module, &signature)
                    .is_some()
                {
                    return Ok(IrSamPlacement::UnnamedDependencyClass);
                }
                let package = self
                    .sources
                    .bodies
                    .module_file_package(module, file)
                    .ok_or_else(|| {
                        KlibBodyDeclineReason::SignatureMismatch(
                            "a file-local declaration names no serialized file".to_owned(),
                        )
                    })?;
                Ok(IrSamPlacement::DependencyFile {
                    package: package_name(package)?,
                })
            }
        }
    }
}

/// A declaration with the public identity `signature`: a member of the classifier its declaration
/// path names, or a top-level declaration of its package.
fn public_placement(
    signature: &KlibPublicIdSignature,
) -> Result<IrSamPlacement, KlibBodyDeclineReason> {
    Ok(match signature.declaration().segments() {
        [_] => IrSamPlacement::DependencyFile {
            package: package_name(signature.package().segments())?,
        },
        _ => IrSamPlacement::DependencyMember {
            owner: member_owner(signature)?,
        },
    })
}
