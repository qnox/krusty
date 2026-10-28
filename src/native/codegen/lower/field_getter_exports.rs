//! Export declaration-owned default getters without inventing common function identities.

use super::super::super::captures;
use super::*;

impl FileLowering<'_> {
    pub(super) fn define_field_getter_exports(
        &mut self,
        file_init: FuncId,
    ) -> Result<(), Unsupported> {
        let mut getters = self
            .field_getter_exports
            .values()
            .cloned()
            .collect::<Vec<_>>();
        getters.sort_unstable_by_key(|(getter, _)| (getter.class(), getter.field()));
        for (getter, export) in getters {
            let class = getter.class();
            let field = getter.field();
            let target = *self
                .accessors
                .get(&Slot::FieldGetter { class, field })
                .ok_or("a default getter export has no realized field accessor")?;
            let carried = captures::physical_ty(
                self.ir,
                class,
                field,
                self.ir.classes[class as usize].fields[field as usize].ty,
            );
            let result = export.signature.result();
            if &export.signature != getter.signature()
                || export.member_receiver != Some(getter.receiver())
            {
                return Err("a default getter export changed its declared ABI".into());
            }
            let signature = self.signature_of(export.signature.parameters(), result)?;
            let id = self
                .module
                .declare_function(&export.symbol, Linkage::Export, &signature)
                .map_err(|error| format!("declaring a default getter export ({error})"))?;
            self.emit_function(
                id,
                signature,
                result,
                &export.symbol,
                &mut |body, arguments| {
                    let init = body.func_ref(file_init);
                    body.emit_call(init, &[])?;
                    if body.terminated {
                        return Ok(());
                    }
                    let receiver = body
                        .convert(arguments[0], Some(getter.receiver().semantic_type()), any())?
                        .ok_or("a default getter export has no receiver value")?;
                    let target = body.func_ref(target);
                    let call = body.emit_call(target, &[receiver])?;
                    if body.terminated {
                        return Ok(());
                    }
                    let value = if body.carrier(carried) == Carrier::Void {
                        if body.carrier(result) != Carrier::Void {
                            return Err(
                                "a default getter export lost its declared result value".into()
                            );
                        }
                        None
                    } else {
                        let value = body.builder.inst_results(call)[0];
                        body.convert(value, Some(carried), result)?
                    };
                    if body.terminated {
                        return Ok(());
                    }
                    body.builder
                        .ins()
                        .return_(&value.into_iter().collect::<Vec<_>>());
                    body.terminate();
                    Ok(())
                },
            )?;
        }
        Ok(())
    }
}
