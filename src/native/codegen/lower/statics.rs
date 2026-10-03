//! Top-level properties: one global slot each, initialized at program start.
//!
//! A Kotlin top-level property is a *declaration*, and the JVM's realization of it — a private
//! static field, a public `getX`/`setX` pair, an `access$get<X>$p` bridge when a sibling class
//! reads a private one — is entirely a JVM answer to JVM visibility rules. None of it survives
//! here: a native program reads and writes the slot directly, because there is no verifier to
//! satisfy and no class boundary to cross. The IR says the same thing to both backends
//! ([`IrExpr::GetStatic`] / [`IrExpr::SetStatic`] into `IrFile::statics`); only the realization
//! differs, which is the point of the common IR.
//!
//! **Where the initializers run.** On the JVM they run in `<clinit>`, when the facade class is
//! first touched. A program touches the facade by calling its `main`, so running them at process
//! start — in declaration order, before the entry function — is observationally the same thing for
//! a program whose entry lives in the file that declares them. That is every program the generator
//! accepts today, because a cross-file call is still declined; when files start calling each other,
//! this becomes a real initialization-order question and is answered then, not guessed at now.
//!
//! **The collector has to be told.** A freestanding program cannot find its own data section, so a
//! reference-typed slot is registered with `kt_gc_add_global_root` — and registered BEFORE the
//! first initializer runs, not as each one is assigned: the second initializer may allocate, and
//! the collection that follows must already trace the first slot or it frees what that slot alone
//! holds.

use super::*;

/// Every slot is one machine word, whatever it holds. A `Boolean` occupies one byte of it and the
/// rest goes unread; keeping the stride uniform means the collector's root registration and the
/// slot's own alignment need no per-type reasoning.
const SLOT_SIZE: usize = 8;

impl<'a> FileLowering<'a> {
    /// The symbol of this file's static initializer.
    fn statics_init_symbol(&self) -> String {
        match self
            .ir
            .package
            .as_deref()
            .map(super::super::super::symbols::c_identifier)
        {
            Some(package) if !package.is_empty() => format!("kt_{package}_statics_init"),
            _ => "kt_statics_init".to_string(),
        }
    }

    /// Can this property's initializer tell program start from the moment its owner is touched?
    ///
    /// Two cannot. A `const val`'s initializer is a compile-time constant. A delegated property's
    /// `KProperty` metadata is a property REFERENCE, which is one emitted object per property with
    /// no state of its own and nothing to allocate — the same value however early it is asked for.
    /// Running either at program start is indistinguishable from running it with the owner, so the
    /// placement fact the owner carries has nothing to say about them.
    fn order_independent(&self, declaration: &IrStatic) -> bool {
        declaration.is_const
            || matches!(
                self.ir.expr(declaration.init),
                IrExpr::Checked(IrCheckedOperation::PropertyReference { .. })
            )
    }

    /// Declare a slot per top-level property. Declared before any body is compiled, because a
    /// function may read a property declared after it.
    pub(super) fn declare_statics(&mut self) -> Result<(), Unsupported> {
        for (index, declaration) in self.ir.statics.iter().enumerate() {
            // An owner is a PLACEMENT fact the JVM needs — a companion's `const val` becomes a
            // field of the OUTER class there. There is no facade here for a slot to be placed
            // differently from, so the owner says nothing about the slot; what it could say
            // something about is WHEN the initializer runs, since a companion's runs with the
            // companion rather than at program start. An initializer that cannot OBSERVE the
            // difference settles it; anything else owned by a class still declines rather than
            // guess at the order.
            if let Some(owner) = declaration.owner {
                if !self.order_independent(declaration) {
                    return Err(format!(
                        "a non-`const` property stored on `{}`",
                        owner.render()
                    ));
                }
            }
            if carrier(declaration.ty) == Carrier::Void {
                return Err(format!(
                    "a `Unit`-typed top-level property (`{}`)",
                    declaration.name
                ));
            }
            let name = format!(
                "kt_static_{index}_{}",
                super::super::super::symbols::c_identifier(&declaration.name)
            );
            let id = self.declare_local_data(&name, true)?;
            let mut description = DataDescription::new();
            description.define_zeroinit(SLOT_SIZE);
            description.set_align(SLOT_SIZE as u64);
            self.module
                .define_data(id, &description)
                .map_err(|error| format!("defining `{name}` ({error})"))?;
            self.statics.push(id);
        }
        Ok(())
    }

