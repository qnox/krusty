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
use super::type_checks::{is_runtime_constructed, ThrowableOperands};
use super::*;
use crate::types::TypeName;

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
    /// Only a callable reference's descriptor has one, and only a BOUND one a non-zero one. Sits
    /// in the padding `interface_count` leaves before the pointer below, so the record is the same
    /// size it was.
    pub const REFERENCE_RECEIVER_OFFSET: u32 = 68;
    /// Only a callable reference's descriptor has one; see the note on `KType`.
    pub const REFERENCE_TARGET: u32 = 72;
    /// The three thunks through which the runtime walks an object of a class of this file; see
    /// the note on `KType`. Absent for every type but such a class.
    pub const WALK_ITERATOR: u32 = 80;
    pub const WALK_HAS_NEXT: u32 = 88;
    pub const WALK_NEXT: u32 = 96;
    pub const WALK_LENGTH: u32 = 104;
    pub const WALK_CHAR_AT: u32 = 112;
    pub const SIZE: usize = 120;
}

/// Where an object keeps its type: the header is one pointer.
const TYPE_OFFSET: i32 = 0;

/// The thunks through which the runtime WALKS an object of a class of this file: its own
/// `iterator`, `hasNext` and `next`, each behind a fixed signature this side can call.
///
/// See the note on `KType::walk_iterator` for why a thunk rather than a vtable slot: an emitted
/// method has the signature its DECLARATION states, and neither `hasNext`'s machine `Boolean` nor
/// an `Iterator<Int>`'s unboxed element is something the runtime could read from a slot number.
#[derive(Clone, Copy, Default)]
pub(super) struct WalkMembers {
    pub(super) iterator: Option<FuncId>,
    pub(super) has_next: Option<FuncId>,
    pub(super) next: Option<FuncId>,
    /// For a class implementing `kotlin.CharSequence`, which is walked by its LENGTH and its
    /// indexed read: Kotlin's `CharSequence` declares no iterator at all.
    pub(super) length: Option<FuncId>,
    pub(super) char_at: Option<FuncId>,
}

/// The same, as the SLOTS the declaration pass reads out of the override edges, before the thunks
/// that dispatch to them are emitted.
#[derive(Clone, Copy, Default)]
pub(super) struct WalkSlots {
    /// Each PLUS ONE, so that 0 says the class declares none — slot 0 is `kotlin.Any.equals`.
    pub(super) iterator: u32,
    pub(super) has_next: u32,
    pub(super) next: u32,
    pub(super) length: u32,
    pub(super) char_at: u32,
    /// Whether the class answers for `kotlin.sequences.Sequence` rather than for an eager
    /// iterable. The walk is the same one — a sequence hands out an iterator like anything else —
    /// but the set of MEMBERS a receiver of it may be asked is narrower, because every walk this
    /// runtime has is eager; see [`super::lists::walking_member`].
    pub(super) sequence: bool,
}

impl WalkSlots {
    /// Whether the class declares any of the three, so a subclass knows whether to inherit.
    fn is_empty(self) -> bool {
        self.iterator == 0 && self.has_next == 0 && self.next == 0 && self.length == 0
    }
}

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
    pub(super) secondaries: Vec<FuncId>,
}

