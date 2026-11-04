//! The exported wrapper around a public file-level function.
//!
//! The function itself keeps the module symbol (`kt_mod_<id>`, hidden). This wrapper is what a C
//! caller links, under the name the header declares. It runs the file's initializer first, so a
//! property defined here has its value even though the caller is not Kotlin.

use super::*;

impl<'a> FileLowering<'a> {
    /// Export exact dependency declaration identities as target symbols. The ordinary
    /// module-local entry remains unchanged; this narrow trampoline is the only bridge from a
    /// later module's provider-selected `Callee::External` to this file's executable body.
    pub(super) fn define_dependency_exports(
        &mut self,
        file_init: FuncId,
        exports: &HashMap<FunId, super::super::DependencyExportDefinition>,
    ) -> Result<(), Unsupported> {
        for (&function, export) in exports {
            self.define_dependency_export(file_init, function, export)?;
        }
        Ok(())
    }

    /// Export a source-module dependency body under its exact KLIB identity. Unlike a C source
    /// export, an instance member keeps its dispatch receiver as the first ABI operand.
    fn define_dependency_export(
        &mut self,
        file_init: FuncId,
        function: u32,
        export: &super::super::DependencyExportDefinition,
    ) -> Result<(), Unsupported> {
        let actual = crate::backend::DependencyFunctionSignature::from_ir(
            self.ir,
            function,
            export.member_receiver,
        )
        .map_err(Unsupported::from)?;
        if actual != export.signature {
            return Err("a dependency export changed its frozen function signature".into());
        }
        self.define_export_with_params(
            file_init,
            function,
            &export.symbol,
            export.signature.parameters().to_vec(),
        )?;
        let Some(symbol) = &export.default_entry else {
            return Ok(());
        };
        // The module default entry takes the declaration's operands and the omission masks, so
        // its export is the same trampoline over that entry.
        let Some(&(_, entry)) = self
            .module_default_entries
            .iter()
            .find(|(declared, _)| *declared == function)
        else {
            return Err(declined!("`{symbol}` has no module default entry"));
        };
        let mut params = export.signature.parameters().to_vec();
        params.extend(super::defaults::module_default_masks(params.len()));
        self.define_trampoline(file_init, entry, function, symbol, params)
    }

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
            self.define_export(file_init, *function, symbol)?;
        }
        Ok(())
    }

    fn define_export(
        &mut self,
        file_init: FuncId,
        function: u32,
        symbol: &str,
    ) -> Result<(), Unsupported> {
        let declared = &self.ir.functions[function as usize];
        let params = declared.params.clone();
        self.define_export_with_params(file_init, function, symbol, params)
    }

    fn define_export_with_params(
        &mut self,
        file_init: FuncId,
        function: u32,
        symbol: &str,
        params: Vec<Ty>,
    ) -> Result<(), Unsupported> {
        let Some(target) = self.functions[function as usize] else {
            return Err(declined!("`{symbol}` has no body"));
        };
        self.define_trampoline(file_init, target, function, symbol, params)
    }

    /// Export `symbol`, which runs the file's initializer and then calls `target` with its own
    /// operands, returning what `function` returns.
    fn define_trampoline(
        &mut self,
        file_init: FuncId,
        target: FuncId,
        function: u32,
        symbol: &str,
        params: Vec<Ty>,
    ) -> Result<(), Unsupported> {
        let ret = self.ir.functions[function as usize].ret;
        let signature = self.signature_of(&params, ret)?;
        let id = self
            .module
            .declare_function(symbol, Linkage::Export, &signature)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
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
