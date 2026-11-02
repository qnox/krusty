//! The WasmGC form of one file's classes, built on the shared class tables
//! (`crate::backend::class_tables`): a struct type per class, a dispatch-table type per class and
//! one instance of it, and the functions every class needs beyond its methods.
//!
//! **Objects.** A class's struct is the root `$Object`'s single field — the dispatch table — then
//! the fields of its superclasses in this file, root first, then its own: the tables' field order.
//! It is declared a subtype of its superclass's struct, so `ref.test` and `ref.cast` answer `is` and
//! `as` along the class hierarchy directly.
//!
//! **Tables.** A class's table is a struct with one immutable field per slot of its shared table,
//! each a typed function reference, and the interface table after `kotlin.Any`'s three
//! (`super::super::objects::slot_field`). It is declared a subtype of its superclass's table, or of
//! `$AnyTable` for a root class. A slot a table does not type is `funcref` and null.
//!
//! **Interfaces.** Interface members are numbered once for the module, from the declaration
//! headers (`class_tables::module_interfaces`), and kept out of the class tables: a call through
//! one goes to the member's stub, which the finished module writes from every class's
//! implementations (`super::super::interface_dispatch`, `super::interface_implementations`).
//!
//! **Refusals.** Anything this form does not cover declines by name: a class kind other than an
//! ordinary class, interface or `object`, a superclass declared in another file, and an override
//! whose signature needs a bridge in a class table.

use crate::fx_hash::HashMap;

use crate::backend::class_tables::module_classes::ModuleClasses;
use crate::backend::class_tables::module_slots::ModuleSlotPlan;
use crate::backend::class_tables::{
    self, AnyMember, NumberedClassTables, Representation, Slot, SlotKey, ANY_SLOTS,
};
use crate::backend::local_properties::RealizedLocalPropertyAccess;
use crate::fir::{PropertyId, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrClass, IrFile, IrLocalPropertyLayout};
use crate::types::{Ty, TypeName};

use super::super::encode::{
    Code, CompositeType, FieldType, HeapType, Module, StorageType, SubType, ValType,
};
use super::super::interface_dispatch::ClassRef;
use super::super::objects::{immutable, ObjectRoot, ITABLE_FIELD, REFERENCE};
use super::lower::{carrier, Unsupported};

#[cfg(test)]
mod constructor_prefix_tests;

/// How the WasmGC form carries values, as the shared tables ask it.
struct WasmRepresentation;

impl Representation for WasmRepresentation {
    fn same(&self, a: Ty, b: Ty) -> bool {
        a == b
            || match (carrier(a), carrier(b)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
    }

    fn is_reference(&self, ty: Ty) -> bool {
        matches!(carrier(ty), Ok(Some(ValType::Ref { .. })))
    }

    fn owns_base(&self, _: TypeName) -> bool {
        false
    }
}

/// A synthesized property accessor a table slot holds: the field it reads or writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Accessor {
    pub(super) class: ClassId,
    pub(super) field: u32,
    pub(super) setter: bool,
}

/// The lazily constructed instance of an `object` declaration.
#[derive(Clone, Copy, Debug)]
pub(super) struct Singleton {
    /// The global holding the instance once it is constructed, null before.
    pub(super) global: u32,
    /// `() -> instance`: constructs it on first use.
    pub(super) getter: u32,
}

/// The same-file property accesses common realization turned into member reads and writes, each
/// with the checked property it selected.
pub(super) type RealizedProperties = [RealizedLocalPropertyAccess];

/// One class's WasmGC form.
#[derive(Debug, Default)]
pub(super) struct ClassForm {
    /// The struct its instances are; `None` for an interface.
    pub(super) structure: Option<u32>,
    /// Its dispatch table's type; `None` for an interface.
    pub(super) table_type: Option<u32>,
    /// The global holding its dispatch table; `None` for a class with no instances of its own.
    pub(super) table: Option<u32>,
    pub(super) constructor: Option<u32>,
    /// Parallel to `IrClass::secondary_ctors`.
    pub(super) secondaries: Vec<u32>,
    pub(super) singleton: Option<Singleton>,
    /// The class as the program's interface dispatch knows it; `None` for a class with no
    /// instances of its own.
    pub(super) record: Option<ClassRef>,
}

