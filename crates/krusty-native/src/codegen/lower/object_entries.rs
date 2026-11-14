//! Native object-table entries whose signatures adapt another declaration or representation.

use super::*;

/// What a converting bridge forwards to.
#[derive(Clone, Copy)]
pub(super) enum BridgeTarget {
    /// The receiver's own table, at this slot.
    Slot(u32),
    /// This method of the file, called outright.
    Function(krusty::ir::FunId),
}

impl FileLowering<'_> {
    /// A bridge: the base's signature in, the override's out, and a dispatch between them.
    ///
    /// `A<T : Number>.foo(): T` erases its result to a reference and `Z : A<Int>` returns an
    /// unboxed integer, so the base's slot cannot hold `Z`'s body — a caller reading the slot
    /// through `A` would read an integer as a pointer. This stands there instead: it takes what the
    /// BASE declares, converts each operand to what the override's slot expects, and converts the
    /// answer back.
    ///
    /// It forwards by DISPATCH and not by calling the override, which is what keeps it right under
    /// a further subclass: `Y : Z` replaces the target slot with its own body, and this reaches
    /// whatever the receiver actually is rather than the override that happened to need the bridge.
    /// The FUNCTION SLOT's stand-in: `kotlin.Function{N}.invoke`'s signature, converted onto the
    /// override's own and dispatched through its slot; see [`Slot::FunctionBridge`].
    ///
    /// Apart from [`Self::define_bridge`] only in where the signature it WEARS comes from. That
    /// one reads a base declaration of this file; there is none here, because `kotlin.Function{N}`
    /// is declared in no file this target compiles — so the signature is written out, which is the
    /// one every function value shares: references throughout.
    pub(super) fn define_function_bridge(
        &mut self,
        arity: usize,
        target_slot: u32,
        target: krusty::ir::FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let params = vec![any(); arity + 1];
        let signature = self.signature_of(&params, any())?;
        let forwarded = super::super::super::captures::carried_parameters(self.ir, target);
        let forwarded_ret = self.ir.functions[target as usize].ret;
        if forwarded.len() != arity {
            return Err(declined!(
                "a function-slot bridge to `{}`, which takes {} of {arity} operands",
                self.ir.functions[target as usize].name,
                forwarded.len()
            ));
        }
        let name = format!(
            "function bridge to `{}`",
            self.ir.functions[target as usize].name
        );
        self.emit_function(id, signature, any(), &name, &mut |body, values| {
            let mut arguments = Vec::with_capacity(forwarded.len());
            for (index, &want) in forwarded.iter().enumerate() {
                // From the REFERENCE the caller passed: a function type's operands are boxed, and
                // this is the same unboxing the uniform lambda entry point makes.
                let Some(value) = body.convert(values[index + 1], Some(any()), want)? else {
                    return Err("a `Unit` operand crossing the function slot".into());
                };
                arguments.push(value);
            }
            let answer = body.dispatch(
                values[0],
                target_slot,
                &forwarded,
                forwarded_ret,
                &arguments,
            )?;
            let answer = match answer {
                Some(answer) => body.convert(answer, Some(forwarded_ret), any())?,
                None => None,
            };
            let answer = match answer {
                Some(answer) => answer,
                // A `Unit` body answering a caller that reads a reference: the runtime owns that
                // singleton, and it is the Kotlin value such a call gets back.
                None => body
                    .runtime_call("kt_unit", &[], any(), &[])?
                    .expect("`kt_unit` returns the singleton"),
            };
            body.builder.ins().return_(&[answer]);
            body.terminate();
            Ok(())
        })
    }

    pub(super) fn define_bridge(
        &mut self,
        declared: krusty::ir::FunId,
        target_slot: Option<u32>,
        target: krusty::ir::FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let base = self.ir.functions[declared as usize].clone();
        let carried = super::super::super::captures::carried_parameters(self.ir, declared);
        let forwarded = super::super::super::captures::carried_parameters(self.ir, target);
        let forwarded_ret = self.ir.functions[target as usize].ret;
        // Through the slot where there is one, so a further subclass's override is reached
        // through the same base. An INTERFACE's own bridge has none — see `Slot::Bridge` — and
        // calls the default body outright, which is where it is the only implementation.
        let target = match target_slot {
            Some(slot) => BridgeTarget::Slot(slot),
            None => BridgeTarget::Function(target),
        };
        self.define_converting_bridge(
            (&carried, base.ret),
            (&forwarded, forwarded_ret),
            target,
            &format!("bridge to `{}`", base.name),
            id,
        )
    }

    /// The bridge [`Self::define_bridge`] writes, between two signatures given by their types:
    /// `declared`'s operands in (after the receiver), each converted to `implemented`'s, the
    /// call to `target`, and its answer converted back. A member of a dependency's interface an
    /// override carries differently (`next(): Int` implementing `Iterator<T>.next(): T`) stands
    /// in the type's records as one dispatching through the override's slot.
    pub(super) fn define_converting_bridge(
        &mut self,
        declared: (&[Ty], Ty),
        implemented: (&[Ty], Ty),
        target: BridgeTarget,
        name: &str,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (carried, result) = declared;
        let (forwarded, forwarded_ret) = implemented;
        let mut params = vec![any()];
        params.extend(carried.iter().copied());
        let signature = self.signature_of(&params, result)?;
        self.emit_function(id, signature, result, name, &mut |body, values| {
            let mut arguments = Vec::with_capacity(forwarded.len());
            for (index, &want) in forwarded.iter().enumerate() {
                let have = carried.get(index).copied();
                let Some(value) = body.convert(values[index + 1], have, want)? else {
                    return Err("a `Unit` operand crossing a bridge".into());
                };
                arguments.push(value);
            }
            let answer = match target {
                BridgeTarget::Slot(slot) => {
                    body.dispatch(values[0], slot, forwarded, forwarded_ret, &arguments)?
                }
                BridgeTarget::Function(target) => {
                    let Some(target) = body.file.functions[target as usize] else {
                        return Err("a bridge to a method with no body".into());
                    };
                    let func_ref = body.func_ref(target);
                    let mut operands = vec![values[0]];
                    operands.extend_from_slice(&arguments);
                    let call = body.emit_call(func_ref, &operands)?;
                    body.builder.inst_results(call).first().copied()
                }
            };
            match (answer, body.carrier(result)) {
                (Some(answer), Carrier::Void) => {
                    let _ = answer;
                    body.builder.ins().return_(&[]);
                }
                (Some(answer), _) => {
                    let Some(answer) = body.convert(answer, Some(forwarded_ret), result)? else {
                        return Err("an answer that does not cross a bridge".into());
                    };
                    body.builder.ins().return_(&[answer]);
                }
                // `open fun foo(): Any` overridden by `fun foo(): Unit`. The override produces
                // no machine value, and `Unit` is still the Kotlin value a caller reading the
                // base's slot gets back — the runtime owns that singleton, so hand it over.
                (None, Carrier::Ref) => {
                    let unit = body
                        .runtime_call("kt_unit", &[], any(), &[])?
                        .expect("`kt_unit` returns the singleton");
                    body.builder.ins().return_(&[unit]);
                }
                (None, Carrier::Void) => {
                    body.builder.ins().return_(&[]);
                }
                (None, Carrier::Scalar(_, _)) => {
                    return Err("a `Unit` answer where the base declares a primitive".into());
                }
            }
            body.terminate();
            Ok(())
        })
    }

    /// A property accessor's bridge: the interface's carrier in, the implementation's out.
    ///
    /// The same shape as [`Self::define_bridge`] and for the same reason, except that neither end
    /// is named by a declaration — a synthesized field access has none — so the two property TYPES
    /// stand in for the two signatures.
    pub(super) fn define_accessor_bridge(
        &mut self,
        declared: Ty,
        implemented: Ty,
        receiver: Option<(Ty, Ty)>,
        setter: bool,
        target_slot: u32,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (params, result) =
            accessor_signature(declared, receiver.map(|(declared, _)| declared), setter);
        let (forwarded, forwarded_result) = accessor_signature(
            implemented,
            receiver.map(|(_, implemented)| implemented),
            setter,
        );
        let signature = self.signature_of(&params, result)?;
        let name = format!("accessor bridge to slot {target_slot}");
        self.emit_function(id, signature, result, &name, &mut |body, values| {
            let mut operands = Vec::with_capacity(forwarded.len() - 1);
            for ((&value, &have), &want) in values.iter().zip(&params).zip(&forwarded).skip(1) {
                let Some(value) = body.convert(value, Some(have), want)? else {
                    return Err("a `Unit` operand crossing an accessor bridge".into());
                };
                operands.push(value);
            }
            let answer = body.dispatch(
                values[0],
                target_slot,
                &forwarded[1..],
                forwarded_result,
                &operands,
            )?;
            if setter {
                body.builder.ins().return_(&[]);
                body.terminate();
                return Ok(());
            }
            let Some(answer) = answer else {
                return Err("a `Unit` answer crossing an accessor bridge".into());
            };
            let Some(answer) = body.convert(answer, Some(implemented), declared)? else {
                return Err("a `Unit` answer crossing an accessor bridge".into());
            };
            body.builder.ins().return_(&[answer]);
            body.terminate();
            Ok(())
        })
    }

    /// The type an object of `class` is held at while it is being built or dispatched on: the
    /// class itself, except for a value class, whose instance IS its value everywhere but here —
    /// a constructor fills a box, so its `this` is that box, a reference like any other.
    pub(super) fn object_type(&self, class: ClassId) -> Ty {
        let declaration = &self.ir.classes[class as usize];
        if self.values.is_value_class(declaration.fq_name) {
            any()
        } else {
            Ty::obj_name(declaration.fq_name_id())
        }
    }

    /// Where a value class's box keeps its value: the field's offset, and the declared type of
    /// what it holds.
    pub(super) fn value_storage(&self, class: ClassId) -> Result<(i32, Ty), Unsupported> {
        let field = model::value_field(self.ir, class)?;
        let offset = self.model.layout(class).fields[field as usize].offset as i32;
        Ok((
            offset,
            model::field_storage_ty(self.values, self.ir, class, field)?,
        ))
    }

    /// A value class's own member reached through its box: read the value out and call the
    /// member with it as `this`, every other operand and the answer passing straight through.
    pub(super) fn define_value_bridge(
        &mut self,
        class: ClassId,
        function: FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (offset, ty) = self.value_storage(class)?;
        let clif = self.carrier(ty).clif().expect("a value is never `Unit`");
        let Some(target) = self.functions[function as usize] else {
            return Err(declined!(
                "a value class member with no body (`{}`)",
                self.ir.functions[function as usize].name
            ));
        };
        let mut params = vec![any()];
        params.extend(super::super::super::captures::carried_parameters(
            self.ir, function,
        ));
        let ret = self.ir.functions[function as usize].ret;
        let signature = self.signature_of(&params, ret)?;
        let name = format!("{}.<boxed>", self.ir.functions[function as usize].name);
        self.emit_function(id, signature, ret, &name, &mut |body, operands| {
            let value = body
                .builder
                .ins()
                .load(clif, trusted(), operands[0], offset);
            let mut arguments = vec![value];
            arguments.extend(operands[1..].iter().copied());
            let func_ref = body.func_ref(target);
            let call = body.emit_call(func_ref, &arguments)?;
            let results = body.builder.inst_results(call).to_vec();
            body.builder.ins().return_(&results);
            body.terminate();
            Ok(())
        })
    }
}

/// An accessor's physical operands and answer: the object, a member extension's receiver, and a
/// setter's value; a getter answers the property's type and a setter nothing.
pub(super) fn accessor_signature(
    property: Ty,
    receiver: Option<Ty>,
    setter: bool,
) -> (Vec<Ty>, Ty) {
    let mut params = vec![any()];
    params.extend(receiver);
    if setter {
        params.push(property);
        (params, Ty::Unit)
    } else {
        (params, property)
    }
}
