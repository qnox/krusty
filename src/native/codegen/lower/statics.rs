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

use super::objects::trusted;
use super::*;

/// Every slot is one machine word, whatever it holds. A `Boolean` occupies one byte of it and the
/// rest goes unread; keeping the stride uniform means the collector's root registration and the
/// slot's own alignment need no per-type reasoning.
const SLOT_SIZE: usize = 8;

impl<'a> FileLowering<'a> {
    /// The symbol of this file's static initializer.
    fn statics_init_symbol(&self) -> String {
        match self.ir.package.as_deref().map(model::c_identifier) {
            Some(package) if !package.is_empty() => format!("kt_{package}_statics_init"),
            _ => "kt_statics_init".to_string(),
        }
    }

    /// Declare a slot per top-level property. Declared before any body is compiled, because a
    /// function may read a property declared after it.
    pub(super) fn declare_statics(&mut self) -> Result<(), Unsupported> {
        for (index, declaration) in self.ir.statics.iter().enumerate() {
            if let Some(owner) = declaration.owner {
                return Err(format!(
                    "a property stored on `{}` (a companion's storage lives on its outer class)",
                    owner.render()
                ));
            }
            check_carried(declaration.ty)?;
            if carrier(declaration.ty) == Carrier::Void {
                return Err(format!(
                    "a `Unit`-typed top-level property (`{}`)",
                    declaration.name
                ));
            }
            let name = format!(
                "kt_static_{index}_{}",
                model::c_identifier(&declaration.name)
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

        self.emit_function(id, signature, Carrier::Void, &symbol, &mut |body, _| {
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
enum TopLevel {
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
    fn top_level(&self, name: &str, setter: bool) -> Result<TopLevel, Unsupported> {
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
    fn accessor_call(
        &mut self,
        function: u32,
        arguments: &[Value],
    ) -> Result<Option<Value>, Unsupported> {
        let id = self.file.functions[function as usize]
            .ok_or_else(|| "an accessor with no body".to_string())?;
        let func_ref = self.func_ref(id);
        let call = self.builder.ins().call(func_ref, arguments);
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
