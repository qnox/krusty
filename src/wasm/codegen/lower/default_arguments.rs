//! Wasm realization of checked calls with omitted source parameters.
//!
//! Each top-level function that declares a default gets one mask entry beside its ordinary entry.
//! The entry owns evaluation because a default may read an earlier parameter and because a caller
//! in another file or source module does not own the declaration's default-expression IR.

use crate::fx_hash::{HashMap, HashSet};

use super::*;
use crate::wasm::encode::HeapType;

/// Define every default mask entry declared for this source file.
pub(super) fn define(
    context: &FileContext<'_>,
    module: &mut Module,
    runtime: &mut Runtime,
) -> Result<(), Unsupported> {
    for (function, entry) in context.default_functions.iter().copied().enumerate() {
        let Some(entry) = entry else { continue };
        let function = function as FunId;
        let declaration = &context.ir.functions[function as usize];
        let defaults = context
            .ir
            .param_defaults(function)
            .ok_or_else(|| "a Wasm default entry without recorded defaults".to_string())?;
        let masks = declaration.params.len().div_ceil(32);
        let mut physical = declaration
            .params
            .iter()
            .map(|parameter| carrier(*parameter)?.ok_or_else(|| "a `Unit` parameter".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        physical.extend(std::iter::repeat_n(ValType::I32, masks));
        let results = carrier(declaration.ret)?.into_iter().collect::<Vec<_>>();
        let signature = module.func_type(physical, results);

        let mut declared = HashMap::default();
        for expression in defaults.iter().flatten() {
            declared.extend(context.ir.declared_value_slot_types(*expression));
        }
        let mut body = Body::with_physical_parameters(
            context,
            module,
            runtime,
            &declaration.params,
            declaration.params.len() + masks,
            declared,
            declaration.ret,
        );
        for (ordinal, default) in defaults.iter().enumerate() {
            let Some(default) = default else { continue };
            let mask = declaration.params.len() as u32 + (ordinal / 32) as u32;
            let bit = 1_i32.wrapping_shl((ordinal % 32) as u32);
            body.code
                .local_get(mask)
                .i32_const(bit)
                .op(AND32)
                .if_(BlockType::Empty);
            body.open();
            body.value(*default, declaration.params[ordinal])?;
            body.code.local_set(ordinal as u32);
            body.close();
        }
        for ordinal in 0..declaration.params.len() {
            body.code.local_get(ordinal as u32);
        }
        let target = context.functions[function as usize]
            .ok_or_else(|| "a Wasm default entry whose function was not emitted".to_string())?;
        body.code.call(target);
        let Body { code, locals, .. } = body;
        module.define(
            entry,
            Function {
                ty: signature,
                locals,
                code,
            },
        );
    }
    Ok(())
}

impl Body<'_> {
    /// Call a mask entry after materializing the declaration's full parameter frame.
    pub(super) fn defaulted_call(
        &mut self,
        entry: u32,
        params: &[Ty],
        omitted: &[u32],
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let omitted_count = omitted.len();
        let omitted = omitted.iter().copied().collect::<HashSet<_>>();
        if omitted.len() != omitted_count
            || omitted
                .iter()
                .any(|ordinal| *ordinal as usize >= params.len())
            || args.len() + omitted.len() != params.len()
        {
            return Err("a defaulted call whose arguments do not match its parameters".to_string());
        }
        let mut supplied = args.iter();
        for (ordinal, parameter) in params.iter().copied().enumerate() {
            if omitted.contains(&(ordinal as u32)) {
                push_zero(
                    &mut self.code,
                    carrier(parameter)?
                        .ok_or_else(|| "a defaulted call with a `Unit` parameter".to_string())?,
                );
            } else {
                let argument = supplied
                    .next()
                    .ok_or_else(|| "a missing supplied default-call argument".to_string())?;
                self.value(*argument, parameter)?;
            }
        }
        for word in 0..params.len().div_ceil(32) {
            let mut mask = 0_u32;
            for ordinal in &omitted {
                if (*ordinal as usize) / 32 == word {
                    mask |= 1_u32 << ((*ordinal as usize) % 32);
                }
            }
            self.code.i32_const(mask as i32);
        }
        self.code.call(entry);
        Ok(())
    }
}

pub(super) fn push_zero(code: &mut Code, carrier: ValType) {
    match carrier {
        ValType::I32 => code.i32_const(0),
        ValType::I64 => code.i64_const(0),
        ValType::F32 => code.f32_const(0.0),
        ValType::F64 => code.f64_const(0.0),
        ValType::Ref { .. } => code.ref_null(HeapType::None),
    };
}