/// The WasmGC form of every class of one file.
pub(super) struct FileClasses {
    pub(super) tables: NumberedClassTables,
    pub(super) forms: Vec<ClassForm>,
    /// The function type of every typed slot, by class and slot; `None` for an untyped one.
    slot_types: Vec<Vec<Option<u32>>>,
    /// The synthesized accessors the tables hold — each with its function and that function's
    /// type — to be defined with the file's bodies.
    pub(super) accessors: Vec<(Accessor, u32, u32)>,
    /// The checked property each realized member read or write (by expression) selected.
    properties: HashMap<u32, PropertyId>,
}

impl FileClasses {
    /// The function type a call through `slot` of `class` takes.
    pub(super) fn slot_type(&self, class: ClassId, slot: u32) -> Option<u32> {
        self.slot_types[class as usize]
            .get(slot as usize)
            .copied()
            .flatten()
    }

    /// The struct field holding field `index` of `class`.
    pub(super) fn field(&self, class: ClassId, index: u32) -> u32 {
        1 + self.tables.table(class).first_field + index
    }

    /// The checked property the realized member access `operation` reads or writes.
    pub(super) fn realized_property(&self, operation: u32) -> Option<PropertyId> {
        self.properties.get(&operation).copied()
    }

    /// The struct type of `class`, which declined if it had none.
    pub(super) fn structure(&self, class: ClassId) -> u32 {
        self.forms[class as usize]
            .structure
            .expect("only an interface has no struct, and nothing reads an interface's field")
    }
}

/// The wasm signature of a method of the file: the receiver, then its parameters.
pub(super) fn method_type(
    module: &mut Module,
    ir: &IrFile,
    function: FunId,
) -> Result<u32, Unsupported> {
    let declaration = &ir.functions[function as usize];
    let mut params = vec![REFERENCE];
    for param in &declaration.params {
        params.push(carrier(*param)?.ok_or_else(|| "a `Unit` parameter".to_string())?);
    }
    let results = carrier(declaration.ret)?.into_iter().collect();
    Ok(module.func_type(params, results))
}

/// The module-wide numbering of the interface members of `classes`, as every file of the module
/// reads it.
pub(super) fn module_plan(classes: &ModuleClasses) -> ModuleSlotPlan {
    ModuleSlotPlan::interfaces_only(classes, &WasmRepresentation)
}

/// Reject, by name, a class this form does not lay out.
fn check_supported(class: &IrClass) -> Result<(), Unsupported> {
    let name = class.fq_name();
    let construct = if class.is_annotation || class.annotation_impl_of.is_some() {
        "an annotation class"
    } else if class.prop_ref.is_some() || class.func_ref.is_some() {
        "a callable reference"
    } else if class.lambda.is_some() || class.sam_wrapper.is_some() {
        "a function value's class"
    } else if class.is_enum || class.is_enum_entry {
        "an enum class"
    } else if class.is_value {
        "a value class"
    // An inner class's enclosing instance is already an ordinary constructor-prefix argument and
    // field in checked IR, which this object form can carry like every other reference field.
    // Anonymous superclass operands are also ordinary parameters, without capture storage.
    // Other captured classes still need their broader closure construction semantics.
    } else if unsupported_constructor_prefix(class) {
        "a class capturing its surroundings"
    } else if class.companion_class.is_some() {
        "a class with a companion object"
    } else if class
        .supertypes
        .iter()
        .any(|supertype| supertype.non_null().is_fun())
    {
        "a class implementing a function type"
    } else {
        return Ok(());
    };
    Err(format!("{construct} (`{name}`)"))
}

fn unsupported_constructor_prefix(class: &IrClass) -> bool {
    if class.is_inner_class {
        return false;
    }
    let Some(prefix) = class
        .ctor_args
        .get(..class.constructor_prefix_count as usize)
    else {
        return true;
    };
    prefix.iter().enumerate().any(|(index, argument)| {
        argument.provenance != crate::ir::IrCtorParameterProvenance::Value
            || argument.anonymous_super_forward != Some(index as u32 + 1)
            || argument.capture.is_some()
            || argument.capture_identity.is_some()
            || argument.is_field
            || argument.field_index.is_some()
    })
}

