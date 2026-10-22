//! What a type descriptor tells the link about the module interfaces it implements: one record
//! per implemented member number, holding the number, the type and the function at that number of
//! the type's interface table (see `native::interface_calls`).

use super::super::super::interface_calls::{implementations_section, RECORD_SIZE};
use super::*;

impl FileLowering<'_> {
    /// Record, for the link, which function stands at each number of `itable`, the interface
    /// table of the type `descriptor` describes: see `native::interface_calls`. A number the type
    /// does not implement holds the abstract-member trap only to keep the table dense, and is left
    /// out.
    pub(super) fn record_interface_implementations(
        &mut self,
        descriptor: DataId,
        base: &str,
        itable: &[FuncId],
    ) -> Result<(), Unsupported> {
        if itable.is_empty() {
            return Ok(());
        }
        let abstract_member = self.import("kt_abstract_method_called", &[], Ty::Unit)?;
        let implemented = itable
            .iter()
            .enumerate()
            .filter(|(_, function)| **function != abstract_member)
            .map(|(number, function)| (number as u32, *function))
            .collect::<Vec<_>>();
        if implemented.is_empty() {
            return Ok(());
        }
        let id = self.declare_local_data(&format!("kt_itfimpl_{base}"), false)?;
        let mut description = DataDescription::new();
        let mut contents = vec![0; implemented.len() * RECORD_SIZE as usize];
        for (record, &(number, _)) in contents
            .as_chunks_mut::<{ RECORD_SIZE as usize }>()
            .0
            .iter_mut()
            .zip(&implemented)
        {
            record[..8].copy_from_slice(&u64::from(number).to_le_bytes());
        }
        description.define(contents.into_boxed_slice());
        description.set_align(8);
        description.set_custom_section(&implementations_section(self.module_name));
        let ty = self
            .module
            .declare_data_in_data(descriptor, &mut description);
        for (index, &(_, function)) in implemented.iter().enumerate() {
            let at = (index as u64 * RECORD_SIZE) as u32;
            description.write_data_addr(at + 8, ty, 0);
            let function = self.module.declare_func_in_data(function, &mut description);
            description.write_function_addr(at + 16, function);
        }
        self.module
            .define_data(id, &description)
            .map_err(|error| format!("defining `kt_itfimpl_{base}` ({error})"))?;
        Ok(())
    }
}
