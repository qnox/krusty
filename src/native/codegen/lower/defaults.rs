//! Calls that leave arguments out.
//!
//! `fun greet(name: String, greeting: String = "hi")` called as `greet("k")` is not a different
//! function: it is the same one with the missing argument computed at the call. Which computation
//! is not the call site's to make, though — a default may read an earlier PARAMETER (`fun f(a: Int,
//! b: Int = a + 1)`), so it is written in the callee's frame and has to be evaluated there.
//!
//! So each shape of omission gets a small wrapper: a function taking exactly the arguments that
//! were supplied, which declares the callee's whole parameter frame, fills the omitted slots by
//! evaluating their defaults in declaration order, and calls the real function. One wrapper per
//! (function, omitted set) actually used, so a program that always omits the same argument emits
//! one. The JVM's answer to the same problem is a `$default` synthetic taking a bitmask, which
//! exists because its callers may live in another compilation unit; within one file, naming the
//! shape is simpler and leaves no mask to decode at run time.

use super::*;

/// The wrapper a call with omitted arguments goes through, keyed by what it omits.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Omission {
    pub function: u32,
    /// Parameter ordinals the call leaves out, ascending — the IR's own `defaults` vector.
    pub omitted: Vec<u32>,
}

impl<'a> FileLowering<'a> {
    /// Declare a wrapper for every omission shape the file's calls use.
    pub(super) fn declare_default_wrappers(&mut self) -> Result<(), Unsupported> {
        for index in 0..self.ir.exprs.len() {
            let (function, omitted) = match &self.ir.exprs[index] {
                IrExpr::Call {
                    callee:
                        Callee::LocalWithDefaults { function, defaults }
                        | Callee::ClassStaticWithDefaults {
                            function, defaults, ..
                        },
                    ..
                } => (*function, defaults.to_vec()),
                // A defaulted call on a receiver arrives as a method call with holes in its
                // argument list rather than as a callee of its own.
                IrExpr::MethodCall {
                    class, index, args, ..
                } if args.iter().any(Option::is_none) => {
                    let omitted = args
                        .iter()
                        .enumerate()
                        .filter(|(_, argument)| argument.is_none())
                        .map(|(ordinal, _)| ordinal as u32)
                        .collect();
                    (
                        self.ir.classes[*class as usize].methods[*index as usize],
                        omitted,
                    )
                }
                _ => continue,
            };
            let key = Omission { function, omitted };
            if self.default_wrappers.contains_key(&key) {
                continue;
            }
            let declaration = &self.ir.functions[function as usize];
            if declaration.body.is_none() {
                return Err(format!(
                    "a call with a defaulted argument to `{}`, which has no body",
                    declaration.name
                ));
            }
            let mut parameters: Vec<Ty> = declaration
                .dispatch_receiver
                .map(|owner| Ty::Obj(owner, &[]))
                .into_iter()
                .collect();
            for (ordinal, ty) in declaration.params.iter().enumerate() {
                if key.omitted.contains(&(ordinal as u32)) {
                    continue;
                }
                parameters.push(*ty);
            }
            let ret = declaration.ret;
            let symbol = format!(
                "{}__defaults{}",
                self.symbols.functions[function as usize],
                key.omitted
                    .iter()
                    .map(|ordinal| format!("_{ordinal}"))
                    .collect::<String>()
            );
            let id = self.declare_local_function(&symbol, &parameters, ret)?;
            self.default_wrappers.insert(key, id);
        }
        Ok(())
    }

    /// Emit each wrapper: the callee's frame, the defaults evaluated into it, then the call.
    pub(super) fn define_default_wrappers(&mut self) -> Result<(), Unsupported> {
        let wrappers: Vec<(Omission, FuncId)> = self
            .default_wrappers
            .iter()
            .map(|(key, id)| (key.clone(), *id))
            .collect();
        for (key, id) in wrappers {
            self.define_default_wrapper(&key, id)?;
        }
        Ok(())
    }