/// Lay out every class of `ir` in `module`: types, constructors' and accessors' indices, and the
/// dispatch tables, whose entries name `functions` (the file's function indices, by `FunId`).
pub(super) fn declare(
    ir: &IrFile,
    module: &mut Module,
    objects: &mut ObjectRoot,
    functions: &[Option<u32>],
    properties: &RealizedProperties,
    plan: &ModuleSlotPlan,
) -> Result<FileClasses, Unsupported> {
    for class in &ir.classes {
        check_supported(class)?;
    }
    let tables = class_tables::build_numbered(&WasmRepresentation, ir, plan)?;
    let slot_types = slot_types(ir, module, objects, &tables)?;

    let mut forms: Vec<ClassForm> = ir.classes.iter().map(|_| ClassForm::default()).collect();
    for &id in &tables.order {
        let class = &ir.classes[id as usize];
        if class.is_interface {
            continue;
        }
        let table = tables.table(id);
        let parent = table.superclass.map(|parent| &forms[parent as usize]);
        let mut fields = vec![immutable(ValType::reference(objects.any_table))];
        if let Some(parent) = table.superclass {
            fields.extend(inherited_fields(ir, &tables, parent)?);
        }
        fields.extend(own_fields(class)?);
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(fields),
            supertype: Some(parent.map_or(objects.object, |parent| {
                parent.structure.expect("a superclass has a struct")
            })),
            open: true,
        });
        let table_type = module.subtype(SubType {
            composite: table_struct(&slot_types[id as usize], objects.itable),
            supertype: Some(parent.map_or(objects.any_table, |parent| {
                parent.table_type.expect("a superclass has a table type")
            })),
            open: true,
        });
        let form = &mut forms[id as usize];
        form.structure = Some(structure);
        form.table_type = Some(table_type);
        if class.has_primary_ctor {
            form.constructor = Some(module.declare());
        }
        for _ in &class.secondary_ctors {
            form.secondaries.push(module.declare());
        }
        if class.is_object || class.is_companion {
            form.singleton = Some(Singleton {
                global: module.global(REFERENCE),
                getter: module.declare(),
            });
        }
    }

    // One table instance per class that has instances of its own.
    let mut accessors: Vec<(Accessor, u32, u32)> = Vec::new();
    for &id in &tables.order {
        let class = &ir.classes[id as usize];
        if class.is_interface || class.is_abstract || class.is_sealed {
            continue;
        }
        let mut ancestors = Vec::new();
        let mut ancestor = Some(id);
        while let Some(current) = ancestor {
            ancestors.push(
                forms[current as usize]
                    .structure
                    .expect("a class has a struct"),
            );
            ancestor = tables.table(current).superclass;
        }
        let (record, itable) = objects.interfaces.class(module, objects.itable, ancestors);
        let table = tables.table(id);
        let mut init = Code::default();
        for (slot, entry) in table.vtable.iter().enumerate() {
            if slot as u32 == ITABLE_FIELD {
                init.global_get(itable);
            }
            let Some(ty) = slot_types[id as usize][slot] else {
                init.ref_null(HeapType::Func);
                continue;
            };
            let function = match entry {
                Slot::AnyMember(AnyMember::Equals) => objects.identity_equals,
                Slot::AnyMember(AnyMember::HashCode | AnyMember::ToString) | Slot::Abstract => {
                    objects.trap(module, ty)
                }
                Slot::Function(function) => functions
                    .get(*function as usize)
                    .copied()
                    .ok_or_else(|| "an invalid function in a class dispatch table".to_string())?
                    .ok_or_else(|| {
                        "an inline-only function that remained in a class dispatch table"
                            .to_string()
                    })?,
                Slot::FieldGetter { class, field } | Slot::FieldSetter { class, field } => {
                    let accessor = Accessor {
                        class: *class,
                        field: *field,
                        setter: matches!(entry, Slot::FieldSetter { .. }),
                    };
                    accessor_function(module, &mut accessors, accessor, ty)
                }
                Slot::Bridge { .. } | Slot::FunctionBridge { .. } | Slot::AccessorBridge { .. } => {
                    return Err(format!(
                        "an override carried differently from the member it overrides (`{}`)",
                        class.fq_name()
                    ));
                }
            };
            module.reference_function(function);
            init.ref_func(function);
        }
        if table.vtable.len() as u32 <= ITABLE_FIELD {
            init.global_get(itable);
        }
        let table_type = forms[id as usize]
            .table_type
            .expect("a class has a table type");
        init.struct_new(table_type);
        let form = &mut forms[id as usize];
        form.table = Some(module.constant_global(ValType::reference(table_type), init));
        form.record = Some(record);
    }
    Ok(FileClasses {
        tables,
        forms,
        slot_types,
        accessors,
        properties: properties
            .iter()
            .filter(|access| access.direct_member)
            .map(|access| (access.operation, access.target))
            .collect(),
    })
}

