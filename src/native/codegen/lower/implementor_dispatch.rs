//! Dispatch for dependency members whose receiver may be implemented by a class in this file.
//!
//! Runtime entry points only understand runtime-owned objects. These operations test each known
//! source implementor once and enter its own slot before falling through to the runtime shape.

use super::*;

/// A class of this file that implements a dependency declaration, and the slot it does it in.
pub(super) struct ImplementorArm {
    class: ClassId,
    slot: u32,
    params: Vec<ParameterBoundary>,
    ret: Ty,
}

/// One parameter of an implementor's arm, at both of its types. The call site hands the operand to
/// the DEPENDENCY declaration, whose parameter may be a type parameter the implementor fixes:
/// `Comparable<II>.compareTo` takes a `T`, carried as a reference, where `II.compareTo` takes the
/// `II` value. The operand crosses that declared boundary first and becomes the implementation's
/// type from there, as it would through the bridge a virtual call reaches.
struct ParameterBoundary {
    declared: Ty,
    implementation: Ty,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// The slot that implements one exact dependency declaration in `class`, each parameter paired
    /// with `declared`, the parameter types the dependency declaration states.
    ///
    /// Common IR already joined an override to its provider-owned callable identity. Consuming that
    /// edge keeps same-name/same-arity overloads distinct and avoids repeating overload selection in
    /// the backend.
    pub(super) fn external_override_slot(
        &self,
        class: ClassId,
        overridden: crate::fir::ExternalCallableId,
        declared: &[Ty],
    ) -> Option<ImplementorArm> {
        let ir = self.file.ir;
        let class_name = ir.classes[class as usize].fq_name;
        let edge = ir
            .function_overrides
            .get(&class_name)?
            .iter()
            .find(|edge| {
                edge.overridden == crate::fir::ResolvedFunctionOverrideTarget::External(overridden)
            })?;
        let function = edge.implementation_function.or_else(|| {
            let crate::fir::ResolvedFunctionOverrideTarget::Module(callable) = edge.implementation
            else {
                return None;
            };
            ir.checked_callable_functions.get(&callable).copied()
        })?;
        let owner = ir.class_id_by_name(edge.implementation_owner)?;
        let key = super::super::super::classes::function_key(ir, owner, function);
        let slot = self.file.model.slot(class, &key)?;
        let declaration = &ir.functions[function as usize];
        // An override takes the parameters of what it overrides, so a count that differs is a
        // declaration this pairing does not describe.
        if declaration.params.len() != declared.len() {
            return None;
        }
        let params = declared
            .iter()
            .zip(&declaration.params)
            .map(|(&declared, &implementation)| ParameterBoundary {
                declared,
                implementation,
            })
            .collect();
        Some(ImplementorArm {
            class,
            slot,
            params,
            ret: declaration.ret,
        })
    }

    pub(super) fn dispatch_by_implementor_with(
        &mut self,
        implementors: &[ImplementorArm],
        object: Value,
        arguments: &[(Value, Option<Ty>)],
        answer: Ty,
        runtime: impl FnOnce(&mut Self, Value) -> Result<Option<Value>, Unsupported>,
    ) -> Result<Option<Value>, Unsupported> {
        let merge = self.builder.create_block();
        let carried = self.carrier(answer);
        if let Some(clif) = carried.clif() {
            self.builder.append_block_param(merge, clif);
        }
        for arm in implementors {
            let descriptor = self.file.classes[arm.class as usize].descriptor;
            let type_address = self.data_address(descriptor);
            let matches = self
                .runtime_call(
                    "kt_is_instance",
                    &[any(), any()],
                    Ty::Boolean,
                    &[object, type_address],
                )?
                .expect("`kt_is_instance` returns a Boolean");
            let mine = self.builder.create_block();
            let rest = self.builder.create_block();
            self.builder.ins().brif(matches, mine, &[], rest, &[]);

            self.continue_in(mine);
            self.builder.seal_block(mine);
            let mut operands = Vec::with_capacity(arguments.len());
            for (&(value, source), boundary) in arguments.iter().zip(&arm.params) {
                let Some(crossing) = self.convert(value, source, boundary.declared)? else {
                    return Ok(None);
                };
                let Some(converted) =
                    self.convert(crossing, Some(boundary.declared), boundary.implementation)?
                else {
                    return Ok(None);
                };
                operands.push(converted);
            }
            let params: Vec<Ty> = arm.params.iter().map(|p| p.implementation).collect();
            let produced = self.dispatch(object, arm.slot, &params, arm.ret, &operands)?;
            let produced = match produced {
                Some(value) => self.convert(value, Some(arm.ret), answer)?,
                None => self.unit_where_wanted(answer)?,
            };
            self.jump_to_merge(merge, carried, produced);
            self.continue_in(rest);
            self.builder.seal_block(rest);
        }
        let produced = runtime(self, object)?;
        self.jump_to_merge(merge, carried, produced);

        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        Ok(carried.clif().map(|_| self.builder.block_params(merge)[0]))
    }

    fn unit_where_wanted(&mut self, answer: Ty) -> Result<Option<Value>, Unsupported> {
        if self.terminated || self.carrier(answer) != Carrier::Ref {
            return Ok(None);
        }
        self.runtime_call("kt_unit", &[], any(), &[])
    }

    fn jump_to_merge(&mut self, merge: Block, carried: Carrier, produced: Option<Value>) {
        if self.terminated {
            return;
        }
        match (carried.clif(), produced) {
            (Some(_), Some(value)) => {
                self.builder.ins().jump(merge, &[BlockArg::Value(value)]);
            }
            (Some(clif), None) => {
                let filler = match clif {
                    types::F32 => self.builder.ins().f32const(0.0),
                    types::F64 => self.builder.ins().f64const(0.0),
                    integer => self.builder.ins().iconst(integer, 0),
                };
                self.builder.ins().jump(merge, &[BlockArg::Value(filler)]);
            }
            (None, _) => {
                self.builder.ins().jump(merge, &[]);
            }
        }
    }
}
