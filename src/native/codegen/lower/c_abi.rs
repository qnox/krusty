//! The exported wrapper around a public file-level function.
//!
//! The function itself keeps the module symbol (`kt_mod_<id>`, hidden). This wrapper is what a C
//! caller links, under the name the header declares. It runs the file's initializer first, so a
//! property defined here has its value even though the caller is not Kotlin.

use super::*;

impl<'a> FileLowering<'a> {
    pub(super) fn define_c_exports(
        &mut self,
        file_init: FuncId,
        records: &[super::super::super::c_abi::Record],
    ) -> Result<(), Unsupported> {
        for record in records {
            let super::super::super::c_abi::Record::Declaration {
                function, symbol, ..
            } = record
            else {
                continue;
            };
            self.define_c_export(file_init, *function, symbol)?;
        }
        Ok(())
    }

    fn define_c_export(
        &mut self,
        file_init: FuncId,
        function: u32,
        symbol: &str,
    ) -> Result<(), Unsupported> {
        let declared = &self.ir.functions[function as usize];
        let params = declared.params.clone();
        let ret = declared.ret;
        let signature = self.signature_of(&params, ret)?;
        let id = self
            .module
            .declare_function(symbol, Linkage::Export, &signature)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
        let Some(target) = self.functions[function as usize] else {
            return Err(declined!("`{symbol}` has no body"));
        };
        self.emit_function(id, signature, ret, symbol, &mut |body, arguments| {
            let init = body.func_ref(file_init);
            body.emit_call(init, &[])?;
            if body.terminated {
                return Ok(());
            }
            let target = body.func_ref(target);
            let call = body.emit_call(target, arguments)?;
            if body.terminated || body.carrier(ret) == Carrier::Void {
                return Ok(());
            }
            let result = body.builder.inst_results(call)[0];
            body.builder.ins().return_(&[result]);
            body.terminate();
            Ok(())
        })
    }
}
