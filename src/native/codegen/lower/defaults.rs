//! Constructions that leave arguments out.
//!
//! A call of a function that leaves arguments out goes through the declaration's shared
//! default-argument entry (`crate::backend::default_arguments`). A construction does not reach
//! that lowering yet, so each shape of omission a construction uses gets a small wrapper here: a
//! function taking exactly the arguments that were supplied, which declares the constructor's
//! whole parameter frame, fills the omitted slots by evaluating their defaults in declaration
//! order, and calls the real constructor.

use super::super::super::captures;
use super::*;
use crate::types::TypeName;

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
    /// Which constructor of that class, as an index into `secondary_ctors`, or `None` for the
    /// PRIMARY one. A secondary's defaults live on the constructor rather than on the class, so
    /// the two cannot share a wrapper even when they omit the same ordinals.
    pub secondary: Option<usize>,
    /// PHYSICAL constructor-parameter ordinals the construction leaves out, ascending. A
    /// construction's own `defaults` are SOURCE-value ordinals, which begin after the compiler's
    /// leading operands (`default_prefix_count`) — captures and an outer receiver, which are never
    /// omitted.
    pub omitted: Vec<u32>,
}

impl<'a> FileLowering<'a> {
    /// The physical parameter frame of one of a class's constructors — the primary's, or a
    /// SECONDARY's, which leads with the compiler-supplied prefix every constructor of the class
    /// shares before its own declared parameters.
    pub(super) fn constructor_frame_of(
        &self,
        class: ClassId,
        secondary: Option<usize>,
    ) -> Option<Vec<Ty>> {
        let Some(index) = secondary else {
            return Some(self.constructor_frame(class));
        };
        let constructor = self.ir.classes[class as usize].secondary_ctors.get(index)?;
        let mut frame = constructor.prefix_params.clone();
        frame.extend(constructor.params.iter().copied());
        Some(frame)
    }

    /// The defaults of one of a class's constructors, parallel to the frame above.
    ///
    /// The primary's live on the CLASS, as expression ids beside its parameters; a secondary's
    /// live on the constructor. Both are answered in PHYSICAL ordinals, so the prefix a secondary
    /// leads with is padded out — nothing omits a compiler-supplied operand.
    fn constructor_defaults_of(
        &self,
        class: ClassId,
        secondary: Option<usize>,
    ) -> Option<Vec<Option<u32>>> {
        let declaration = &self.ir.classes[class as usize];
        let Some(index) = secondary else {
            return self
                .ir
                .class_ctor_defaults_name(declaration.fq_name_id())
                .cloned();
        };
        let constructor = declaration.secondary_ctors.get(index)?;
        let mut defaults = vec![None; constructor.prefix_params.len()];
        defaults.extend(constructor.defaults.iter().copied());
        Some(defaults)
    }

