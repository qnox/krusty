//! A value class one file of the module declares and another file boxes or unboxes.
//!
//! A value travels as itself, so a file that only passes one along needs nothing from the class.
//! A box is different: it is an object of the class, and only the declaring file knows the class's
//! descriptor and where in the object the value sits. That file therefore defines, for every value
//! class it declares, one entry point that boxes a value and one that reads it back, named from the
//! class's qualified name. Any other file boxes or unboxes through them. Their signatures come from
//! the class's semantic type, which every file of the module projects alike: the value goes in or
//! comes out at its carrier, and the box is a reference.
//!
//! A later source module of the program boxes the class the same way. The declaring file
//! publishes the two entry points by the class's exact KLIB identity, beside its descriptor, and
//! the later module's files find them there by the identity their classifier facts carry.

use super::super::*;
use krusty::types::TypeName;

/// What the box entry point takes and the unbox entry point answers: the value, at the carrier
/// every file carries `classifier` at.
fn value_ty(classifier: TypeName) -> Ty {
    Ty::obj_name(classifier)
}

impl FileLowering<'_> {
    /// Define the box and unbox entry points of every value class this file declares that another
    /// file of the module may box.
    pub(in super::super) fn define_value_box_entry_points(&mut self) -> Result<(), Unsupported> {
        for class in 0..self.ir.classes.len() as ClassId {
            let classifier = self.ir.classes[class as usize].fq_name;
            if !self.values.is_module_declared(classifier) {
                continue;
            }
            let (box_symbol, unbox_symbol) =
                super::super::super::super::symbols::module_value_box_symbols(
                    self.module_name,
                    classifier,
                );
            let value = value_ty(classifier);
            let signature = self.signature_of(&[value], any())?;
            let id = self
                .module
                .declare_function(&box_symbol, Linkage::Hidden, &signature)
                .map_err(|error| format!("declaring `{box_symbol}` ({error})"))?;
            self.emit_function(id, signature, any(), &box_symbol, &mut |body, params| {
                let object = body.box_value(params[0], classifier)?;
                body.builder.ins().return_(&[object]);
                body.terminate();
                Ok(())
            })?;
            let signature = self.signature_of(&[any()], value)?;
            let id = self
                .module
                .declare_function(&unbox_symbol, Linkage::Hidden, &signature)
                .map_err(|error| format!("declaring `{unbox_symbol}` ({error})"))?;
            self.emit_function(id, signature, value, &unbox_symbol, &mut |body, params| {
                let value = body.unbox_value(params[0], classifier)?;
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })?;
            if let Some(identity) = self.dependency_signatures.classifier(class) {
                self.value_boxes.push((
                    identity.clone(),
                    ValueBoxEntries {
                        box_symbol,
                        unbox_symbol,
                    },
                ));
            }
        }
        Ok(())
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Box a value of a value class another file declares, through the entry point it defines.
    pub(in super::super) fn entry_box_value(
        &mut self,
        value: Value,
        classifier: TypeName,
        entries: &ValueBoxEntries,
    ) -> Result<Value, Unsupported> {
        let id = self
            .file
            .import(&entries.box_symbol, &[value_ty(classifier)], any())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &[value])?;
        Ok(self.builder.inst_results(call)[0])
    }

    /// The value a box of a value class another file declares holds, read through the entry
    /// point that file defines.
    pub(in super::super) fn entry_unbox_value(
        &mut self,
        object: Value,
        classifier: TypeName,
        entries: &ValueBoxEntries,
    ) -> Result<Value, Unsupported> {
        let value = value_ty(classifier);
        let id = self.file.import(&entries.unbox_symbol, &[any()], value)?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &[object])?;
        Ok(self.builder.inst_results(call)[0])
    }
}