/// `kotlin.Any`'s three members, by runtime symbol, with their signatures.
fn any_member(symbol: &str) -> Option<(Vec<Ty>, Ty)> {
    Some(match symbol {
        "kt_any_equals" => (vec![any(), any()], Ty::Boolean),
        "kt_any_hash_code" => (vec![any()], Ty::Int),
        "kt_any_to_string" => (vec![any()], any()),
        "kt_enum_to_string" => (vec![any()], any()),
        // A callable reference's own `equals`/`hashCode`: same shapes as `kotlin.Any`'s, different
        // answers. See the note beside them in `krusty_rt.h`.
        "kt_reference_equals" => (vec![any(), any()], Ty::Boolean),
        "kt_reference_hash_code" => (vec![any()], Ty::Int),
        // `Throwable.toString()`, which every class declared under one of the runtime's exception
        // types inherits — the qualified name, and `: message` after it when there is one.
        "kt_throwable_to_string" => (vec![any()], any()),
        _ => return None,
    })
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

/// Everything a runtime type descriptor records about one type beyond its symbol.
pub(super) struct DescriptorShape<'s> {
    /// The Kotlin name the runtime reports the type by.
    pub(super) kotlin_name: &'s str,
    pub(super) instance_size: u32,
    /// Where the collector finds this type's references.
    pub(super) reference_offsets: &'s [u32],
    pub(super) vtable: &'s [FuncId],
    pub(super) superclass: DataId,
    /// Every interface the type implements, transitively.
    pub(super) interfaces: &'s [DataId],
    /// A callable reference's declaration identity, and the byte offset of its bound receiver —
    /// 0 when it binds none. The two travel together because only a reference has either.
    pub(super) reference_target: Option<(DataId, u32)>,
    /// How to reach this type's own `iterator`/`hasNext`/`next`, for a class of this file the
    /// runtime may be asked to walk. Empty for everything else, which is every type emitted here
    /// but a class.
    pub(super) walk: WalkMembers,
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
                if self.carrier(*argument) == Carrier::Void {
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
                        | Slot::AnnotationMember { .. }
                        | Slot::ValueMember { .. }
                        | Slot::ValueBridge { .. }
                        | Slot::Bridge { .. }
                        | Slot::FunctionBridge { .. }
                        | Slot::AccessorBridge { .. }
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
                params.extend(super::super::super::captures::carried_parameters(
                    self.ir, *declared,
                ));
                let ret = base.ret;
                let id = self.declare_local_function(
                    &match target_slot {
                        Some(slot) => format!("kt_bridge_{declared}_{target}_{slot}"),
                        None => format!("kt_bridge_{declared}_{target}_direct"),
                    },
                    &params,
                    ret,
                )?;
                self.accessors.insert(slot, id);
                continue;
            }
            if let Slot::FunctionBridge { arity, .. } = &slot {
                // `kotlin.Function{N}.invoke`'s own signature, which is what a caller reading the
                // function slot passes and reads: the receiver and every operand a reference, and
                // a reference back.
                let params = vec![any(); arity + 1];
                let id = self.declare_local_function(
                    &format!("kt_function_bridge_{}", self.accessors.len()),
                    &params,
                    any(),
                )?;
                self.accessors.insert(slot, id);
                continue;
            }
            if let Slot::AccessorBridge {
                declared, setter, ..
            } = &slot
            {
                // The INTERFACE's accessor signature, which is the point of the entry: a caller
                // reading this number through the interface reads what the interface declares.
                let (params, ret) = if *setter {
                    (vec![any(), *declared], Ty::Unit)
                } else {
                    (vec![any()], *declared)
                };
                // Named by position rather than by what it bridges: neither end need be a source
                // accessor, so there is no declaration id to name it after, and two classes may
                // need one for the same slot number.
                let id = self.declare_local_function(
                    &format!("kt_accessor_bridge_{}", self.accessors.len()),
                    &params,
                    ret,
                )?;
                self.accessors.insert(slot, id);
                continue;
            }
            if let Slot::ValueBridge { class, function } = &slot {
                // The member's own signature with the BOX where `this` goes: that is what a caller
                // reading the slot off an object passes.
                let base = self.class_base(*class).to_string();
                let mut params = vec![any()];
                params.extend(super::super::super::captures::carried_parameters(
                    self.ir, *function,
                ));
                let ret = self.ir.functions[*function as usize].ret;
                let id = self.declare_local_function(
                    &format!("kt_{base}__boxed_{function}"),
                    &params,
                    ret,
                )?;
                self.accessors.insert(slot, id);
                continue;
            }
            if let Slot::ValueMember { class, member } | Slot::AnnotationMember { class, member } =
                &slot
            {
                let prefix = if matches!(slot, Slot::ValueMember { .. }) {
                    "box"
                } else {
                    "value"
                };
                let base = self.class_base(*class).to_string();
                let (suffix, params, ret) = match member {
                    AnyMember::Equals => ("equals", vec![any(), any()], Ty::Boolean),
                    AnyMember::HashCode => ("hash_code", vec![any()], Ty::Int),
                    AnyMember::ToString => ("to_string", vec![any()], any()),
                };
                let id = self.declare_local_function(
                    &format!("kt_{base}__{prefix}_{suffix}"),
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

    /// The Kotlin-facing qualified name of a class: what its default `toString` prints and what a
    /// failed cast reports.
    ///
    /// Only the PACKAGE separator becomes a dot. A `$` is Kotlin's own nesting separator and stays
    /// one — `box$MyLocalObject` is the name Kotlin/Native gives a class local to `box`, and
    /// flattening it to `box.MyLocalObject` reads as a package that does not exist. That name is
    /// already this target's: `lower_ir_file` realized every local classifier from its provenance.
    fn kotlin_name(&self, class: ClassId) -> String {
        self.ir.classes[class as usize].fq_name().replace('/', ".")
    }

    /// Define a `KType` and the two tables it points at, byte for byte as `krusty_rt.h` declares
    /// the struct. Every emitted type goes through here — a class, a lambda, a captured-variable
    /// holder — so the descriptor the collector reads and the layout the code uses are written by
    /// one piece of code.
    pub(super) fn define_type_descriptor(
        &mut self,
        descriptor: DataId,
        base: &str,
        shape: DescriptorShape<'_>,
    ) -> Result<(), Unsupported> {
        let DescriptorShape {
            kotlin_name,
            instance_size,
            reference_offsets,
            vtable,
            superclass,
            interfaces,
            reference_target,
            walk,
        } = shape;
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
        write_u32(
            &mut bytes,
            ktype::REFERENCE_RECEIVER_OFFSET,
            reference_target.map_or(0, |(_, receiver)| receiver),
        );

        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        for (offset, thunk) in [
            (ktype::WALK_ITERATOR, walk.iterator),
            (ktype::WALK_HAS_NEXT, walk.has_next),
            (ktype::WALK_NEXT, walk.next),
            (ktype::WALK_LENGTH, walk.length),
            (ktype::WALK_CHAR_AT, walk.char_at),
        ] {
            let Some(thunk) = thunk else {
                continue;
            };
            let func_ref = self.module.declare_func_in_data(thunk, &mut description);
            description.write_function_addr(offset, func_ref);
        }
        for (offset, data) in [
            (ktype::NAME, Some(name_data)),
            (ktype::REFERENCE_OFFSETS, references),
            (ktype::SUPER, Some(superclass)),
            (ktype::VTABLE, Some(table)),
            (ktype::INTERFACES, implemented),
            (
                ktype::REFERENCE_TARGET,
                reference_target.map(|(marker, _)| marker),
            ),
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
                Slot::Function(fid) => match self.functions[*fid as usize] {
                    Some(id) => id,
                    // A member this file emits no body for — one common lowering spliced into an
                    // `inline` caller and then cleared, or one it marked inline-only. A table
                    // cannot decline per entry the way a call site can, and there is no symbol to
                    // name, so the FILE declines. A reified declaration never arrives here: it
                    // keeps its symbol, with a trap for a body where the body could not be
                    // lowered; see `FileLowering::define_function`.
                    None => {
                        return Err(format!(
                            "a vtable entry for `{}`, which this file emits no body for",
                            self.ir.functions[*fid as usize].name
                        ));
                    }
                },
                Slot::Abstract => self.import("kt_abstract_method_called", &[], Ty::Unit)?,
                Slot::FieldGetter { .. }
                | Slot::FieldSetter { .. }
                | Slot::AnnotationMember { .. }
                | Slot::ValueMember { .. }
                | Slot::ValueBridge { .. }
                | Slot::Bridge { .. }
                | Slot::FunctionBridge { .. }
                | Slot::AccessorBridge { .. } => self.accessors[slot],
            });
        }
        let name = self.kotlin_name(class);
        let descriptor = self.classes[class as usize].descriptor;
        // A class's `super` is its superclass where it has one, because `is` walks that chain.
        // With no superclass in this file it may still have one the RUNTIME owns — `class C :
        // Exception(…)` — and pointing at that descriptor is the whole of what makes `catch (e:
        // Exception)` take `C`: matching a clause walks exactly this chain.
        let superclass = match layout.superclass {
            Some(parent) => self.classes[parent as usize].descriptor,
            None => match model::external_base(self.ir.classes[class as usize].superclass) {
                Some(base) => self.import_data(base.descriptor)?,
                None => self.import_data("kt_type_any")?,
            },
        };
        let interfaces: Vec<DataId> = self.model.interfaces[class as usize]
            .clone()
            .into_iter()
            .map(|interface| self.classes[interface as usize].descriptor)
            .collect();
        let walk = self.define_walk_members(class, &base)?;
        self.define_type_descriptor(
            descriptor,
            &base,
            DescriptorShape {
                kotlin_name: &name,
                instance_size: layout.instance_size,
                reference_offsets: &layout.reference_offsets,
                vtable: &vtable,
                superclass,
                interfaces: &interfaces,
                reference_target: None,
                walk,
            },
        )
    }

    /// The slot a class's own `name` of this ARITY takes, with the parameter types it declares and
    /// the type it answers.
    ///
    /// The arity is the only thing matched on beyond the name. Kotlin admits overloads that differ
    /// in parameter TYPES at one arity, and this would pick whichever came first — so a class with
    /// two of them is refused by the caller, which counts the candidates rather than trusting this.
    pub(super) fn member_slot(
        &self,
        class: ClassId,
        name: &str,
        arity: usize,
    ) -> Option<(u32, Vec<Ty>, Ty)> {
        let ir = self.ir;
        let mut matching = ir.classes[class as usize]
            .methods
            .iter()
            .copied()
            .filter(|&fid| {
                let function = &ir.functions[fid as usize];
                function.name == name
                    && function.params.len() == arity
                    && function.dispatch_receiver.is_some()
            });
        let fid = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        let key = model::function_key(ir, class, fid);
        let slot = self.model.slot(class, &key)?;
        let function = &ir.functions[fid as usize];
        Some((slot, function.params.clone(), function.ret))
    }

    /// The three thunks through which the runtime walks an object of this class, emitted here; see
    /// [`WalkMembers`].
    ///
    /// Each dispatches VIRTUALLY on the slot [`Self::walk_slots`] found, so one thunk serves the
    /// whole subtree below the class that declares the member — a subclass overriding it is
    /// reached through the same thunk, which is also why a subclass inherits the pointer rather
    /// than needing one of its own.
    fn define_walk_members(
        &mut self,
        class: ClassId,
        base: &str,
    ) -> Result<WalkMembers, Unsupported> {
        let slots = self.walk_slots(class);
        let mut members = WalkMembers::default();
        for (biased, name, answers) in [
            (slots.iterator, "iterator", any()),
            (slots.has_next, "hasNext", Ty::Boolean),
            (slots.next, "next", any()),
            (slots.length, "length", Ty::Int),
        ] {
            if biased == 0 {
                continue;
            }
            let slot = biased - 1;
            // What the SLOT answers, which is not what the thunk does: `next` on an
            // `Iterator<Int>` answers an unboxed machine integer and the runtime reads a
            // reference, so the conversion is the ordinary boundary one. Read from the slot
            // rather than from the member's declaration, because the slot is what the dispatch
            // lands on and the two need not agree — an entry standing in for a base whose
            // signature has another representation wears the BASE's.
            let declared = self.walk_slot_result(class, slot)?;
            let thunk =
                self.declare_local_function(&format!("{base}_walk_{name}"), &[any()], answers)?;
            let signature = self.signature_of(&[any()], answers)?;
            let label = format!("{base}_walk_{name}");
            self.emit_function(thunk, signature, answers, &label, &mut |body, params| {
                let Some(produced) = body.dispatch(params[0], slot, &[], declared, &[])? else {
                    return Err(format!("a `Unit` answer from `{label}`"));
                };
                let Some(value) = body.convert(produced, Some(declared), answers)? else {
                    return Err(format!("a `Unit` answer from `{label}`"));
                };
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })?;
            match name {
                "iterator" => members.iterator = Some(thunk),
                "hasNext" => members.has_next = Some(thunk),
                "length" => members.length = Some(thunk),
                _ => members.next = Some(thunk),
            }
        }
        // The indexed read takes an ARGUMENT, which is the whole of what separates it from the
        // four above: the index crosses unboxed, at the width the declaration states.
        if slots.char_at != 0 {
            let slot = slots.char_at - 1;
            let declared = self.walk_slot_result(class, slot)?;
            let name = format!("{base}_walk_get");
            let thunk = self.declare_local_function(&name, &[any(), Ty::Int], Ty::Char)?;
            let signature = self.signature_of(&[any(), Ty::Int], Ty::Char)?;
            let parameter = self.walk_slot_parameter(class, slot)?;
            self.emit_function(thunk, signature, Ty::Char, &name, &mut |body, params| {
                let Some(index) = body.convert(params[1], Some(Ty::Int), parameter)? else {
                    return Err(format!("a `Unit` index in `{name}`"));
                };
                let Some(produced) =
                    body.dispatch(params[0], slot, &[parameter], declared, &[index])?
                else {
                    return Err(format!("a `Unit` answer from `{name}`"));
                };
                let Some(value) = body.convert(produced, Some(declared), Ty::Char)? else {
                    return Err(format!("a `Unit` answer from `{name}`"));
                };
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })?;
            members.char_at = Some(thunk);
        }
        Ok(members)
    }

    /// The type the entry in `slot` takes as its ONE parameter, for the thunk that dispatches
    /// through it to convert the operand to.
    fn walk_slot_parameter(&self, class: ClassId, slot: u32) -> Result<Ty, Unsupported> {
        let entry = self
            .model
            .layout(class)
            .vtable
            .get(slot as usize)
            .cloned()
            .ok_or_else(|| format!("a walk through slot {slot}, which no table has"))?;
        let fid = match entry {
            Slot::Function(fid) => fid,
            Slot::Bridge { declared, .. } => declared,
            other => return Err(format!("a walk through the vtable entry {other:?}")),
        };
        match self.ir.functions[fid as usize].params.as_slice() {
            [parameter] => Ok(*parameter),
            _ => Err(format!(
                "a walk through `{}`, which takes no one operand",
                self.ir.functions[fid as usize].name
            )),
        }
    }

    /// What the entry in `slot` of this class's table ANSWERS, for the thunk that dispatches
    /// through it to convert from.
    fn walk_slot_result(&self, class: ClassId, slot: u32) -> Result<Ty, Unsupported> {
        let entry = self
            .model
            .layout(class)
            .vtable
            .get(slot as usize)
            .cloned()
            .ok_or_else(|| format!("a walk through slot {slot}, which no table has"))?;
        match entry {
            Slot::Function(fid) => Ok(self.ir.functions[fid as usize].ret),
            // A stand-in for a base whose signature has another representation wears that base's,
            // which is what a caller reading the slot gets.
            Slot::Bridge { declared, .. } => Ok(self.ir.functions[declared as usize].ret),
            other => Err(format!("a walk through the vtable entry {other:?}")),
        }
    }

    /// Where a class of this file keeps its own `iterator`, `hasNext` and `next`, for the runtime
    /// to walk an object of it; see [`WalkMembers`].
    ///
    /// Read from the OVERRIDE edges, the same source [`implemented_collections`] reads: what makes
    /// a class walkable is that a member of it ANSWERS for `kotlin.collections.Iterable` or
    /// `Iterator`, which is exactly what an edge to one of their declarations records — and it
    /// holds for a class reaching the type through another dependency type its supertype list does
    /// not name.
    ///
    /// A class declaring none of the three inherits its superclass's, because a slot number
    /// assigned at the declaring class is valid for every subclass and a subclass is walked
    /// through the same member. Inherited as a WHOLE rather than per member: a class that declares
    /// one of them declares the shape's answer, and mixing its number with a parent's for the
    /// other two would describe neither.
    pub(super) fn walk_slots(&self, class: ClassId) -> WalkSlots {
        use super::super::super::intrinsics::CollectionShape;
        let ir = self.ir;
        let declaration = &ir.classes[class as usize];
        // Which ROLE the class answers for, read from the edges; WHICH members to look up then
        // follows from the role rather than from the edges. An override whose result is the
        // interface's own type parameter records no edge of its own — `Iterator<T>.next(): T` is
        // exactly that shape — so a class answering `hasNext` would have been half-recorded, and
        // half a pair is no walk.
        let mut iterable = false;
        let mut iterator = false;
        let mut text = false;
        let mut sequence = false;
        for edge in ir
            .function_overrides
            .get(&declaration.fq_name)
            .into_iter()
            .flatten()
        {
            if !matches!(
                edge.overridden,
                crate::fir::ResolvedFunctionOverrideTarget::External(_)
            ) {
                continue;
            }
            match (
                super::super::super::intrinsics::collection_shape(edge.overridden_owner),
                edge.name.as_str(),
            ) {
                (Some(CollectionShape::Iterable), "iterator") => iterable = true,
                // A SEQUENCE is walked exactly as an iterable is: its one member is `iterator`,
                // and the thunk the descriptor carries dispatches to it the same way. What the
                // shape adds is the narrowing the receiver carries afterwards.
                (Some(CollectionShape::Sequence), "iterator") => {
                    iterable = true;
                    sequence = true;
                }
                (Some(CollectionShape::Iterator), "hasNext" | "next") => iterator = true,
                (Some(CollectionShape::Text), "get" | "subSequence") => text = true,
                _ => {}
            }
        }
        // `length` is a PROPERTY, so it reaches the class through a property-override edge rather
        // than a function one — and a `CharSequence` implementor that overrode nothing else would
        // be missed without it.
        for edge in ir
            .property_overrides
            .get(&declaration.fq_name)
            .into_iter()
            .flatten()
        {
            if matches!(
                edge.overridden,
                crate::fir::ResolvedPropertyOverrideTarget::External(_)
            ) && matches!(
                super::super::super::intrinsics::collection_shape(edge.overridden_owner),
                Some(CollectionShape::Text)
            ) {
                text = true;
            }
        }
        let mut slots = WalkSlots::default();
        if iterable {
            slots.iterator = self.walk_slot(class, "iterator");
            // Only where the iterator was actually found: a class with no slot to walk through is
            // no sequence of this file's either, and the flag must not outlive the walk it narrows.
            slots.sequence = sequence && slots.iterator != 0;
        }
        if iterator {
            slots.has_next = self.walk_slot(class, "hasNext");
            slots.next = self.walk_slot(class, "next");
            // Both or neither: a walk asks an iterator for `hasNext` AND `next`, and half a pair
            // would leave the other read as something this class is not.
            if slots.has_next == 0 || slots.next == 0 {
                slots.has_next = 0;
                slots.next = 0;
            }
        }
        if text {
            slots.length = self.walk_accessor_slot(class, "length");
            slots.char_at = self.walk_slot_of(class, "get", 1);
            // Both or neither, for the reason the iterator's pair is: text is walked by its
            // length AND its indexed read, and half of that is no walk.
            if slots.length == 0 || slots.char_at == 0 {
                slots.length = 0;
                slots.char_at = 0;
            }
        }
        if slots.is_empty() {
            if let Some(parent) = self.model.layout(class).superclass {
                return self.walk_slots(parent);
            }
        }
        slots
    }

    /// The slot of a class's own `name` PROPERTY GETTER, PLUS ONE so that 0 says it has none.
    ///
    /// By NAME up the chain, because a base's accessor need not be a method at all: a field-backed
    /// property's accessors are synthesized, and Kotlin rejects a fresh redeclaration of an
    /// inherited property, so a property of that name IS that one.
    fn walk_accessor_slot(&self, class: ClassId, name: &str) -> u32 {
        let mut at = Some(class);
        while let Some(id) = at {
            if self.ir.classes[id as usize]
                .properties
                .iter()
                .any(|property| property.name == name)
            {
                let key = super::super::super::classes::SlotKey::Getter(id, name.to_string());
                return self.model.slot(class, &key).map_or(0, |slot| slot + 1);
            }
            at = self.model.layout(id).superclass;
        }
        0
    }

    /// The slot of a class's own nullary `name`, PLUS ONE so that 0 says it has none; see
    /// [`WalkSlots`].
    ///
    /// Searched up the super chain, because a class answering for the role may inherit the member
    /// from a base of this file rather than declare it — and a slot number assigned at the
    /// declaring class is valid for every subclass.
    fn walk_slot(&self, class: ClassId, name: &str) -> u32 {
        self.walk_slot_of(class, name, 0)
    }

    /// The same for a member of the given ARITY.
    fn walk_slot_of(&self, class: ClassId, name: &str, arity: usize) -> u32 {
        let mut current = Some(class);
        while let Some(id) = current {
            if let Some((slot, _, _)) = self.member_slot(id, name, arity) {
                return slot + 1;
            }
            current = self.model.layout(id).superclass;
        }
        0
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
        let mut slots = vec![self.object_type(class)];
        slots.extend(secondary.prefix_params.iter().copied());
        slots.extend(secondary.params.iter().copied());
        let signature = self.signature_of(&slots, Ty::Unit)?;

        // Which constructor the delegation reaches. The class's own initializers are not run from
        // here in either case: a `this(…)` delegation reaches a constructor that runs them, and a
        // `super(…)` one belongs to a class with no primary constructor, whose initializers common
        // lowering has already folded into this constructor's body.
        // How many of this constructor's own PREFIX operands the delegation passes on. An inner
        // class's constructors all take the outer instance first, and a `this(…)` delegation
        // reaches one of them — so the prefix this constructor was handed goes to it. A `super(…)`
        // delegation passes none: the parent's prefix is the parent's own outer instance, which
        // this constructor does not have.
        let mut prefix_operands = 0usize;
        // Whether the `super(…)` this constructor writes reaches a `Throwable` the RUNTIME owns.
        // There is no constructor to call then, so the two fields are written here instead.
        let mut throwable_base = false;
        // Whether this constructor writes its own prefix into the fields those parameters back.
        let mut stores_its_prefix = false;
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
                prefix_operands = secondary.prefix_params.len();
                (Some(target), target_params.clone())
            }
            crate::ir::CtorDelegateTarget::Super {
                owner,
                target_params,
                ..
            } => {
                // A `super(…)` delegation does not reach a primary constructor, so the PREFIX
                // this one was handed — an inner class's outer instance, a local class's captures
                // — is stored here. A `this(…)` delegation needs none of this: the constructor it
                // reaches stores them.
                stores_its_prefix = true;
                match self.ir.class_id_by_name(*owner) {
                    Some(parent) => {
                        // The checker selected an EXACT constructor, and it may be a SECONDARY one:
                        // `class E : A { constructor() : super() }` where `A`'s no-argument
                        // constructor is secondary. Taking the primary for every `super(…)` called it
                        // with the wrong arguments, which Cranelift's own verifier caught as a
                        // mismatched argument count — a decline rather than a wrong answer, and one
                        // that took every file holding such a constructor with it.
                        //
                        // Kotlin admits no two constructors of one class with the same parameter list,
                        // so a secondary matching `target_params` is the selection and the primary is
                        // what remains when none does.
                        let sibling = self.ir.classes[parent as usize]
                            .secondary_ctors
                            .iter()
                            .position(|candidate| candidate.params == *target_params);
                        let target = match sibling {
                            Some(sibling) => {
                                // A secondary carrying a PREFIX — an inner class's outer instance, or
                                // a local class's captures — takes operands this delegation has no
                                // way to supply, so it declines rather than calling it one short.
                                if !self.ir.classes[parent as usize].secondary_ctors[sibling]
                                    .prefix_params
                                    .is_empty()
                                {
                                    return Err(format!(
                                        "a delegation to a superclass secondary constructor with \
                                     compiler-supplied parameters (`{}`)",
                                        owner.render()
                                    ));
                                }
                                self.classes[parent as usize].secondaries[sibling]
                            }
                            None => {
                                // A parent whose own constructor carries a PREFIX — an inner parent's
                                // outer instance — wants an operand this constructor was never handed.
                                if constructor_parameters(self.ir, parent).len()
                                    != target_params.len()
                                {
                                    return Err(format!(
                                        "a delegation to a superclass constructor with \
                                     compiler-supplied parameters (`{}`)",
                                        owner.render()
                                    ));
                                }
                                self.classes[parent as usize].constructor.ok_or_else(|| {
                                    format!(
                                        "a delegation to a superclass with no primary constructor \
                                     (`{}`)",
                                        owner.render()
                                    )
                                })?
                            }
                        };
                        (Some(target), target_params.clone())
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
                    // A `Throwable` base. It has no constructor here to call — the class is the
                    // runtime's — so its storage is written in place, exactly as the PRIMARY
                    // constructor path writes it for a class whose only supertype is one of these.
                    // The operands are read from the delegation's own arguments, so all four of
                    // Kotlin's forms reach the same two fields.
                    None if model::external_base(*owner).is_some() => {
                        throwable_base = true;
                        (None, target_params.clone())
                    }
                    None => {
                        return Err(format!(
                    "a secondary constructor delegating to a superclass outside this file (`{}`)",
                    owner.render()
                ))
                    }
                }
            }
            // An enum secondary constructor with no written `this(…)`: Kotlin initializes the
            // language enum base with the compiler-supplied entry name and ordinal, which this
            // generator has no base to initialize and no prefix storage to put them in. It is the
            // same shape the constructor SELECTION path already declines a line at a time
            // ("a secondary constructor with compiler-supplied parameters"), so it declines by
            // name here rather than emitting a constructor that leaves `name` and `ordinal`
            // unwritten.
            crate::ir::CtorDelegateTarget::ImplicitEnumBase => {
                return Err(format!(
                    "an enum secondary constructor initializing the implicit enum base (`{}`)",
                    declaration.fq_name()
                ))
            }
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
        // Where each prefix parameter's field sits, or `None` for one that backs no field or was
        // already written before the delegation.
        let prefix_fields: Vec<Option<i32>> = if stores_its_prefix {
            let layout = self.model.layout(class).clone();
            declaration
                .ctor_args
                .iter()
                .take(secondary.prefix_params.len())
                .enumerate()
                .map(|(parameter, argument)| {
                    let written_early = declaration
                        .pre_super_param_fields
                        .iter()
                        .any(|(pre, _)| *pre as usize == parameter);
                    (argument.is_field && !written_early)
                        .then(|| {
                            layout
                                .fields
                                .get(parameter)
                                .map(|field| field.offset as i32)
                        })
                        .flatten()
                })
                .collect()
        } else {
            Vec::new()
        };
        // An `inner` class stores its outer reference BEFORE the base's constructor runs, which is
        // Kotlin's own order and observable: a base `init` calling an overridden method that reads
        // the outer instance sees it set. A class with no PRIMARY constructor has only these, so
        // leaving them out left the field null and every read through it faulted.
        let pre_super_stores: Vec<(usize, i32)> = if stores_its_prefix {
            let layout = self.model.layout(class).clone();
            declaration
                .pre_super_param_fields
                .iter()
                .filter_map(|&(parameter, field)| {
                    Some((
                        parameter as usize + 1,
                        layout.fields.get(field as usize)?.offset as i32,
                    ))
                })
                .collect()
        } else {
            Vec::new()
        };
        let arguments = secondary.delegate_args.clone();
        let prelude = secondary.delegate_prelude.clone();
        let body_expression = secondary.body;
        self.emit_function(id, signature, Ty::Unit, &name, &mut |body, params| {
            for (slot, (value, ty)) in params.iter().zip(&slots).enumerate() {
                let variable = body.declare_value(slot as u32, *ty)?;
                body.builder.def_var(variable, *value);
            }
            let this = params[0];
            if let Some(getter) = companion {
                let func_ref = body.func_ref(getter);
                body.emit_call(func_ref, &[])?;
            }
            for &statement in &prelude {
                body.statement(statement)?;
            }
            if arguments.len() != target_params.len() {
                return Err("a constructor delegation of a different arity".to_string());
            }
            for &(slot, offset) in &pre_super_stores {
                body.builder
                    .ins()
                    .store(trusted(), params[slot], this, offset);
            }
            let mut operands = vec![this];
            // The prefix this constructor was handed, passed on unchanged: a sibling of an inner
            // class's constructor takes the same outer instance.
            for index in 0..prefix_operands {
                operands.push(params[index + 1]);
            }
            for (&argument, ty) in arguments.iter().zip(&target_params) {
                let Some(value) = body.coerce(argument, *ty)? else {
                    return Err("a `Unit` constructor delegation argument".to_string());
                };
                operands.push(value);
            }
            if let Some(target) = target {
                let func_ref = body.func_ref(target);
                body.emit_call(func_ref, &operands)?;
            } else if throwable_base {
                // The base's storage, written in the place and order its constructor would have
                // run. The operands are the delegation's own arguments, read the same way a direct
                // `Exception(…)` reads them, so all four of Kotlin's forms land here.
                let Some(written) =
                    body.throwable_operands(&name, &arguments, Some(&target_params))?
                else {
                    return Ok(());
                };
                if body.terminated {
                    return Ok(());
                }
                let (message, cause) = match written {
                    ThrowableOperands::Message(message) => {
                        (message, body.builder.ins().iconst(types::I64, 0))
                    }
                    ThrowableOperands::MessageAndCause { message, cause } => (message, cause),
                    ThrowableOperands::Cause(cause) => {
                        let rendered = body.runtime_call(
                            "kt_throwable_message_of_cause",
                            &[any()],
                            any(),
                            &[cause],
                        )?;
                        if body.terminated {
                            return Ok(());
                        }
                        let Some(rendered) = rendered else {
                            return Ok(());
                        };
                        (rendered, cause)
                    }
                };
                // BOTH are written, the `null` cause included: an unwritten field is whatever the
                // allocation left there, and the collector traces this one.
                body.builder.ins().store(
                    trusted(),
                    message,
                    this,
                    model::EXTERNAL_BASE_FIELD_OFFSET as i32,
                );
                body.builder.ins().store(
                    trusted(),
                    cause,
                    this,
                    model::THROWABLE_CAUSE_OFFSET as i32,
                );
            }
            // The prefix parameters that BACK a field, written after the base's constructor has
            // run — the same place the primary writes its own parameter-backed fields. A field
            // already written before the delegation is not written again.
            for (parameter, offsets) in prefix_fields.iter().enumerate() {
                let Some(offset) = offsets else {
                    continue;
                };
                body.builder
                    .ins()
                    .store(trusted(), params[parameter + 1], this, *offset);
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
        if let Slot::FunctionBridge {
            arity,
            target_slot,
            target,
        } = slot
        {
            return self.define_function_bridge(*arity, *target_slot, *target, id);
        }
        if let Slot::AccessorBridge {
            declared,
            implemented,
            setter,
            target_slot,
        } = slot
        {
            return self.define_accessor_bridge(*declared, *implemented, *setter, *target_slot, id);
        }
        if let Slot::AnnotationMember { class, member } = slot {
            return self.define_annotation_member(*class, *member, id);
        }
        if let Slot::ValueMember { class, member } = slot {
            return self.define_value_member(*class, *member, id);
        }
        if let Slot::ValueBridge { class, function } = slot {
            return self.define_value_bridge(*class, *function, id);
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
        let name = format!(
            "{}.{}",
            self.ir.classes[*class as usize].fq_name(),
            self.ir.classes[*class as usize].fields[*field as usize].name
        );
        if matches!(slot, Slot::FieldGetter { .. }) {
            let signature = self.signature_of(&[any()], ty)?;
            let class = *class;
            let field = *field;
            self.emit_function(id, signature, ty, &name, &mut |body, params| {
                // Through `load_field` and not a bare load, because this is the FOURTH path that
                // reads a field and a `lateinit` one is guarded on all of them. It is the path a
                // property that OVERRIDES another reaches: the read goes through a vtable slot, so
                // it arrives at this synthesized getter rather than at the field.
                let value = body.load_field(params[0], class, field, ty)?;
                if body.terminated {
                    return Ok(());
                }
                body.builder.ins().return_(&[value]);
                body.terminate();
                Ok(())
            })
        } else {
            let signature = self.signature_of(&[any(), ty], Ty::Unit)?;
            self.emit_function(id, signature, Ty::Unit, &name, &mut |body, params| {
                body.builder
                    .ins()
                    .store(trusted(), params[1], params[0], offset);
                Ok(())
            })
        }
    }

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
    /// The FUNCTION SLOT's stand-in: `kotlin.Function{N}.invoke`'s signature, converted onto the
    /// override's own and dispatched through its slot; see [`Slot::FunctionBridge`].
    ///
    /// Apart from [`Self::define_bridge`] only in where the signature it WEARS comes from. That
    /// one reads a base declaration of this file; there is none here, because `kotlin.Function{N}`
    /// is declared in no file this target compiles — so the signature is written out, which is the
    /// one every function value shares: references throughout.
    fn define_function_bridge(
        &mut self,
        arity: usize,
        target_slot: u32,
        target: crate::ir::FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let params = vec![any(); arity + 1];
        let signature = self.signature_of(&params, any())?;
        let forwarded = super::super::super::captures::carried_parameters(self.ir, target);
        let forwarded_ret = self.ir.functions[target as usize].ret;
        if forwarded.len() != arity {
            return Err(format!(
                "a function-slot bridge to `{}`, which takes {} of {arity} operands",
                self.ir.functions[target as usize].name,
                forwarded.len()
            ));
        }
        let name = format!(
            "function bridge to `{}`",
            self.ir.functions[target as usize].name
        );
        self.emit_function(id, signature, any(), &name, &mut |body, values| {
            let mut arguments = Vec::with_capacity(forwarded.len());
            for (index, &want) in forwarded.iter().enumerate() {
                // From the REFERENCE the caller passed: a function type's operands are boxed, and
                // this is the same unboxing the uniform lambda entry point makes.
                let Some(value) = body.convert(values[index + 1], Some(any()), want)? else {
                    return Err("a `Unit` operand crossing the function slot".to_string());
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
            let answer = match answer {
                Some(answer) => body.convert(answer, Some(forwarded_ret), any())?,
                None => None,
            };
            let answer = match answer {
                Some(answer) => answer,
                // A `Unit` body answering a caller that reads a reference: the runtime owns that
                // singleton, and it is the Kotlin value such a call gets back.
                None => body
                    .runtime_call("kt_unit", &[], any(), &[])?
                    .expect("`kt_unit` returns the singleton"),
            };
            body.builder.ins().return_(&[answer]);
            body.terminate();
            Ok(())
        })
    }

    fn define_bridge(
        &mut self,
        declared: crate::ir::FunId,
        target_slot: Option<u32>,
        target: crate::ir::FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let base = self.ir.functions[declared as usize].clone();
        let carried = super::super::super::captures::carried_parameters(self.ir, declared);
        let mut params = vec![any()];
        params.extend(carried.iter().copied());
        let result = base.ret;
        let signature = self.signature_of(&params, result)?;
        let forwarded = super::super::super::captures::carried_parameters(self.ir, target);
        let forwarded_ret = self.ir.functions[target as usize].ret;
        let name = format!("bridge to `{}`", base.name);
        self.emit_function(id, signature, result, &name, &mut |body, values| {
            let mut arguments = Vec::with_capacity(forwarded.len());
            for (index, &want) in forwarded.iter().enumerate() {
                let have = carried.get(index).copied();
                let Some(value) = body.convert(values[index + 1], have, want)? else {
                    return Err("a `Unit` operand crossing a bridge".to_string());
                };
                arguments.push(value);
            }
            // Through the slot where there is one, so a further subclass's override is reached
            // through the same base. An INTERFACE's own bridge has none — see `Slot::Bridge` —
            // and calls the default body outright, which is where it is the only implementation.
            let answer = match target_slot {
                Some(slot) => {
                    body.dispatch(values[0], slot, &forwarded, forwarded_ret, &arguments)?
                }
                None => {
                    let Some(target) = body.file.functions[target as usize] else {
                        return Err("a bridge to a method with no body".to_string());
                    };
                    let func_ref = body.func_ref(target);
                    let mut operands = vec![values[0]];
                    operands.extend_from_slice(&arguments);
                    let call = body.emit_call(func_ref, &operands)?;
                    body.builder.inst_results(call).first().copied()
                }
            };
            match (answer, body.carrier(result)) {
                (Some(answer), Carrier::Void) => {
                    let _ = answer;
                    body.builder.ins().return_(&[]);
                }
                (Some(answer), _) => {
                    let Some(answer) = body.convert(answer, Some(forwarded_ret), result)? else {
                        return Err("an answer that does not cross a bridge".to_string());
                    };
                    body.builder.ins().return_(&[answer]);
                }
                // `open fun foo(): Any` overridden by `fun foo(): Unit`. The override produces
                // no machine value, and `Unit` is still the Kotlin value a caller reading the
                // base's slot gets back — the runtime owns that singleton, so hand it over.
                (None, Carrier::Ref) => {
                    let unit = body
                        .runtime_call("kt_unit", &[], any(), &[])?
                        .expect("`kt_unit` returns the singleton");
                    body.builder.ins().return_(&[unit]);
                }
                (None, Carrier::Void) => {
                    body.builder.ins().return_(&[]);
                }
                (None, Carrier::Scalar(_, _)) => {
                    return Err("a `Unit` answer where the base declares a primitive".to_string());
                }
            }
            body.terminate();
            Ok(())
        })
    }

    /// A property accessor's bridge: the interface's carrier in, the implementation's out.
    ///
    /// The same shape as [`Self::define_bridge`] and for the same reason, except that neither end
    /// is named by a declaration — a synthesized field access has none — so the two property TYPES
    /// stand in for the two signatures.
    fn define_accessor_bridge(
        &mut self,
        declared: Ty,
        implemented: Ty,
        setter: bool,
        target_slot: u32,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (params, result) = if setter {
            (vec![any(), declared], Ty::Unit)
        } else {
            (vec![any()], declared)
        };
        let signature = self.signature_of(&params, result)?;
        let name = format!("accessor bridge to slot {target_slot}");
        self.emit_function(id, signature, result, &name, &mut |body, values| {
            if setter {
                let Some(value) = body.convert(values[1], Some(declared), implemented)? else {
                    return Err("a `Unit` value crossing an accessor bridge".to_string());
                };
                body.dispatch(values[0], target_slot, &[implemented], Ty::Unit, &[value])?;
                body.builder.ins().return_(&[]);
                body.terminate();
                return Ok(());
            }
            let answer = body.dispatch(values[0], target_slot, &[], implemented, &[])?;
            let Some(answer) = answer else {
                return Err("a `Unit` answer crossing an accessor bridge".to_string());
            };
            let Some(answer) = body.convert(answer, Some(implemented), declared)? else {
                return Err("a `Unit` answer crossing an accessor bridge".to_string());
            };
            body.builder.ins().return_(&[answer]);
            body.terminate();
            Ok(())
        })
    }

    /// The type an object of `class` is held at while it is being built or dispatched on: the
    /// class itself, except for a value class, whose instance IS its value everywhere but here —
    /// a constructor fills a box, so its `this` is that box, a reference like any other.
    pub(super) fn object_type(&self, class: ClassId) -> Ty {
        let declaration = &self.ir.classes[class as usize];
        if self.values.is_value_class(declaration.fq_name) {
            any()
        } else {
            Ty::Obj(declaration.fq_name_id(), &[])
        }
    }

    /// Where a value class's box keeps its value: the field's offset, and the declared type of
    /// what it holds.
    pub(super) fn value_storage(&self, class: ClassId) -> Result<(i32, Ty), Unsupported> {
        let field = model::value_field(self.values, self.ir, class)?;
        let offset = self.model.layout(class).fields[field as usize].offset as i32;
        Ok((
            offset,
            model::field_storage_ty(self.values, self.ir, class, field),
        ))
    }

    /// A value class's `equals`, `hashCode` or `toString`, answered through its box by the value it
    /// holds — Kotlin's answer, which is the wrapped value's and not the wrapper's.
    ///
    /// `equals` checks the other operand's type before reading its field: a reference that is not
    /// one of these boxes has no value at that offset, so it is simply not equal. Two boxes compare
    /// their values by the rule a data class compares a field by. `toString` renders `V(x=1)`:
    /// the class's Kotlin name, the property's, and the value through the runtime's rendering.
    fn define_value_member(
        &mut self,
        class: ClassId,
        member: AnyMember,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (offset, ty) = self.value_storage(class)?;
        let property = self
            .values
            .property(self.ir.classes[class as usize].fq_name)
            .expect("a value field was found by this name")
            .to_string();
        let kotlin_name = self.kotlin_name(class);
        let name = format!("{}.{member:?}", self.ir.classes[class as usize].fq_name());
        let clif = self.carrier(ty).clif().expect("a value is never `Unit`");
        let descriptor = self.classes[class as usize].descriptor;
        match member {
            AnyMember::Equals => {
                let signature = self.signature_of(&[any(), any()], Ty::Boolean)?;
                self.emit_function(id, signature, Ty::Boolean, &name, &mut |body, params| {
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
                })
            }
            AnyMember::HashCode => {
                let signature = self.signature_of(&[any()], Ty::Int)?;
                self.emit_function(id, signature, Ty::Int, &name, &mut |body, params| {
                    let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                    let hash = body.value_hash(value, ty)?;
                    body.builder.ins().return_(&[hash]);
                    body.terminate();
                    Ok(())
                })
            }
            AnyMember::ToString => {
                let opening = format!("{kotlin_name}({property}=");
                let signature = self.signature_of(&[any()], any())?;
                self.emit_function(id, signature, any(), &name, &mut |body, params| {
                    let value = body.builder.ins().load(clif, trusted(), params[0], offset);
                    let boxed = body
                        .convert(value, Some(ty), any())?
                        .expect("a value is never `Unit`");
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

    /// A value class's own member reached through its box: read the value out and call the
    /// member with it as `this`, every other operand and the answer passing straight through.
    fn define_value_bridge(
        &mut self,
        class: ClassId,
        function: FunId,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let (offset, ty) = self.value_storage(class)?;
        let clif = self.carrier(ty).clif().expect("a value is never `Unit`");
        let Some(target) = self.functions[function as usize] else {
            return Err(format!(
                "a value class member with no body (`{}`)",
                self.ir.functions[function as usize].name
            ));
        };
        let mut params = vec![any()];
        params.extend(super::super::super::captures::carried_parameters(
            self.ir, function,
        ));
        let ret = self.ir.functions[function as usize].ret;
        let signature = self.signature_of(&params, ret)?;
        let name = format!("{}.<boxed>", self.ir.functions[function as usize].name);
        self.emit_function(id, signature, ret, &name, &mut |body, operands| {
            let value = body
                .builder
                .ins()
                .load(clif, trusted(), operands[0], offset);
            let mut arguments = vec![value];
            arguments.extend(operands[1..].iter().copied());
            let func_ref = body.func_ref(target);
            let call = body.emit_call(func_ref, &arguments)?;
            let results = body.builder.inst_results(call).to_vec();
            body.builder.ins().return_(&results);
            body.terminate();
            Ok(())
        })
    }

    /// An ANNOTATION instance's `equals`, `hashCode` and `toString`: the same answers Kotlin gives,
    /// which are the MEMBERS' and not the object's identity.
    ///
    /// An array member is compared, hashed and rendered by CONTENT — the one place these differ
    /// from a data class's, where an array member is compared by identity. `hashCode` is the
    /// contract sum of `(127 * name.hashCode()) xor value.hashCode()` over the members, and a
    /// program can read it: the corpus computes that sum in Kotlin and compares.
    ///
    /// The member's NAME hash is taken at run time rather than folded here, so it is the same
    /// `String.hashCode` the program's own `name.hashCode()` reaches. Folding it would be a second
    /// statement of that function, and the two would have to be kept equal by hand.
    fn define_annotation_member(
        &mut self,
        class: ClassId,
        member: AnyMember,
        id: FuncId,
    ) -> Result<(), Unsupported> {
        let layout = self.model.layout(class).clone();
        let declaration = self.ir.classes[class as usize].clone();
        let kotlin_name = self.kotlin_name(class);
        let name = format!("{}.{member:?}", declaration.fq_name());
        let descriptor = self.classes[class as usize].descriptor;
        // Each member as (its Kotlin name, its type, where it sits).
        let members: Vec<(String, Ty, i32)> = declaration
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                (
                    field.name.clone(),
                    field.ty,
                    layout.fields[index].offset as i32,
                )
            })
            .collect();
        for (_, ty, _) in &members {
            if self.carrier(*ty).clif().is_none() {
                return Err(format!(
                    "an annotation member of `{ty:?}` (`{}`)",
                    declaration.fq_name()
                ));
            }
        }
        match member {
            AnyMember::Equals => {
                let signature = self.signature_of(&[any(), any()], Ty::Boolean)?;
                self.emit_function(id, signature, Ty::Boolean, &name, &mut |body, params| {
                    let (left, right) = (params[0], params[1]);
                    let differs = body.builder.create_block();
                    let type_address = body.data_address(descriptor);
                    let same_type = body
                        .runtime_call(
                            "kt_is_instance",
                            &[any(), any()],
                            Ty::Boolean,
                            &[right, type_address],
                        )?
                        .expect("`kt_is_instance` returns a Boolean");
                    let compare = body.builder.create_block();
                    body.builder
                        .ins()
                        .brif(same_type, compare, &[], differs, &[]);
                    body.continue_in(compare);
                    body.builder.seal_block(compare);
                    for (_, ty, offset) in &members {
                        let clif = body.carrier(*ty).clif().expect("checked above");
                        let mine = body.builder.ins().load(clif, trusted(), left, *offset);
                        let theirs = body.builder.ins().load(clif, trusted(), right, *offset);
                        let equal = if ty.non_null().is_array() {
                            body.runtime_call(
                                "kt_array_content_equals",
                                &[any(), any()],
                                Ty::Boolean,
                                &[mine, theirs],
                            )?
                            .expect("`kt_array_content_equals` returns a Boolean")
                        } else {
                            body.values_equal(mine, theirs, *ty)?
                        };
                        let next = body.builder.create_block();
                        body.builder.ins().brif(equal, next, &[], differs, &[]);
                        body.continue_in(next);
                        body.builder.seal_block(next);
                    }
                    let yes = body.builder.ins().iconst(types::I8, 1);
                    body.builder.ins().return_(&[yes]);
                    body.terminate();
                    body.continue_in(differs);
                    body.builder.seal_block(differs);
                    let no = body.builder.ins().iconst(types::I8, 0);
                    body.builder.ins().return_(&[no]);
                    body.terminate();
                    Ok(())
                })
            }
            AnyMember::HashCode => {
                let signature = self.signature_of(&[any()], Ty::Int)?;
                self.emit_function(id, signature, Ty::Int, &name, &mut |body, params| {
                    let mut sum = body.builder.ins().iconst(types::I32, 0);
                    for (member_name, ty, offset) in &members {
                        let clif = body.carrier(*ty).clif().expect("checked above");
                        let value = body.builder.ins().load(clif, trusted(), params[0], *offset);
                        let hash = if ty.non_null().is_array() {
                            body.runtime_call(
                                "kt_array_content_hash_code",
                                &[any()],
                                Ty::Int,
                                &[value],
                            )?
                            .expect("`kt_array_content_hash_code` returns an Int")
                        } else {
                            body.value_hash(value, *ty)?
                        };
                        let text = body.string_literal(member_name.as_bytes())?;
                        let name_hash = body
                            .runtime_call("kt_hash_code", &[any()], Ty::Int, &[text])?
                            .expect("`kt_hash_code` returns an Int");
                        let weight = body.builder.ins().imul_imm_s(name_hash, 127);
                        let contribution = body.builder.ins().bxor(weight, hash);
                        sum = body.builder.ins().iadd(sum, contribution);
                    }
                    body.builder.ins().return_(&[sum]);
                    body.terminate();
                    Ok(())
                })
            }
            AnyMember::ToString => {
                let signature = self.signature_of(&[any()], any())?;
                self.emit_function(id, signature, any(), &name, &mut |body, params| {
                    let mut text = body.string_literal(format!("@{kotlin_name}(").as_bytes())?;
                    for (index, (member_name, ty, offset)) in members.iter().enumerate() {
                        let separator = if index == 0 {
                            format!("{member_name}=")
                        } else {
                            format!(", {member_name}=")
                        };
                        let head = body.string_literal(separator.as_bytes())?;
                        text = body.join(text, head)?;
                        let clif = body.carrier(*ty).clif().expect("checked above");
                        let value = body.builder.ins().load(clif, trusted(), params[0], *offset);
                        let rendered = if ty.non_null().is_array() {
                            body.runtime_call(
                                "kt_array_content_to_string",
                                &[any()],
                                any(),
                                &[value],
                            )?
                            .expect("`kt_array_content_to_string` returns a string")
                        } else {
                            let boxed = body
                                .convert(value, Some(*ty), any())?
                                .expect("a member is never `Unit`");
                            body.runtime_call("kt_to_string", &[any()], any(), &[boxed])?
                                .expect("`kt_to_string` returns a string")
                        };
                        text = body.join(text, rendered)?;
                    }
                    let tail = body.string_literal(b")")?;
                    let whole = body.join(text, tail)?;
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
        let mut slots = vec![self.object_type(class)];
        slots.extend(constructor_parameters(self.ir, class));
        let signature = self.signature_of(&slots, Ty::Unit)?;
        // An enum-entry subclass has no `super(…)` written anywhere: it passes on exactly the
        // parameters it was given, which is the whole of what its constructor does beyond running
        // the constant's own body.
        let forwards_to_parent = declaration.enum_entry_of.is_some();

        let parent = match layout.superclass {
            Some(parent) => {
                let parent_declaration = &self.ir.classes[parent as usize];
                // `class B : A(4)` names the constructor the checker selected, which may be a
                // SECONDARY one — `sealed class A() { constructor(i: Int) : this() }` is that
                // shape. Reading the primary's parameter list for every base call made the arity
                // disagree and declined the file. Kotlin admits no two constructors of one class
                // with the same parameter list, so a secondary matching the selection is it.
                let sibling = if forwards_to_parent {
                    None
                } else {
                    parent_declaration
                        .secondary_ctors
                        .iter()
                        .position(|candidate| {
                            candidate.prefix_params.is_empty()
                                && candidate.params == declaration.super_ctor_params
                        })
                };
                let params: Vec<Ty> = match sibling {
                    Some(sibling) => parent_declaration.secondary_ctors[sibling].params.clone(),
                    None => constructor_parameters(self.ir, parent),
                };
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
                    .map(|(_, _, omitted)| omitted)
                    .unwrap_or_default();
                let parent_constructor = if omitted.is_empty() {
                    match sibling {
                        Some(sibling) => self.classes[parent as usize].secondaries[sibling],
                        None => self.classes[parent as usize].constructor.ok_or_else(|| {
                            format!(
                                "a superclass with no primary constructor (`{}`)",
                                parent_declaration.fq_name()
                            )
                        })?,
                    }
                } else {
                    // The wrapper fills whichever constructor's frame the delegation names — a
                    // secondary's defaults are its own, and the key says which.
                    let key = defaults::CtorOmission {
                        class: parent,
                        secondary: sibling,
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
            // A base the runtime owns has no constructor to call: the object is already
            // allocated, and what the base's constructor would have done is store what it was
            // given. Which argument shapes that covers is the same question a direct
            // `Exception(…)` asks, and it is asked in the same place — see
            // `throwable_message_operand`, which declines a `cause` this storage cannot hold.
            None if model::external_base(declaration.superclass).is_some() => None,
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
        self.emit_function(id, signature, Ty::Unit, &name, &mut |body, params| {
            for (slot, (value, ty)) in params.iter().zip(&slots).enumerate() {
                let variable = body.declare_value(slot as u32, *ty)?;
                body.builder.def_var(variable, *value);
            }
            let this = params[0];
            if let Some(getter) = companion {
                let func_ref = body.func_ref(getter);
                body.emit_call(func_ref, &[])?;
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
                body.emit_call(func_ref, &arguments)?;
            }
            // The runtime-owned base has no constructor to call; its storage is written here
            // instead, in the same place and order the call would have run. The operands are
            // computed exactly as a direct `Exception(…)` computes them, so `Exception()` leaves
            // Kotlin's `null` message and a `null` cause.
            if parent.is_none() && model::external_base(declaration.superclass).is_some() {
                let Some(operands) = body.throwable_operands(
                    &declaration.fq_name(),
                    &declaration.super_args,
                    Some(&declaration.super_ctor_params),
                )?
                else {
                    return Ok(());
                };
                if body.terminated {
                    return Ok(());
                }
                // `Throwable(cause)` renders its message from the cause, and that rendering is the
                // runtime's. Asking the runtime for a throwable only to copy two fields out of it
                // would allocate one to throw away, so the entry point answering the pair is asked
                // for the MESSAGE and the cause is stored beside it.
                let (message, cause) = match operands {
                    ThrowableOperands::Message(message) => {
                        (message, body.builder.ins().iconst(types::I64, 0))
                    }
                    ThrowableOperands::MessageAndCause { message, cause } => (message, cause),
                    ThrowableOperands::Cause(cause) => {
                        let rendered = body.runtime_call(
                            "kt_throwable_message_of_cause",
                            &[any()],
                            any(),
                            &[cause],
                        )?;
                        if body.terminated {
                            return Ok(());
                        }
                        let Some(rendered) = rendered else {
                            return Ok(());
                        };
                        (rendered, cause)
                    }
                };
                // BOTH are written, the `null` cause included: an unwritten field is whatever the
                // allocation left there, and the collector traces this one.
                body.builder.ins().store(
                    trusted(),
                    message,
                    this,
                    model::EXTERNAL_BASE_FIELD_OFFSET as i32,
                );
                body.builder.ins().store(
                    trusted(),
                    cause,
                    this,
                    model::THROWABLE_CAUSE_OFFSET as i32,
                );
            }
            if !declaration.explicit_param_stores {
                let mut next_field = 0;
                for (index, argument) in declaration.ctor_args.iter().enumerate() {
                    if !argument.is_field {
                        continue;
                    }
                    let field = argument.field_index.unwrap_or(next_field);
                    next_field = field + 1;
                    let field_type =
                        model::field_storage_ty(body.file.values, body.file.ir, class, field);
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
        self.emit_function(getter, signature, any(), &name, &mut |body, _| {
            if let Some(initializer) = enclosing_enum {
                let func_ref = body.func_ref(initializer);
                body.emit_call(func_ref, &[])?;
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
            body.undone_on_failure(
                &mut |body| body.emit_call(func_ref, &[instance]).map(drop),
                // The next access constructs it again, and throws again, where Kotlin throws.
                &mut |body| {
                    let null = body.builder.ins().iconst(types::I64, 0);
                    body.builder.ins().store(trusted(), null, slot_address, 0);
                    Ok(())
                },
            )?;
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

/// The declaration a `super` call names: its class, its member, and how the frontend selected it.
pub(super) struct SuperTarget<'c> {
    pub(super) owner: TypeName,
    pub(super) name: &'c str,
    pub(super) kind: crate::ir::IrSuperCallKind,
    pub(super) source: Option<crate::fir::CallableId>,
    pub(super) params: Option<&'c [Ty]>,
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
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
        // A dispatched call is as able to throw as a direct one, and MORE able to be forgotten:
        // it is the one call this backend emits that does not go through `emit_call`. A `try`
        // whose body invokes a lambda is the common shape, and the exception walked straight out
        // of the `try` until this check existed.
        self.check_pending()?;
        Ok(self.builder.inst_results(call).first().copied())
    }

    pub(super) fn field_read(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
    ) -> Result<Option<Value>, Unsupported> {
        // A value class's own property, read off an occurrence carried as the value, IS that
        // value: there is no object to load it from, and none is needed.
        let classifier = self.file.ir.classes[class as usize].fq_name;
        if self.file.values.is_value_class(classifier)
            && self
                .type_of(receiver)
                .and_then(|ty| self.file.values.unboxed(ty))
                == Some(classifier)
            && model::value_field(self.file.values, self.file.ir, class)? == index
        {
            let Some(value) = self.coerce(receiver, Ty::Obj(classifier, &[]))? else {
                return Ok(None);
            };
            // The value is held at the declared underlying type; the node reads the field at the
            // type the IR gives it, which a generic value class spells as its type parameter.
            let stored = model::field_storage_ty(self.file.values, self.file.ir, class, index);
            let field = captures::physical_ty(
                self.file.ir,
                class,
                index,
                self.file.ir.classes[class as usize].fields[index as usize].ty,
            );
            return self.convert(value, Some(stored), field);
        }
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let ty = captures::physical_ty(
            self.file.ir,
            class,
            index,
            self.file.ir.classes[class as usize].fields[index as usize].ty,
        );
        self.load_field(object, class, index, ty).map(Some)
    }

    /// Load a field, with the throw-if-null a `lateinit` one carries.
    ///
    /// Every path that reads a field goes through here, and there are three: the `GetField` node,
    /// the property read that finds storage rather than a getter, and the `super` read that does.
    /// The guard being on the LOAD rather than on the node is what makes that true — a `lateinit`
    /// property read from outside its class reaches the second of those, and reading it before
    /// anything assigned it answered null quietly until it did.
    pub(super) fn load_field(
        &mut self,
        object: Value,
        class: ClassId,
        index: u32,
        ty: Ty,
    ) -> Result<Value, Unsupported> {
        let offset = self.file.model.layout(class).fields[index as usize].offset as i32;
        let stored = model::field_storage_ty(self.file.values, self.file.ir, class, index);
        let clif = self
            .carrier(stored)
            .clif()
            .expect("fields are never `Unit`");
        let value = self.builder.ins().load(clif, trusted(), object, offset);
        let field = &self.file.ir.classes[class as usize].fields[index as usize];
        if field.is_lateinit() {
            let name = field.name.clone();
            self.lateinit_guard(value, &name)?;
        }
        if stored == ty {
            return Ok(value);
        }
        Ok(self
            .convert(value, Some(stored), ty)?
            .expect("a field is never `Unit`"))
    }

    /// The RAW value of a `lateinit` field, behind `::prop.isInitialized`.
    ///
    /// The one read of such a field that must NOT carry the throw-if-null guard: the guard answers
    /// this question by throwing, and the caller answers it with a comparison instead. Null IS the
    /// evidence — that is why `lateinit` is only allowed on a type with a null to be distinguished
    /// by. The comparison is NOT built here: common lowering wraps this node in the ordinary
    /// null-comparison node, so this answers the field and building a `Boolean` here would be
    /// compared against null a second time.
    pub(super) fn lateinit_initialized(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        let offset = self.file.model.layout(class).fields[index as usize].offset as i32;
        let ty = captures::physical_ty(
            self.file.ir,
            class,
            index,
            self.file.ir.classes[class as usize].fields[index as usize].ty,
        );
        let clif = self.carrier(ty).clif().expect("fields are never `Unit`");
        // The RAW load, not `load_field`: that one carries the guard this is asking about.
        Ok(Some(self.builder.ins().load(
            clif,
            trusted(),
            object,
            offset,
        )))
    }

    /// A `lateinit` read whose storage is not an instance field — a local slot or a top-level
    /// property — where common lowering names the guard as its own node instead of leaving it to
    /// be inferred from the field. The guard is the field read's, because it is the same guard:
    /// null is the evidence either way.
    pub(super) fn lateinit_check(
        &mut self,
        operand: u32,
        name: &str,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.expression(operand)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(value) = value else {
            return Err("a `lateinit` read of a `Unit` value".to_string());
        };
        self.lateinit_guard(value, name)?;
        Ok(Some(value))
    }

    /// The throw-if-null every read of a `lateinit` property carries.
    ///
    /// Kotlin puts the guard at the READ rather than tracking initialization, because the field
    /// being null IS the evidence — and it is why `lateinit` is only allowed on a type that has a
    /// null to be distinguishable by. `LateinitInitialized` is the one read that must NOT carry
    /// it: `::prop.isInitialized` asks the question this guard answers by throwing.
    pub(super) fn lateinit_guard(&mut self, value: Value, name: &str) -> Result<(), Unsupported> {
        let initialized = self.builder.create_block();
        let missing = self.builder.create_block();
        self.builder
            .ins()
            .brif(value, initialized, &[], missing, &[]);

        self.builder.switch_to_block(missing);
        self.builder.seal_block(missing);
        let name = self.string_literal(name.as_bytes())?;
        self.runtime_call("kt_uninitialized_property", &[any()], Ty::Unit, &[name])?;
        // The call's own check sees the exception this just raised and leaves; the jump is the
        // terminator that block still needs, and nothing reaches it.
        if !self.terminated {
            self.builder.ins().jump(initialized, &[]);
        }

        self.continue_in(initialized);
        self.builder.seal_block(initialized);
        Ok(())
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
        let ty = model::field_storage_ty(self.file.values, self.file.ir, class, index);
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
        if is_runtime_constructed(internal) {
            return self.runtime_construction(internal, args, selected);
        }
        let class = self.file.class_of(internal, "construction of")?;
        let declaration = &self.file.ir.classes[class as usize];
        if declaration.is_object {
            return Err(format!("construction of the object declaration `{name}`"));
        }
        if declaration.is_abstract || declaration.is_sealed {
            return Err(format!("construction of the abstract class `{name}`"));
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
                let ordinal = declaration.secondary_ctors.iter().position(|candidate| {
                    // Either spelling of the same constructor: the declaration's own parameter
                    // list, or that list behind the PREFIX an inner class's constructors lead
                    // with. Which one the call node carries depends on whether the outer instance
                    // was part of the selection it recorded.
                    if candidate.params == selected {
                        return true;
                    }
                    let mut physical = candidate.prefix_params.clone();
                    physical.extend(candidate.params.iter().copied());
                    !candidate.prefix_params.is_empty() && physical == selected
                });
                match ordinal {
                    Some(ordinal) => {
                        let secondary = &declaration.secondary_ctors[ordinal];
                        // A PREFIX is what an inner class's constructors all lead with — the outer
                        // instance — and the construction supplies it as an ordinary argument, the
                        // same way the primary's is supplied. So the physical list is the prefix
                        // and then the declaration's own; the arity check below is what says
                        // whether the call really carries one.
                        let mut params = secondary.prefix_params.clone();
                        params.extend(secondary.params.iter().copied());
                        (
                            self.file.classes[class as usize].secondaries[ordinal],
                            params,
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
        self.emit_call(func_ref, &arguments)?;
        self.constructed(object, class)
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
        let params = super::super::super::captures::carried_parameters(self.file.ir, fid);
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

    /// What a construction of `class` answers, given the object its constructor filled: that
    /// object, except for a value class, whose instance is the VALUE. The constructor ran on a box
    /// — its `init` blocks and property initializers are the class's own, and that is where they
    /// run — and the value is read back out of it.
    pub(super) fn constructed(
        &mut self,
        object: Value,
        class: ClassId,
    ) -> Result<Option<Value>, Unsupported> {
        if self.terminated {
            return Ok(Some(object));
        }
        let classifier = self.file.ir.classes[class as usize].fq_name;
        if !self.file.values.is_value_class(classifier) {
            return Ok(Some(object));
        }
        self.unbox_value(object, classifier).map(Some)
    }

    /// A value of value class `classifier` put in a box of its own type — what `Any`, a type
    /// parameter or an interface holds it as. Only the value is stored: Kotlin's box runs no
    /// constructor, the value having been validated when it was made.
    pub(super) fn box_value(
        &mut self,
        value: Value,
        classifier: TypeName,
    ) -> Result<Value, Unsupported> {
        let class = self.value_class(classifier)?;
        let (offset, _) = self.file.value_storage(class)?;
        let descriptor = self.file.classes[class as usize].descriptor;
        let size = self.file.model.layout(class).instance_size;
        let object = self.allocate(descriptor, size)?;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(object)
    }

    /// The value a box of value class `classifier` holds.
    pub(super) fn unbox_value(
        &mut self,
        object: Value,
        classifier: TypeName,
    ) -> Result<Value, Unsupported> {
        let class = self.value_class(classifier)?;
        let (offset, ty) = self.file.value_storage(class)?;
        let clif = self.carrier(ty).clif().expect("a value is never `Unit`");
        Ok(self.builder.ins().load(clif, trusted(), object, offset))
    }

    /// The class of this file a boxed value class is laid out by. A value class declared
    /// somewhere else has no layout here, and boxing one declines.
    fn value_class(&self, classifier: TypeName) -> Result<ClassId, Unsupported> {
        self.file.ir.class_id_by_name(classifier).ok_or_else(|| {
            format!(
                "a boxed `{}`, a value class this file does not declare",
                classifier.render().replace('/', ".")
            )
        })
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
            .is_some_and(|storage| self.carrier(storage) != self.carrier(property.ty))
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
            // A value class's getter is its own member and takes the VALUE as `this`; what is in
            // hand here is the object, so the value is read out of it first.
            let classifier = self.file.ir.classes[class as usize].fq_name;
            let this = if self.file.values.is_value_class(classifier) {
                self.unbox_value(object, classifier)?
            } else {
                object
            };
            let id = self.file.functions[getter as usize].expect("a getter has a body");
            let func_ref = self.func_ref(id);
            let call = self.emit_call(func_ref, &[this])?;
            return Ok(self.builder.inst_results(call).first().copied());
        }
        match property.backing_field {
            Some(field) => {
                let ty = self.file.ir.classes[class as usize].fields[field as usize].ty;
                self.load_field(object, class, field, ty).map(Some)
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
            self.emit_call(func_ref, &[object, value])?;
            return Ok(());
        }
        let field = property.backing_field.expect("checked above");
        let offset = self.file.model.layout(class).fields[field as usize].offset as i32;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(())
    }

    /// The instance of an `object` declaration.
    pub(super) fn singleton(&mut self, classifier: TypeName) -> Result<Option<Value>, Unsupported> {
        // `Unit` is the runtime's, not the program's: every file that mentions it means the same
        // one value, so there is nothing per-file to declare.
        if classifier.matches("kotlin/Unit") {
            return self.runtime_call("kt_unit", &[], any(), &[]);
        }
        // An object the runtime realizes entirely has no instance and needs none: every member of
        // it is answered without reading the receiver. One provider materializes that receiver
        // before the call reaches the table that says so, and this is what it materializes.
        // The companion object of a BUILT-IN type. Its members are constants the frontend folds,
        // so the object itself is only ever an identity — and that identity is asked about:
        // `o === Int.Companion` is a corpus case, and `Int` written as a value is the same object.
        // The runtime holds one static object per companion, each with its own descriptor.
        if let Some(symbol) = super::super::super::intrinsics::builtin_companion(classifier) {
            return self.runtime_call(symbol, &[], any(), &[]);
        }
        if super::super::super::intrinsics::is_stateless_runtime_object(classifier)
            || self.is_result_companion(classifier)
        {
            return Ok(Some(self.builder.ins().iconst(types::I64, 0)));
        }
        let class = self.file.class_of(classifier, "the object")?;
        let Some((_, getter)) = self.file.classes[class as usize].singleton else {
            return Err(format!(
                "a singleton value of `{}`, which is not an object declaration",
                classifier.render()
            ));
        };
        let func_ref = self.func_ref(getter);
        let call = self.emit_call(func_ref, &[])?;
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
        let carried = super::super::super::captures::carried_parameters(ir, fid);
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

    /// The slot a class's own `name` of this ARITY takes; see [`FileLowering::member_slot`].
    pub(super) fn member_slot(
        &self,
        class: ClassId,
        name: &str,
        arity: usize,
    ) -> Option<(u32, Vec<Ty>, Ty)> {
        self.file.member_slot(class, name, arity)
    }

    /// A member asked of a type this file implements ITSELF, chosen by what the receiver turns
    /// out to be.
    ///
    /// The runtime answers such a member for the objects IT makes, and a class of this file's is
    /// not one of them — which is why this was a decline. But the file knows every class of its own
    /// that could stand behind that static type, so the choice is made here: test the receiver
    /// against each, and dispatch on that class's own slot when it matches. No program-wide slot
    /// number is needed, because the implementor is in this file by the very condition that raised
    /// the decline.
    ///
    /// Each operand, the receiver included, is evaluated ONCE, before the tests, and converted per
    /// arm: an implementor's own parameter type in its arm, the runtime entry point's carrier in
    /// the last one. The two sides state the operand differently and the value is the same, so the
    /// conversion is the ordinary boundary one and nothing is evaluated twice.
    pub(super) fn dispatch_by_implementor_with(
        &mut self,
        implementors: &[(ClassId, u32, Vec<Ty>, Ty)],
        object: Value,
        arguments: &[(Value, Option<Ty>)],
        answer: Ty,
        runtime: impl FnOnce(&mut Self, Value) -> Result<Option<Value>, Unsupported>,
    ) -> Result<Option<Value>, Unsupported> {
        let merge = self.builder.create_block();
        let carried = self.carrier(answer);
        if let Some(clif) = carried.clif() {
            self.builder.append_block_param(merge, clif);
        }
        for (class, slot, params, declared) in implementors {
            let descriptor = self.file.classes[*class as usize].descriptor;
            let type_address = self.data_address(descriptor);
            let matches = self
                .runtime_call(
                    "kt_is_instance",
                    &[any(), any()],
                    Ty::Boolean,
                    &[object, type_address],
                )?
                .expect("`kt_is_instance` returns a Boolean");
            let mine = self.builder.create_block();
            let rest = self.builder.create_block();
            self.builder.ins().brif(matches, mine, &[], rest, &[]);

            self.continue_in(mine);
            self.builder.seal_block(mine);
            // The carried list is the member's PARAMETERS, which `dispatch` prepends the receiver
            // to. Each operand crosses at the type this implementor declares for it.
            let mut operands = Vec::with_capacity(arguments.len());
            for ((value, source), target) in arguments.iter().zip(params.iter()) {
                let Some(converted) = self.convert(*value, *source, *target)? else {
                    return Ok(None);
                };
                operands.push(converted);
            }
            let produced = self.dispatch(object, *slot, params, *declared, &operands)?;
            // The slot's answer is the DECLARATION's; the site wants what the runtime entry point
            // would have handed back, so it is reconciled here as every other boundary is.
            let produced = match produced {
                Some(value) => self.convert(value, Some(*declared), answer)?,
                None => self.unit_where_wanted(answer)?,
            };
            self.jump_to_merge(merge, carried, produced);
            self.continue_in(rest);
            self.builder.seal_block(rest);
        }
        let produced = runtime(self, object)?;
        self.jump_to_merge(merge, carried, produced);

        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        Ok(carried.clif().map(|_| self.builder.block_params(merge)[0]))
    }

    /// `Unit` as the VALUE an arm answered where the site wants a reference: an override that
    /// answers `Unit` yields no machine value, and the merge still needs the object Kotlin hands
    /// back. `None` where no value is wanted, or where the arm has already left.
    fn unit_where_wanted(&mut self, answer: Ty) -> Result<Option<Value>, Unsupported> {
        if self.terminated || self.carrier(answer) != Carrier::Ref {
            return Ok(None);
        }
        self.runtime_call("kt_unit", &[], any(), &[])
    }

    /// Leave the current block for `merge`, carrying the arm's value where there is one.
    fn jump_to_merge(&mut self, merge: Block, carried: Carrier, produced: Option<Value>) {
        if self.terminated {
            return;
        }
        match (carried.clif(), produced) {
            (Some(_), Some(value)) => {
                self.builder.ins().jump(merge, &[BlockArg::Value(value)]);
            }
            // An arm that produced nothing where a value is wanted cannot reach the merge: `Unit`
            // was materialized before this, so the call it made diverged, and `terminated` above
            // is the ordinary way that is seen.
            (Some(clif), None) => {
                let filler = match clif {
                    types::F32 => self.builder.ins().f32const(0.0),
                    types::F64 => self.builder.ins().f64const(0.0),
                    integer => self.builder.ins().iconst(integer, 0),
                };
                self.builder.ins().jump(merge, &[BlockArg::Value(filler)]);
            }
            (None, _) => {
                self.builder.ins().jump(merge, &[]);
            }
        }
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
        kind: crate::ir::IrSuperCallKind,
        receiver: u32,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        // `name` is the ACCESSOR's own name as selection resolved it — `getB`, not `b` — because a
        // property's accessors are published accessor-shaped. `IrSuperCallKind` is the semantic
        // fact that says which accessor, and its contract is that a backend REALIZES the spelling
        // rather than recovering a property from one: so each candidate property's own accessor
        // name is derived here and compared forwards. Parsing `getB` back into `b` would be the
        // same inversion that named `getGetValue` on the JVM side, and it cannot be right for a
        // property whose accessor carries a `@JvmName` the spelling does not encode.
        let index = self.file.ir.classes[class as usize]
            .properties
            .iter()
            .position(|property| match kind {
                crate::ir::IrSuperCallKind::Function => property.name == name,
                crate::ir::IrSuperCallKind::PropertyGetter => {
                    crate::names::property_getter_name(&property.name) == name
                }
                crate::ir::IrSuperCallKind::PropertySetter => {
                    crate::names::property_setter_name(&property.name) == name
                }
            })?;
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
            let call = self.emit_call(func_ref, &[object])?;
            return Ok(self.builder.inst_results(call).first().copied());
        }
        let Some(field) = property.backing_field else {
            return Err(format!(
                "a `super` read of `{}`, which has neither storage nor a getter",
                property.name
            ));
        };
        let ty = self.file.ir.classes[class as usize].fields[field as usize].ty;
        self.load_field(object, class, field, ty).map(Some)
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
            self.emit_call(func_ref, &[object, value])?;
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
        target: SuperTarget<'_>,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let SuperTarget {
            owner,
            name,
            kind,
            source,
            params,
        } = target;
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
            if let Some(realized) = self.direct_property(class, name, kind, receiver, args) {
                return realized;
            }
            return Err(format!("a `super` call to an unknown method (`{name}`)"));
        };
        let Some(id) = self.file.functions[fid as usize] else {
            return Err(format!("a `super` call to the abstract method `{name}`"));
        };
        let params = super::super::super::captures::carried_parameters(ir, fid);
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let mut arguments = vec![object];
        arguments.extend(self.arguments(args, &params)?);
        if self.terminated {
            return Ok(None);
        }
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &arguments)?;
        Ok(self.builder.inst_results(call).first().copied())
    }
}

/// Marks a checked property that turned out to be top-level, so the caller routes it to
/// `super::statics` instead of looking for a class member. Never reaches a diagnostic.
pub(super) const TOP_LEVEL: &str = "\u{0}top-level";
