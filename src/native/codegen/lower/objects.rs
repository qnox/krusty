//! Classes through the code generator: type descriptors, vtables, constructors, dispatch, field
//! access, `object` singletons and the `is`/`as` family.
//!
//! The model — where each field lives, which vtable slot each member has — comes from
//! `super::super::super::classes`, unchanged from the C emitter's days: it was always a pure
//! function over the IR. What this file adds is the emission: a `KType` descriptor per class laid
//! out byte for byte as `src/native/runtime/krusty_rt.h` declares the struct, a vtable of function
//! addresses, a constructor that runs the superclass's first, and loads and stores at the offsets
//! the descriptor tells the collector to trace. Because one place computes the offsets and both the
//! code and the descriptor read from it, the collector and the program cannot disagree about where
//! a reference is.

use super::*;
use crate::types::TypeName;
use cranelift_codegen::ir::MemFlagsData;

/// The `KType` struct of `krusty_rt.h`, as byte offsets. Every supported target is LP64 with
/// natural alignment, so one layout serves all three.
mod ktype {
    pub const NAME: u32 = 0;
    pub const NAME_LENGTH: u32 = 8;
    pub const INSTANCE_SIZE: u32 = 12;
    pub const REFERENCE_COUNT: u32 = 16;
    pub const REFERENCE_OFFSETS: u32 = 24;
    pub const SUPER: u32 = 32;
    pub const VTABLE: u32 = 40;
    pub const VTABLE_LENGTH: u32 = 48;
    pub const SIZE: usize = 56;
}

/// Where an object keeps its type: the header is one pointer.
const TYPE_OFFSET: i32 = 0;

/// The emitted items of one class.
pub(super) struct ClassItems {
    /// Its `KType`.
    descriptor: DataId,
    /// `kt_<class>__init(this, args…)`.
    constructor: FuncId,
    /// For an `object` declaration: the static slot holding the instance, and its getter.
    singleton: Option<(DataId, FuncId)>,
}

/// `kotlin.Any`'s three members, by runtime symbol, with their signatures.
fn any_member(symbol: &str) -> Option<(Vec<Ty>, Ty)> {
    Some(match symbol {
        "kt_any_equals" => (vec![any(), any()], Ty::Boolean),
        "kt_any_hash_code" => (vec![any()], Ty::Int),
        "kt_any_to_string" => (vec![any()], any()),
        _ => return None,
    })
}

/// Flags for a load or store through a reference the program already holds: aligned by
/// construction, and non-trapping because null receivers are checked before any access.
pub(super) fn trusted() -> MemFlagsData {
    MemFlagsData::trusted()
}

