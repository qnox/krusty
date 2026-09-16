//! Enum classes: constants, and what Kotlin lets a program ask of one.
//!
//! An enum reaches the backend almost bare. The checked IR keeps the constants as a list of names
//! and leaves every realization open: `Color.RED` is an [`IrExpr::EnumEntry`] node, `values()` is
//! [`IrExpr::EnumValues`], and `name`/`ordinal` are reads of `kotlin.Enum`'s properties — a class
//! no file declares. So all of it is built here.
//!
//! **Touching an enum builds all of it.** Kotlin initializes an enum class as a whole: every
//! constant in declaration order, and then the companion object — `E.init(x); E.init(y);
//! E.companion.init;` for a program that only ever mentions `E.Y`. So each constant has a static
//! slot, registered as a collector root, and there is one initializer per enum that fills every
//! slot in order and then asks for the companion; reading a constant, `values()` and `valueOf` all
//! run it first. A flag makes it idempotent, and is set BEFORE the constants are built so that a
//! constant's own constructor reaching back into the enum finds the work in progress rather than
//! starting it again — which is what the JVM's re-entrant class initialization does.
//!
//! **`values()` is a fresh array each call**, which is Kotlin's contract: the caller may write to
//! it without another caller seeing the change. `valueOf` compares the argument against each
//! constant's name in declaration order, and fails loudly on no match, as Kotlin does.

use super::objects::trusted;
use super::*;
use crate::types::TypeName;

/// What one enum class needs: a slot per constant, a flag saying the class has been initialized,
/// and the initializer itself.
#[derive(Clone)]
pub(super) struct EnumItems {
    pub(super) slots: Vec<DataId>,
    flag: DataId,
    initializer: FuncId,
}

impl EnumItems {
    /// The function that builds every constant of this enum and then its companion.
    pub(super) fn initializer(&self) -> FuncId {
        self.initializer
    }
}

impl<'a> FileLowering<'a> {
    /// Declare a slot per constant, plus the enum's own initializer, before any body is compiled.
    pub(super) fn declare_enum_entries(&mut self) -> Result<(), Unsupported> {
        for class in 0..self.ir.classes.len() as ClassId {
            // Enum-NESS, not "has constants". `enum class Empty` is a real Kotlin declaration with
            // no entries, and keying off the entry list made it indistinguishable from a class
            // that is not an enum at all — so it was skipped here and then panicked the moment
            // `Empty.values()` looked itself up. Registering it gives the natural answers: no
            // slots, so `values()` builds a zero-length array and `valueOf` finds no candidate and
            // takes the failure path, which is what Kotlin specifies for both.
            if !super::super::super::classes::is_enum(&self.ir.classes[class as usize]) {
                continue;
            }
            let base = self.class_base(class).to_string();
            let mut slots = Vec::new();
            for entry in self.ir.classes[class as usize].enum_entries.clone() {
                let symbol = format!("{base}_{}", model::c_identifier(&entry.name));
                slots.push(self.declare_local_data(&format!("kt_enum_{symbol}"), true)?);
            }
            let flag = self.declare_local_data(&format!("kt_enum_{base}_ready"), true)?;
            let initializer =
                self.declare_local_function(&format!("kt_enum_{base}_init"), &[], Ty::Unit)?;
            self.enum_entries.insert(
                class,
                EnumItems {
                    slots,
                    flag,
                    initializer,
                },
            );
        }
        Ok(())
    }

    /// Emit each enum's initializer.
    pub(super) fn define_enum_entries(&mut self) -> Result<(), Unsupported> {
        for class in self.enum_entries.keys().copied().collect::<Vec<_>>() {
            self.define_enum_initializer(class)?;
        }
        Ok(())
    }

