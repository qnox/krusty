//! Publish only physical class descriptors this object has successfully defined.

use super::super::*;

impl FileLowering<'_> {
    pub(in super::super) fn record_published_descriptor(
        &mut self,
        class: ClassId,
        descriptor: DataId,
    ) -> Result<(), Unsupported> {
        let Some(identity) = self.dependency_signatures.classifier(class) else {
            return Ok(());
        };
        let declaration = self.module.declarations().get_data_decl(descriptor);
        if !matches!(declaration.linkage, Linkage::Hidden | Linkage::Export) {
            return Err(
                "an exported KLIB classifier without a linkable descriptor definition".into(),
            );
        }
        let symbol = declaration
            .name
            .as_ref()
            .ok_or("an exported KLIB classifier with an anonymous descriptor")?;
        self.class_descriptors
            .push((identity.clone(), symbol.clone()));
        Ok(())
    }
}