fn write_u32(bytes: &mut [u8], offset: u32, value: u32) {
    let offset = offset as usize;
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

impl<'a> FileLowering<'a> {
    fn class_base(&self, class: ClassId) -> &str {
        &self.symbols.classes[class as usize]
    }

    /// The in-file class a name denotes, or the decline for one declared elsewhere.
    fn class_of(&self, internal: TypeName, what: &str) -> Result<ClassId, Unsupported> {
        self.ir.class_id_by_name(internal).ok_or_else(|| {
            format!(
                "{what} `{}`, which is not declared in this file",
                internal.render()
            )
        })
    }

    /// A descriptor the runtime defines, declared as an import on first use.
    pub(super) fn import_data(&mut self, symbol: &str) -> Result<DataId, Unsupported> {
        if let Some(id) = self.data_imports.get(symbol) {
            return Ok(*id);
        }
        let id = self
            .module
            .declare_data(symbol, Linkage::Import, false, false)
            .map_err(|error| format!("importing `{symbol}` ({error})"))?;
        self.data_imports.insert(symbol.to_string(), id);
        Ok(id)
    }

    /// The runtime descriptor for a type an `is`/`as` names: an in-file class or a built-in.
    fn type_descriptor(&mut self, ty: Ty) -> Result<Option<DataId>, Unsupported> {
        let target = ty.non_null();
        if let Some(class) = target
            .obj_internal()
            .and_then(|name| self.ir.class_id_by_name(name))
        {
            return Ok(Some(self.classes[class as usize].descriptor));
        }
        let symbol = match target {
            Ty::String => "kt_type_string",
            Ty::Boolean => "kt_type_boolean",
            Ty::Byte => "kt_type_byte",
            Ty::Short => "kt_type_short",
            Ty::Int => "kt_type_int",
            Ty::Long => "kt_type_long",
            Ty::Char => "kt_type_char",
            _ => return Ok(None),
        };
        self.import_data(symbol).map(Some)
    }

    pub(super) fn declare_local_data(
        &mut self,
        name: &str,
        writable: bool,
    ) -> Result<DataId, Unsupported> {
        self.module
            .declare_data(name, Linkage::Local, writable, false)
            .map_err(|error| format!("declaring `{name}` ({error})"))
    }

    pub(super) fn declare_local_function(
        &mut self,
        name: &str,
        params: &[Ty],
        ret: Ty,
    ) -> Result<FuncId, Unsupported> {
        let signature = self.signature_of(params, ret)?;
        self.module
            .declare_function(name, Linkage::Local, &signature)
            .map_err(|error| format!("declaring `{name}` ({error})"))
    }

    /// Declare every class's descriptor, constructor and singleton, and the field accessors the
    /// vtables synthesize — everything a body may reference, before any body is compiled.
    pub(super) fn declare_classes(&mut self) -> Result<(), Unsupported> {
        for class in 0..self.ir.classes.len() as ClassId {
            let base = self.class_base(class).to_string();
            let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
            let params: Vec<Ty> = std::iter::once(any())
                .chain(
                    self.ir.classes[class as usize]
                        .ctor_args
                        .iter()
                        .map(|argument| argument.ty),
                )
                .collect();
            for argument in &params[1..] {
                if carrier(*argument) == Carrier::Void {
                    return Err(format!(
                        "a `Unit` constructor parameter of `{}`",
                        self.ir.classes[class as usize].fq_name()
                    ));
                }
            }
            let constructor =
                self.declare_local_function(&format!("kt_{base}__init"), &params, Ty::Unit)?;
            let singleton = if self.ir.classes[class as usize].is_object {
                let slot = self.declare_local_data(&format!("kt_singleton_{base}"), true)?;
                let getter =
                    self.declare_local_function(&format!("kt_singleton_{base}_get"), &[], any())?;
                Some((slot, getter))
            } else {
                None
            };
            self.classes.push(ClassItems {
                descriptor,
                constructor,
                singleton,
            });
        }

        let synthesized: Vec<Slot> = self
            .model
            .layouts
            .iter()
            .flat_map(|layout| layout.vtable.iter().cloned())
            .filter(|slot| matches!(slot, Slot::FieldGetter { .. } | Slot::FieldSetter { .. }))
            .collect();
        for slot in synthesized {
            if self.accessors.contains_key(&slot) {
                continue;
            }
            let (Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field }) = &slot
            else {
                unreachable!("filtered to accessors");
            };
            let ty = self.ir.classes[*class as usize].fields[*field as usize].ty;
            let base = self.class_base(*class).to_string();
            let id = if matches!(slot, Slot::FieldGetter { .. }) {
                self.declare_local_function(&format!("kt_{base}__get_f{field}"), &[any()], ty)?
            } else {
                self.declare_local_function(
                    &format!("kt_{base}__set_f{field}"),
                    &[any(), ty],
                    Ty::Unit,
                )?
            };
            self.accessors.insert(slot, id);
        }
        Ok(())
    }

    /// Define what `declare_classes` declared.
    pub(super) fn define_classes(&mut self) -> Result<(), Unsupported> {
        for &class in &self.model.order.clone() {
            self.define_descriptor(class)?;
        }
        let accessors: Vec<(Slot, FuncId)> = self
            .accessors
            .iter()
            .map(|(slot, id)| (slot.clone(), *id))
            .collect();
        for (slot, id) in accessors {
            self.define_accessor(&slot, id)?;
        }
        for class in 0..self.ir.classes.len() as ClassId {
            self.define_constructor(class)?;
            if self.classes[class as usize].singleton.is_some() {
                self.define_singleton_getter(class)?;
            }
        }
        Ok(())
    }

    /// The Kotlin-facing qualified name of a class: what its default `toString` prints.
    fn kotlin_name(&self, class: ClassId) -> String {
        self.ir.classes[class as usize]
            .fq_name()
            .replace(['/', '$'], ".")
    }

    /// Define a `KType` and the two tables it points at, byte for byte as `krusty_rt.h` declares
    /// the struct. Every emitted type goes through here — a class, a lambda, a captured-variable
    /// holder — so the descriptor the collector reads and the layout the code uses are written by
    /// one piece of code.
    pub(super) fn define_type_descriptor(
        &mut self,
        descriptor: DataId,
        base: &str,
        kotlin_name: &str,
        instance_size: u32,
        reference_offsets: &[u32],
        vtable: &[FuncId],
        superclass: DataId,
    ) -> Result<(), Unsupported> {
        let references = if reference_offsets.is_empty() {
            None
        } else {
            let id = self.declare_local_data(&format!("kt_refs_{base}"), false)?;
            let mut description = DataDescription::new();
            let bytes: Vec<u8> = reference_offsets
                .iter()
                .flat_map(|offset| offset.to_le_bytes())
                .collect();
            description.define(bytes.into_boxed_slice());
            description.set_align(4);
            self.module
                .define_data(id, &description)
                .map_err(|error| format!("defining `kt_refs_{base}` ({error})"))?;
            Some(id)
        };

        let table = self.declare_local_data(&format!("kt_vtable_{base}"), false)?;
        let mut description = DataDescription::new();
        description.define(vec![0; vtable.len() * 8].into_boxed_slice());
        description.set_align(8);
        for (index, function) in vtable.iter().enumerate() {
            let func_ref = self
                .module
                .declare_func_in_data(*function, &mut description);
            description.write_function_addr(index as u32 * 8, func_ref);
        }
        self.module
            .define_data(table, &description)
            .map_err(|error| format!("defining `kt_vtable_{base}` ({error})"))?;

        let name_data = self.string_data(kotlin_name.as_bytes())?;
        let mut bytes = vec![0u8; ktype::SIZE];
        write_u32(&mut bytes, ktype::NAME_LENGTH, kotlin_name.len() as u32);
        write_u32(&mut bytes, ktype::INSTANCE_SIZE, instance_size);
        write_u32(
            &mut bytes,
            ktype::REFERENCE_COUNT,
            reference_offsets.len() as u32,
        );
        write_u32(&mut bytes, ktype::VTABLE_LENGTH, vtable.len() as u32);
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        for (offset, data) in [
            (ktype::NAME, Some(name_data)),
            (ktype::REFERENCE_OFFSETS, references),
            (ktype::SUPER, Some(superclass)),
            (ktype::VTABLE, Some(table)),
        ] {
            let Some(data) = data else {
                continue;
            };
            let global = self.module.declare_data_in_data(data, &mut description);
            description.write_data_addr(offset, global, 0);
        }
        self.module
            .define_data(descriptor, &description)
            .map_err(|error| format!("defining `kt_type_{base}` ({error})"))?;
        Ok(())
    }

    /// `kotlin.Any`'s three vtable entries, the prefix of every table.
    pub(super) fn any_vtable(&mut self) -> Result<Vec<FuncId>, Unsupported> {
        let mut entries = Vec::with_capacity(3);
        for symbol in ["kt_any_equals", "kt_any_hash_code", "kt_any_to_string"] {
            let (params, ret) = any_member(symbol).expect("a kotlin.Any member");
            entries.push(self.import(symbol, &params, ret)?);
        }
        Ok(entries)
    }

    /// The reference-offset table, the vtable and the `KType` of one class.
    fn define_descriptor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let layout = self.model.layout(class).clone();
        let base = self.class_base(class).to_string();

        let mut vtable = Vec::with_capacity(layout.vtable.len());
        for slot in &layout.vtable {
            vtable.push(match slot {
                Slot::Runtime(symbol) => {
                    let (params, ret) = any_member(symbol).expect("a kotlin.Any member");
                    self.import(symbol, &params, ret)?
                }
                Slot::Function(fid) => {
                    self.functions[*fid as usize].expect("a vtable entry has a body")
                }
                Slot::Abstract => self.import("kt_abstract_method_called", &[], Ty::Unit)?,
                Slot::FieldGetter { .. } | Slot::FieldSetter { .. } => self.accessors[slot],
            });
        }
        let name = self.kotlin_name(class);
        let descriptor = self.classes[class as usize].descriptor;
        // A class's `super` is its superclass where it has one, because `is` walks that chain.
        let superclass = match layout.superclass {
            Some(parent) => self.classes[parent as usize].descriptor,
            None => self.import_data("kt_type_any")?,
        };
        self.define_type_descriptor(
            descriptor,
            &base,
            &name,
            layout.instance_size,
            &layout.reference_offsets,
            &vtable,
            superclass,
        )
    }

    /// A synthesized accessor: the field load or store an open property without a source
    /// accessor dispatches to.
    fn define_accessor(&mut self, slot: &Slot, id: FuncId) -> Result<(), Unsupported> {
        let (Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field }) = slot else {
            unreachable!("only field accessors are synthesized");
        };
        let offset = self.model.layout(*class).fields[*field as usize].offset as i32;
        let ty = self.ir.classes[*class as usize].fields[*field as usize].ty;
        let clif = carrier(ty).clif().expect("fields are never `Unit`");
        let name = format!(
            "{}.{}",
            self.ir.classes[*class as usize].fq_name(),
            self.ir.classes[*class as usize].fields[*field as usize].name
        );
        if matches!(slot, Slot::FieldGetter { .. }) {
            let signature = self.signature_of(&[any()], ty)?;
            self.emit_function(id, signature, carrier(ty), &name, &mut |body, params| {
                let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })
        } else {
            let signature = self.signature_of(&[any(), ty], Ty::Unit)?;
            self.emit_function(id, signature, Carrier::Void, &name, &mut |body, params| {
                body.builder
                    .ins()
                    .store(trusted(), params[1], params[0], offset);
                Ok(())
            })
        }
    }

    /// The constructor: the superclass constructor first, then this class's parameter stores,
    /// then its initializers in source order — Kotlin's order, which a base-class `init` that
    /// prints can observe.
    fn define_constructor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[class as usize].clone();
        let layout = self.model.layout(class).clone();
        let id = self.classes[class as usize].constructor;
        let mut slots = vec![Ty::Obj(declaration.fq_name_id(), &[])];
        slots.extend(declaration.ctor_args.iter().map(|argument| argument.ty));
        let signature = self.signature_of(&slots, Ty::Unit)?;

        let parent = match layout.superclass {
            Some(parent) => {
                let parent_declaration = &self.ir.classes[parent as usize];
                if declaration.super_args.len() != parent_declaration.ctor_args.len() {
                    return Err(format!(
                        "a superclass constructor call with defaulted arguments (`{}`)",
                        declaration.fq_name()
                    ));
                }
                let params: Vec<Ty> = parent_declaration
                    .ctor_args
                    .iter()
                    .map(|argument| argument.ty)
                    .collect();
                Some((self.classes[parent as usize].constructor, params))
            }
            None if !declaration.super_args.is_empty() => {
                return Err(format!(
                    "a superclass constructor call to `{}`",
                    declaration.superclass.render()
                ));
            }
            None => None,
        };

        // Constructing a class is the moment the JVM would have run its `<clinit>`, and what a
        // `<clinit>` does for a class with a companion is create the companion instance — running
        // its initializers. The singleton getter is idempotent and lazy, so calling it here gives
        // Kotlin's order (companion initializers, then the superclass constructor, then this
        // class's) and leaves a class nobody constructs untouched. Construction is the ONLY such
        // trigger the generator can see today; a class-static call, the JVM's other one, is
        // declined by name.
        let companion = declaration
            .companion_class
            .and_then(|companion| self.ir.class_id_by_name(companion))
            .and_then(|companion| self.classes[companion as usize].singleton)
            .map(|(_, getter)| getter);

        let name = format!("{}.<init>", declaration.fq_name());
        self.emit_function(id, signature, Carrier::Void, &name, &mut |body, params| {
            for (slot, (value, ty)) in params.iter().zip(&slots).enumerate() {
                let variable = body.declare_value(slot as u32, *ty)?;
                body.builder.def_var(variable, *value);
            }
            let this = params[0];
            if let Some(getter) = companion {
                let func_ref = body.func_ref(getter);
                body.builder.ins().call(func_ref, &[]);
            }
            for &statement in &declaration.super_arg_prelude {
                body.statement(statement)?;
            }
            if let Some((constructor, parent_params)) = &parent {
                let mut arguments = vec![this];
                for (&argument, ty) in declaration.super_args.iter().zip(parent_params) {
                    let Some(value) = body.coerce(argument, *ty)? else {
                        return Err("a `Unit` superclass constructor argument".to_string());
                    };
                    arguments.push(value);
                }
                let func_ref = body.func_ref(*constructor);
                body.builder.ins().call(func_ref, &arguments);
            }
            if !declaration.explicit_param_stores {
                let mut next_field = 0;
                for (index, argument) in declaration.ctor_args.iter().enumerate() {
                    if !argument.is_field {
                        continue;
                    }
                    let field = argument.field_index.unwrap_or(next_field);
                    next_field = field + 1;
                    let field_type = declaration.fields[field as usize].ty;
                    let Some(value) =
                        body.convert(params[index + 1], Some(argument.ty), field_type)?
                    else {
                        return Err("a `Unit` field".to_string());
                    };
                    let offset = layout.fields[field as usize].offset as i32;
                    body.builder.ins().store(trusted(), value, this, offset);
                }
            }
            if let Some(init) = declaration.init_body {
                body.statement(init)?;
            }
            Ok(())
        })
    }

    /// An `object` declaration: one lazily constructed instance in a static slot that is a
    /// registered collector root, so whatever the singleton references stays alive through every
    /// collection. The slot is assigned before the constructor runs, so the root exists before
    /// anything the constructor allocates.
    fn define_singleton_getter(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let (slot, getter) = self.classes[class as usize]
            .singleton
            .expect("declared as an object");
        let descriptor = self.classes[class as usize].descriptor;
        let constructor = self.classes[class as usize].constructor;
        let size = self.model.layout(class).instance_size;
        let mut description = DataDescription::new();
        description.define_zeroinit(8);
        description.set_align(8);
        self.module
            .define_data(slot, &description)
            .map_err(|error| format!("defining the singleton slot ({error})"))?;

        let name = format!("{}.INSTANCE", self.ir.classes[class as usize].fq_name());
        let signature = self.signature_of(&[], any())?;
        self.emit_function(getter, signature, Carrier::Ref, &name, &mut |body, _| {
            let slot_address = body.data_address(slot);
            let current = body
                .builder
                .ins()
                .load(types::I64, trusted(), slot_address, 0);
            let is_null = body.is_null(current);
            let construct = body.builder.create_block();
            let done = body.builder.create_block();
            body.builder.ins().brif(is_null, construct, &[], done, &[]);

            body.continue_in(construct);
            body.runtime_call("kt_gc_add_global_root", &[any()], Ty::Unit, &[slot_address])?;
            let instance = body.allocate(descriptor, size)?;
            body.builder
                .ins()
                .store(trusted(), instance, slot_address, 0);
            let func_ref = body.func_ref(constructor);
            body.builder.ins().call(func_ref, &[instance]);
            body.builder.ins().jump(done, &[]);

            body.continue_in(done);
            let instance = body
                .builder
                .ins()
                .load(types::I64, trusted(), slot_address, 0);
            body.builder.ins().return_(&[instance]);
            body.terminate();
            Ok(())
        })
    }
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// `value == null`, as a Boolean.
    fn is_null(&mut self, value: Value) -> Value {
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().icmp(IntCC::Equal, value, zero)
    }

    /// The address of a data item.
    pub(super) fn data_address(&mut self, data: DataId) -> Value {
        let global = self
            .file
            .module
            .declare_data_in_func(data, self.builder.func);
        self.builder.ins().symbol_value(types::I64, global)
    }

    /// A fresh, zeroed instance of the class `descriptor` describes.
    pub(super) fn allocate(&mut self, descriptor: DataId, size: u32) -> Result<Value, Unsupported> {
        let descriptor = self.data_address(descriptor);
        let size = self.builder.ins().iconst(types::I32, i64::from(size));
        let object = self.runtime_call(
            "kt_gc_allocate",
            &[any(), Ty::Int],
            any(),
            &[descriptor, size],
        )?;
        Ok(object.expect("`kt_gc_allocate` returns the object"))
    }

    /// Fail loudly on a null receiver, as the runtime's `kt_dispatch` did: a member access on
    /// `null` is a checked failure, never a load from address zero.
    pub(super) fn null_check(&mut self, receiver: Value) -> Result<(), Unsupported> {
        let is_null = self.is_null(receiver);
        let fail = self.builder.create_block();
        let proceed = self.builder.create_block();
        self.builder.ins().brif(is_null, fail, &[], proceed, &[]);
        self.continue_in(fail);
        self.runtime_call("kt_null_receiver", &[], Ty::Unit, &[])?;
        self.builder.ins().trap(TrapCode::unwrap_user(2));
        self.continue_in(proceed);
        Ok(())
    }

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
        Ok(self.builder.inst_results(call).first().copied())
    }

    /// The receiver of a member access, evaluated to a reference.
    pub(super) fn receiver(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        Ok((!self.terminated).then_some(value))
    }

    pub(super) fn field_read(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let ty = self.file.ir.classes[class as usize].fields[index as usize].ty;
        let offset = self.file.model.layout(class).fields[index as usize].offset as i32;
        let clif = carrier(ty).clif().expect("fields are never `Unit`");
        Ok(Some(self.builder.ins().load(
            clif,
            trusted(),
            object,
            offset,
        )))
    }

    pub(super) fn field_write(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(());
        };
        let ty = self.file.ir.classes[class as usize].fields[index as usize].ty;
        let offset = self.file.model.layout(class).fields[index as usize].offset as i32;
        let value = self.coerce(value, ty)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err("a `Unit` value stored to a field".to_string());
        };
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(())
    }

    pub(super) fn construction(
        &mut self,
        internal: TypeName,
        args: &[u32],
        secondary: bool,
        defaulted: bool,
    ) -> Result<Option<Value>, Unsupported> {
        let name = internal.render();
        if secondary {
            return Err(format!("a secondary constructor call (`{name}`)"));
        }
        if defaulted {
            return Err(format!("a constructor default argument (`{name}`)"));
        }
        let class = self.file.class_of(internal, "construction of")?;
        let declaration = &self.file.ir.classes[class as usize];
        if declaration.is_object {
            return Err(format!("construction of the object declaration `{name}`"));
        }
        if declaration.is_abstract || declaration.is_sealed {
            return Err(format!("construction of the abstract class `{name}`"));
        }
        if args.len() != declaration.ctor_args.len() {
            return Err(format!(
                "a constructor call with omitted arguments (`{name}`)"
            ));
        }
        let params: Vec<Ty> = declaration
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .collect();
        let descriptor = self.file.classes[class as usize].descriptor;
        let constructor = self.file.classes[class as usize].constructor;
        let size = self.file.model.layout(class).instance_size;

        // Arguments first, then the allocation: an argument that allocates cannot then leave a
        // half-built object for the collector to find with a stale field.
        let mut arguments = self.arguments(args, &params)?;
        if self.terminated {
            return Ok(None);
        }
        let object = self.allocate(descriptor, size)?;
        arguments.insert(0, object);
        let func_ref = self.func_ref(constructor);
        self.builder.ins().call(func_ref, &arguments);
        Ok(Some(object))
    }

    pub(super) fn method_call(
        &mut self,
        class: ClassId,
        index: u32,
        receiver: u32,
        args: &[Option<u32>],
    ) -> Result<Option<Value>, Unsupported> {
        let fid = self.file.ir.classes[class as usize].methods[index as usize];
        let function = &self.file.ir.functions[fid as usize];
        let arguments: Option<Vec<u32>> = args.iter().copied().collect();
        let Some(arguments) = arguments else {
            return Err(format!(
                "a call with a defaulted argument (`{}.{}`)",
                self.file.ir.classes[class as usize].fq_name(),
                function.name
            ));
        };
        if function.dispatch_receiver.is_none() {
            return Err(format!("a class-static call (`{}`)", function.name));
        }
        let key = model::function_key(self.file.ir, class, fid);
        let Some(slot) = self.file.model.slot(class, &key) else {
            return Err(format!(
                "a method with no dispatch slot (`{}`)",
                function.name
            ));
        };
        let params = super::functions::carried_parameters(self.file.ir, function);
        let ret = function.ret;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(&arguments, &params)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch(object, slot, &params, ret, &arguments)
    }

    /// The property a checked operation names, as (class, property index).
    pub(super) fn checked_property(
        &self,
        target: &crate::fir::PropertyId,
    ) -> Result<(ClassId, usize), Unsupported> {
        let Some(property) = self.file.ir.checked_properties.get(target) else {
            return Err("a property with no checked declaration".to_string());
        };
        let Some(class) = property.class else {
            // A top-level property: not a class member at all, so it has no (class, index).
            return Err(TOP_LEVEL.to_string());
        };
        let index = self.file.ir.classes[class as usize]
            .properties
            .iter()
            .position(|candidate| candidate.name == property.name)
            .ok_or_else(|| format!("an undeclared property (`{}`)", property.name))?;
        Ok((class, index))
    }

    pub(super) fn property_read(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: Option<u32>,
    ) -> Result<Option<Value>, Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        let Some(receiver) = receiver else {
            return Err(format!("a receiver-less read of `{}`", property.name));
        };
        if property
            .storage_ty
            .is_some_and(|storage| carrier(storage) != carrier(property.ty))
        {
            return Err(format!(
                "a property whose storage differs from its type (`{}`)",
                property.name
            ));
        }
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let key = model::SlotKey::Getter(class, property.name.clone());
        if let Some(slot) = self.file.model.slot(class, &key) {
            return self.dispatch(object, slot, &[], property.ty, &[]);
        }
        if let Some(getter) = property.getter {
            let id = self.file.functions[getter as usize].expect("a getter has a body");
            let func_ref = self.func_ref(id);
            let call = self.builder.ins().call(func_ref, &[object]);
            return Ok(self.builder.inst_results(call).first().copied());
        }
        match property.backing_field {
            Some(field) => {
                let ty = self.file.ir.classes[class as usize].fields[field as usize].ty;
                let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
                let clif = carrier(ty).clif().expect("fields are never `Unit`");
                Ok(Some(self.builder.ins().load(
                    clif,
                    trusted(),
                    object,
                    offset,
                )))
            }
            None => Err(format!(
                "a property with neither storage nor a getter (`{}`)",
                property.name
            )),
        }
    }

    pub(super) fn property_write(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: Option<u32>,
        value: u32,
    ) -> Result<(), Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        let Some(receiver) = receiver else {
            return Err(format!("a receiver-less write of `{}`", property.name));
        };
        let Some(object) = self.receiver(receiver)? else {
            return Ok(());
        };
        let key = model::SlotKey::Setter(class, property.name.clone());
        let through_slot = self.file.model.slot(class, &key);
        let target_ty = match (through_slot, property.setter, property.backing_field) {
            (Some(_), _, _) | (None, Some(_), _) => property.ty,
            (None, None, Some(field)) => {
                self.file.ir.classes[class as usize].fields[field as usize].ty
            }
            (None, None, None) => {
                return Err(format!(
                    "a property with neither storage nor a setter (`{}`)",
                    property.name
                ))
            }
        };
        let value = self.coerce(value, target_ty)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err(format!("a `Unit` value assigned to `{}`", property.name));
        };
        if let Some(slot) = through_slot {
            self.dispatch(object, slot, &[property.ty], Ty::Unit, &[value])?;
            return Ok(());
        }
        if let Some(setter) = property.setter {
            let id = self.file.functions[setter as usize].expect("a setter has a body");
            let func_ref = self.func_ref(id);
            self.builder.ins().call(func_ref, &[object, value]);
            return Ok(());
        }
        let field = property.backing_field.expect("checked above");
        let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(())
    }

    /// `is`, `as`, `as?` and the coercions the frontend inserts.
    pub(super) fn type_operation(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        type_operand: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let class_target = type_operand
            .non_null()
            .obj_internal()
            .and_then(|name| self.file.ir.class_id_by_name(name));
        match op {
            IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => {
                let Some(descriptor) = self.file.type_descriptor(type_operand)? else {
                    return Err(format!(
                        "an `is` check against `{}`",
                        type_name_of(type_operand)
                    ));
                };
                if self
                    .type_of(arg)
                    .is_some_and(|ty| carrier(ty) != Carrier::Ref)
                {
                    return Err("an `is` check on a scalar".to_string());
                }
                let Some(object) = self.receiver(arg)? else {
                    return Ok(None);
                };
                let descriptor = self.data_address(descriptor);
                let mut result = self
                    .runtime_call(
                        "kt_is_instance",
                        &[any(), any()],
                        Ty::Boolean,
                        &[object, descriptor],
                    )?
                    .expect("`kt_is_instance` returns a Boolean");
                if type_operand.is_nullable() {
                    // `x is T?` admits `null`; `kt_is_instance(null, …)` is false, so `or` it in.
                    let is_null = self.is_null(object);
                    result = self.builder.ins().bor(result, is_null);
                }
                if op == IrTypeOp::NotInstanceOf {
                    let one = self.builder.ins().iconst(types::I8, 1);
                    result = self.builder.ins().bxor(result, one);
                }
                Ok(Some(result))
            }
            IrTypeOp::Cast | IrTypeOp::CastNonNull if class_target.is_some() => {
                let descriptor =
                    self.file.classes[class_target.expect("checked") as usize].descriptor;
                let helper = if op == IrTypeOp::Cast || type_operand.is_nullable() {
                    "kt_cast"
                } else {
                    "kt_cast_non_null"
                };
                let Some(object) = self.receiver(arg)? else {
                    return Ok(None);
                };
                let descriptor = self.data_address(descriptor);
                self.runtime_call(helper, &[any(), any()], any(), &[object, descriptor])
            }
            IrTypeOp::SafeCast => {
                let Some(class) = class_target else {
                    return Err(format!("an `as?` to `{}`", type_name_of(type_operand)));
                };
                let descriptor = self.file.classes[class as usize].descriptor;
                let Some(object) = self.receiver(arg)? else {
                    return Ok(None);
                };
                let descriptor = self.data_address(descriptor);
                self.runtime_call(
                    "kt_safe_cast",
                    &[any(), any()],
                    any(),
                    &[object, descriptor],
                )
            }
            IrTypeOp::ImplicitCoercion | IrTypeOp::Cast | IrTypeOp::CastNonNull => {
                self.coerce(arg, type_operand)
            }
        }
    }

    /// The instance of an `object` declaration.
    pub(super) fn singleton(&mut self, classifier: TypeName) -> Result<Option<Value>, Unsupported> {
        // `Unit` is the runtime's, not the program's: every file that mentions it means the same
        // one value, so there is nothing per-file to declare.
        if classifier.matches("kotlin/Unit") {
            return self.runtime_call("kt_unit", &[], any(), &[]);
        }
        let class = self.file.class_of(classifier, "the object")?;
        let Some((_, getter)) = self.file.classes[class as usize].singleton else {
            return Err(format!(
                "a singleton value of `{}`, which is not an object declaration",
                classifier.render()
            ));
        };
        let func_ref = self.func_ref(getter);
        let call = self.builder.ins().call(func_ref, &[]);
        Ok(Some(self.builder.inst_results(call)[0]))
    }

    /// A non-virtual call to the named class's own implementation: `super.f()`.
    pub(super) fn direct_call(
        &mut self,
        owner: TypeName,
        name: &str,
        source: Option<crate::fir::CallableId>,
        params: Option<&[Ty]>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        if owner.matches("kotlin/Any") {
            let symbol = match (name, args.len()) {
                ("toString", 0) => "kt_any_to_string",
                ("hashCode", 0) => "kt_any_hash_code",
                ("equals", 1) => "kt_any_equals",
                _ => return Err(format!("a `super` call to `Any.{name}`")),
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
        let ir = self.file.ir;
        let fid = source
            .and_then(|callable| ir.checked_callable_functions.get(&callable).copied())
            .or_else(|| {
                ir.classes[class as usize]
                    .methods
                    .iter()
                    .copied()
                    .find(|&fid| {
                        let function = &ir.functions[fid as usize];
                        function.name == name
                            && params.is_none_or(|params| function.params == params)
                    })
            })
            .ok_or_else(|| format!("a `super` call to an unknown method (`{name}`)"))?;
        let function = &ir.functions[fid as usize];
        let Some(id) = self.file.functions[fid as usize] else {
            return Err(format!("a `super` call to the abstract method `{name}`"));
        };
        let params = super::functions::carried_parameters(ir, function);
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let mut arguments = vec![object];
        arguments.extend(self.arguments(args, &params)?);
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.builder.ins().call(func_ref, &arguments);
        Ok(self.builder.inst_results(call).first().copied())
    }
}

/// Marks a checked property that turned out to be top-level, so the caller routes it to
/// `super::statics` instead of looking for a class member. Never reaches a diagnostic.
pub(super) const TOP_LEVEL: &str = "\u{0}top-level";

/// A type's spelling for a diagnostic: the class name when it has one, else the debug form.
fn type_name_of(ty: Ty) -> String {
    match ty.non_null().obj_internal() {
        Some(internal) => internal.render().replace('/', "."),
        None => format!("{ty:?}"),
    }
}
