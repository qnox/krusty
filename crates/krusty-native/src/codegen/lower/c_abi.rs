//! The exported wrapper around a public file-level function.
//!
//! The function itself keeps the module symbol (`kt_mod_<id>`, hidden). This wrapper is what a C
//! caller links, under the name the header declares. It runs the file's initializer first, so a
//! property defined here has its value even though the caller is not Kotlin.

use super::*;

impl<'a> FileLowering<'a> {
    /// Reuse the normal initialization/pending-exception trampoline for dev-only checked entries.
    #[cfg(feature = "test-support")]
    pub(super) fn driver_records(
        &self,
        entries: &[super::super::DriverEntry],
        symbols: &mut krusty::fx_hash::HashSet<String>,
    ) -> Result<Vec<super::super::super::c_abi::Record>, Unsupported> {
        let mut records = Vec::new();
        for entry in entries {
            let Some(&function) = self.ir.checked_callable_functions.get(&entry.callable) else {
                continue; // This file does not declare the selected function.
            };
            if !super::super::super::c_abi::is_file_level(self.ir, function as usize)
                || self.ir.suspend_funs.contains(&function)
                || self
                    .ir
                    .signatures
                    .get(&function)
                    .is_some_and(|s| !s.type_params.is_empty())
                || self.ir.functions[function as usize]
                    .params
                    .iter()
                    .any(|ty| self.carrier(*ty) != Carrier::Ref)
            {
                return Err("a driver entry without a plain reference-argument frame".into());
            }
            let result = match self.carrier(self.ir.functions[function as usize].ret) {
                Carrier::Ref => "kt_ref",
                Carrier::Void => "void",
                Carrier::Scalar(..) => return Err("a driver entry with a scalar result".into()),
            };
            if !super::super::super::c_abi::valid_driver_symbol(&entry.symbol)
                || !symbols.insert(entry.symbol.clone())
            {
                return Err("an invalid or duplicate driver entry symbol".into());
            }
            let parameters = self.ir.functions[function as usize]
                .params
                .iter()
                .map(|_| "kt_ref")
                .collect::<Vec<_>>();
            let parameters = if parameters.is_empty() {
                "void".into()
            } else {
                parameters.join(", ")
            };
            records.push(super::super::super::c_abi::Record::Declaration {
                function,
                symbol: entry.symbol.clone(),
                prototype: format!("{result} {}({parameters})", entry.symbol),
            });
        }
        Ok(records)
    }

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
        let actual = krusty::backend::DependencyFunctionSignature::from_ir(
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
        for alias in &export.aliases {
            self.define_export_with_params(
                file_init,
                function,
                alias,
                export.signature.parameters().to_vec(),
            )?;
        }
        if let Some(symbol) = &export.dispatch_entry {
            // An ordinary call from another module runs the receiver's override.
            let Some(entry) = self
                .ir
                .dispatch_entries()
                .get(&function)
                .and_then(|&entry| self.functions[entry as usize])
            else {
                return Err(declined!("`{symbol}` has no dispatching entry"));
            };
            self.define_trampoline(
                file_init,
                entry,
                function,
                symbol,
                export.signature.parameters().to_vec(),
            )?;
        }
        let Some(symbol) = &export.default_entry else {
            return Ok(());
        };
        // The default-argument entry takes the declaration's operands and the omission masks, so
        // its export is the same trampoline over that entry.
        let Some(entry) = krusty::backend::default_arguments::published_entry(self.ir, function)
            .and_then(|entry| self.functions[entry as usize])
        else {
            return Err(declined!("`{symbol}` has no default-argument entry"));
        };
        // One mask word per 32 of the declaration's own parameters: a member's receiver, which
        // leads the export's operands, is not one of them.
        // A suspend declaration's continuation is not one either.
        let coroutine = export.signature.has_coroutine_abi();
        let declared = self.ir.functions[function as usize].params.len() - usize::from(coroutine);
        let params = krusty::backend::default_arguments::entry_frame(
            export.signature.parameters(),
            coroutine,
            std::iter::repeat_n(
                Ty::Int,
                krusty::backend::default_arguments::mask_words(declared),
            ),
        );
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
