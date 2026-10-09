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
//! it without another caller seeing the change. **`entries` is one list per enum**, kept in a
//! static slot the initializer fills after the last constant and before the companion, as
//! Kotlin/Native assigns `$ENTRIES`: every read answers the same object, and a read from inside a
//! constant's construction finds the slot still empty (`null`), as Kotlin/Native's does. `valueOf` compares the argument against each
//! constant's name in declaration order, and fails loudly on no match, as Kotlin does.

use super::*;
use crate::types::TypeName;

/// What one enum class needs: a slot per constant, a flag saying the class has been initialized,
/// the slot keeping its `entries` list once made, and the initializer itself.
#[derive(Clone)]
pub(super) struct EnumItems {
    pub(super) slots: Vec<DataId>,
    flag: DataId,
    entries: DataId,
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
                let symbol = format!(
                    "{base}_{}",
                    super::super::super::symbols::c_identifier(&entry.name)
                );
                slots.push(self.declare_local_data(&format!("kt_enum_{symbol}"), true)?);
            }
            let flag = self.declare_local_data(&format!("kt_enum_{base}_ready"), true)?;
            let entries = self.declare_local_data(&format!("kt_enum_{base}_entries"), true)?;
            let initializer =
                self.declare_local_function(&format!("kt_enum_{base}_init"), &[], Ty::Unit)?;
            self.enum_entries.insert(
                class,
                EnumItems {
                    slots,
                    flag,
                    entries,
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
        // The subclass's constructor takes nothing and calls the enum's with the constant's
        // arguments, so nothing else about the constant changes.
        let mut shapes = Vec::with_capacity(declaration.enum_entries.len());
        for entry in &declaration.enum_entries {
            let owner = match entry.subclass {
                Some(subclass) => self.class_of(subclass, "the body of the enum constant")?,
                None => class,
            };
            // Checked FIR already selected the constructor. A bodied entry constructs its
            // synthesized subclass's primary constructor; that constructor separately carries
            // the exact enum-super target selected for the entry.
            let selected = entry.subclass.is_none().then_some(entry.constructor);
            let constructor = match selected {
                Some(target) if !target.primary() => self.classes[owner as usize]
                    .secondaries
                    .get(target.ordinal as usize - 1)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "an enum constant selecting an unknown secondary constructor (`{}.{}`)",
                            declaration.fq_name(),
                            entry.name
                        )
                    })?,
                _ => self.classes[owner as usize].constructor.ok_or_else(|| {
                    format!(
                        "an enum constant whose class has no primary constructor (`{}.{}`)",
                        declaration.fq_name(),
                        entry.name
                    )
                })?,
            };
            shapes.push((
                self.classes[owner as usize].descriptor,
                self.model.layout(owner).instance_size,
                constructor,
            ));
        }
        for slot in &items.slots {
            let mut description = DataDescription::new();
            description.define_zeroinit(8);
            description.set_align(8);
            self.module
                .define_data(*slot, &description)
                .map_err(|error| format!("defining an enum constant's slot ({error})"))?;
        }
        for (slot, what) in [
            (items.flag, "initialization flag"),
            (items.entries, "entries slot"),
        ] {
            let mut description = DataDescription::new();
            description.define_zeroinit(8);
            description.set_align(8);
            self.module
                .define_data(slot, &description)
                .map_err(|error| format!("defining an enum's {what} ({error})"))?;
        }

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
            Ty::Unit,
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
                let slots = items.slots.clone();
                body.undone_on_failure(
                    &mut |body| {
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
                            // A constant with a body is constructed through its synthesized
                            // subclass's bare primary constructor. Its checked arguments and exact
                            // enum-constructor target live on that subclass's superclass call, where
                            // defaults are realized. An ordinary constant calls its selected enum
                            // constructor here, through the defaults wrapper when necessary.
                            let (target, carried) = if entry.subclass.is_some() {
                                (constructor, Vec::new())
                            } else {
                                let mut omitted = entry.default_parameters.clone();
                                omitted.sort_unstable();
                                let secondary = (!entry.constructor.primary())
                                    .then_some(entry.constructor.ordinal as usize - 1);
                                let frame = body
                                    .file
                                    .constructor_frame_of(class, secondary)
                                    .ok_or_else(|| {
                                        format!(
                                            "an enum constant selecting an unknown constructor (`{}.{}`)",
                                            declaration.fq_name(),
                                            entry.name
                                        )
                                    })?;
                                let carried = frame
                                    .into_iter()
                                    .enumerate()
                                    .filter(|(ordinal, _)| !omitted.contains(&(*ordinal as u32)))
                                    .map(|(_, ty)| ty)
                                    .collect();
                                let target = if omitted.is_empty() {
                                    constructor
                                } else {
                                    let key = super::defaults::CtorOmission {
                                        class,
                                        secondary,
                                        omitted,
                                    };
                                    let Some(&wrapper) = body.file.default_constructors.get(&key)
                                    else {
                                        return Err(format!(
                                            "an enum constant omitting a constructor argument (`{}`)",
                                            entry.name
                                        ));
                                    };
                                    wrapper
                                };
                                (target, carried)
                            };
                            if entry.args.len() != carried.len() {
                                return Err(
                                    "an enum constant with a mismatched argument list".to_string()
                                );
                            }
                            let mut operands = vec![instance];
                            for (&argument, ty) in entry.args.iter().zip(&carried) {
                                let Some(value) = body.coerce(argument, *ty)? else {
                                    return Err("a `Unit` enum constant argument".to_string());
                                };
                                operands.push(value);
                            }
                            let func_ref = body.func_ref(target);
                            body.emit_call(func_ref, &operands)?;
                        }
                        // `$ENTRIES`: after every constant exists and before the companion runs,
                        // over an array of its own.
                        let values = body.enum_values_array(class)?;
                        let list = body
                            .runtime_call("kt_enum_entries_of", &[any()], any(), &[values])?
                            .expect("`kt_enum_entries_of` returns the list");
                        let entries_address = body.data_address(items.entries);
                        body.runtime_call(
                            "kt_gc_add_global_root",
                            &[any()],
                            Ty::Unit,
                            &[entries_address],
                        )?;
                        body.builder
                            .ins()
                            .store(trusted(), list, entries_address, 0);
                        // The companion comes after every constant, which is the order a program sees.
                        if let Some(getter) = companion {
                            let func_ref = body.func_ref(getter);
                            body.emit_call(func_ref, &[])?;
                        }
                        Ok(())
                    },
                    // A constant whose constructor threw leaves the enum unbuilt, every constant
                    // withdrawn: the next access builds it again and throws again, where Kotlin
                    // throws, rather than handing out the constants that did get made.
                    &mut |body| {
                        let zero = body.builder.ins().iconst(types::I64, 0);
                        body.builder.ins().store(trusted(), zero, flag_address, 0);
                        for &slot in slots.iter().chain([&items.entries]) {
                            let address = body.data_address(slot);
                            body.builder.ins().store(trusted(), zero, address, 0);
                        }
                        Ok(())
                    },
                )?;
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
        self.build_enum(class)?;
        self.enum_values_array(class).map(Some)
    }

    /// A fresh array over the constants' slots as they stand, without initializing the enum: the
    /// initializer itself builds `$ENTRIES` from one.
    fn enum_values_array(&mut self, class: ClassId) -> Result<Value, Unsupported> {
        let classifier = self.file.ir.classes[class as usize].fq_name;
        let count = self.file.ir.classes[class as usize].enum_entries.len();
        let slots = self.file.enum_entries[&class].slots.clone();
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
        Ok(array)
    }

    /// `Color.entries` and `enumEntries<Color>()`: the enum's one `EnumEntriesList`, which its
    /// initializer made. Read after initializing the enum, as reading `$ENTRIES` initializes the
    /// class; from inside that initialization, before the constants are all built, it is `null`.
    pub(super) fn enum_entries(
        &mut self,
        classifier: TypeName,
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(classifier, "the enum")?;
        let slot = self.file.enum_entries[&class].entries;
        self.build_enum(class)?;
        let address = self.data_address(slot);
        Ok(Some(self.builder.ins().load(
            types::I64,
            trusted(),
            address,
            0,
        )))
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
    /// getter frozen at the frontend/backend boundary. Emission consumes that selected identity;
    /// it never reopens the provider or looks the property up by spelling.
    pub(super) fn enum_member_name(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        super::super::super::intrinsics::enum_member(getter.physical_owner, &property.name)
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
