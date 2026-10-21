//! An `object` one file of the module declares and another file uses.
//!
//! Only the declaring file holds the object's descriptor, layout and lazily filled instance slot,
//! so it defines one entry point per object, named from the object's qualified name, and every
//! other file reaches the instance through it.

use super::super::*;
use crate::types::TypeName;

impl FileLowering<'_> {
    /// Define, for every `object` this file declares, the entry point another file of the module
    /// reaches the instance through. Like a constructor's entry point it runs this file's
    /// top-level initializer first, so the object's initializer sees this file's state as a use
    /// here would, and then answers the instance through the file's own getter.
    pub(in super::super) fn define_singleton_entry_points(
        &mut self,
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        if self.unit_kind != crate::backend::CheckedIrUnitKind::Source {
            return Ok(());
        }
        for class in 0..self.ir.classes.len() as ClassId {
            let Some((_, getter)) = self.classes[class as usize].singleton else {
                continue;
            };
            let symbol = super::super::super::super::symbols::module_singleton_symbol(
                self.module_name,
                self.ir.classes[class as usize].fq_name_id(),
            );
            let signature = self.signature_of(&[], any())?;
            let id = self
                .module
                .declare_function(&symbol, Linkage::Hidden, &signature)
                .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
            self.emit_function(id, signature, any(), &symbol, &mut |body, _| {
                let init = body.func_ref(file_init);
                body.emit_call(init, &[])?;
                if body.terminated {
                    return Ok(());
                }
                let getter = body.func_ref(getter);
                let call = body.emit_call(getter, &[])?;
                let instance = body.builder.inst_results(call)[0];
                body.builder.ins().return_(&[instance]);
                body.terminate();
                Ok(())
            })?;
        }
        Ok(())
    }
}

impl BodyLowering<'_, '_, '_> {
    /// The instance of an `object` another file of the module declares, through that file's entry
    /// point, or `None` when `classifier` is no such object.
    pub(in super::super) fn module_singleton(
        &mut self,
        classifier: TypeName,
    ) -> Result<Option<Value>, Unsupported> {
        let declared_elsewhere = self.file.ir.class_id_by_name(classifier).is_none()
            && self
                .file
                .ir
                .referenced_module_classifiers
                .get(&classifier)
                .is_some_and(|record| record.singleton);
        if !declared_elsewhere {
            return Ok(None);
        }
        let symbol = super::super::super::super::symbols::module_singleton_symbol(
            self.file.module_name,
            classifier,
        );
        let id = self.file.import(&symbol, &[], any())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &[])?;
        Ok(Some(self.builder.inst_results(call)[0]))
    }
}