    /// Emit the function that roots every reference slot and runs every initializer in declaration
    /// order. Returns `None` when the file declares no top-level property.
    pub(super) fn define_statics_init(&mut self) -> Result<Option<FuncId>, Unsupported> {
        if self.ir.statics.is_empty() {
            return Ok(None);
        }
        let symbol = self.statics_init_symbol();
        let id = self.declare_local_function(&symbol, &[], Ty::Unit)?;
        let signature = self.signature_of(&[], Ty::Unit)?;
        let declarations: Vec<(DataId, Ty, u32)> = self
            .ir
            .statics
            .iter()
            .enumerate()
            .map(|(index, declaration)| (self.statics[index], declaration.ty, declaration.init))
            .collect();

        self.emit_function(id, signature, Ty::Unit, &symbol, &mut |body, _| {
            for (slot, ty, _) in &declarations {
                if carrier(*ty) != Carrier::Ref {
                    continue;
                }
                let address = body.data_address(*slot);
                body.runtime_call("kt_gc_add_global_root", &[any()], Ty::Unit, &[address])?;
            }
            for (slot, ty, init) in &declarations {
                let value = body.coerce(*init, *ty)?;
                if body.terminated {
                    return Ok(());
                }
                let Some(value) = value else {
                    return Err("a top-level property initialized from a `Unit` value".to_string());
                };
                let address = body.data_address(*slot);
                body.builder.ins().store(trusted(), value, address, 0);
            }
            Ok(())
        })?;
        Ok(Some(id))
    }
}

/// How an access to a top-level property is realized.
///
/// The rule is the source's own shape, not a convention: **common lowering emits an accessor
/// function exactly when the source wrote one** (`val doubled get() = …` becomes `getDoubled`),
/// and a property with no written accessor appears only as an entry in `IrFile::statics`. So an
/// accessor, where one exists, is the realization — it is the code the programmer wrote, and its
/// `field` reads lower to the slot anyway — and everything else is the slot itself. None of the
/// JVM's synthesized `getX`/`setX` ABI is involved; that is a separate realization pass over the
/// same IR.
pub(super) enum TopLevel {
    /// The backing slot, by index into `IrFile::statics`.
    Slot(u32),
    /// A source-written accessor, by index into `IrFile::functions`.
    Accessor(u32),
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// The declared name of a checked property.
    pub(super) fn checked_property_name(
        &self,
        target: &crate::fir::PropertyId,
    ) -> Result<String, Unsupported> {
        self.file
            .ir
            .checked_properties
            .get(target)
            .map(|property| property.name.clone())
            .ok_or_else(|| "a property with no checked declaration".to_string())
    }

    /// Resolve a checked top-level property to what realizes it.
    pub(super) fn top_level(&self, name: &str, setter: bool) -> Result<TopLevel, Unsupported> {
        let accessor = if setter {
            crate::names::property_setter_name(name)
        } else {
            crate::names::property_getter_name(name)
        };
        let arity = usize::from(setter);
        let written = self.file.ir.functions.iter().position(|function| {
            function.name == accessor
                && function.dispatch_receiver.is_none()
                && function.params.len() == arity
        });
        if let Some(index) = written {
            return Ok(TopLevel::Accessor(index as u32));
        }
        let stored = self
            .file
            .ir
            .statics
            .iter()
            .position(|declaration| declaration.is_facade_owned() && declaration.name == name);
        match stored {
            Some(index) => Ok(TopLevel::Slot(index as u32)),
            None => Err(format!(
                "a top-level property with neither storage nor an accessor (`{name}`)"
            )),
        }
    }

    /// Call a source-written top-level accessor.
    pub(super) fn accessor_call(
        &mut self,
        function: u32,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        let id = self.file.functions[function as usize]
            .ok_or_else(|| "an accessor with no body".to_string())?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    pub(super) fn top_level_read(&mut self, name: &str) -> Result<Option<Value>, Unsupported> {
        match self.top_level(name, false)? {
            TopLevel::Slot(index) => self.static_read(index),
            TopLevel::Accessor(function) => self.accessor_call(function, &[]),
        }
    }

    pub(super) fn top_level_write(&mut self, name: &str, value: u32) -> Result<(), Unsupported> {
        match self.top_level(name, true)? {
            TopLevel::Slot(index) => self.static_write(index, value),
            TopLevel::Accessor(function) => {
                let ty = self.file.ir.functions[function as usize].params[0];
                let value = self.coerce(value, ty)?;
                if self.terminated {
                    return Ok(());
                }
                let Some(value) = value else {
                    return Err("a `Unit` value assigned to a top-level property".to_string());
                };
                self.accessor_call(function, &[value])?;
                Ok(())
            }
        }
    }

    pub(super) fn static_read(&mut self, index: u32) -> Result<Option<Value>, Unsupported> {
        let ty = self.file.ir.statics[index as usize].ty;
        let slot = self.file.statics[index as usize];
        let clif = carrier(ty).clif().expect("declined at declaration");
        let address = self.data_address(slot);
        Ok(Some(self.builder.ins().load(clif, trusted(), address, 0)))
    }

    pub(super) fn static_write(&mut self, index: u32, value: u32) -> Result<(), Unsupported> {
        let ty = self.file.ir.statics[index as usize].ty;
        let slot = self.file.statics[index as usize];
        let value = self.coerce(value, ty)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err("a `Unit` value assigned to a top-level property".to_string());
        };
        let address = self.data_address(slot);
        self.builder.ins().store(trusted(), value, address, 0);
        Ok(())
    }
}

