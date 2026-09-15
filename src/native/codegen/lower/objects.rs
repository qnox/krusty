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

use super::super::super::captures;
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
    pub const INTERFACES: u32 = 56;
    pub const INTERFACE_COUNT: u32 = 64;
    pub const SIZE: usize = 72;
}

/// Where an object keeps its type: the header is one pointer.
const TYPE_OFFSET: i32 = 0;

/// The emitted items of one class.
pub(super) struct ClassItems {
    /// Its `KType`.
    pub(super) descriptor: DataId,
    /// `kt_<class>__init(this, args…)`, absent for a class with no primary constructor: every
    /// `<init>` of one comes from its secondaries.
    pub(super) constructor: Option<FuncId>,
    /// For an `object` declaration: the static slot holding the instance, and its getter.
    pub(super) singleton: Option<(DataId, FuncId)>,
    /// One entry per `secondary_ctors` entry, in the same order.
    secondaries: Vec<FuncId>,
}

/// `kotlin.Any`'s three members, by runtime symbol, with their signatures.
fn any_member(symbol: &str) -> Option<(Vec<Ty>, Ty)> {
    Some(match symbol {
        "kt_any_equals" => (vec![any(), any()], Ty::Boolean),
        "kt_any_hash_code" => (vec![any()], Ty::Int),
        "kt_any_to_string" => (vec![any()], any()),
        "kt_enum_to_string" => (vec![any()], any()),
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

/// The parameters a class's primary constructor takes, beyond `this`.
///
/// Normally its `ctor_args`. A synthesized ENUM-ENTRY SUBCLASS is the exception: `Op.ADD { … }` is
/// an instance of `Op$ADD`, which declares no constructor parameters of its own because the JVM's
/// enum ABI gives it `(String name, int ordinal, <user>)` — a realization, not a Kotlin fact, which
/// is why common IR records only the user types (`IrClass::enum_entry_of`). This generator stores
/// the name and ordinal itself, at the layout `kotlin.Enum` contributes, so the subclass's
/// constructor takes exactly those user parameters and passes them to the enum's.
pub(super) fn constructor_parameters(ir: &IrFile, class: ClassId) -> Vec<Ty> {
    let declaration = &ir.classes[class as usize];
    if let Some(user) = &declaration.enum_entry_of {
        return user.clone();
    }
    declaration
        .ctor_args
        .iter()
        .enumerate()
        .map(|(index, argument)| captures::physical_ty(ir, class, index as u32, argument.ty))
        .collect()
}

impl<'a> FileLowering<'a> {
    pub(super) fn class_base(&self, class: ClassId) -> &str {
        &self.symbols.classes[class as usize]
    }

    /// The in-file class a name denotes, or the decline for one declared elsewhere.
    pub(super) fn class_of(&self, internal: TypeName, what: &str) -> Result<ClassId, Unsupported> {
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
    pub(super) fn type_descriptor(&mut self, ty: Ty) -> Result<Option<DataId>, Unsupported> {
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
            Ty::Float => "kt_type_float",
            Ty::Double => "kt_type_double",
            Ty::Unit => "kt_type_unit",
            // The four unsigned types. Each is a descriptor of its own precisely so that an `is`
            // against it can answer, and so that `1u as? Int` cannot.
            Ty::UByte => "kt_type_ubyte",
            Ty::UShort => "kt_type_ushort",
            Ty::UInt => "kt_type_uint",
            Ty::ULong => "kt_type_ulong",
            // An array is not a class this file declares, so there is no class id to look one up
            // by — but it is a type the RUNTIME names, and the descriptor an allocation already
            // stamps on it is the one a check has to ask about. What that descriptor separates is
            // the element WIDTH, which is Kotlin's own erasure: `Array<String>` and `Array<Foo>`
            // are one type here, and `IntArray` is neither of them. An element the runtime lays
            // out no array for has no descriptor to name, which is what the `None` says.
            // `Unit` reaches a type check spelled as the object it is rather than as the carrier
            // `Ty::Unit` names, and it is one type either way.
            _ if target
                .obj_internal()
                .is_some_and(|name| name.matches("kotlin/Unit")) =>
            {
                "kt_type_unit"
            }
            _ if target.is_array() => match super::arrays::array_type(target) {
                Ok((symbol, _)) => symbol,
                Err(_) => return Ok(None),
            },
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
                .chain(constructor_parameters(self.ir, class))
                .collect();
            for argument in &params[1..] {
                if carrier(*argument) == Carrier::Void {
                    return Err(format!(
                        "a `Unit` constructor parameter of `{}`",
                        self.ir.classes[class as usize].fq_name()
                    ));
                }
            }
            let constructor = self.ir.classes[class as usize]
                .has_primary_ctor
                .then(|| {
                    self.declare_local_function(&format!("kt_{base}__init"), &params, Ty::Unit)
                })
                .transpose()?;
            // Each `constructor(…)` beyond the primary is its own entry point: it delegates to
            // another constructor and then runs its own body.
            let mut secondaries = Vec::new();
            for (ordinal, secondary) in self.ir.classes[class as usize]
                .secondary_ctors
                .clone()
                .iter()
                .enumerate()
            {
                let mut params = vec![any()];
                params.extend(secondary.prefix_params.iter().copied());
                params.extend(secondary.params.iter().copied());
                secondaries.push(self.declare_local_function(
                    &format!("kt_{base}__init_{ordinal}"),
                    &params,
                    Ty::Unit,
                )?);
            }
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
                secondaries,
            });
        }

        let synthesized: Vec<Slot> = self
            .model
            .layouts
            .iter()
            .flat_map(|layout| layout.vtable.iter().cloned())
            .filter(|slot| {
                matches!(
                    slot,
                    Slot::FieldGetter { .. }
                        | Slot::FieldSetter { .. }
                        | Slot::ValueMember { .. }
                        | Slot::Bridge { .. }
                )
            })
            .collect();
        for slot in synthesized {
            if self.accessors.contains_key(&slot) {
                continue;
            }
            if let Slot::Bridge {
                declared,
                target_slot,
                target,
            } = &slot
            {
                // The BASE's signature, which is the whole point of the entry: a caller reading
                // this slot through the base reads what the base declares.
                let base = &self.ir.functions[*declared as usize];
                let mut params = vec![any()];
                params.extend(super::functions::carried_parameters(self.ir, *declared));
                let ret = base.ret;
                let id = self.declare_local_function(
                    &format!("kt_bridge_{declared}_{target}_{target_slot}"),
                    &params,
                    ret,
                )?;
                self.accessors.insert(slot, id);
                continue;
            }
            if let Slot::ValueMember { class, member } = &slot {
                let base = self.class_base(*class).to_string();
                let (suffix, params, ret) = match member {
                    ValueMember::Equals => ("equals", vec![any(), any()], Ty::Boolean),
                    ValueMember::HashCode => ("hash_code", vec![any()], Ty::Int),
                    ValueMember::ToString => ("to_string", vec![any()], any()),
                };
                let id = self.declare_local_function(
                    &format!("kt_{base}__value_{suffix}"),
                    &params,
                    ret,
                )?;
                self.accessors.insert(slot, id);
                continue;
            }
            let (Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field }) = &slot
            else {
                unreachable!("filtered to accessors");
            };
            let ty = captures::physical_ty(
                self.ir,
                *class,
                *field,
                self.ir.classes[*class as usize].fields[*field as usize].ty,
            );
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
            if self.classes[class as usize].constructor.is_some() {
                self.define_constructor(class)?;
            }
            for ordinal in 0..self.classes[class as usize].secondaries.len() {
                self.define_secondary_constructor(class, ordinal)?;
            }
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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn define_type_descriptor(
        &mut self,
        descriptor: DataId,
        base: &str,
        kotlin_name: &str,
        instance_size: u32,
        reference_offsets: &[u32],
        vtable: &[FuncId],
        superclass: DataId,
        interfaces: &[DataId],
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

        // The interfaces this type implements, transitively. An interface is not on the super
        // chain — that chain is single inheritance — so `is` finds it here instead.
        let implemented = if interfaces.is_empty() {
            None
        } else {
            let id = self.declare_local_data(&format!("kt_ifaces_{base}"), false)?;
            let mut description = DataDescription::new();
            description.define(vec![0; interfaces.len() * 8].into_boxed_slice());
            description.set_align(8);
            for (index, interface) in interfaces.iter().enumerate() {
                let global = self
                    .module
                    .declare_data_in_data(*interface, &mut description);
                description.write_data_addr(index as u32 * 8, global, 0);
            }
            self.module
                .define_data(id, &description)
                .map_err(|error| format!("defining `kt_ifaces_{base}` ({error})"))?;
            Some(id)
        };

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
        write_u32(&mut bytes, ktype::INTERFACE_COUNT, interfaces.len() as u32);
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        for (offset, data) in [
            (ktype::NAME, Some(name_data)),
            (ktype::REFERENCE_OFFSETS, references),
            (ktype::SUPER, Some(superclass)),
            (ktype::VTABLE, Some(table)),
            (ktype::INTERFACES, implemented),
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
    /// One of `kotlin.Any`'s defaults, imported by its runtime symbol.
    pub(super) fn runtime_member_import(&mut self, symbol: &str) -> Result<FuncId, Unsupported> {
        let (params, ret) = any_member(symbol).ok_or_else(|| {
            format!("a vtable entry naming the unknown runtime symbol `{symbol}`")
        })?;
        self.import(symbol, &params, ret)
    }

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
                Slot::FieldGetter { .. }
                | Slot::FieldSetter { .. }
                | Slot::ValueMember { .. }
                | Slot::Bridge { .. } => self.accessors[slot],
            });
        }
        let name = self.kotlin_name(class);
        let descriptor = self.classes[class as usize].descriptor;
        // A class's `super` is its superclass where it has one, because `is` walks that chain.
        let superclass = match layout.superclass {
            Some(parent) => self.classes[parent as usize].descriptor,
            None => self.import_data("kt_type_any")?,
        };
        let interfaces: Vec<DataId> = self.model.interfaces[class as usize]
            .clone()
            .into_iter()
            .map(|interface| self.classes[interface as usize].descriptor)
            .collect();
        self.define_type_descriptor(
            descriptor,
            &base,
            &name,
            layout.instance_size,
            &layout.reference_offsets,
            &vtable,
            superclass,
            &interfaces,
        )
    }

    /// A `constructor(…)` other than the primary: delegate, then run this constructor's body.
    ///
    /// Kotlin's order is the one being realized. A `this(…)` delegation reaches another constructor
    /// of the same class, which runs the class's initializers; a `super(…)` delegation reaches the
    /// superclass and then runs THIS class's initializers here, because a class with no primary
    /// constructor has nowhere else to run them. Either way this constructor's own body runs last.
    fn define_secondary_constructor(
        &mut self,
        class: ClassId,
        ordinal: usize,
    ) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[class as usize].clone();
        let secondary = declaration.secondary_ctors[ordinal].clone();
        let id = self.classes[class as usize].secondaries[ordinal];
        if !secondary.default_parameters.is_empty() {
            return Err(format!(
                "a secondary constructor delegating with omitted arguments (`{}`)",
                declaration.fq_name()
            ));
        }
        let mut slots = vec![Ty::Obj(declaration.fq_name_id(), &[])];
        slots.extend(secondary.prefix_params.iter().copied());
        slots.extend(secondary.params.iter().copied());
        let signature = self.signature_of(&slots, Ty::Unit)?;

        // Which constructor the delegation reaches. The class's own initializers are not run from
        // here in either case: a `this(…)` delegation reaches a constructor that runs them, and a
        // `super(…)` one belongs to a class with no primary constructor, whose initializers common
        // lowering has already folded into this constructor's body.
        let (target, target_params): (Option<FuncId>, Vec<Ty>) = match &secondary.delegate {
            crate::ir::CtorDelegateTarget::This {
                target_params,
                to_primary,
                ..
            } => {
                let target = if *to_primary {
                    self.classes[class as usize].constructor.ok_or_else(|| {
                        format!(
                            "a delegation to a primary constructor the class does not have (`{}`)",
                            declaration.fq_name()
                        )
                    })?
                } else {
                    let sibling = declaration
                        .secondary_ctors
                        .iter()
                        .position(|candidate| candidate.params == *target_params)
                        .ok_or_else(|| {
                            format!(
                                "a secondary constructor delegating to no known sibling (`{}`)",
                                declaration.fq_name()
                            )
                        })?;
                    self.classes[class as usize].secondaries[sibling]
                };
                (Some(target), target_params.clone())
            }
            crate::ir::CtorDelegateTarget::Super {
                owner,
                target_params,
                ..
            } => match self.ir.class_id_by_name(*owner) {
                Some(parent) => {
                    let parent_constructor =
                        self.classes[parent as usize].constructor.ok_or_else(|| {
                            format!(
                                "a delegation to a superclass with no primary constructor (`{}`)",
                                owner.render()
                            )
                        })?;
                    (Some(parent_constructor), target_params.clone())
                }
                // `kotlin.Any` is the root and declares no state, so `super()` reaching it has
                // nothing to run — the same reason `define_constructor` calls no parent for a class
                // whose only supertype is `Any`. Any OTHER superclass outside this file is a
                // constructor this generator cannot see, and still declines.
                None if super::super::super::intrinsics::is_any(*owner)
                    && target_params.is_empty() =>
                {
                    (None, Vec::new())
                }
                None => {
                    return Err(format!(
                    "a secondary constructor delegating to a superclass outside this file (`{}`)",
                    owner.render()
                ))
                }
            },
        };
        // A delegation argument may call a companion member (`constructor() : this(foo() + prop)`),
        // and those run before the primary constructor this delegates to would have created the
        // companion. So this constructor asks for it too; the getter is idempotent.
        let companion = declaration
            .companion_class
            .and_then(|companion| self.ir.class_id_by_name(companion))
            .and_then(|companion| self.classes[companion as usize].singleton)
            .map(|(_, getter)| getter);
        let name = format!("{}.<init>#{ordinal}", declaration.fq_name());
        let arguments = secondary.delegate_args.clone();
        let prelude = secondary.delegate_prelude.clone();
        let body_expression = secondary.body;
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
            for &statement in &prelude {
                body.statement(statement)?;
            }
            if arguments.len() != target_params.len() {
                return Err("a constructor delegation of a different arity".to_string());
            }
            let mut operands = vec![this];
            for (&argument, ty) in arguments.iter().zip(&target_params) {
                let Some(value) = body.coerce(argument, *ty)? else {
                    return Err("a `Unit` constructor delegation argument".to_string());
                };
                operands.push(value);
            }
            if let Some(target) = target {
                let func_ref = body.func_ref(target);
                body.builder.ins().call(func_ref, &operands);
            }
            if let Some(own) = body_expression {
                body.statement(own)?;
            }
            Ok(())
        })
    }

    /// A synthesized accessor: the field load or store an open property without a source
    /// accessor dispatches to.
    fn define_accessor(&mut self, slot: &Slot, id: FuncId) -> Result<(), Unsupported> {
        if let Slot::Bridge {
            declared,
            target_slot,
            target,
        } = slot
        {
            return self.define_bridge(*declared, *target_slot, *target, id);
        }
        if let Slot::ValueMember { class, member } = slot {
            return self.define_value_member(*class, *member, id);
        }
        let (Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field }) = slot else {
            unreachable!("only field accessors are synthesized");
        };
        let offset = self.model.layout(*class).fields[*field as usize].offset as i32;
        let ty = captures::physical_ty(
            self.ir,
            *class,
            *field,
            self.ir.classes[*class as usize].fields[*field as usize].ty,
        );
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

    /// A `value class`'s `equals`, `hashCode` or `toString`: the same answer Kotlin gives, which is
    /// the WRAPPED value's, not the wrapper's.
    ///
    /// `equals` is the one with a shape of its own. It has to check the other operand's type before
    /// reading its field — a `KRef` that is not one of these has no field at that offset — so it
    /// branches: not an instance yields `false`, and an instance compares the two underlying
    /// values by the same rule a data class compares a field by. `hashCode` is that field's hash.
    /// `toString` renders `IC(n=1)`: the class's Kotlin name, the property's name, and the value
    /// through the runtime's own rendering.
    /// A bridge: the base's signature in, the override's out, and a dispatch between them.
    ///
    /// `A<T : Number>.foo(): T` erases its result to a reference and `Z : A<Int>` returns an
    /// unboxed integer, so the base's slot cannot hold `Z`'s body — a caller reading the slot
    /// through `A` would read an integer as a pointer. This stands there instead: it takes what the
    /// BASE declares, converts each operand to what the override's slot expects, and converts the
    /// answer back.
    ///
    /// It forwards by DISPATCH and not by calling the override, which is what keeps it right under
    /// a further subclass: `Y : Z` replaces the target slot with its own body, and this reaches
    /// whatever the receiver actually is rather than the override that happened to need the bridge.
    fn define_bridge(
        &mut self,
        declared: crate::ir::FunId,
        target_slot: u32,
        target: crate::ir::FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let base = self.ir.functions[declared as usize].clone();
        let carried = super::functions::carried_parameters(self.ir, declared);
        let mut params = vec![any()];
        params.extend(carried.iter().copied());
        let result = base.ret;
        let signature = self.signature_of(&params, result)?;
        let forwarded = super::functions::carried_parameters(self.ir, target);
        let forwarded_ret = self.ir.functions[target as usize].ret;
        let name = format!("bridge to `{}`", base.name);
        self.emit_function(
            id,
            signature,
            carrier(result),
            &name,
            &mut |body, values| {
                let mut arguments = Vec::with_capacity(forwarded.len());
                for (index, &want) in forwarded.iter().enumerate() {
                    let have = carried.get(index).copied();
                    let Some(value) = body.convert(values[index + 1], have, want)? else {
                        return Err("a `Unit` operand crossing a bridge".to_string());
                    };
                    arguments.push(value);
                }
                let answer = body.dispatch(
                    values[0],
                    target_slot,
                    &forwarded,
                    forwarded_ret,
                    &arguments,
                )?;
                match answer {
                    Some(answer) => {
                        let Some(answer) = body.convert(answer, Some(forwarded_ret), result)?
                        else {
                            return Err("a `Unit` answer crossing a bridge".to_string());
                        };
                        body.builder.ins().return_(&[answer]);
                    }
                    None => {
                        body.builder.ins().return_(&[]);
                    }
                }
                body.terminate();
                Ok(())
            },
        )
    }

    fn define_value_member(
        &mut self,
        class: ClassId,
        member: ValueMember,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let offset = self.model.layout(class).fields[0].offset as i32;
        let declaration = &self.ir.classes[class as usize];
        let ty = declaration.fields[0].ty;
        let field_name = declaration.fields[0].name.clone();
        let kotlin_name = self.kotlin_name(class);
        let name = format!("{}.{member:?}", declaration.fq_name());
        let clif = carrier(ty).clif().expect("a field is never `Unit`");
        let descriptor = self.classes[class as usize].descriptor;
        match member {
            ValueMember::Equals => {
                let signature = self.signature_of(&[any(), any()], Ty::Boolean)?;
                self.emit_function(
                    id,
                    signature,
                    carrier(Ty::Boolean),
                    &name,
                    &mut |body, params| {
                        let (left, right) = (params[0], params[1]);
                        let type_address = body.data_address(descriptor);
                        let same_type = body
                            .runtime_call(
                                "kt_is_instance",
                                &[any(), any()],
                                Ty::Boolean,
                                &[right, type_address],
                            )?
                            .expect("`kt_is_instance` returns a Boolean");
                        let merge = body.builder.create_block();
                        body.builder.append_block_param(merge, types::I8);
                        let compare = body.builder.create_block();
                        let other = body.builder.create_block();
                        body.builder.ins().brif(same_type, compare, &[], other, &[]);

                        body.continue_in(other);
                        body.builder.seal_block(other);
                        let no = body.builder.ins().iconst(types::I8, 0);
                        body.builder.ins().jump(merge, &[BlockArg::Value(no)]);

                        body.continue_in(compare);
                        body.builder.seal_block(compare);
                        let mine = body.builder.ins().load(clif, trusted(), left, offset);
                        let theirs = body.builder.ins().load(clif, trusted(), right, offset);
                        let equal = body.values_equal(mine, theirs, ty)?;
                        body.builder.ins().jump(merge, &[BlockArg::Value(equal)]);

                        body.continue_in(merge);
                        body.builder.seal_block(merge);
                        let answer = body.builder.block_params(merge)[0];
                        body.builder.ins().return_(&[answer]);
                        body.terminate();
                        Ok(())
                    },
                )
            }
            ValueMember::HashCode => {
                let signature = self.signature_of(&[any()], Ty::Int)?;
                self.emit_function(
                    id,
                    signature,
                    carrier(Ty::Int),
                    &name,
                    &mut |body, params| {
                        let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                        let hash = body.value_hash(value, ty)?;
                        body.builder.ins().return_(&[hash]);
                        body.terminate();
                        Ok(())
                    },
                )
            }
            ValueMember::ToString => {
                let opening = format!("{kotlin_name}({field_name}=");
                let signature = self.signature_of(&[any()], any())?;
                self.emit_function(id, signature, Carrier::Ref, &name, &mut |body, params| {
                    let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                    let boxed = body
                        .convert(value, Some(ty), any())?
                        .expect("a field is never `Unit`");
                    let rendered = body
                        .runtime_call("kt_to_string", &[any()], any(), &[boxed])?
                        .expect("`kt_to_string` returns a string");
                    let head = body.string_literal(opening.as_bytes())?;
                    let joined = body
                        .runtime_call("kt_string_plus", &[any(), any()], any(), &[head, rendered])?
                        .expect("`kt_string_plus` returns a string");
                    let tail = body.string_literal(b")")?;
                    let whole = body
                        .runtime_call("kt_string_plus", &[any(), any()], any(), &[joined, tail])?
                        .expect("`kt_string_plus` returns a string");
                    body.builder.ins().return_(&[whole]);
                    body.terminate();
                    Ok(())
                })
            }
        }
    }

    /// The constructor: the superclass constructor first, then this class's parameter stores,
    /// then its initializers in source order — Kotlin's order, which a base-class `init` that
    /// prints can observe.
    fn define_constructor(&mut self, class: ClassId) -> Result<(), Unsupported> {
        let declaration = self.ir.classes[class as usize].clone();
        let layout = self.model.layout(class).clone();
        let id = self.classes[class as usize]
            .constructor
            .expect("a primary constructor is defined only where one was declared");
        let mut slots = vec![Ty::Obj(declaration.fq_name_id(), &[])];
        slots.extend(constructor_parameters(self.ir, class));
        let signature = self.signature_of(&slots, Ty::Unit)?;
        // An enum-entry subclass has no `super(…)` written anywhere: it passes on exactly the
        // parameters it was given, which is the whole of what its constructor does beyond running
        // the constant's own body.
        let forwards_to_parent = declaration.enum_entry_of.is_some();

        let parent = match layout.superclass {
            Some(parent) => {
                let parent_declaration = &self.ir.classes[parent as usize];
                let params: Vec<Ty> = constructor_parameters(self.ir, parent);
                if !forwards_to_parent && declaration.super_args.len() != params.len() {
                    return Err(format!(
                        "a superclass constructor call of a different arity (`{}`)",
                        declaration.fq_name()
                    ));
                }
                // `class B : A()` where `A` has a defaulted parameter still passes an operand for
                // it — common IR fills the hole with a zero placeholder so that a target's own
                // default ABI has something to put a mask against — and the ordinals it actually
                // omitted are recorded beside the class. So the placeholders are DROPPED here and
                // the call goes through the same wrapper an `A()` written as an expression would,
                // because a default belongs to the callee's frame either way.
                let omitted: Vec<u32> = self
                    .omitted_super_arguments(class)
                    .map(|(_, omitted)| omitted)
                    .unwrap_or_default();
                let parent_constructor = if omitted.is_empty() {
                    self.classes[parent as usize].constructor.ok_or_else(|| {
                        format!(
                            "a superclass with no primary constructor (`{}`)",
                            parent_declaration.fq_name()
                        )
                    })?
                } else {
                    let key = defaults::CtorOmission {
                        class: parent,
                        omitted: omitted.clone(),
                    };
                    *self.default_constructors.get(&key).ok_or_else(|| {
                        format!(
                            "a superclass constructor call with defaulted arguments (`{}`)",
                            declaration.fq_name()
                        )
                    })?
                };
                Some((parent_constructor, params, omitted))
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
        // An ENUM's companion is not created here: Kotlin builds every constant first and the
        // companion after, and the enum's own initializer keeps that order. Triggering it from the
        // constructor would run the companion's `init` in the middle of the first constant.
        let companion = (!model::is_enum(&declaration))
            .then_some(declaration.companion_class)
            .flatten()
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
            // An `inner` class stores its outer reference BEFORE the superclass constructor runs.
            // The JVM needs that order to satisfy its verifier; here it is kept because it is
            // Kotlin's own order and a superclass constructor can observe it — an `init` in the
            // base calling an overridden method that reads the outer instance sees it set.
            for &(parameter, field) in &declaration.pre_super_param_fields {
                let Some(&(variable, _)) = body.values.get(&(parameter + 1)) else {
                    return Err(format!(
                        "a pre-super store from an unknown parameter (`{}`)",
                        declaration.fq_name()
                    ));
                };
                let value = body.builder.use_var(variable);
                let offset = body.file.model.layout(class).fields[field as usize].offset as i32;
                body.builder.ins().store(trusted(), value, this, offset);
            }
            if let Some((constructor, parent_params, omitted)) = &parent {
                let mut arguments = vec![this];
                if forwards_to_parent {
                    arguments.extend_from_slice(&params[1..]);
                } else {
                    for (ordinal, (&argument, ty)) in
                        declaration.super_args.iter().zip(parent_params).enumerate()
                    {
                        if omitted.contains(&(ordinal as u32)) {
                            continue;
                        }
                        let Some(value) = body.coerce(argument, *ty)? else {
                            return Err("a `Unit` superclass constructor argument".to_string());
                        };
                        arguments.push(value);
                    }
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
                    let field_type = captures::physical_ty(
                        body.file.ir,
                        class,
                        field,
                        declaration.fields[field as usize].ty,
                    );
                    let argument_type =
                        captures::physical_ty(body.file.ir, class, index as u32, argument.ty);
                    let Some(value) =
                        body.convert(params[index + 1], Some(argument_type), field_type)?
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
        let constructor = self.classes[class as usize].constructor.ok_or_else(|| {
            format!(
                "an `object` with no primary constructor (`{}`)",
                self.ir.classes[class as usize].fq_name()
            )
        })?;
        let size = self.model.layout(class).instance_size;
        let mut description = DataDescription::new();
        description.define_zeroinit(8);
        description.set_align(8);
        self.module
            .define_data(slot, &description)
            .map_err(|error| format!("defining the singleton slot ({error})"))?;

        // A companion of an ENUM: touching it touches the enum, so the constants are built first.
        // The enum's initializer calls this getter in turn, and its flag ends the recursion there.
        let enclosing_enum = self
            .ir
            .classes
            .iter()
            .position(|candidate| {
                candidate.companion_class == Some(self.ir.classes[class as usize].fq_name_id())
                    && model::is_enum(candidate)
            })
            .map(|outer| outer as ClassId)
            .and_then(|outer| {
                self.enum_entries
                    .get(&outer)
                    .map(|items| items.initializer())
            });

        let name = format!("{}.INSTANCE", self.ir.classes[class as usize].fq_name());
        let signature = self.signature_of(&[], any())?;
        self.emit_function(getter, signature, Carrier::Ref, &name, &mut |body, _| {
            if let Some(initializer) = enclosing_enum {
                let func_ref = body.func_ref(initializer);
                body.builder.ins().call(func_ref, &[]);
            }
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

/// What an `is` against a type Kotlin settles by itself answers.
#[derive(Clone, Copy)]
enum Settled {
    /// True of every value.
    Always,
    /// True of no value.
    Never,
    /// True of `null` only.
    Null,
    /// True of everything but `null`.
    NotNull,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// `value == null`, as a Boolean.
    pub(super) fn is_null(&mut self, value: Value) -> Value {
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
        let ty = captures::physical_ty(
            self.file.ir,
            class,
            index,
            self.file.ir.classes[class as usize].fields[index as usize].ty,
        );
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
        let ty = captures::physical_ty(
            self.file.ir,
            class,
            index,
            self.file.ir.classes[class as usize].fields[index as usize].ty,
        );
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
        selected: Option<&[Ty]>,
        defaulted: Option<&[u32]>,
    ) -> Result<Option<Value>, Unsupported> {
        let name = internal.render();
        if let Some(omitted) = defaulted {
            return self.defaulted_construction(internal, args, selected, omitted);
        }
        // `Any()` is declared in no file and needs none: the root has no state and no constructor,
        // so the whole of constructing one is an object with its type and nothing after the
        // header. `kt_type_any` is the runtime's, the same descriptor every other type points at
        // as its super.
        if super::super::super::intrinsics::is_any(internal) && args.is_empty() {
            let descriptor = self.file.import_data("kt_type_any")?;
            return Ok(Some(self.allocate(descriptor, model::HEADER_SIZE)?));
        }
        let class = self.file.class_of(internal, "construction of")?;
        let declaration = &self.file.ir.classes[class as usize];
        if declaration.is_object {
            return Err(format!("construction of the object declaration `{name}`"));
        }
        if declaration.is_abstract || declaration.is_sealed {
            return Err(format!("construction of the abstract class `{name}`"));
        }
        // An annotation INSTANCE is a value whose `equals`, `hashCode` and `toString` Kotlin
        // defines over its arguments — arrays by content — and this backend would give it
        // `kotlin.Any`'s identity ones. Reading its members would work and comparing two would
        // not, which is the kind of half-right that answers rather than declines. Declaring and
        // applying an annotation is unaffected: neither produces a value.
        if declaration.is_annotation {
            return Err(format!("construction of the annotation class `{name}`"));
        }
        // Matching uses the DECLARED list, because that is what the construction node names;
        // filling the frame uses the physical one, because that is what the constructor declares.
        let primary_declared: Vec<Ty> = declaration
            .ctor_args
            .iter()
            .map(|argument| argument.ty)
            .collect();
        let primary_params: Vec<Ty> = declaration
            .ctor_args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                captures::physical_ty(self.file.ir, class, index as u32, argument.ty)
            })
            .collect();
        // `ctor_params` names a constructor by its parameter list, and is absent only when the
        // lowering already knows the call goes to the primary one. It is present for a secondary,
        // and also for a primary the lowering could not recognize as such — an anonymous object's,
        // whose constructor is not its class's first declaration. So match the secondaries first,
        // and fall back to the primary when the list is its own.
        let (constructor, params) = match selected {
            Some(selected) => {
                let ordinal = declaration
                    .secondary_ctors
                    .iter()
                    .position(|candidate| candidate.params == selected);
                match ordinal {
                    Some(ordinal) => {
                        let secondary = &declaration.secondary_ctors[ordinal];
                        if !secondary.prefix_params.is_empty() {
                            return Err(format!(
                                "a secondary constructor with compiler-supplied parameters (`{name}`)"
                            ));
                        }
                        (
                            self.file.classes[class as usize].secondaries[ordinal],
                            secondary.params.clone(),
                        )
                    }
                    None if primary_declared == selected => (
                        self.file.classes[class as usize]
                            .constructor
                            .ok_or_else(|| {
                                format!("a call to a primary constructor `{name}` lacks")
                            })?,
                        primary_params,
                    ),
                    None => return Err(format!("a call to an unknown constructor (`{name}`)")),
                }
            }
            None => (
                self.file.classes[class as usize]
                    .constructor
                    .ok_or_else(|| format!("a call to a primary constructor `{name}` lacks"))?,
                primary_params,
            ),
        };
        if args.len() != params.len() {
            return Err(format!(
                "a constructor call with omitted arguments (`{name}`)"
            ));
        }
        let descriptor = self.file.classes[class as usize].descriptor;
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
            // Arguments left out: the wrapper for this omission shape fills them, and dispatches
            // on the receiver itself, so an open method still reaches its override.
            let omitted: Vec<u32> = args
                .iter()
                .enumerate()
                .filter(|(_, argument)| argument.is_none())
                .map(|(ordinal, _)| ordinal as u32)
                .collect();
            let supplied: Vec<u32> = args.iter().flatten().copied().collect();
            return self.defaulted_call(fid, &omitted, Some(receiver), &supplied);
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
        let params = super::functions::carried_parameters(self.file.ir, fid);
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
    /// Follow one enclosing-instance edge: `this@Outer` from inside an `inner` class.
    ///
    /// An `inner` class carries its outer instance in a field, written before the superclass
    /// constructor runs — the same store the JVM spells `this$0`. Which field it is, the IR says:
    /// the pre-super store from the constructor's leading prefix parameter is that field, and the
    /// node is one EDGE, so a nested `inner` class follows one node per level rather than needing a
    /// path here.
    pub(super) fn enclosing_instance(
        &mut self,
        receiver: u32,
        inner: TypeName,
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(inner, "the enclosing instance of")?;
        let declaration = &self.file.ir.classes[class as usize];
        let Some(&(_, field)) = declaration
            .pre_super_param_fields
            .iter()
            .find(|(parameter, _)| *parameter == 0)
        else {
            return Err(format!(
                "an enclosing instance with no stored field (`{}`)",
                declaration.fq_name()
            ));
        };
        let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        self.null_check(object)?;
        Ok(Some(self.builder.ins().load(
            types::I64,
            trusted(),
            object,
            offset,
        )))
    }

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
        let Some(receiver) = receiver else {
            let name = &self.file.ir.classes[class as usize].properties[index].name;
            return Err(format!("a receiver-less read of `{name}`"));
        };
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        self.property_read_of(class, index, object)
    }

    /// [`Self::property_read`] with the receiver already evaluated — what a synthesized body has.
    pub(super) fn property_read_of(
        &mut self,
        class: ClassId,
        index: usize,
        object: Value,
    ) -> Result<Option<Value>, Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        if property
            .storage_ty
            .is_some_and(|storage| carrier(storage) != carrier(property.ty))
        {
            return Err(format!(
                "a property whose storage differs from its type (`{}`)",
                property.name
            ));
        }
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
        let Some(receiver) = receiver else {
            let name = &self.file.ir.classes[class as usize].properties[index].name;
            return Err(format!("a receiver-less write of `{name}`"));
        };
        let target_ty = self.written_property_ty(class, index)?;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(());
        };
        let value = self.coerce(value, target_ty)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            let name = &self.file.ir.classes[class as usize].properties[index].name;
            return Err(format!("a `Unit` value assigned to `{name}`"));
        };
        self.property_write_of(class, index, object, value)
    }

    /// The carrier a write of one of a class's properties must produce: the property's own type
    /// when a setter or a dispatch slot takes it, and the FIELD's when the store is direct.
    pub(super) fn written_property_ty(
        &mut self,
        class: ClassId,
        index: usize,
    ) -> Result<Ty, Unsupported> {
        let property = &self.file.ir.classes[class as usize].properties[index];
        let key = model::SlotKey::Setter(class, property.name.clone());
        let through_slot = self.file.model.slot(class, &key);
        match (through_slot, property.setter, property.backing_field) {
            (Some(_), _, _) | (None, Some(_), _) => Ok(property.ty),
            (None, None, Some(field)) => {
                Ok(self.file.ir.classes[class as usize].fields[field as usize].ty)
            }
            (None, None, None) => Err(format!(
                "a property with neither storage nor a setter (`{}`)",
                property.name
            )),
        }
    }

    /// [`Self::property_write`] with the receiver and the value already evaluated.
    pub(super) fn property_write_of(
        &mut self,
        class: ClassId,
        index: usize,
        object: Value,
        value: Value,
    ) -> Result<(), Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        let key = model::SlotKey::Setter(class, property.name.clone());
        let through_slot = self.file.model.slot(class, &key);
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

    /// The descriptor a CAST to `ty` must be checked against, or `None` when the cast is a
    /// representation change rather than a question about an object.
    ///
    /// Anything the target holds as a REFERENCE and the runtime names is a question about an
    /// object, so `as` and `as?` ask it exactly as `is` does — a class of this file, an array, a
    /// `String`. A SCALAR target is excluded although
    /// [`FileLowering::type_descriptor`] names one for it: `x as Int` is an unboxing, whose whole
    /// realization is the coercion the caller falls through to, and routing it through `kt_cast`
    /// would hand back the box where the site expects the number.
    fn checked_cast_target(&mut self, ty: Ty) -> Result<Option<DataId>, Unsupported> {
        let target = ty.non_null();
        if let Some(class) = target
            .obj_internal()
            .and_then(|name| self.file.ir.class_id_by_name(name))
        {
            return Ok(Some(self.file.classes[class as usize].descriptor));
        }
        if carrier(target) != Carrier::Ref {
            return Ok(None);
        }
        self.file.type_descriptor(target)
    }

    /// The `is` checks Kotlin settles by the type alone — `Nothing`, which has no instances, and
    /// `Any`, which every non-`null` value is one of.
    ///
    /// `Some(answer)` when this is one of them, and `None` when the question really is about the
    /// object. The receiver is evaluated either way: the constant is the answer, not the
    /// expression, and a receiver may have effects.
    #[allow(clippy::type_complexity)]
    fn language_instance_check(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        type_operand: Ty,
    ) -> Result<Option<Option<Value>>, Unsupported> {
        let target = type_operand.non_null();
        let nullable = type_operand.is_nullable();
        let any = target
            .obj_internal()
            .is_some_and(super::super::super::intrinsics::is_any);
        let settled = match (target, any) {
            // `x is Nothing` is false; `x is Nothing?` admits only `null`.
            (Ty::Nothing, _) => Settled::Null,
            // `x is Any` is "not null"; `x is Any?` is true of everything.
            (_, true) if nullable => Settled::Always,
            (_, true) => Settled::NotNull,
            _ => return Ok(None),
        };
        let settled = match (settled, nullable) {
            (Settled::Null, false) => Settled::Never,
            (other, _) => other,
        };
        let Some(object) = self.receiver(arg)? else {
            return Ok(Some(None));
        };
        let mut answer = match settled {
            Settled::Always => self.builder.ins().iconst(types::I8, 1),
            Settled::Never => self.builder.ins().iconst(types::I8, 0),
            Settled::Null => self.is_null(object),
            Settled::NotNull => {
                let is_null = self.is_null(object);
                let one = self.builder.ins().iconst(types::I8, 1);
                self.builder.ins().bxor(is_null, one)
            }
        };
        if op == IrTypeOp::NotInstanceOf {
            let one = self.builder.ins().iconst(types::I8, 1);
            answer = self.builder.ins().bxor(answer, one);
        }
        Ok(Some(Some(answer)))
    }

    /// `is`, `as`, `as?` and the coercions the frontend inserts.
    pub(super) fn type_operation(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        type_operand: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        match op {
            IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => {
                // Two of Kotlin's types answer without asking the object anything, because their
                // membership is settled by the language rather than by a descriptor. `Nothing` has
                // no instances at all, and everything that is not `null` is an `Any`. The
                // nullability of the operand carries the rest: `is Nothing?` is exactly `== null`,
                // and `is Any?` is true of everything. The receiver is still evaluated — the
                // constant is the answer, not the expression.
                if let Some(constant) = self.language_instance_check(op, arg, type_operand)? {
                    return Ok(constant);
                }
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
            // A cast to something WEARING a descriptor is a question the runtime answers; a cast
            // to anything else is a representation change, and the coercion is its whole
            // realization.
            IrTypeOp::Cast | IrTypeOp::CastNonNull => {
                let Some(descriptor) = self.checked_cast_target(type_operand)? else {
                    return self.coerce(arg, type_operand);
                };
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
                let Some(descriptor) = self.checked_cast_target(type_operand)? else {
                    return Err(format!("an `as?` to `{}`", type_name_of(type_operand)));
                };
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
            IrTypeOp::ImplicitCoercion => self.coerce(arg, type_operand),
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

    /// A call dispatched on the receiver's own type, named by the STATIC type it is made through:
    /// `Callee::Virtual`. The class or interface that declares the member fixes the slot number —
    /// `place_interface_slots` gives an interface member one number that means the same thing in
    /// every implementation — so the slot is looked up on the DECLARING classifier and the vtable
    /// it indexes is the receiver's. That is what makes `class C(a: I) : I by a` work: the
    /// forwarder synthesized on `C` knows only `I`.
    pub(super) fn virtual_call(
        &mut self,
        owner: TypeName,
        name: &str,
        params: Option<&(Vec<Ty>, Ty)>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(owner, "a virtual call to a member of")?;
        let ir = self.file.ir;
        let declared = params.map(|(params, _)| params.as_slice());
        let fid = ir.classes[class as usize]
            .methods
            .iter()
            .copied()
            .find(|&fid| {
                let function = &ir.functions[fid as usize];
                function.name == name && declared.is_none_or(|params| function.params == params)
            })
            .ok_or_else(|| {
                format!(
                    "a virtual call to an unknown member (`{}.{name}`)",
                    owner.render().replace('/', ".")
                )
            })?;
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() {
            return Err(format!("a virtual call to the static member `{name}`"));
        }
        let key = model::function_key(ir, class, fid);
        let Some(slot) = self.file.model.slot(class, &key) else {
            return Err(format!("a virtual call with no dispatch slot (`{name}`)"));
        };
        // The ABI is the DECLARATION's, not the call site's: every override fills this slot with a
        // body compiled to the declaration's carriers, so an argument whose checked type is narrower
        // still crosses as what the slot expects.
        let carried = super::functions::carried_parameters(ir, fid);
        let ret = function.ret;
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let arguments = self.arguments(args, &carried)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch(object, slot, &carried, ret, &arguments)
    }

    /// `super.p` and `super.p = v` — the named class's own realization of a PROPERTY.
    ///
    /// Returns `None` when the class declares no such property, so the caller keeps its decline.
    ///
    /// Nothing here may dispatch. `super.p` is written inside the override of `p`, and reaching the
    /// slot would reach that override — which is the accessor doing the asking. So a source-written
    /// accessor of the named class is called directly, and a default one is the field that class
    /// contributes: a distinct field from the override's, because an overriding `var` declares
    /// storage of its own and `super.b` is the reason a program can tell.
    fn direct_property(
        &mut self,
        class: ClassId,
        name: &str,
        receiver: u32,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let index = self.file.ir.classes[class as usize]
            .properties
            .iter()
            .position(|property| property.name == name)?;
        Some(match args {
            [] => self.direct_property_read(class, index, receiver),
            [value] => self
                .direct_property_write(class, index, receiver, *value)
                .map(|()| None),
            _ => Err(format!(
                "a `super` access to `{name}` with {} operands",
                args.len()
            )),
        })
    }

    fn direct_property_read(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        if let Some(getter) = property.getter {
            let Some(id) = self.file.functions[getter as usize] else {
                return Err(format!(
                    "a `super` read of the abstract `{}`",
                    property.name
                ));
            };
            let func_ref = self.func_ref(id);
            let call = self.builder.ins().call(func_ref, &[object]);
            return Ok(self.builder.inst_results(call).first().copied());
        }
        let Some(field) = property.backing_field else {
            return Err(format!(
                "a `super` read of `{}`, which has neither storage nor a getter",
                property.name
            ));
        };
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

    fn direct_property_write(
        &mut self,
        class: ClassId,
        index: usize,
        receiver: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let property = self.file.ir.classes[class as usize].properties[index].clone();
        // The setter takes the property's own type; a direct store takes the FIELD's, which is the
        // same rule `written_property_ty` states for an ordinary write.
        let target = match (property.setter, property.backing_field) {
            (Some(_), _) => property.ty,
            (None, Some(field)) => self.file.ir.classes[class as usize].fields[field as usize].ty,
            (None, None) => {
                return Err(format!(
                    "a `super` write of `{}`, which has neither storage nor a setter",
                    property.name
                ));
            }
        };
        let Some(object) = self.receiver(receiver)? else {
            return Ok(());
        };
        let value = self.coerce(value, target)?;
        if self.terminated {
            return Ok(());
        }
        let Some(value) = value else {
            return Err(format!("a `Unit` value assigned to `{}`", property.name));
        };
        if let Some(setter) = property.setter {
            let Some(id) = self.file.functions[setter as usize] else {
                return Err(format!(
                    "a `super` write of the abstract `{}`",
                    property.name
                ));
            };
            let func_ref = self.func_ref(id);
            self.builder.ins().call(func_ref, &[object, value]);
            return Ok(());
        }
        let field = property.backing_field.expect("checked above");
        let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(())
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
        let found = source
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
            });
        // `super.p` on a PROPERTY names the property, not an accessor, and a class whose accessors
        // are the default ones declares no method at all for it — so the search above finds
        // nothing to call. What the program asked for is still perfectly well defined: the named
        // class's own realization, reached without dispatch.
        let Some(fid) = found else {
            if let Some(realized) = self.direct_property(class, name, receiver, args) {
                return realized;
            }
            return Err(format!("a `super` call to an unknown method (`{name}`)"));
        };
        let Some(id) = self.file.functions[fid as usize] else {
            return Err(format!("a `super` call to the abstract method `{name}`"));
        };
        let params = super::functions::carried_parameters(ir, fid);
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
