//! Publish only physical class descriptors this object has successfully defined.

use super::super::*;

impl FileLowering<'_> {
    pub(in super::super) fn record_published_descriptor(
        &mut self,
        class: ClassId,
        descriptor: DataId,
        publication: Option<(krusty::backend::KlibClassIdentity, String)>,
    ) -> Result<(), Unsupported> {
        let Some((identity, expected_symbol)) = publication else {
            return Ok(());
        };
        if self.dependency_signatures.classifier(class) != Some(&identity) {
            return Err(
                "a Native descriptor publication disagrees with its KLIB classifier".into(),
            );
        }
        let declaration = self.module.declarations().get_data_decl(descriptor);
        if !matches!(declaration.linkage, Linkage::Hidden | Linkage::Export) {
            return Err(
                "a planned KLIB descriptor publication without a linkable definition".into(),
            );
        }
        let symbol = declaration
            .name
            .as_ref()
            .ok_or("an exported KLIB classifier with an anonymous descriptor")?;
        if symbol != &expected_symbol {
            return Err("a Native descriptor publication names another physical definition".into());
        }
        self.class_descriptors.push((identity, expected_symbol));
        Ok(())
    }
}