/// The function standing for `accessor`, carried as `ty`: declared the first time a table names
/// it, and defined with the file's bodies.
pub(super) fn accessor_function(
    module: &mut Module,
    accessors: &mut Vec<(Accessor, u32, u32)>,
    accessor: Accessor,
    ty: u32,
) -> u32 {
    match accessors.iter().find(|(known, ..)| *known == accessor) {
        Some((_, function, _)) => *function,
        None => {
            let function = module.declare();
            accessors.push((accessor, function, ty));
            function
        }
    }
}

/// A table struct: one immutable typed function reference per typed slot, `funcref` elsewhere,
/// and the interface table after `kotlin.Any`'s slots.
fn table_struct(slots: &[Option<u32>], itable: u32) -> CompositeType {
    let mut fields = Vec::with_capacity(slots.len() + 1);
    for (slot, ty) in slots.iter().enumerate() {
        if slot as u32 == ITABLE_FIELD {
            fields.push(immutable(ValType::nullable(itable)));
        }
        fields.push(immutable(match ty {
            Some(ty) => ValType::reference(*ty),
            None => ValType::Ref {
                nullable: true,
                heap: HeapType::Func,
            },
        }));
    }
    if slots.len() as u32 <= ITABLE_FIELD {
        fields.push(immutable(ValType::nullable(itable)));
    }
    CompositeType::Struct(fields)
}

/// The struct fields `class` declares, each mutable: a field is written by the constructor after
/// the object exists.
fn own_fields(class: &IrClass) -> Result<Vec<FieldType>, Unsupported> {
    class
        .fields
        .iter()
        .map(|field| {
            let value = carrier(field.ty)?.ok_or_else(|| {
                format!(
                    "a `Unit`-typed field (`{}.{}`)",
                    class.fq_name(),
                    field.name
                )
            })?;
            Ok(FieldType {
                storage: StorageType::Val(value),
                mutable: true,
            })
        })
        .collect()
}

/// Every field of `class` and its superclasses in this file, root first.
fn inherited_fields(
    ir: &IrFile,
    tables: &NumberedClassTables,
    class: ClassId,
) -> Result<Vec<FieldType>, Unsupported> {
    let mut fields = match tables.table(class).superclass {
        Some(parent) => inherited_fields(ir, tables, parent)?,
        None => Vec::new(),
    };
    fields.extend(own_fields(&ir.classes[class as usize])?);
    Ok(fields)
}

/// The function type of every typed slot of every class's table.
///
/// A slot is typed by what introduced it: `kotlin.Any`'s member, the method or synthesized
/// accessor standing in it, or — for an abstract one — the member its key names. Every class
/// repeats its superclass's types, which is what makes its table type a subtype of the
/// superclass's; a disagreement is an override this form cannot carry, and declines.
fn slot_types(
    ir: &IrFile,
    module: &mut Module,
    objects: &ObjectRoot,
    tables: &NumberedClassTables,
) -> Result<Vec<Vec<Option<u32>>>, Unsupported> {
    let mut types: Vec<Vec<Option<u32>>> = vec![Vec::new(); ir.classes.len()];
    for &id in &tables.order {
        let table = tables.table(id);
        let mut keys: HashMap<u32, &SlotKey> = HashMap::default();
        for (key, slot) in &table.slots {
            keys.entry(*slot)
                .and_modify(|known| {
                    if key_order(key) < key_order(known) {
                        *known = key;
                    }
                })
                .or_insert(key);
        }
        let mut own = vec![None; table.vtable.len()];
        for (slot, entry) in table.vtable.iter().enumerate() {
            let ty = match entry {
                Slot::AnyMember(member) => Some(objects.any_slot_types[member.slot() as usize]),
                Slot::Function(function) => Some(method_type(module, ir, *function)?),
                Slot::FieldGetter { class, field } => {
                    let value = field_carrier(ir, *class, *field)?;
                    Some(module.func_type(vec![REFERENCE], vec![value]))
                }
                Slot::FieldSetter { class, field } => {
                    let value = field_carrier(ir, *class, *field)?;
                    Some(module.func_type(vec![REFERENCE, value], Vec::new()))
                }
                Slot::Abstract => match keys.get(&(slot as u32)) {
                    Some(key) => Some(key_type(ir, module, objects, key)?),
                    None => None,
                },
                Slot::Bridge { declared, .. } => Some(method_type(module, ir, *declared)?),
                Slot::FunctionBridge { .. } | Slot::AccessorBridge { .. } => None,
            };
            own[slot] = ty;
        }
        if let Some(parent) = table.superclass {
            for (slot, inherited) in types[parent as usize].iter().enumerate() {
                match (inherited, own[slot]) {
                    (Some(inherited), Some(mine)) if *inherited != mine => {
                        return Err(format!(
                            "an override whose signature differs from the member it overrides \
                             (`{}`)",
                            ir.classes[id as usize].fq_name()
                        ));
                    }
                    (Some(inherited), None) => own[slot] = Some(*inherited),
                    _ => {}
                }
            }
        }
        types[id as usize] = own;
    }
    Ok(types)
}

