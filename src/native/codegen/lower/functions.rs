//! Function values: lambdas, the objects that carry their captures, and calling one.
//!
//! A lambda is an object, and that is not an analogy — it is allocated by the same collector,
//! described by the same `KType`, and dispatched through the same vtable as any class here. Common
//! lowering has already done the hard half: the body is a synthesized top-level function
//! ([`IrExpr::Lambda::impl_fn`]) whose LEADING parameters are the captured values and whose
//! trailing ones are the lambda's own. So a function value is a small object holding the captures,
//! and calling it is a vtable dispatch to a thunk that unpacks them and calls that function.
//!
//! **The thunk's signature is Kotlin's own.** On the JVM a lambda is a `FunctionN` whose `invoke`
//! takes and returns `Object`; the IR says so where it describes [`IrExpr::InvokeFunction`]
//! ("arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`"). The same
//! convention is what a native call site needs, and for the same reason: the site knows the
//! function's arity but not which lambda it holds, so every function value of a given arity must
//! be callable one way. The thunk converts at the boundary — unbox each argument to what the body
//! declares, box the result — and the body itself is compiled with its real types.
//!
//! **A captured `var` is a holder.** Common lowering boxes a mutable local a closure captures into
//! a [`IrExpr::RefNew`] holder and rewrites reads and writes as [`IrExpr::RefGet`]/[`IrExpr::RefSet`],
//! so that the closure and the enclosing frame share one cell rather than a copy. That is a
//! one-field object here, with the field traced when it holds a reference.

use super::objects::trusted;
use super::*;

/// The vtable slot a function value's body occupies: right after `kotlin.Any`'s three, so a
/// function value answers `equals`/`hashCode`/`toString` like any other object.
const INVOKE_SLOT: u32 = 3;

/// The emitted pieces of one lambda site.
pub(super) struct LambdaItems {
    descriptor: DataId,
    instance_size: u32,
    /// Byte offset of each capture, parallel to the site's `captures`.
    capture_offsets: Vec<u32>,
    /// A lambda that captures nothing is ONE object, not one per evaluation: `{}` has the same
    /// `hashCode` every time it is written, which a program can see. With no fields to hold there
    /// is nothing to allocate, so the instance is static storage — the collector never sees it,
    /// and a call site costs an address rather than a collection.
    singleton: Option<DataId>,
}