    fn define_default_wrapper(&mut self, key: &Omission, id: FuncId) -> Result<(), Unsupported> {
        let declaration = self.ir.functions[key.function as usize].clone();
        let Some(defaults) = self
            .ir
            .fn_params
            .get(&key.function)
            .and_then(|info| info.defaults.clone())
        else {
            return Err(format!(
                "a call with a defaulted argument to `{}`, whose defaults were not recorded",
                declaration.name
            ));
        };
        let receiver = usize::from(declaration.dispatch_receiver.is_some());
        let mut parameters: Vec<Ty> = declaration
            .dispatch_receiver
            .map(|owner| Ty::Obj(owner, &[]))
            .into_iter()
            .collect();
        let mut supplied = Vec::new();
        for (ordinal, ty) in declaration.params.iter().enumerate() {
            if key.omitted.contains(&(ordinal as u32)) {
                continue;
            }
            supplied.push(ordinal);
            parameters.push(*ty);
        }
        let signature = self.signature_of(&parameters, declaration.ret)?;
        let target = self.functions[key.function as usize]
            .ok_or_else(|| "a defaulted call to a function with no body".to_string())?;
        let slots: Vec<Ty> = declaration
            .dispatch_receiver
            .map(|owner| Ty::Obj(owner, &[]))
            .into_iter()
            .chain(functions::carried_parameters(self.ir, &declaration))
            .collect();
        let name = format!("{}$defaults", declaration.name);
        let omitted = key.omitted.clone();
        // A defaulted call to an OPEN member still dispatches on its receiver: filling the
        // arguments does not decide which implementation runs. Where the method has a slot, the
        // wrapper dispatches through it; a method with none — a top-level function, or a final
        // member with no vtable entry — is called directly.
        let virtual_slot = declaration
            .dispatch_receiver
            .and_then(|owner| self.ir.class_id_by_name(owner))
            .and_then(|class| {
                let key = model::function_key(self.ir, class, key.function);
                self.model.slot(class, &key)
            });
        if declaration.dispatch_receiver.is_some() && virtual_slot.is_none() {
            // Calling the declaration directly would run the base's body for a receiver whose
            // class overrides it. A member with no slot is one this model does not dispatch — a
            // member extension today — so the call is declined rather than answered by the wrong
            // implementation.
            return Err(format!(
                "a defaulted call to a member with no dispatch slot (`{}`)",
                declaration.name
            ));
        }
        let result = declaration.ret;
        self.emit_function(
            id,
            signature,
            carrier(result),
            &name,
            &mut |body, params| {
                // The callee's own frame: `this` at slot 0 where there is one, then every
                // parameter at its own ordinal, so a default reading an earlier one finds it. Each
                // slot is declared ONCE — declaring it again would hand back a fresh, undefined
                // variable and lose what was put in it.
                let mut variables = Vec::with_capacity(slots.len());
                for (slot, ty) in slots.iter().enumerate() {
                    variables.push(body.declare_value(slot as u32, *ty)?);
                }
                if receiver == 1 {
                    body.builder.def_var(variables[0], params[0]);
                }
                for (index, ordinal) in supplied.iter().enumerate() {
                    let slot = *ordinal + receiver;
                    body.builder
                        .def_var(variables[slot], params[index + receiver]);
                }
                // Declaration order, because a later default may read an earlier one's parameter.
                for ordinal in &omitted {
                    let Some(Some(expression)) = defaults.get(*ordinal as usize) else {
                        return Err(format!(
                            "an omitted argument with no default (`{}`)",
                            declaration.name
                        ));
                    };
                    let slot = (*ordinal as usize) + receiver;
                    let Some(value) = body.coerce(*expression, slots[slot])? else {
                        return Err(format!(
                            "a `Unit` default argument (`{}`)",
                            declaration.name
                        ));
                    };
                    if body.terminated {
                        return Ok(());
                    }
                    body.builder.def_var(variables[slot], value);
                }
                let arguments: Vec<Value> = variables
                    .iter()
                    .map(|variable| body.builder.use_var(*variable))
                    .collect();
                let returned = match virtual_slot {
                    Some(slot) => {
                        body.dispatch(arguments[0], slot, &slots[1..], result, &arguments[1..])?
                    }
                    None => {
                        let func_ref = body.func_ref(target);
                        let call = body.builder.ins().call(func_ref, &arguments);
                        body.builder.inst_results(call).first().copied()
                    }
                };
                match returned {
                    Some(value) if carrier(result) != Carrier::Void => {
                        body.builder.ins().return_(&[value]);
                    }
                    _ => {
                        body.builder.ins().return_(&[]);
                    }
                }
                body.terminate();
                Ok(())
            },
        )
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Call through the wrapper for this omission shape.
    pub(super) fn defaulted_call(
        &mut self,
        function: u32,
        omitted: &[u32],
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let key = super::defaults::Omission {
            function,
            omitted: omitted.to_vec(),
        };
        let Some(&id) = self.file.default_wrappers.get(&key) else {
            return Err("a call with a defaulted argument".to_string());
        };
        let declaration = &self.file.ir.functions[function as usize];
        let wants_receiver = declaration.dispatch_receiver.is_some();
        let parameters: Vec<Ty> = declaration
            .params
            .iter()
            .enumerate()
            .filter(|(ordinal, _)| !omitted.contains(&(*ordinal as u32)))
            .map(|(_, ty)| *ty)
            .collect();
        let mut arguments = Vec::with_capacity(parameters.len() + 1);
        match (wants_receiver, receiver) {
            (true, Some(receiver)) => {
                let Some(object) = self.receiver(receiver)? else {
                    return Ok(None);
                };
                self.null_check(object)?;
                arguments.push(object);
            }
            (true, None) => return Err("a defaulted member call with no receiver".to_string()),
            (false, _) => {}
        }
        arguments.extend(self.arguments(args, &parameters)?);
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.builder.ins().call(func_ref, &arguments);
        Ok(self.builder.inst_results(call).first().copied())
    }
}
