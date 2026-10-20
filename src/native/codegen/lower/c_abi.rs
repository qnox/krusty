//! The exported wrapper around a public file-level function.
//!
//! The function itself keeps the module symbol (`kt_mod_<id>`, hidden). This wrapper is what a C
//! caller links, under the name the header declares. It runs the file's initializer first, so a
//! property defined here has its value even though the caller is not Kotlin.

use super::*;
use crate::types::TyKind;

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
            self.define_dependency_export(
                file_init,
                function,
                &export.symbol,
                export
                    .member_receiver
                    .map(|receiver| receiver.semantic_type()),
            )?;
        }
        Ok(())
    }

    /// Export a source-module dependency body under its exact KLIB identity. Unlike a C source
    /// export, an instance member keeps its dispatch receiver as the first ABI operand.
    fn define_dependency_export(
        &mut self,
        file_init: FuncId,
        function: u32,
        symbol: &str,
        member_receiver: Option<Ty>,
    ) -> Result<(), Unsupported> {
        let declared = &self.ir.functions[function as usize];
        let mut params =
            Vec::with_capacity(declared.params.len() + usize::from(member_receiver.is_some()));
        match (member_receiver.map(Ty::kind), declared.dispatch_receiver) {
            (Some(TyKind::Obj(receiver, arguments)), Some(owner))
                if receiver == owner && arguments.is_empty() =>
            {
                params.push(member_receiver.expect("matched a member receiver"));
            }
            (None, None) => {}
            _ => {
                return Err(declined!(
                    "a dependency export whose member receiver disagrees with its body"
                ))
            }
        }
        params.extend(declared.params.iter().copied());
        self.define_export_with_params(file_init, function, symbol, params)
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
        let declared = &self.ir.functions[function as usize];
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
