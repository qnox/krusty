//! Where the code of the dependency body being lowered stands, for a SAM conversion in it.

use super::Linker;
use crate::ir::IrSamPlacement;
use crate::metadata::id_signature::KlibDeclarationSignature;
use crate::metadata::klib_ir::KlibIrSignature;

use super::super::decline::KlibBodyDeclineReason;
use super::super::site_containers::site_container;

impl Linker<'_, '_> {
    /// The placement of the body being lowered: the container that names its local classifiers,
    /// or a member of a class private to its file, which the library does not name.
    pub(super) fn active_sam_placement(&self) -> Result<IrSamPlacement, KlibBodyDeclineReason> {
        if let &KlibDeclarationSignature::FileLocal { file, slot } =
            &self.active_identity.declaration
        {
            let signature = KlibIrSignature::FileLocal { file, slot };
            if self
                .sources
                .bodies
                .module_class_declaring(self.active_identity.module.as_str(), &signature)
                .is_some()
            {
                return Ok(IrSamPlacement::UnnamedDependencyClass);
            }
        }
        site_container(self.sources.bodies, &self.active_identity)
            .map(IrSamPlacement::Dependency)
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a dependency declaration whose container has no name".to_owned(),
                )
            })
    }
}