    fn define_enum_initializer(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[class as usize].clone();
        let items = self.enum_entries[&class].clone();
        // A constant with a BODY is an instance of a synthesized subclass of the enum — that is
        // how it can override a member — so its own type, size and constructor are the ones used.
        // The subclass declares no fields of its own and delegates to the enum's constructor with
        // the same arguments, so nothing else about the constant changes.
        let mut shapes = Vec::with_capacity(declaration.enum_entries.len());
        for entry in &declaration.enum_entries {
            let owner = match entry.subclass {
                Some(subclass) => self.class_of(subclass, "the body of the enum constant")?,
                None => class,
            };
            let constructor = self.classes[owner as usize].constructor.ok_or_else(|| {
                format!(
                    "an enum constant whose class has no primary constructor (`{}.{}`)",
                    declaration.fq_name(),
                    entry.name
                )
            })?;
            shapes.push((
                self.classes[owner as usize].descriptor,
                self.model.layout(owner).instance_size,
                constructor,
            ));
        }
        for (slot, entry) in items.slots.iter().zip(&declaration.enum_entries) {
            if !entry.default_parameters.is_empty() {
                return Err(format!(
                    "an enum constant omitting a constructor argument (`{}.{}`)",
                    declaration.fq_name(),
                    entry.name
                ));
            }
            let mut description = DataDescription::new();
            description.define_zeroinit(8);
            description.set_align(8);
            self.module
                .define_data(*slot, &description)
                .map_err(|error| format!("defining an enum constant's slot ({error})"))?;
        }
        let mut description = DataDescription::new();
        description.define_zeroinit(8);
        description.set_align(8);
        self.module
            .define_data(items.flag, &description)
            .map_err(|error| format!("defining an enum's initialization flag ({error})"))?;

        let companion = declaration
            .companion_class
            .and_then(|companion| self.ir.class_id_by_name(companion))
            .and_then(|companion| self.classes[companion as usize].singleton)
            .map(|(_, getter)| getter);
        let entries = declaration.enum_entries.clone();
        let name = format!("{}.<constants>", declaration.fq_name());
        let signature = self.signature_of(&[], Ty::Unit)?;
        self.emit_function(
            items.initializer,
            signature,
            Carrier::Void,
            &name,
            &mut |body, _| {
                let flag_address = body.data_address(items.flag);
                let ready = body
                    .builder
                    .ins()
                    .load(types::I64, trusted(), flag_address, 0);
                let build = body.builder.create_block();
                let done = body.builder.create_block();
                let started = body.is_null(ready);
                body.builder.ins().brif(started, build, &[], done, &[]);

                body.continue_in(build);
                body.builder.seal_block(build);
                // Marked BEFORE anything is built: a constant's constructor that reaches back into
                // this enum must find the work under way, not start it again.
                let one = body.builder.ins().iconst(types::I64, 1);
                body.builder.ins().store(trusted(), one, flag_address, 0);
                for (ordinal, entry) in entries.iter().enumerate() {
                    let (descriptor, size, constructor) = shapes[ordinal];
                    let slot_address = body.data_address(items.slots[ordinal]);
                    body.runtime_call(
                        "kt_gc_add_global_root",
                        &[any()],
                        Ty::Unit,
                        &[slot_address],
                    )?;
                    let instance = body.allocate(descriptor, size)?;
                    body.builder
                        .ins()
                        .store(trusted(), instance, slot_address, 0);
                    let text = body.string_literal(entry.name.as_bytes())?;
                    body.builder.ins().store(
                        trusted(),
                        text,
                        instance,
                        model::ENUM_NAME_OFFSET as i32,
                    );
                    let position = body.builder.ins().iconst(types::I32, ordinal as i64);
                    body.builder.ins().store(
                        trusted(),
                        position,
                        instance,
                        model::ENUM_ORDINAL_OFFSET as i32,
                    );
                    for &statement in &entry.argument_prelude {
                        body.statement(statement)?;
                    }
                    if entry.args.len() != entry.constructor_parameter_types.len() {
                        return Err("an enum constant with a mismatched argument list".to_string());
                    }
                    let mut operands = vec![instance];
                    for (&argument, ty) in entry.args.iter().zip(&entry.constructor_parameter_types)
                    {
                        let Some(value) = body.coerce(argument, *ty)? else {
                            return Err("a `Unit` enum constant argument".to_string());
                        };
                        operands.push(value);
                    }
                    let func_ref = body.func_ref(constructor);
                    body.emit_call(func_ref, &operands)?;
                }
                // The companion comes after every constant, which is the order a program sees.
                if let Some(getter) = companion {
                    let func_ref = body.func_ref(getter);
                    body.emit_call(func_ref, &[])?;
                }
                body.builder.ins().jump(done, &[]);

                body.continue_in(done);
                body.builder.seal_block(done);
                Ok(())
            },
        )
    }
}

