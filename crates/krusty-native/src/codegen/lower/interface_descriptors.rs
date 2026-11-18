//! Nominal runtime identities for selected external KLIB interfaces.
//!
//! A library interface need not have a materialized body or class layout. Its checked declaration
//! still identifies a type question and the memberships of an emitted implementation. These
//! immutable descriptors use the same exact identity in every object, independently of member
//! numbering. Existing source descriptors and already-published physical descriptors take priority.

use super::*;
use krusty::libraries::{KlibClassIdentity, TypeKind};

fn descriptor_symbol(identity: &KlibClassIdentity) -> String {
    let mut symbol = String::from("kt_klib_interface_");
    for byte in krusty::backend::interface_identity::classifier_identity(identity) {
        use std::fmt::Write as _;
        write!(symbol, "{byte:02x}").expect("writing to String");
    }
    symbol
}

impl FileLowering<'_> {
    /// Called only after local/source, published and built-in descriptor paths have been checked.
    pub(super) fn dependency_interface_descriptor(
        &mut self,
        ty: Ty,
    ) -> Result<Option<DataId>, Unsupported> {
        let Some(name) = ty.obj_internal() else {
            return Ok(None);
        };
        // Any is the genuine erased bound, not a nominal interface whose evidence was lost.
        if name == krusty::types::wk::any() {
            return Ok(None);
        }
        let fact = self
            .classifiers
            .classifier(name)
            .ok_or("a nominal runtime type without frozen classifier facts")?;
        if fact.kind != TypeKind::Interface || fact.source || fact.role.is_some() {
            return Ok(None);
        }
        let identity = fact
            .klib_identity
            .as_ref()
            .ok_or("a selected dependency interface without its KLIB declaration identity")?;
        if let Some(descriptor) = self.dependency_interface_descriptors.get(identity) {
            return Ok(Some(*descriptor));
        }
        let names = krusty::backend::class_names::published_classifier(self.classifiers, name)
            .ok_or("a selected dependency interface without its declaration name facts")?;
        let qualified = names
            .qualified
            .ok_or("a selected dependency interface without its qualified name")?;
        let simple = names
            .simple
            .ok_or("a selected dependency interface without its simple name")?;
        let symbol = descriptor_symbol(identity);
        // The existing shared immutable-data writer puts this in a COMDAT group. Separate
        // compilations may both require the same external interface without emitting its body.
        let descriptor = self.declare_reflection_object(Some(&symbol), &symbol)?;
        let mut bytes = vec![0; objects::ktype::SIZE];
        for (offset, value) in [
            (objects::ktype::NAME_LENGTH, qualified.len() as u32),
            (
                objects::ktype::QUALIFIED_NAME_LENGTH,
                qualified.len() as u32,
            ),
            (objects::ktype::SIMPLE_NAME_LENGTH, simple.len() as u32),
            (objects::ktype::CLASS_NAMES, 1), // non-local declaration, as for source interfaces
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
        self.dependency_interface_descriptors
            .insert(identity.clone(), descriptor);
        self.class_descriptors.push((identity.clone(), symbol));
        Ok(Some(descriptor))
    }

    /// Non-numbered dependency interfaces do not appear in `model.implemented`. Retain their
    /// nominal identities alongside that existing numbered closure and the built-in markers.
    /// A class below a library superclass (a coroutine frame below the library's continuation
    /// implementation) is an instance of the interfaces that superclass's published supertypes
    /// reach, too.
    pub(super) fn dependency_interface_memberships(
        &mut self,
        class: ClassId,
        local_interfaces: &[ClassId],
    ) -> Result<Vec<DataId>, Unsupported> {
        let declared = &self.ir.classes[class as usize];
        let inherited = self.classifiers.inherited_interfaces(declared.fq_name_id());
        let mut pending = declared
            .interfaces
            .iter()
            .chain(
                local_interfaces
                    .iter()
                    .flat_map(|interface| self.ir.classes[*interface as usize].interfaces.iter()),
            )
            .chain(inherited)
            .filter(|name| self.ir.class_id_by_name(*name).is_none())
            .collect::<Vec<_>>();
        let mut visited = krusty::fx_hash::HashSet::default();
        let mut descriptors = Vec::new();
        while let Some(name) = pending.pop() {
            if !visited.insert(name) {
                continue;
            }
            let fact = self
                .classifiers
                .classifier(name)
                .ok_or("an implemented dependency interface without frozen classifier facts")?;
            if !fact.is_interface() {
                return Err("an implemented dependency interface with a non-interface kind".into());
            }
            // Function and mapped-collection roles retain their existing semantic/physical
            // markers below; nominal descriptors must not replace that representation contract.
            if fact.role.is_none() {
                let descriptor = self
                    .type_descriptor(Ty::obj_name(name))?
                    .ok_or("an implemented dependency interface without a runtime descriptor")?;
                if !descriptors.contains(&descriptor) {
                    descriptors.push(descriptor);
                }
            }
            for parent in fact.supertypes.iter().copied() {
                // Interfaces' ordinary Any supertype is not an interface membership.
                let parent_fact = self
                    .classifiers
                    .classifier(parent)
                    .ok_or("a dependency interface supertype without frozen classifier facts")?;
                if parent_fact.is_interface() {
                    pending.push(parent);
                }
            }
        }
        Ok(descriptors)
    }
}