impl BodyLowering<'_, '_, '_> {
    /// Read or write a property that has a receiver its owner does not supply: an extension
    /// property (`val Foo.bar get() = …`) or one with context parameters.
    ///
    /// There is no storage to reach — an extension property cannot have a backing field, because
    /// there is no object of its own to keep one in — so every such access is a call to the
    /// accessor the checked lowering already built. What that lowering also recorded is the
    /// accessor's parameter ORDER, and it is the one Kotlin declares: the context parameters,
    /// then the extension receiver, then (for a setter) the value. Reading the order back from
    /// `IrFile::local_property_layouts` is the point of that table; deriving it from the
    /// accessor's name or arity would be guessing at what is already written down.
    pub(super) fn receiver_property(
        &mut self,
        target: &crate::fir::PropertyId,
        dispatch_receiver: Option<u32>,
        extension_receiver: Option<u32>,
        context_arguments: &[u32],
        value: Option<u32>,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(layout) = self.file.ir.local_property_layouts.get(target).cloned() else {
            return Err("an extension or context property with no realization".to_string());
        };
        // Which accessor, and whether it belongs to an owner — a member accessor is an instance
        // method and may be overridden, so it is reached through the owner's vtable slot rather
        // than called directly. The layout says which of the three shapes this is; nothing here
        // derives it from an accessor's name or arity.
        let (function, owner) = match &layout {
            IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. } => {
                (accessor(*getter, *setter, value.is_some())?, None)
            }
            IrLocalPropertyLayout::MemberExtension {
                owner,
                getter,
                setter,
                ..
            } => (accessor(*getter, *setter, value.is_some())?, Some(*owner)),
            // A MEMBER with a receiver reaching here has context parameters — an ordinary member
            // read takes the storage path and never arrives.
            IrLocalPropertyLayout::Member {
                owner,
                getter,
                setter,
                ..
            } => {
                let Some(getter) = *getter else {
                    return Err("a context property with no getter".to_string());
                };
                (accessor(getter, *setter, value.is_some())?, Some(*owner))
            }
            IrLocalPropertyLayout::TopLevelStorage { .. } => {
                return Err("a stored property reached through a receiver".to_string())
            }
        };
        let parameters = self.file.ir.functions[function as usize].params.clone();
        let supplied = context_arguments
            .iter()
            .copied()
            .chain(extension_receiver)
            .chain(value)
            .collect::<Vec<_>>();
        if supplied.len() != parameters.len() {
            return Err(format!(
                "an accessor taking {} operands for {} parameters",
                parameters.len(),
                supplied.len()
            ));
        }
        let mut arguments = Vec::with_capacity(supplied.len() + 1);
        // The owner's instance is the accessor's first operand, exactly as it is for any other
        // instance method; the declared parameters follow in the order the layout recorded.
        let dispatch = match owner {
            None => None,
            Some(owner) => {
                let receiver = dispatch_receiver
                    .ok_or_else(|| "a member accessor with no receiver".to_string())?;
                let Some(object) = self.receiver(receiver)? else {
                    return Ok(None);
                };
                arguments.push(object);
                Some((owner, object))
            }
        };
        for (operand, ty) in supplied.iter().zip(&parameters) {
            let Some(argument) = self.coerce(*operand, *ty)? else {
                return Err("a `Unit` operand of a property accessor".to_string());
            };
            if self.terminated {
                return Ok(None);
            }
            arguments.push(argument);
        }
        let Some((owner, object)) = dispatch else {
            let id = self.file.functions[function as usize]
                .ok_or_else(|| "a property accessor with no body".to_string())?;
            let func_ref = self.func_ref(id);
            let call = self.emit_call(func_ref, &arguments)?;
            return Ok(self.builder.inst_results(call).first().copied());
        };
        let class = self.file.class_of(owner, "a property accessor of")?;
        let key = model::function_key(self.file.ir, class, function);
        let Some(slot) = self.file.model.slot(class, &key) else {
            return Err("a member accessor with no dispatch slot".to_string());
        };
        let ret = self.file.ir.functions[function as usize].ret;
        self.dispatch(object, slot, &parameters, ret, &arguments[1..])
    }
}

/// The accessor a read or a write of a property with a receiver calls.
fn accessor(getter: FunId, setter: Option<FunId>, writing: bool) -> Result<FunId, Unsupported> {
    match writing {
        false => Ok(getter),
        true => setter.ok_or_else(|| "a write to a read-only property".to_string()),
    }
}
