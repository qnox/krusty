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

use super::super::super::captures;
use super::*;
use crate::types::TypeName;

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

/// The wrapper a CONSTRUCTION with omitted arguments goes through, keyed by what it omits.
///
/// A constructor's defaults are the same problem as a function's, and get the same answer: a small
/// function declaring the constructor's whole frame, which fills the omitted slots by evaluating
/// their defaults in declaration order and then calls the real constructor. It differs only in
/// that the object already exists — the call site allocates, because an allocation is not a
/// default's to make — so the wrapper takes `this` and returns nothing, exactly as a constructor
/// does.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct CtorOmission {
    pub class: ClassId,
    /// PHYSICAL constructor-parameter ordinals the construction leaves out, ascending. A
    /// construction's own `defaults` are SOURCE-value ordinals, which begin after the compiler's
    /// leading operands (`default_prefix_count`) — captures and an outer receiver, which are never
    /// omitted.
    pub omitted: Vec<u32>,
}

impl<'a> FileLowering<'a> {
    /// The physical parameter frame of a class's primary constructor.
    fn constructor_frame(&self, class: ClassId) -> Vec<Ty> {
        self.ir.classes[class as usize]
            .ctor_args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                captures::physical_ty(self.ir, class, index as u32, argument.ty)
            })
            .collect()
    }

    /// Which physical ordinals a construction omits, or `None` when it omits nothing.
    pub(super) fn omitted_constructor_arguments(
        expression: &IrExpr,
    ) -> Option<(TypeName, Vec<u32>)> {
        let IrExpr::New {
            internal,
            defaults,
            default_prefix_count,
            ..
        } = expression
        else {
            return None;
        };
        if defaults.is_empty() {
            return None;
        }
        let mut omitted: Vec<u32> = defaults
            .iter()
            .map(|ordinal| ordinal + default_prefix_count)
            .collect();
        omitted.sort_unstable();
        Some((*internal, omitted))
    }

    /// The physical ordinals a class's primary `super(…)` delegation omits, if any.
    ///
    /// The IR records SEMANTIC ordinals — a backend's own prefix is not its to know — so they are
    /// shifted past the superclass's compiler-supplied leading parameters here.
    pub(super) fn omitted_super_arguments(&self, class: ClassId) -> Option<(TypeName, Vec<u32>)> {
        let declaration = &self.ir.classes[class as usize];
        let omitted = self
            .ir
            .super_constructor_default_arguments
            .get(&declaration.fq_name_id())?;
        if omitted.is_empty() {
            return None;
        }
        let parent = self.ir.class_id_by_name(declaration.superclass)?;
        let prefix = self.ir.classes[parent as usize].constructor_prefix_count;
        let mut physical: Vec<u32> = omitted.iter().map(|ordinal| ordinal + prefix).collect();
        physical.sort_unstable();
        Some((declaration.superclass, physical))
    }

    /// Declare a wrapper for every omission shape the file's constructions use.
    pub(super) fn declare_default_constructors(&mut self) -> Result<(), Unsupported> {
        // A `class B : A()` whose base leaves arguments out needs the same wrapper a `A()` written
        // as an expression would, and the delegation is not an expression, so both are collected.
        let shapes: Vec<(TypeName, Vec<u32>)> = (0..self.ir.classes.len() as ClassId)
            .filter_map(|class| self.omitted_super_arguments(class))
            .chain(
                self.ir
                    .exprs
                    .iter()
                    .filter_map(Self::omitted_constructor_arguments),
            )
            .collect();
        for (internal, omitted) in shapes {
            // A construction naming a class this file does not declare, or a SECONDARY
            // constructor, declares no wrapper; the call site then finds none and declines there,
            // rather than this phase failing the whole file for one construction.
            let Some(class) = self.ir.class_id_by_name(internal) else {
                continue;
            };
            if self.classes[class as usize].constructor.is_none() {
                continue;
            }
            let key = CtorOmission { class, omitted };
            if self.default_constructors.contains_key(&key) {
                continue;
            }
            let frame = self.constructor_frame(class);
            let mut parameters = vec![Ty::Obj(self.ir.classes[class as usize].fq_name_id(), &[])];
            for (ordinal, ty) in frame.iter().enumerate() {
                if key.omitted.contains(&(ordinal as u32)) {
                    continue;
                }
                parameters.push(*ty);
            }
            let symbol = format!(
                "kt_{}__init__defaults{}",
                self.class_base(class),
                key.omitted
                    .iter()
                    .map(|ordinal| format!("_{ordinal}"))
                    .collect::<String>()
            );
            let id = self.declare_local_function(&symbol, &parameters, Ty::Unit)?;
            self.default_constructors.insert(key, id);
        }
        Ok(())
    }

    /// Emit each wrapper: the constructor's frame, the defaults evaluated into it, then the call.
    pub(super) fn define_default_constructors(&mut self) -> Result<(), Unsupported> {
        let wrappers: Vec<(CtorOmission, FuncId)> = self
            .default_constructors
            .iter()
            .map(|(key, id)| (key.clone(), *id))
            .collect();
        for (key, id) in wrappers {
            self.define_default_constructor(&key, id)?;
        }
        Ok(())
    }

    fn define_default_constructor(
        &mut self,
        key: &CtorOmission,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[key.class as usize].clone();
        let name = declaration.fq_name();
        let Some(defaults) = self
            .ir
            .class_ctor_defaults_name(declaration.fq_name_id())
            .cloned()
        else {
            return Err(format!(
                "a construction with a defaulted argument of `{name}`, whose defaults were not recorded"
            ));
        };
        let frame = self.constructor_frame(key.class);
        let mut parameters = vec![Ty::Obj(declaration.fq_name_id(), &[])];
        let mut supplied = Vec::new();
        for (ordinal, ty) in frame.iter().enumerate() {
            if key.omitted.contains(&(ordinal as u32)) {
                continue;
            }
            supplied.push(ordinal);
            parameters.push(*ty);
        }
        let signature = self.signature_of(&parameters, Ty::Unit)?;
        let target = self.classes[key.class as usize]
            .constructor
            .expect("checked when the wrapper was declared");
        let mut slots = vec![Ty::Obj(declaration.fq_name_id(), &[])];
        slots.extend(frame.iter().copied());
        let omitted = key.omitted.clone();
        let label = format!("{name}.<init>$defaults");
        self.emit_function(id, signature, Carrier::Void, &label, &mut |body, params| {
            // `this` at slot 0 and every parameter at its own ordinal, so a default reading an
            // earlier one finds it. Each slot is declared ONCE: declaring it again hands back a
            // fresh, undefined variable and loses what was put in it.
            let mut variables = Vec::with_capacity(slots.len());
            for (slot, ty) in slots.iter().enumerate() {
                variables.push(body.declare_value(slot as u32, *ty)?);
            }
            body.builder.def_var(variables[0], params[0]);
            for (index, ordinal) in supplied.iter().enumerate() {
                body.builder
                    .def_var(variables[*ordinal + 1], params[index + 1]);
            }
            // Declaration order, because a later default may read an earlier parameter.
            for ordinal in &omitted {
                let Some(Some(expression)) = defaults.get(*ordinal as usize) else {
                    return Err(format!(
                        "an omitted constructor argument with no default (`{name}`)"
                    ));
                };
                let slot = *ordinal as usize + 1;
                let Some(value) = body.coerce(*expression, slots[slot])? else {
                    return Err(format!("a `Unit` default constructor argument (`{name}`)"));
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
            let func_ref = body.func_ref(target);
            body.builder.ins().call(func_ref, &arguments);
            body.builder.ins().return_(&[]);
            body.terminate();
            Ok(())
        })
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Construct through the wrapper for this omission shape: the object is allocated here — an
    /// allocation is not a default's to make — and the wrapper fills the frame and runs the
    /// constructor.
    pub(super) fn defaulted_construction(
        &mut self,
        internal: TypeName,
        args: &[u32],
        selected: Option<&[Ty]>,
        omitted: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let name = internal.render();
        let class = self.file.class_of(internal, "construction of")?;
        let declaration = &self.file.ir.classes[class as usize];
        let primary: Vec<Ty> = declaration
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .collect();
        // Only the PRIMARY constructor's defaults are filled here: a secondary's live on the
        // constructor rather than the class, and naming one by its parameter list cannot be done
        // against a list with holes in it.
        if selected.is_some_and(|selected| selected != primary) {
            return Err(format!(
                "a defaulted call to a secondary constructor (`{name}`)"
            ));
        }
        let key = super::defaults::CtorOmission {
            class,
            omitted: omitted.to_vec(),
        };
        let Some(&id) = self.file.default_constructors.get(&key) else {
            return Err(format!("a constructor default argument (`{name}`)"));
        };
        let parameters: Vec<Ty> = self.file.ir.classes[class as usize]
            .ctor_args
            .iter()
            .enumerate()
            .filter(|(ordinal, _)| !omitted.contains(&(*ordinal as u32)))
            .map(|(ordinal, argument)| {
                captures::physical_ty(self.file.ir, class, ordinal as u32, argument.ty)
            })
            .collect();
        if args.len() != parameters.len() {
            return Err(format!(
                "a construction supplying {} of {} arguments (`{name}`)",
                args.len(),
                parameters.len()
            ));
        }
        // Arguments first, then the allocation: an argument that allocates cannot then leave a
        // half-built object for the collector to find with a stale field.
        let mut arguments = self.arguments(args, &parameters)?;
        if self.terminated {
            return Ok(None);
        }
        let descriptor = self.file.classes[class as usize].descriptor;
        let size = self.file.model.layout(class).instance_size;
        let object = self.allocate(descriptor, size)?;
        arguments.insert(0, object);
        let func_ref = self.func_ref(id);
        self.builder.ins().call(func_ref, &arguments);
        Ok(Some(object))
    }
}