/// A total order over the keys naming one slot, so the one a slot's type is read from does not
/// depend on a map's iteration order.
fn key_order(key: &SlotKey) -> (u8, u32) {
    match key {
        SlotKey::Any(slot) => (0, *slot),
        SlotKey::Function(function) => (1, *function),
        SlotKey::Getter(target) => (2, property_order(*target)),
        SlotKey::Setter(target) => (3, property_order(*target)),
    }
}

fn property_order(target: ResolvedPropertyOverrideTarget) -> u32 {
    match target {
        ResolvedPropertyOverrideTarget::Module(property) => property.raw(),
        ResolvedPropertyOverrideTarget::External(property) => property.raw(),
    }
}

/// The function type of the member `key` names.
fn key_type(
    ir: &IrFile,
    module: &mut Module,
    objects: &ObjectRoot,
    key: &SlotKey,
) -> Result<u32, Unsupported> {
    match key {
        SlotKey::Any(slot) => Ok(objects.any_slot_types[*slot as usize % ANY_SLOTS as usize]),
        SlotKey::Function(function) => method_type(module, ir, *function),
        SlotKey::Getter(target) => property_key_type(ir, module, target, false),
        SlotKey::Setter(target) => property_key_type(ir, module, target, true),
    }
}

/// The type of one property-accessor slot. A materialized accessor is authoritative for its
/// complete parameter list; only a plain field accessor has no function and uses the property's
/// checked value type to choose its representation signature.
fn property_key_type(
    ir: &IrFile,
    module: &mut Module,
    target: &ResolvedPropertyOverrideTarget,
    setter: bool,
) -> Result<u32, Unsupported> {
    let ResolvedPropertyOverrideTarget::Module(property) = target else {
        return Err("a member overriding a library property".to_string());
    };
    let function = match ir.local_property_layouts.get(property) {
        Some(IrLocalPropertyLayout::Member {
            getter,
            setter: property_setter,
            ty,
            ..
        }) => {
            let function = if setter { *property_setter } else { *getter };
            if let Some(function) = function {
                return method_type(module, ir, function);
            }
            let value = carrier(*ty)?.ok_or_else(|| "a `Unit` property".to_string())?;
            let mut params = vec![REFERENCE];
            let results = if setter {
                params.push(value);
                Vec::new()
            } else {
                vec![value]
            };
            return Ok(module.func_type(params, results));
        }
        Some(IrLocalPropertyLayout::MemberExtension {
            getter,
            setter: property_setter,
            ..
        }) => {
            if setter {
                *property_setter
            } else {
                Some(*getter)
            }
        }
        _ => None,
    }
    .ok_or_else(|| "a property table slot with no recorded accessor".to_string())?;
    method_type(module, ir, function)
}

/// How field `field` of `class` is carried.
pub(super) fn field_carrier(
    ir: &IrFile,
    class: ClassId,
    field: u32,
) -> Result<ValType, Unsupported> {
    let declaration = &ir.classes[class as usize];
    carrier(declaration.fields[field as usize].ty)?.ok_or_else(|| {
        format!(
            "a `Unit`-typed field (`{}.{}`)",
            declaration.fq_name(),
            declaration.fields[field as usize].name
        )
    })
}
