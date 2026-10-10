//! Native object-table entries whose signatures adapt another declaration or representation.

use super::*;

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
        target: crate::ir::FunId,
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
        declared: crate::ir::FunId,
        target_slot: Option<u32>,
        target: crate::ir::FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let base = self.ir.functions[declared as usize].clone();
        let carried = super::super::super::captures::carried_parameters(self.ir, declared);
        let mut params = vec![any()];
        params.extend(carried.iter().copied());
        let result = base.ret;
        let signature = self.signature_of(&params, result)?;
        let forwarded = super::super::super::captures::carried_parameters(self.ir, target);
        let forwarded_ret = self.ir.functions[target as usize].ret;
        let name = format!("bridge to `{}`", base.name);
        self.emit_function(id, signature, result, &name, &mut |body, values| {
            let mut arguments = Vec::with_capacity(forwarded.len());
            for (index, &want) in forwarded.iter().enumerate() {
                let have = carried.get(index).copied();
                let Some(value) = body.convert(values[index + 1], have, want)? else {
                    return Err("a `Unit` operand crossing a bridge".into());
                };
                arguments.push(value);
            }
            // Through the slot where there is one, so a further subclass's override is reached
            // through the same base. An INTERFACE's own bridge has none — see `Slot::Bridge` —
            // and calls the default body outright, which is where it is the only implementation.
            let answer = match target_slot {
                Some(slot) => {
                    body.dispatch(values[0], slot, &forwarded, forwarded_ret, &arguments)?
                }
                None => {
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
        setter: bool,
        target_slot: u32,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (params, result) = if setter {
            (vec![any(), declared], Ty::Unit)
        } else {
            (vec![any()], declared)
        };
        let signature = self.signature_of(&params, result)?;
        let name = format!("accessor bridge to slot {target_slot}");
        self.emit_function(id, signature, result, &name, &mut |body, values| {
            if setter {
                let Some(value) = body.convert(values[1], Some(declared), implemented)? else {
                    return Err("a `Unit` value crossing an accessor bridge".into());
                };
                body.dispatch(values[0], target_slot, &[implemented], Ty::Unit, &[value])?;
                body.builder.ins().return_(&[]);
                body.terminate();
                return Ok(());
            }
            let answer = body.dispatch(values[0], target_slot, &[], implemented, &[])?;
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
            Ty::Obj(declaration.fq_name_id(), &[])
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

    /// A value class's `equals`, `hashCode` or `toString`, answered through its box by the value it
    /// holds — Kotlin's answer, which is the wrapped value's and not the wrapper's.
    ///
    /// `equals` checks the other operand's type before reading its field: a reference that is not
    /// one of these boxes has no value at that offset, so it is simply not equal. Two boxes compare
    /// their values by the rule a data class compares a field by. `toString` renders `V(x=1)`:
    /// the class's Kotlin name, the property's, and the value through the runtime's rendering.
    pub(super) fn define_value_member(
        &mut self,
        class: ClassId,
        member: AnyMember,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (offset, ty) = self.value_storage(class)?;
        let property_field = model::value_field(self.ir, class)?;
        let property = self.ir.classes[class as usize]
            .properties
            .iter()
            .find(|property| property.backing_field == Some(property_field))
            .expect("value_field validated exactly one recorded property coordinate")
            .name
            .clone();
        let kotlin_name = self.kotlin_name(class);
        let name = format!("{}.{member:?}", self.ir.classes[class as usize].fq_name());
        let clif = self.carrier(ty).clif().expect("a value is never `Unit`");
        let descriptor = self.classes[class as usize].descriptor;
        match member {
            AnyMember::Equals => {
                let signature = self.signature_of(&[any(), any()], Ty::Boolean)?;
                self.emit_function(id, signature, Ty::Boolean, &name, &mut |body, params| {
                    let (left, right) = (params[0], params[1]);
                    let type_address = body.data_address(descriptor);
                    let same_type = body
                        .runtime_call(
                            "kt_is_instance",
                            &[any(), any()],
                            Ty::Boolean,
                            &[right, type_address],
                        )?
                        .expect("`kt_is_instance` returns a Boolean");
                    let merge = body.builder.create_block();
                    body.builder.append_block_param(merge, types::I8);
                    let compare = body.builder.create_block();
                    let other = body.builder.create_block();
                    body.builder.ins().brif(same_type, compare, &[], other, &[]);

                    body.continue_in(other);
                    body.builder.seal_block(other);
                    let no = body.builder.ins().iconst(types::I8, 0);
                    body.builder.ins().jump(merge, &[BlockArg::Value(no)]);

                    body.continue_in(compare);
                    body.builder.seal_block(compare);
                    let mine = body.builder.ins().load(clif, trusted(), left, offset);
                    let theirs = body.builder.ins().load(clif, trusted(), right, offset);
                    let equal = body.values_equal(mine, theirs, ty)?;
                    body.builder.ins().jump(merge, &[BlockArg::Value(equal)]);

                    body.continue_in(merge);
                    body.builder.seal_block(merge);
                    let answer = body.builder.block_params(merge)[0];
                    body.builder.ins().return_(&[answer]);
                    body.terminate();
                    Ok(())
                })
            }
            AnyMember::HashCode => {
                let signature = self.signature_of(&[any()], Ty::Int)?;
                self.emit_function(id, signature, Ty::Int, &name, &mut |body, params| {
                    let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                    let hash = body.value_hash(value, ty)?;
                    body.builder.ins().return_(&[hash]);
                    body.terminate();
                    Ok(())
                })
            }
            AnyMember::ToString => {
                let opening = format!("{kotlin_name}({property}=");
                let signature = self.signature_of(&[any()], any())?;
                self.emit_function(id, signature, any(), &name, &mut |body, params| {
                    let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                    let boxed = body
                        .convert(value, Some(ty), any())?
                        .expect("a value is never `Unit`");
                    let rendered = body
                        .runtime_call("kt_to_string", &[any()], any(), &[boxed])?
                        .expect("`kt_to_string` returns a string");
                    let head = body.string_literal(opening.as_bytes())?;
                    let joined = body
                        .runtime_call("kt_string_plus", &[any(), any()], any(), &[head, rendered])?
                        .expect("`kt_string_plus` returns a string");
                    let tail = body.string_literal(b")")?;
                    let whole = body
                        .runtime_call("kt_string_plus", &[any(), any()], any(), &[joined, tail])?
                        .expect("`kt_string_plus` returns a string");
                    body.builder.ins().return_(&[whole]);
                    body.terminate();
                    Ok(())
                })
            }
        }
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