impl BodyLowering<'_, '_, '_> {
    /// `Color.RED`.
    pub(super) fn enum_entry(
        &mut self,
        classifier: TypeName,
        name: &str,
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(classifier, "the enum")?;
        let ordinal = self.file.ir.classes[class as usize]
            .enum_entries
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| format!("an unknown enum constant (`{}`)", name))?;
        let slot = self.file.enum_entries[&class].slots[ordinal];
        self.build_enum(class)?;
        let address = self.data_address(slot);
        Ok(Some(self.builder.ins().load(
            types::I64,
            trusted(),
            address,
            0,
        )))
    }

    /// `Color.values()`: a fresh array holding every constant, in declaration order.
    pub(super) fn enum_values(
        &mut self,
        classifier: TypeName,
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(classifier, "the enum")?;
        let count = self.file.ir.classes[class as usize].enum_entries.len();
        let slots = self.file.enum_entries[&class].slots.clone();
        self.build_enum(class)?;
        // The constants first, then the array: a constant's construction allocates, and a
        // half-filled array must not be what a collection finds.
        let mut values = Vec::with_capacity(slots.len());
        for slot in slots {
            let address = self.data_address(slot);
            values.push(self.builder.ins().load(types::I64, trusted(), address, 0));
        }
        let element = Ty::Obj(classifier, &[]);
        let shape = self.array_shape(Ty::obj_args("kotlin/Array", &[element]))?;
        let descriptor = self.data_address(shape.descriptor);
        let length = self.builder.ins().iconst(types::I32, count as i64);
        let array = self
            .runtime_call(
                "kt_array_new",
                &[any(), Ty::Int],
                any(),
                &[descriptor, length],
            )?
            .expect("`kt_array_new` returns the array");
        for (ordinal, value) in values.into_iter().enumerate() {
            let offset = super::arrays::ELEMENTS + (ordinal as i64) * i64::from(shape.stride);
            self.builder
                .ins()
                .store(trusted(), value, array, offset as i32);
        }
        Ok(Some(array))
    }

    /// `Color.valueOf(text)`: the constant of that name, or a loud failure.
    pub(super) fn enum_value_of(
        &mut self,
        classifier: TypeName,
        argument: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(classifier, "the enum")?;
        let names: Vec<String> = self.file.ir.classes[class as usize]
            .enum_entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        let slots = self.file.enum_entries[&class].slots.clone();
        self.build_enum(class)?;
        let text = self.reference(argument)?;
        if self.terminated {
            return Ok(None);
        }
        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, types::I64);
        for (name, slot) in names.iter().zip(slots) {
            let candidate = self.string_literal(name.as_bytes())?;
            let same = self
                .runtime_call(
                    "kt_equals",
                    &[any(), any()],
                    Ty::Boolean,
                    &[text, candidate],
                )?
                .expect("`kt_equals` returns a Boolean");
            let found = self.builder.create_block();
            let next = self.builder.create_block();
            self.builder.ins().brif(same, found, &[], next, &[]);

            self.continue_in(found);
            self.builder.seal_block(found);
            let address = self.data_address(slot);
            let value = self.builder.ins().load(types::I64, trusted(), address, 0);
            self.builder.ins().jump(merge, &[BlockArg::Value(value)]);

            self.continue_in(next);
            self.builder.seal_block(next);
        }
        // No constant of that name: Kotlin throws, and the runtime's failure is this one.
        self.runtime_call("kt_no_such_enum_constant", &[any()], Ty::Unit, &[text])?;
        let unreachable = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .jump(merge, &[BlockArg::Value(unreachable)]);

        self.continue_in(merge);
        self.builder.seal_block(merge);
        Ok(Some(self.builder.block_params(merge)[0]))
    }

    /// Run this enum's initializer, which builds every constant and then its companion. Idempotent,
    /// so every reach into the enum can simply ask.
    fn build_enum(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let initializer = self.file.enum_entries[&class].initializer;
        let func_ref = self.func_ref(initializer);
        self.emit_call(func_ref, &[])?;
        Ok(())
    }

    /// Whether an external property is one of `kotlin.Enum`'s two, and which.
    ///
    /// The property reaches here as an opaque identity; what it names is recovered through the
    /// getter the provider interned for it, and `intrinsics` turns that provider's spelling into
    /// Kotlin's.
    pub(super) fn enum_member_name(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.classpath.external_property(target)?;
        let getter = self.file.classpath.external_callable(property.getter)?;
        super::super::super::intrinsics::enum_member(
            &getter.callable.owner.render(),
            &getter.callable.name,
        )
    }

    /// `name` and `ordinal`, which every enum constant answers from its own storage.
    pub(super) fn enum_member(
        &mut self,
        member: &str,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        self.null_check(object)?;
        Ok(Some(match member {
            "name" => self.builder.ins().load(
                types::I64,
                trusted(),
                object,
                model::ENUM_NAME_OFFSET as i32,
            ),
            _ => self.builder.ins().load(
                types::I32,
                trusted(),
                object,
                model::ENUM_ORDINAL_OFFSET as i32,
            ),
        }))
    }
}
