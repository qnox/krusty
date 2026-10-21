//! Calls that reach a member through its class: dispatch through the receiver's vtable for a
//! virtual call, and the named class's own implementation for a `super` call.

use super::objects::{any_member, ktype, CheckedProperty, TYPE_OFFSET};
use super::*;
use crate::types::TypeName;

/// The declaration a `super` call names: its class, its member, and how the frontend selected it.
pub(super) struct SuperTarget<'c> {
    pub(super) owner: TypeName,
    pub(super) name: &'c str,
    pub(super) kind: crate::ir::IrSuperCallKind,
    pub(super) declaration: Option<crate::fir::ResolvedFunctionOverrideTarget>,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// A call through the receiver's vtable: `receiver.type.vtable[slot](receiver, args…)`.
    pub(super) fn dispatch(
        &mut self,
        receiver: Value,
        slot: u32,
        params: &[Ty],
        ret: Ty,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        self.null_check(receiver)?;
        let flags = trusted();
        let ty = self
            .builder
            .ins()
            .load(types::I64, flags, receiver, TYPE_OFFSET);
        let vtable = self
            .builder
            .ins()
            .load(types::I64, flags, ty, ktype::VTABLE as i32);
        let function = self
            .builder
            .ins()
            .load(types::I64, flags, vtable, (slot * 8) as i32);
        let mut all_params = vec![any()];
        all_params.extend_from_slice(params);
        let signature = self.file.signature_of(&all_params, ret)?;
        let signature = self.builder.import_signature(signature);
        let mut all_arguments = vec![receiver];
        all_arguments.extend_from_slice(arguments);
        let call = self
            .builder
            .ins()
            .call_indirect(signature, function, &all_arguments);
        // A dispatched call is as able to throw as a direct one, and MORE able to be forgotten:
        // it is the one call this backend emits that does not go through `emit_call`. A `try`
        // whose body invokes a lambda is the common shape, and the exception walked straight out
        // of the `try` until this check existed.
        self.check_pending()?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// A call dispatched on the receiver's own type, named by the STATIC type it is made through:
    /// `Callee::Virtual`. The class or interface that declares the member fixes the slot number —
    /// `place_interface_slots` gives an interface member one number that means the same thing in
    /// every implementation — so the slot is looked up on the DECLARING classifier and the vtable
    /// it indexes is the receiver's. That is what makes `class C(a: I) : I by a` work: the
    /// forwarder synthesized on `C` knows only `I`.
    pub(super) fn virtual_call(
        &mut self,
        owner: TypeName,
        name: &str,
        params: Option<&(Vec<Ty>, Ty)>,
        target: Option<crate::ir::IrVirtualTarget>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(owner, "a virtual call to a member of")?;
        let ir = self.file.ir;
        let unknown = || {
            format!(
                "a virtual call to an unknown member (`{}.{name}`)",
                owner.render().replace('/', ".")
            )
        };
        let methods = &ir.classes[class as usize].methods;
        // The member the checker selected, by its declaration: the owner's own or one it
        // inherits, whose key every table below its declaring class carries. Only a call that
        // names no module function (one a plugin synthesized, or a dependency member the class
        // overrides) is matched among the owner's methods by its signature.
        let fid = match target {
            Some(crate::ir::IrVirtualTarget::Function(
                crate::fir::ResolvedFunctionOverrideTarget::Module(callable),
            )) => ir
                .checked_callable_functions
                .get(&callable)
                .copied()
                .ok_or_else(unknown)?,
            _ => {
                let declared = params.map(|(params, _)| params.as_slice());
                methods
                    .iter()
                    .copied()
                    .find(|&fid| {
                        let function = &ir.functions[fid as usize];
                        function.name == name
                            && declared.is_none_or(|params| function.params == params)
                    })
                    .ok_or_else(unknown)?
            }
        };
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() {
            return Err(declined!("a virtual call to the static member `{name}`"));
        }
        let declaring = function
            .dispatch_receiver
            .and_then(|receiver| ir.class_id_by_name(receiver))
            .ok_or_else(unknown)?;
        let key = model::function_key(ir, declaring, fid);
        let Some(slot) = self.file.model.slot(class, &key) else {
            return Err(declined!("a virtual call with no dispatch slot (`{name}`)"));
        };
        // The ABI is the DECLARATION's, not the call site's: every override fills this slot with a
        // body compiled to the declaration's carriers, so an argument whose checked type is narrower
        // still crosses as what the slot expects.
        let carried = super::super::super::captures::carried_parameters(ir, fid);
        let ret = function.ret;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, &carried)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch(object, slot, &carried, ret, &arguments)
    }

    /// `super.p` and `super.p = v` — the named class's own realization of a PROPERTY.
    ///
    /// Returns `None` when the class declares no such property, so the caller keeps its decline.
    ///
    /// Nothing here may dispatch. `super.p` is written inside the override of `p`, and reaching the
    /// slot would reach that override — which is the accessor doing the asking. So a source-written
    /// accessor of the named class is called directly, and a default one is the field that class
    /// contributes: a distinct field from the override's, because an overriding `var` declares
    /// storage of its own and `super.b` is the reason a program can tell.
    fn direct_property(
        &mut self,
        class: ClassId,
        target: crate::fir::PropertyId,
        setter: bool,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let CheckedProperty::Member(property_class, index) = self.checked_property(&target)? else {
            return Err("a `super` access to a top-level property".into());
        };
        if property_class != class {
            return Err("a `super` property target owned by another class".into());
        }
        let name = self.file.ir.classes[class as usize].properties[index]
            .name
            .clone();
        match (setter, args) {
            (false, []) => self.direct_property_read(class, index, receiver),
            (true, [value]) => self
                .direct_property_write(class, index, receiver, *value)
                .map(|()| None),
            _ => Err(declined!(
                "a `super` access to `{name}` with {} operands",
                args.len()
            )),
        }
    }

    fn direct_property_read(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        if let Some(getter) = property.getter {
            let Some(id) = self.file.functions[getter as usize] else {
                return Err(declined!(
                    "a `super` read of the abstract `{}`",
                    property.name
                ));
            };
            let func_ref = self.func_ref(id);
            let call = self.emit_call(func_ref, &[object])?;
            return Ok(self.builder.inst_results(call).first().copied());
        }
        let Some(field) = property.backing_field else {
            return Err(declined!(
                "a `super` read of `{}`, which has neither storage nor a getter",
                property.name
            ));
        };
        let ty = self.file.ir.classes[class as usize].fields[field as usize].ty;
        self.load_field(object, class, field, ty).map(Some)
    }

    fn direct_property_write(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        // The setter takes the property's own type; a direct store takes the FIELD's, which is the
        // same rule `written_property_ty` states for an ordinary write.
        let target = match (property.setter, property.backing_field) {
            (Some(_), _) => property.ty,
            (None, Some(field)) => self.file.ir.classes[class as usize].fields[field as usize].ty,
            (None, None) => {
                return Err(declined!(
                    "a `super` write of `{}`, which has neither storage nor a setter",
                    property.name
                ));
            }
        };
        let Some(object) = self.receiver(receiver)? else {
            return Ok(());
        };
        let value = self.coerce(value, target)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err(declined!("a `Unit` value assigned to `{}`", property.name));
        };
        if let Some(setter) = property.setter {
            let Some(id) = self.file.functions[setter as usize] else {
                return Err(declined!(
                    "a `super` write of the abstract `{}`",
                    property.name
                ));
            };
            let func_ref = self.func_ref(id);
            self.emit_call(func_ref, &[object, value])?;
            return Ok(());
        }
        let field = property.backing_field.expect("checked above");
        let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(())
    }

    /// A non-virtual call to the named class's own implementation: `super.f()`.
    pub(super) fn direct_call(
        &mut self,
        target: SuperTarget<'_>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let SuperTarget {
            owner,
            name,
            kind,
            declaration,
        } = target;
        if super::super::super::intrinsics::is_any(owner) {
            let symbol = match (name, args.len()) {
                ("toString", 0) => "kt_any_to_string",
                ("hashCode", 0) => "kt_any_hash_code",
                ("equals", 1) => "kt_any_equals",
                _ => return Err(declined!("a `super` call to `Any.{name}`")),
            };
            let (params, ret) = any_member(symbol).expect("a kotlin.Any member");
            let mut arguments = vec![self.reference(receiver)?];
            for argument in args {
                arguments.push(self.reference(*argument)?);
            }
            if self.terminated {
                return Ok(None);
            }
            return self.runtime_call(symbol, &params, ret, &arguments);
        }
        let class = self.file.class_of(owner, "a `super` call to a method of")?;
        match kind {
            crate::ir::IrSuperCallKind::PropertyGetter(property) => {
                return self.direct_property(class, property, false, receiver, args);
            }
            crate::ir::IrSuperCallKind::PropertySetter(property) => {
                return self.direct_property(class, property, true, receiver, args);
            }
            crate::ir::IrSuperCallKind::Function => {}
        }
        let ir = self.file.ir;
        let Some(crate::fir::ResolvedFunctionOverrideTarget::Module(source)) = declaration else {
            return Err(declined!(
                "a `super` call without a source declaration (`{name}`)"
            ));
        };
        let Some(&fid) = ir.checked_callable_functions.get(&source) else {
            return Err(declined!("a `super` call to an unknown method (`{name}`)"));
        };
        let Some(id) = self.file.functions[fid as usize] else {
            return Err(declined!("a `super` call to the abstract method `{name}`"));
        };
        let params = super::super::super::captures::carried_parameters(ir, fid);
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let mut arguments = vec![object];
        arguments.extend(self.arguments(args, &params)?);
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }
}