    /// Which constructor of `class` a selected parameter list names: `None` for the primary, and
    /// the index of the secondary that declares exactly those parameters otherwise.
    ///
    /// A construction that names no list is the primary's, which is what an enum constant and a
    /// `super(…)` delegation both leave unsaid.
    pub(super) fn selected_constructor(
        &self,
        class: ClassId,
        selected: Option<&[Ty]>,
    ) -> Option<Option<usize>> {
        let declaration = &self.ir.classes[class as usize];
        let primary: Vec<Ty> = declaration
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .collect();
        let Some(selected) = selected else {
            return Some(None);
        };
        if selected == primary {
            return Some(None);
        }
        declaration
            .secondary_ctors
            .iter()
            .position(|candidate| candidate.params == selected)
            .map(Some)
    }

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
    ) -> Option<(TypeName, Option<Vec<Ty>>, Vec<u32>)> {
        let IrExpr::New {
            internal,
            defaults,
            default_prefix_count,
            ctor_params,
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
        Some((*internal, ctor_params.clone(), omitted))
    }

    /// Which constructor of its superclass `class`'s `super(…)` delegation calls: `None` for the
    /// primary, the secondary's index otherwise, as the checker recorded it. `Err` when the
    /// recorded ordinal names no constructor this file declares for the superclass.
    pub(super) fn super_constructor(&self, class: ClassId) -> Result<Option<usize>, Unsupported> {
        let declaration = &self.ir.classes[class as usize];
        let Some(secondary) = declaration.super_ctor.ordinal.checked_sub(1) else {
            return Ok(None);
        };
        let parent = self
            .ir
            .class_id_by_name(declaration.superclass)
            .map(|parent| &self.ir.classes[parent as usize]);
        match parent.and_then(|parent| parent.secondary_ctors.get(secondary as usize)) {
            Some(constructor) if constructor.prefix_params.is_empty() => {
                Ok(Some(secondary as usize))
            }
            _ => Err(declined!(
                "a superclass call of `{}` to a constructor its superclass does not declare here",
                declaration.fq_name()
            )),
        }
    }

    /// The physical ordinals a class's `super(…)` delegation omits, if any, with the superclass
    /// constructor it calls.
    ///
    /// The IR records SEMANTIC ordinals — a backend's own prefix is not its to know — so they are
    /// shifted past the superclass's compiler-supplied leading parameters here.
    pub(super) fn omitted_super_arguments(
        &self,
        class: ClassId,
    ) -> Option<(TypeName, Option<usize>, Vec<u32>)> {
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
        // A base whose only constructor is a SECONDARY is reached this way — `class C : B()`
        // where `B`'s one constructor defaults its argument — and reading the primary's frame
        // for it would fill a frame that does not exist.
        let secondary = self.super_constructor(class).ok()?;
        Some((declaration.superclass, secondary, physical))
    }

    /// Declare a wrapper for every omission shape the file's constructions use.
    pub(super) fn declare_default_constructors(&mut self) -> Result<(), Unsupported> {
        // A `class B : A()` whose base leaves arguments out needs the same wrapper a `A()` written
        // as an expression would, and the delegation is not an expression, so both are collected.
        let shapes: Vec<(TypeName, Option<usize>, Vec<u32>)> = (0..self.ir.classes.len()
            as ClassId)
            .filter_map(|class| self.omitted_super_arguments(class))
            .chain(
                self.ir
                    .exprs
                    .iter()
                    .filter_map(Self::omitted_constructor_arguments)
                    .filter_map(|(internal, selected, omitted)| {
                        let class = self.ir.class_id_by_name(internal)?;
                        let secondary = self.selected_constructor(class, selected.as_deref())?;
                        Some((internal, secondary, omitted))
                    }),
            )
            // An enum CONSTANT leaving an argument out is the same omission written a third way:
            // `RED` where the enum's constructor declares `(val rgb: Int = 0)` is not an
            // expression and not a supertype call, so neither collector above sees it. A constant
            // with a BODY is left out: its instance is a synthesized subclass whose own
            // constructor takes the entry's arguments, and the defaults are recorded against the
            // enum rather than against it.
            .chain(self.ir.classes.iter().flat_map(|class| {
                class.enum_entries.iter().filter_map(move |entry| {
                    if entry.default_parameters.is_empty() || entry.subclass.is_some() {
                        return None;
                    }
                    let mut omitted = entry.default_parameters.clone();
                    omitted.sort_unstable();
                    let secondary = (!entry.constructor.primary())
                        .then_some(entry.constructor.ordinal as usize - 1);
                    Some((class.fq_name_id(), secondary, omitted))
                })
            }))
            .collect();
        for (internal, secondary, omitted) in shapes {
            // A construction naming a class this file does not declare, or a constructor whose
            // parameter list names none of this class's, declares no wrapper; the call site then
            // finds none and declines there, rather than this phase failing the whole file for one
            // construction.
            let Some(class) = self.ir.class_id_by_name(internal) else {
                continue;
            };
            let declared = match secondary {
                None => self.classes[class as usize].constructor,
                Some(index) => self.classes[class as usize].secondaries.get(index).copied(),
            };
            if declared.is_none() {
                continue;
            }
            let key = CtorOmission {
                class,
                secondary,
                omitted,
            };
            if self.default_constructors.contains_key(&key) {
                continue;
            }
            let Some(frame) = self.constructor_frame_of(class, secondary) else {
                continue;
            };
            // The constructor fills the object the call site allocated, a value class's box
            // included, so `this` is a reference whatever the class's own carrier is.
            let mut parameters = vec![any()];
            for (ordinal, ty) in frame.iter().enumerate() {
                if key.omitted.contains(&(ordinal as u32)) {
                    continue;
                }
                parameters.push(*ty);
            }
            let symbol = format!(
                "kt_{}__init{}__defaults{}",
                self.class_base(class),
                match secondary {
                    None => String::new(),
                    Some(index) => format!("#{index}"),
                },
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
        let Some(defaults) = self.constructor_defaults_of(key.class, key.secondary) else {
            return Err(declined!(
                "a construction with a defaulted argument of `{name}`, whose defaults were not recorded"
            ));
        };
        let Some(frame) = self.constructor_frame_of(key.class, key.secondary) else {
            return Err(declined!(
                "a construction with a defaulted argument of `{name}`, whose constructor is absent"
            ));
        };
        let mut parameters = vec![any()];
        let mut supplied = Vec::new();
        for (ordinal, ty) in frame.iter().enumerate() {
            if key.omitted.contains(&(ordinal as u32)) {
                continue;
            }
            supplied.push(ordinal);
            parameters.push(*ty);
        }
        let signature = self.signature_of(&parameters, Ty::Unit)?;
        let target = match key.secondary {
            None => self.classes[key.class as usize].constructor,
            Some(index) => self.classes[key.class as usize]
                .secondaries
                .get(index)
                .copied(),
        }
        .expect("checked when the wrapper was declared");
        let mut slots = vec![any()];
        slots.extend(frame.iter().copied());
        let mut semantic_slots = vec![Ty::obj_name(declaration.fq_name)];
        match key.secondary {
            None => semantic_slots.extend(declaration.ctor_args.iter().map(|argument| argument.ty)),
            Some(index) => {
                let secondary = &declaration.secondary_ctors[index];
                semantic_slots.extend(secondary.prefix_params.iter().copied());
                semantic_slots.extend(secondary.params.iter().copied());
            }
        }
        let omitted = key.omitted.clone();
        let label = format!("{name}.<init>$defaults");
        self.emit_function(id, signature, Ty::Unit, &label, &mut |body, params| {
            body.record_semantic_slots(&semantic_slots);
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
                    return Err(declined!(
                        "an omitted constructor argument with no default (`{name}`)"
                    ));
                };
                let slot = *ordinal as usize + 1;
                let Some(value) = body.coerce(*expression, slots[slot])? else {
                    return Err(declined!(
                        "a `Unit` default constructor argument (`{name}`)"
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
            let func_ref = body.func_ref(target);
            body.emit_call(func_ref, &arguments)?;
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
        // Which constructor the selection names — the primary's, or a secondary's, whose defaults
        // live on the constructor rather than on the class.
        let Some(secondary) = self.file.selected_constructor(class, selected) else {
            return Err(declined!(
                "a defaulted call to a constructor of `{name}` this file does not declare"
            ));
        };
        let key = super::defaults::CtorOmission {
            class,
            secondary,
            omitted: omitted.to_vec(),
        };
        let Some(&id) = self.file.default_constructors.get(&key) else {
            return Err(declined!("a constructor default argument (`{name}`)"));
        };
        let parameters: Vec<Ty> = match secondary {
            // The primary's frame reads each parameter's PHYSICAL type, which a value-class
            // parameter's carrier differs from.
            None => self.file.ir.classes[class as usize]
                .ctor_args
                .iter()
                .enumerate()
                .filter(|(ordinal, _)| !omitted.contains(&(*ordinal as u32)))
                .map(|(ordinal, argument)| {
                    captures::physical_ty(self.file.ir, class, ordinal as u32, argument.ty)
                })
                .collect(),
            // A secondary's parameters are its own, and back no field, so there is no physical
            // form of them to look up.
            Some(index) => {
                let constructor = &self.file.ir.classes[class as usize].secondary_ctors[index];
                let mut frame = constructor.prefix_params.clone();
                frame.extend(constructor.params.iter().copied());
                frame
                    .into_iter()
                    .enumerate()
                    .filter(|(ordinal, _)| !omitted.contains(&(*ordinal as u32)))
                    .map(|(_, ty)| ty)
                    .collect()
            }
        };
        if args.len() != parameters.len() {
            return Err(declined!(
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
        self.emit_call(func_ref, &arguments)?;
        self.constructed(object, class)
    }
}