/// The types a function's parameters are CARRIED as, which are not always the types it declares.
///
/// A `var` that a closure captures is replaced by a holder, and what is passed to the lambda's body
/// is that cell — but the parameter still says `Int`, because `Int` is what the programmer wrote.
/// The BODY is what settles it: it reaches a holder through [`IrExpr::RefGet`]/[`IrExpr::RefSet`]
/// rather than using the value directly. Believing the declaration instead truncates a pointer into
/// a 32-bit parameter, which is a miscompile with no symptom where it happens — the collector's
/// stress tests are what caught it, because the damage only became visible once something was kept
/// alive across a collection and read back.
pub(super) fn carried_parameters(ir: &IrFile, function: &crate::ir::IrFunction) -> Vec<Ty> {
    let Some(body) = function.body else {
        return function.params.clone();
    };
    // `this` occupies slot 0 when there is one, so a parameter's slot is offset by it.
    let first = usize::from(function.dispatch_receiver.is_some());
    let mut holders = vec![false; function.params.len() + first];
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let IrExpr::RefGet { holder, .. } | IrExpr::RefSet { holder, .. } = ir.expr(id) {
            if let IrExpr::GetValue(slot) = ir.expr(*holder) {
                if let Some(flag) = holders.get_mut(*slot as usize) {
                    *flag = true;
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, id, &mut |child| pending.push(child));
    }
    function
        .params
        .iter()
        .enumerate()
        .map(|(index, declared)| {
            if holders[index + first] {
                any()
            } else {
                *declared
            }
        })
        .collect()
}

/// Lay out values after the object header, each aligned to its own size — the same rule the class
/// model applies to fields, for the same reason: the collector is told where the references are
/// and must be told the truth.
fn layout(types: &[Ty]) -> (Vec<u32>, u32, Vec<u32>) {
    let mut offsets = Vec::with_capacity(types.len());
    let mut references = Vec::new();
    let mut end = model::HEADER_SIZE;
    for ty in types {
        let size = carrier(*ty).clif().map_or(8, |clif| clif.bytes());
        let offset = end.next_multiple_of(size);
        if carrier(*ty) == Carrier::Ref {
            references.push(offset);
        }
        offsets.push(offset);
        end = offset + size;
    }
    (offsets, end.next_multiple_of(8), references)
}

impl<'a> FileLowering<'a> {
    /// Declare a type and a thunk for every lambda in the file, before any body is compiled.
    pub(super) fn declare_lambdas(&mut self) -> Result<(), Unsupported> {
        for index in 0..self.ir.exprs.len() {
            let IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                sam,
                ..
            } = &self.ir.exprs[index]
            else {
                continue;
            };
            if sam.is_some() {
                return Err("a lambda for a functional interface".to_string());
            }
            let (impl_fn, arity, captures) = (*impl_fn, usize::from(*arity), captures.clone());
            let body = self
                .ir
                .functions
                .get(impl_fn as usize)
                .ok_or_else(|| "a lambda with no body function".to_string())?;
            if body.params.len() != captures.len() + arity {
                return Err(format!(
                    "a lambda whose body takes {} parameters for {} captures and arity {arity}",
                    body.params.len(),
                    captures.len()
                ));
            }
            let capture_types: Vec<Ty> =
                carried_parameters(self.ir, body)[..captures.len()].to_vec();
            let (capture_offsets, instance_size, references) = layout(&capture_types);

            let base = format!("kt_fn_{index}");
            let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
            let thunk = self.declare_local_function(
                &format!("{base}_invoke"),
                &vec![any(); arity + 1],
                any(),
            )?;
            let mut vtable = self.any_vtable()?;
            vtable.push(thunk);
            // `toString` on a function value prints this name, as `Function1` would on the JVM.
            let kotlin_name = format!("kotlin.Function{arity}");
            let any_type = self.import_data("kt_type_any")?;
            self.define_type_descriptor(
                descriptor,
                &base,
                &kotlin_name,
                instance_size,
                &references,
                &vtable,
                any_type,
            )?;
            self.define_thunk(thunk, &base, impl_fn, &capture_offsets, arity)?;
            let singleton = if captures.is_empty() {
                let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
                let mut description = DataDescription::new();
                description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
                description.set_align(8);
                let global = self
                    .module
                    .declare_data_in_data(descriptor, &mut description);
                description.write_data_addr(0, global, 0);
                self.module
                    .define_data(instance, &description)
                    .map_err(|error| format!("defining `{base}_instance` ({error})"))?;
                Some(instance)
            } else {
                None
            };
            self.lambdas.insert(
                index as u32,
                LambdaItems {
                    descriptor,
                    instance_size,
                    capture_offsets,
                    singleton,
                },
            );
        }
        Ok(())
    }

    /// The uniform entry point: unpack the captures this object carries, convert each argument
    /// from the reference the caller passed, call the body, and hand the result back as a
    /// reference.
    fn define_thunk(
        &mut self,
        thunk: FuncId,
        base: &str,
        impl_fn: u32,
        capture_offsets: &[u32],
        arity: usize,
    ) -> Result<(), Unsupported> {
        let body = &self.ir.functions[impl_fn as usize];
        let parameters = carried_parameters(self.ir, body);
        let ret = body.ret;
        let target = self.functions[impl_fn as usize]
            .ok_or_else(|| "a lambda whose body has no code".to_string())?;
        let signature = self.signature_of(&vec![any(); arity + 1], any())?;
        let capture_offsets = capture_offsets.to_vec();
        let name = format!("{base}_invoke");

        self.emit_function(
            thunk,
            signature,
            Carrier::Ref,
            &name,
            &mut |body, params| {
                let mut arguments = Vec::with_capacity(parameters.len());
                for (offset, ty) in capture_offsets.iter().zip(&parameters) {
                    let clif = carrier(*ty).clif().expect("a capture is never `Unit`");
                    arguments.push(body.builder.ins().load(
                        clif,
                        trusted(),
                        params[0],
                        *offset as i32,
                    ));
                }
                for (index, ty) in parameters[capture_offsets.len()..].iter().enumerate() {
                    let Some(value) = body.convert(params[index + 1], Some(any()), *ty)? else {
                        return Err("a `Unit` lambda parameter".to_string());
                    };
                    arguments.push(value);
                }
                let func_ref = body.func_ref(target);
                let call = body.builder.ins().call(func_ref, &arguments);
                let result = body.builder.inst_results(call).first().copied();
                // A `Unit`-returning lambda still answers with a reference: the runtime's `Unit`.
                let result = match result {
                    Some(value) => body
                        .convert(value, Some(ret), any())?
                        .expect("a reference carrier"),
                    None => body
                        .runtime_call("kt_unit", &[], any(), &[])?
                        .expect("`kt_unit` returns the singleton"),
                };
                body.builder.ins().return_(&[result]);
                body.terminate();
                Ok(())
            },
        )
    }

    /// The holder type for a captured `var` of the given carrier, declared on first use.
    pub(super) fn holder_type(&mut self, elem: Ty) -> Result<DataId, Unsupported> {
        let clif = carrier(elem)
            .clif()
            .ok_or_else(|| "a captured `Unit` variable".to_string())?;
        let key = clif.to_string();
        if let Some(id) = self.holders.get(&key) {
            return Ok(*id);
        }
        let base = format!("kt_ref_{key}");
        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let (_, instance_size, references) = layout(&[elem]);
        let vtable = self.any_vtable()?;
        let any_type = self.import_data("kt_type_any")?;
        self.define_type_descriptor(
            descriptor,
            &base,
            "kotlin.jvm.internal.Ref",
            instance_size,
            &references,
            &vtable,
            any_type,
        )?;
        self.holders.insert(key, descriptor);
        Ok(descriptor)
    }
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// Allocate a function value and fill in what it captured.
    pub(super) fn lambda(&mut self, site: u32) -> Result<Option<Value>, Unsupported> {
        let IrExpr::Lambda {
            impl_fn, captures, ..
        } = self.file.ir.expr(site).clone()
        else {
            unreachable!("only a lambda site reaches here");
        };
        let items = self
            .file
            .lambdas
            .get(&site)
            .ok_or_else(|| "an undeclared lambda".to_string())?;
        if let Some(instance) = items.singleton {
            return Ok(Some(self.data_address(instance)));
        }
        let (descriptor, instance_size, offsets) = (
            items.descriptor,
            items.instance_size,
            items.capture_offsets.clone(),
        );
        let parameters =
            carried_parameters(self.file.ir, &self.file.ir.functions[impl_fn as usize]);

        // Captures first, then the allocation: a capture that allocates must not leave a
        // half-built object for a collection to find.
        let mut values = Vec::with_capacity(captures.len());
        for (capture, ty) in captures.iter().zip(&parameters) {
            let Some(value) = self.coerce(*capture, *ty)? else {
                return Err("a `Unit` capture".to_string());
            };
            if self.terminated {
                return Ok(None);
            }
            values.push(value);
        }
        let object = self.allocate(descriptor, instance_size)?;
        for (value, offset) in values.into_iter().zip(&offsets) {
            self.builder
                .ins()
                .store(trusted(), value, object, *offset as i32);
        }
        Ok(Some(object))
    }

    /// Call a function value: box each argument, dispatch through the invoke slot, convert the
    /// result back. Kotlin's own calling convention for a `FunctionN`.
    pub(super) fn invoke_function(
        &mut self,
        func: u32,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(function) = self.receiver(func)? else {
            return Ok(None);
        };
        let mut arguments = Vec::with_capacity(args.len());
        for (argument, ty) in args.iter().zip(params) {
            let Some(value) = self.coerce(*argument, *ty)? else {
                return Err("a `Unit` argument to a function value".to_string());
            };
            if self.terminated {
                return Ok(None);
            }
            let Some(value) = self.convert(value, Some(*ty), any())? else {
                return Err("a `Unit` argument to a function value".to_string());
            };
            arguments.push(value);
        }
        let result = self.dispatch(
            function,
            INVOKE_SLOT,
            &vec![any(); arguments.len()],
            any(),
            &arguments,
        )?;
        match result {
            Some(value) if carrier(ret) != Carrier::Void => self.convert(value, Some(any()), ret),
            _ => Ok(None),
        }
    }

    /// The cell a captured `var` lives in, so the closure and the frame that made it share one.
    pub(super) fn ref_new(&mut self, elem: Ty, init: u32) -> Result<Option<Value>, Unsupported> {
        let descriptor = self.file.holder_type(elem)?;
        let (offsets, instance_size, _) = super::functions::layout(&[elem]);
        let Some(value) = self.coerce(init, elem)? else {
            return Err("a captured `Unit` variable".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        let holder = self.allocate(descriptor, instance_size)?;
        self.builder
            .ins()
            .store(trusted(), value, holder, offsets[0] as i32);
        Ok(Some(holder))
    }

    pub(super) fn ref_get(&mut self, holder: u32, elem: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(holder) = self.receiver(holder)? else {
            return Ok(None);
        };
        let (offsets, _, _) = super::functions::layout(&[elem]);
        let clif = carrier(elem)
            .clif()
            .ok_or_else(|| "a captured `Unit` variable".to_string())?;
        Ok(Some(self.builder.ins().load(
            clif,
            trusted(),
            holder,
            offsets[0] as i32,
        )))
    }

    /// A write to a captured `var`, which evaluates to the value written.
    pub(super) fn ref_set(
        &mut self,
        holder: u32,
        elem: Ty,
        value: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(holder) = self.receiver(holder)? else {
            return Ok(None);
        };
        let value = self.coerce(value, elem)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(value) = value else {
            return Err("a `Unit` value assigned to a captured variable".to_string());
        };
        let (offsets, _, _) = super::functions::layout(&[elem]);
        self.builder
            .ins()
            .store(trusted(), value, holder, offsets[0] as i32);
        Ok(Some(value))
    }
}
