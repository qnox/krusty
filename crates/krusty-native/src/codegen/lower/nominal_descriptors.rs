//! Runtime identities for dependency classes this program names but does not lay out.
//!
//! A check against a library class asks what its declaration identifies, whoever supplies the
//! objects: a class compiled from the library, or one the runtime provides under the same
//! qualified name. The descriptor is derived from the declaration alone, the same way for every
//! class, and the runtime treats a nominal descriptor as equal to any descriptor publishing that
//! qualified name. Source descriptors and already-published physical ones take priority.

use super::*;

fn descriptor_symbol(qualified: &str) -> String {
    let mut symbol = String::from("kt_klib_class_");
    for byte in qualified.bytes() {
        use std::fmt::Write as _;
        write!(symbol, "{byte:02x}").expect("writing to String");
    }
    symbol
}

impl FileLowering<'_> {
    /// Called only after the source, published and built-in descriptor paths have been checked.
    pub(super) fn nominal_descriptor(&mut self, ty: Ty) -> Result<Option<DataId>, Unsupported> {
        let Some(name) = ty.obj_internal() else {
            return Ok(None);
        };
        if name == krusty::types::wk::any() {
            return Ok(None);
        }
        let Some(fact) = self.classifiers.classifier(name) else {
            return Ok(None);
        };
        // An interface is a membership each implementor records, so it needs the implementors'
        // evidence as well; that is the interface identity descriptors' business.
        if fact.kind != krusty::libraries::TypeKind::Class || fact.source || fact.role.is_some() {
            return Ok(None);
        }
        if fact.klib_identity.is_none() {
            return Ok(None);
        }
        let Some(names) =
            krusty::backend::class_names::published_classifier(self.classifiers, name)
        else {
            return Ok(None);
        };
        let (Some(qualified), Some(simple)) = (names.qualified, names.simple) else {
            return Ok(None);
        };
        let symbol = descriptor_symbol(&qualified);
        if let Some(descriptor) = self.nominal_descriptors.get(&symbol) {
            return Ok(Some(*descriptor));
        }
        let descriptor = self.declare_reflection_object(Some(&symbol), &symbol)?;
        let mut bytes = vec![0; objects::ktype::SIZE];
        for (offset, value) in [
            (objects::ktype::NAME_LENGTH, qualified.len() as u32),
            (
                objects::ktype::QUALIFIED_NAME_LENGTH,
                qualified.len() as u32,
            ),
            (objects::ktype::SIMPLE_NAME_LENGTH, simple.len() as u32),
            (objects::ktype::CLASS_NAMES, 1),
            (objects::ktype::NOMINAL, 1),
        ] {
            bytes[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
        }
        let qualified_data = self.string_data(qualified.as_bytes())?;
        let simple_data = self.string_data(simple.as_bytes())?;
        let any = self.import_data("kt_type_any")?;
        self.define_reflection_object(
            descriptor,
            Some(&symbol),
            bytes,
            &[
                (objects::ktype::NAME, qualified_data),
                (objects::ktype::QUALIFIED_NAME, qualified_data),
                (objects::ktype::SIMPLE_NAME, simple_data),
                (objects::ktype::SUPER, any),
            ],
        )?;
        self.nominal_descriptors.insert(symbol, descriptor);
        Ok(Some(descriptor))
    }
}
