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
//! a program whose entry lives in the file that declares them, and when another file first calls
//! into this one. Both go through `kt_fileinit_<source>`, which runs the initializers at most once.
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
            || declaration.init.is_none_or(|init| {
                matches!(
                    self.ir.expr(init),
                    IrExpr::Checked(IrCheckedOperation::PropertyReference { .. })
                )
            })
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
                    return Err(declined!(
                        "a non-`const` property stored on `{}`",
                        owner.render()
                    ));
                }
            }
            if self.carrier(declaration.ty) == Carrier::Void {
                return Err(declined!(
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
        let declarations: Vec<(DataId, Ty, Option<u32>)> = self
            .ir
            .statics
            .iter()
            .enumerate()
            .map(|(index, declaration)| (self.statics[index], declaration.ty, declaration.init))
            .collect();

        self.emit_function(id, signature, Ty::Unit, &symbol, &mut |body, _| {
            for (slot, ty, _) in &declarations {
                if body.carrier(*ty) != Carrier::Ref {
                    continue;
                }
                let address = body.data_address(*slot);
                body.runtime_call("kt_gc_add_global_root", &[any()], Ty::Unit, &[address])?;
            }
            for (slot, ty, init) in &declarations {
                let Some(init) = init else {
                    continue;
                };
                let value = body.coerce(*init, *ty)?;
                if body.terminated {
                    return Ok(());
                }
                let Some(value) = value else {
                    return Err("a top-level property initialized from a `Unit` value".into());
                };
                let address = body.data_address(*slot);
                body.builder.ins().store(trusted(), value, address, 0);
            }
            Ok(())
        })?;
        Ok(Some(id))
    }

    /// `kt_fileinit_<source>`: this file's top-level initializers, run at most once.
    ///
    /// The entry calls it before `main` or `box`. A call into this file from another file calls it
    /// first, so a property defined here has its value even when this file is not the entry. The
    /// flag is set before the initializers run: a property initializer that reaches back into this
    /// file sees the default, which is what Kotlin does, and does not recurse.
    pub(super) fn define_file_init(
        &mut self,
        source: crate::fir::SourceFileId,
        statics: Option<FuncId>,
    ) -> Result<FuncId, Unsupported> {
        let void = Signature::new(CallConv::SystemV);
        let symbol = super::super::super::symbols::file_init_symbol(source);
        let id = self
            .module
            .declare_function(&symbol, Linkage::Hidden, &void)
            .map_err(|error| format!("declaring `{symbol}` ({error})"))?;
        let flag_name = format!("kt_fileinit_done_{}", source.raw());
        let flag = self
            .module
            .declare_data(&flag_name, Linkage::Local, true, false)
            .map_err(|error| format!("declaring `{flag_name}` ({error})"))?;
        let mut description = DataDescription::new();
        description.define(Box::new([0u8]));
        description.set_align(1);
        self.module
            .define_data(flag, &description)
            .map_err(|error| format!("defining `{flag_name}` ({error})"))?;

        let frontend_config = self.module.target_config();
        let mut context = self.module.make_context();
        context.func.signature = void;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let head = builder.create_block();
            let run = builder.create_block();
            let done = builder.create_block();
            builder.switch_to_block(head);
            let global = self.module.declare_data_in_func(flag, builder.func);
            // A Cranelift value belongs to the block that defines it. The flag's address is a
            // global, so each block that touches the flag recomputes the pointer.
            let pointer = builder.ins().symbol_value(types::I64, global);
            let state = builder.ins().load(types::I8, trusted(), pointer, 0);
            let already = builder.ins().icmp_imm_u(IntCC::NotEqual, state, 0);
            builder.ins().brif(already, done, &[], run, &[]);
            builder.seal_block(head);
            builder.switch_to_block(run);
            let pointer = builder.ins().symbol_value(types::I64, global);
            let one = builder.ins().iconst(types::I8, 1);
            builder.ins().store(trusted(), one, pointer, 0);
            if let Some(statics) = statics {
                let statics_ref = self.module.declare_func_in_func(statics, builder.func);
                builder.ins().call(statics_ref, &[]);
            }
            builder.ins().jump(done, &[]);
            builder.seal_block(run);
            builder.switch_to_block(done);
            builder.ins().return_(&[]);
            builder.seal_block(done);
            builder.finalize(frontend_config);
        }
        self.module
            .define_function(id, &mut context)
            .map_err(|error| format!("defining `{symbol}` ({error})"))?;
        Ok(id)
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
    /// Resolve a checked top-level property to what realizes it.
    pub(super) fn top_level(
        &self,
        target: &crate::fir::PropertyId,
        setter: bool,
    ) -> Result<TopLevel, Unsupported> {
        match self.file.ir.local_property_layouts.get(target) {
            Some(IrLocalPropertyLayout::TopLevelStorage {
                storage,
                getter,
                setter: property_setter,
                ..
            }) => Ok(match if setter { *property_setter } else { *getter } {
                Some(accessor) => TopLevel::Accessor(accessor),
                None => TopLevel::Slot(*storage),
            }),
            Some(IrLocalPropertyLayout::TopLevelAccessor {
                getter,
                setter: property_setter,
                ..
            }) => {
                let accessor = if setter {
                    property_setter
                        .ok_or_else(|| "a read-only top-level property was written".to_string())?
                } else {
                    *getter
                };
                Ok(TopLevel::Accessor(accessor))
            }
            Some(
                IrLocalPropertyLayout::Member { .. }
                | IrLocalPropertyLayout::MemberExtension { .. },
            ) => Err("a member property reached top-level realization".into()),
            None => Err("a top-level property with no recorded realization".into()),
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

    pub(super) fn top_level_read(
        &mut self,
        target: &crate::fir::PropertyId,
    ) -> Result<Option<Value>, Unsupported> {
        match self.top_level(target, false)? {
            TopLevel::Slot(index) => {
                let declaration = &self.file.ir.statics[index as usize];
                let guarded = declaration.is_lateinit;
                let name = declaration.name.clone();
                let value = self.static_read(index)?;
                if guarded {
                    let value = value.expect("a `lateinit` property has a reference carrier");
                    self.lateinit_guard(value, &name)?;
                }
                Ok(value)
            }
            TopLevel::Accessor(function) => self.accessor_call(function, &[]),
        }
    }

    pub(super) fn top_level_write(
        &mut self,
        target: &crate::fir::PropertyId,
        value: u32,
    ) -> Result<(), Unsupported> {
        match self.top_level(target, true)? {
            TopLevel::Slot(index) => self.static_write(index, value),
            TopLevel::Accessor(function) => {
                let ty = self.file.ir.functions[function as usize].params[0];
                let value = self.coerce(value, ty)?;
                if self.terminated {
                    return Ok(());
                }
                let Some(value) = value else {
                    return Err("a `Unit` value assigned to a top-level property".into());
                };
                self.accessor_call(function, &[value])?;
                Ok(())
            }
        }
    }

    /// The carrier a write of a top-level property must produce.
    pub(super) fn top_level_written_ty(
        &mut self,
        target: &crate::fir::PropertyId,
    ) -> Result<Ty, Unsupported> {
        Ok(match self.top_level(target, true)? {
            TopLevel::Slot(index) => self.file.ir.statics[index as usize].ty,
            TopLevel::Accessor(function) => self.file.ir.functions[function as usize].params[0],
        })
    }

    /// [`Self::top_level_write`] with the value already evaluated at that carrier.
    pub(super) fn top_level_write_value(
        &mut self,
        target: &crate::fir::PropertyId,
        value: Value,
    ) -> Result<(), Unsupported> {
        match self.top_level(target, true)? {
            TopLevel::Slot(index) => {
                let slot = self.file.statics[index as usize];
                let address = self.data_address(slot);
                self.builder.ins().store(trusted(), value, address, 0);
                Ok(())
            }
            TopLevel::Accessor(function) => {
                self.accessor_call(function, &[value])?;
                Ok(())
            }
        }
    }

    pub(super) fn static_read(&mut self, index: u32) -> Result<Option<Value>, Unsupported> {
        let ty = self.file.ir.statics[index as usize].ty;
        let slot = self.file.statics[index as usize];
        let clif = self.carrier(ty).clif().expect("declined at declaration");
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
            return Err("a `Unit` value assigned to a top-level property".into());
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
            return Err("an extension or context property with no realization".into());
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
                    return Err("a context property with no getter".into());
                };
                (accessor(getter, *setter, value.is_some())?, Some(*owner))
            }
            IrLocalPropertyLayout::TopLevelStorage { .. } => {
                return Err("a stored property reached through a receiver".into())
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
            return Err(declined!(
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
                return Err("a `Unit` operand of a property accessor".into());
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
            return Err("a member accessor with no dispatch slot".into());
        };
        let ret = self.file.ir.functions[function as usize].ret;
        self.dispatch(object, slot, &parameters, ret, &arguments[1..])
    }
}

/// The accessor a read or a write of a property with a receiver calls.
fn accessor(getter: FunId, setter: Option<FunId>, writing: bool) -> Result<FunId, Unsupported> {
    match writing {
        false => Ok(getter),
        true => setter.ok_or_else(|| "a write to a read-only property".into()),
    }
}
