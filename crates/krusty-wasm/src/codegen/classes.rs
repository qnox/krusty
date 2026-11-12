//! The WasmGC form of one file's classes, built on the shared class tables
//! (`krusty::backend::class_tables`): a struct type per class, a dispatch-table type per class and
//! one instance of it, and the functions every class needs beyond its methods.
//!
//! **Objects.** A class's struct is `$Identified`'s two fields — the dispatch table, then the
//! object's identity hash — then the fields of the base no file declares: `kotlin.Enum`'s for an
//! enum class, a runtime exception's message and cause for a class extending one. The fields of its
//! superclasses in this file follow, root first, then its own: the tables' field order.
//! It is declared a subtype of its superclass's struct, so `ref.test` and `ref.cast` answer `is` and
//! `as` along the class hierarchy directly.
//!
//! **Superclasses of earlier files.** A class whose superclass an earlier file of the source module
//! laid out extends that file's record of it (`super::laid_out_classes`): its struct begins with the
//! recorded fields after the identity prefix, its table repeats the recorded slot types and holds
//! the recorded function wherever the class overrides nothing (`Slot::Inherited`). Every class this
//! file lays out is recorded in turn.
//!
//! **Tables.** A class's table is a struct with one immutable field per slot of its shared table,
//! each a typed function reference, and the interface table and the class's type information
//! record after `kotlin.Any`'s three
//! (`super::super::objects::slot_field`). It is declared a subtype of its superclass's table, or of
//! `$AnyTable` for a root class. A slot a table does not type is `funcref` and null. A class that
//! inherits `kotlin.Any`'s own `hashCode` gets its identity hash; one that inherits `toString` gets
//! `kotlin.Enum`'s, its `name`, `kotlin.Throwable`'s for a class extending a runtime exception, or
//! otherwise `kotlin.Any`'s default rendering. Both defaults print the name Kotlin gives the source
//! class (`krusty::backend::class_names::source_class_names`). A compiler-generated class has no
//! such name, so its slot holds the trap of a member with no body, and [`FileClasses::realizes`]
//! says so.
//!
//! **Interfaces.** Interface members are numbered once for the module, from the declaration
//! headers (`class_tables::module_interfaces`), and kept out of the class tables: a call through
//! one goes to the member's stub, which the finished module writes from every class's
//! implementations (`super::super::interface_dispatch`, `super::interface_implementations`).
//!
//! **Refusals.** Anything this form does not cover declines by name: a class kind other than an
//! ordinary class, interface or `object`, a superclass declared in a file not laid out yet, and an
//! `invoke` whose function-type signature needs a bridge in a class table.
//!
//! **Bridges.** An override whose representation differs from the base member it overrides
//! (`Derived : Base<Int>` returning an `i32` where `Base<T>` returns a reference) stands in the
//! base's slot through a bridge: a function of the base's signature that converts each operand
//! and dispatches through the override's own slot (see `Slot::Bridge`). A property accessor is
//! bridged the same way (`Slot::AccessorBridge`): `override val value: String` stands in for
//! `Holder<String?>.value: T & Any`, whose getter answers a reference.

use krusty::fx_hash::HashMap;

use krusty::backend::class_names::source_class_names;
use krusty::backend::class_tables::module_classes::ModuleClasses;
use krusty::backend::class_tables::module_slots::ModuleSlotPlan;
use krusty::backend::class_tables::{
    self, AnyMember, LaidOutSuperclass, NumberedClassTables, Representation, Slot, SlotKey,
    ANY_SLOTS,
};
use krusty::backend::local_properties::RealizedLocalPropertyAccess;
use krusty::backend::BackendClassifierSource;
use krusty::fir::{CallableId, PropertyId, ResolvedPropertyOverrideTarget};
use krusty::ir::{ClassId, FunId, IrClass, IrExpr, IrFile, IrLocalPropertyLayout};
use krusty::types::{Ty, TypeName};

use super::super::encode::{
    Code, CompositeType, FieldType, HeapType, Module, StorageType, SubType, ValType,
};
use super::super::exception_transport::ExceptionTransport;
use super::super::identity::PREFIX_FIELDS;
use super::super::interface_dispatch::{ClassRef, MemberRef};
use super::super::objects::{immutable, ObjectRoot, ITABLE_FIELD, REFERENCE};
use super::super::runtime::Runtime;
use super::laid_out_classes::{DependencyImplementation, LaidOutClass, LaidOutClasses, Placed};
use super::lower::{carrier, cell_reference, parameter_carrier, Unsupported, STRING};

mod constructor_prefix;
pub(super) use constructor_prefix::enclosing_field;

#[cfg(test)]
mod annotation_class_tests;
#[cfg(test)]
mod companion_construction_tests;
#[cfg(test)]
mod constructor_prefix_tests;

/// How the WasmGC form carries values, as the shared tables ask it.
struct WasmRepresentation<'a> {
    classifiers: &'a dyn BackendClassifierSource,
}

impl Representation for WasmRepresentation<'_> {
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

    /// `kotlin.Enum` is a base no file declares: its `name` and `ordinal` are the `$Enum` struct's
    /// ([`ObjectRoot::enum_base`]). So is a runtime exception class: its message and cause are its
    /// runtime struct's ([`super::super::exception_transport`]).
    fn owns_base(&self, superclass: TypeName) -> bool {
        superclass == krusty::types::wk::kotlin_enum()
            || is_runtime_exception(self.classifiers, superclass)
    }
}

/// Whether `classifier` is a runtime exception class: one whose authenticated declaration gives it
/// the runtime's Throwable storage.
pub(super) fn is_runtime_exception(
    classifiers: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> bool {
    classifiers
        .classifier(classifier)
        .is_some_and(|fact| fact.runtime_exception.is_some())
}

/// The base no file declares that a class's struct extends, through its superclasses of this file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OwnedBase {
    /// `kotlin.Any`: only the dispatch table.
    None,
    /// `kotlin.Enum`: `$Enum`'s `name` and `ordinal`.
    Enum,
    /// A runtime exception class, by its runtime struct: its message and cause.
    Exception(u32),
}

/// A synthesized property accessor a table slot holds: the field it reads or writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Accessor {
    pub(super) class: ClassId,
    pub(super) field: u32,
    pub(super) setter: bool,
}

/// A representation bridge a table slot holds (see [`Slot::Bridge`] and [`Slot::AccessorBridge`]):
/// `class` is the class that introduced it, whose table type every receiver reaching the bridge
/// has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct OverrideBridge {
    pub(super) class: ClassId,
    pub(super) member: BridgedMember,
}

/// The member a bridge wears the base's signature of, and where it forwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum BridgedMember {
    /// A method: the base method `declared`, forwarded through `target_slot` (see
    /// [`Slot::Bridge`]) to `target`.
    Method {
        declared: FunId,
        target_slot: Option<u32>,
        target: FunId,
    },
    /// A property accessor: the property type as the base declares it and as the implementation
    /// has it, forwarded through `target_slot` (see [`Slot::AccessorBridge`]).
    Accessor {
        declared: Ty,
        implemented: Ty,
        setter: bool,
        target_slot: u32,
    },
}

impl BridgedMember {
    /// The bridge `entry` stands for, or `None` for an entry that is not one this form carries.
    fn of(entry: &Slot) -> Option<Self> {
        match *entry {
            Slot::Bridge {
                declared,
                target_slot,
                target,
            } => Some(Self::Method {
                declared,
                target_slot,
                target,
            }),
            Slot::AccessorBridge {
                declared,
                implemented,
                receiver: None,
                setter,
                target_slot,
            } => Some(Self::Accessor {
                declared,
                implemented,
                setter,
                target_slot,
            }),
            _ => None,
        }
    }
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
    /// The representation bridges the tables hold, each with its function and that function's
    /// type, likewise defined with the file's bodies.
    pub(super) bridges: Vec<(OverrideBridge, u32, u32)>,
    /// The checked property each realized member read or write (by expression) selected.
    properties: HashMap<u32, PropertyId>,
    /// The base no file declares that each class extends, itself or through its superclasses,
    /// whose fields lead its struct after the identity prefix.
    bases: Vec<OwnedBase>,
    /// The record of every superclass an earlier file laid out that a class here extends.
    laid_out: HashMap<TypeName, LaidOutClass>,
    /// The function each slot of each class's table holds; empty for an interface.
    entries: Vec<Vec<Option<u32>>>,
    /// Each class's struct, then every struct it is declared a subtype of, nearest first; empty
    /// for an interface.
    ancestors: Vec<Vec<u32>>,
    /// The function each class's interface table holds at each number that no class slot fills,
    /// as interface recording placed it.
    pub(super) interface_bodies: Vec<HashMap<u32, Placed>>,
    /// How each class implements each member of a dependency's interface, as interface recording
    /// found it.
    pub(super) dependency_implementations: Vec<Vec<(MemberRef, DependencyImplementation)>>,
    /// Why a class with no instances of its own cannot be extended by a later file: what its
    /// table or interface entries needed that this form does not provide.
    pub(super) refusals: Vec<Option<Unsupported>>,
    /// Which of `kotlin.Any`'s members each class's table has a body for, by class and slot.
    realized: Vec<[bool; ANY_SLOTS as usize]>,
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
        PREFIX_FIELDS + self.base_fields(class) + self.tables.table(class).first_field + index
    }

    /// Whether the table of `class`, a class with instances of its own, has a body for `member`:
    /// `false` where the slot traps.
    pub(super) fn realizes(&self, class: ClassId, member: AnyMember) -> bool {
        self.realized[class as usize][member.slot() as usize]
    }

    /// How many fields of `class`'s struct, after the identity prefix, a base outside this file
    /// contributes: a superclass an earlier file laid out, `kotlin.Enum`'s `name` and `ordinal`,
    /// or a runtime exception's message and cause.
    pub(super) fn base_fields(&self, class: ClassId) -> u32 {
        match (self.laid_out_superclass(class), self.bases[class as usize]) {
            (Some(superclass), _) => superclass.fields.len() as u32,
            (None, OwnedBase::None) => 0,
            (None, OwnedBase::Enum) => ENUM_BASE_FIELDS,
            (None, OwnedBase::Exception(_)) => EXCEPTION_BASE_FIELDS,
        }
    }

    /// Whether `class`'s struct extends `kotlin.Enum`'s.
    pub(super) fn is_enum_rooted(&self, class: ClassId) -> bool {
        self.bases[class as usize] == OwnedBase::Enum
    }

    /// The runtime exception struct `class`'s struct extends, if it extends one.
    pub(super) fn exception_base(&self, class: ClassId) -> Option<u32> {
        match self.bases[class as usize] {
            OwnedBase::Exception(structure) => Some(structure),
            OwnedBase::None | OwnedBase::Enum => None,
        }
    }

    /// The nearest superclass of `class` an earlier file laid out, if it has one.
    pub(super) fn laid_out_superclass(&self, class: ClassId) -> Option<&LaidOutClass> {
        self.tables
            .table(class)
            .laid_out_superclass
            .map(|name| &self.laid_out[&name])
    }

    /// The function and type of `callable`, a method with a body of a superclass an earlier file
    /// laid out.
    pub(super) fn laid_out_method(&self, callable: CallableId) -> Option<(u32, u32)> {
        self.laid_out
            .values()
            .find_map(|superclass| superclass.methods.get(&callable).copied())
    }

    /// What a later file extending each class of this file reads of it. `functions` and
    /// `signatures` are the file's function indices and types, by `FunId`.
    pub(super) fn records(
        &self,
        ir: &IrFile,
        functions: &[Option<u32>],
        signatures: &[Option<u32>],
    ) -> Vec<(TypeName, LaidOutClass)> {
        let callables: HashMap<FunId, CallableId> = ir
            .checked_callable_functions
            .iter()
            .map(|(&callable, &function)| (function, callable))
            .collect();
        let mut records = Vec::new();
        for &id in &self.tables.order {
            let class = &ir.classes[id as usize];
            let (Some(structure), Some(table_type)) = (
                self.forms[id as usize].structure,
                self.forms[id as usize].table_type,
            ) else {
                continue;
            };
            let first_own =
                PREFIX_FIELDS + self.base_fields(id) + self.tables.table(id).first_field;
            let mut fields: Vec<FieldType> =
                match (self.laid_out_superclass(id), self.bases[id as usize]) {
                    (Some(superclass), _) => superclass.fields.clone(),
                    (None, OwnedBase::None) => Vec::new(),
                    (None, OwnedBase::Enum) => vec![immutable(STRING), immutable(ValType::I32)],
                    (None, OwnedBase::Exception(_)) => {
                        ExceptionTransport::base_fields(STRING).to_vec()
                    }
                };
            let mut chain = vec![id];
            while let Some(parent) = self
                .tables
                .table(*chain.last().expect("non-empty"))
                .superclass
            {
                chain.push(parent);
            }
            for &owner in chain.iter().rev() {
                fields.extend(
                    own_fields(&ir.classes[owner as usize])
                        .expect("every field was carried when the struct was declared"),
                );
            }
            debug_assert_eq!(
                fields.len() as u32,
                first_own - PREFIX_FIELDS + class.fields.len() as u32
            );
            let form = &self.forms[id as usize];
            let mut constructors = vec![form.constructor.map(|constructor| {
                (
                    constructor,
                    class.ctor_args.iter().map(|argument| argument.ty).collect(),
                )
            })];
            for (secondary, &function) in class.secondary_ctors.iter().zip(&form.secondaries) {
                let mut params = secondary.prefix_params.clone();
                params.extend(secondary.params.iter().copied());
                constructors.push(Some((function, params)));
            }
            let mut activation = self
                .laid_out_superclass(id)
                .map(|superclass| superclass.activation.clone())
                .unwrap_or_default();
            for &owner in chain.iter().rev() {
                let declaration = &ir.classes[owner as usize];
                // As construction initializes them: an enum's companion is the enum's own.
                if declaration.is_enum || declaration.is_enum_entry {
                    continue;
                }
                let companion = declaration
                    .companion_class
                    .and_then(|companion| ir.class_id_by_name(companion));
                if let Some(getter) = companion
                    .and_then(|companion| self.forms[companion as usize].singleton)
                    .map(|singleton| singleton.getter)
                {
                    activation.push(getter);
                }
            }
            let mut methods = self
                .laid_out_superclass(id)
                .map(|superclass| superclass.methods.clone())
                .unwrap_or_default();
            for &owner in chain.iter().rev() {
                for &method in &ir.classes[owner as usize].methods {
                    let (Some(&callable), Some(Some(function)), Some(Some(ty)), Some(_)) = (
                        callables.get(&method),
                        functions.get(method as usize),
                        signatures.get(method as usize),
                        ir.functions[method as usize].body,
                    ) else {
                        continue;
                    };
                    methods.insert(callable, (*function, *ty));
                }
            }
            let table = self.tables.table(id);
            records.push((
                class.fq_name,
                LaidOutClass {
                    structure,
                    table_type,
                    fields,
                    base: self.bases[id as usize],
                    slot_types: self.slot_types[id as usize].clone(),
                    entries: self.entries[id as usize].clone(),
                    ancestors: self.ancestors[id as usize].clone(),
                    constructors,
                    methods,
                    activation,
                    table: LaidOutSuperclass {
                        slots: table.vtable.len() as u32,
                        interfaces: self.tables.interface_tables.implemented[id as usize].clone(),
                        interface_entries: self.tables.interface_tables.members[id as usize]
                            .clone(),
                    },
                    interface_bodies: self.interface_bodies[id as usize].clone(),
                    dependency_implementations: self.dependency_implementations[id as usize]
                        .clone(),
                    refusal: self.refusals[id as usize].clone(),
                },
            ));
        }
        records
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

/// `kotlin.Enum`'s `name` and `ordinal`: the fields of `$Enum` after the identity prefix.
pub(super) const ENUM_BASE_FIELDS: u32 = 2;

/// A runtime exception's message and cause: the fields of its runtime struct after the identity
/// prefix.
const EXCEPTION_BASE_FIELDS: u32 = 2;

/// The wasm signature of a method of the file: the receiver, then its parameters.
pub(super) fn method_type(
    module: &mut Module,
    ir: &IrFile,
    function: FunId,
) -> Result<u32, Unsupported> {
    let declaration = &ir.functions[function as usize];
    let mut params = vec![REFERENCE];
    for parameter in 0..declaration.params.len() {
        params.push(
            parameter_carrier(module, ir, function, parameter as u32)?
                .ok_or_else(|| "a `Unit` parameter".to_string())?,
        );
    }
    let results = carrier(declaration.ret)?.into_iter().collect();
    Ok(module.func_type(params, results))
}

/// The function type of an accessor of a property of type `ty`, as a class table carries it: the
/// receiver, then the value a setter takes; a getter answers the value.
fn accessor_type(module: &mut Module, ty: Ty, setter: bool) -> Result<u32, Unsupported> {
    let value = carrier(ty)?.ok_or_else(|| "a `Unit` property".to_string())?;
    Ok(if setter {
        module.func_type(vec![REFERENCE, value], Vec::new())
    } else {
        module.func_type(vec![REFERENCE], vec![value])
    })
}

/// The module-wide numbering of the dispatch tables of `classes` and of their interfaces'
/// members, as every file of the module reads it: what lets a class extend one an earlier file
/// laid out.
pub(super) fn module_plan(
    classes: &ModuleClasses,
    classifiers: &dyn BackendClassifierSource,
) -> ModuleSlotPlan {
    ModuleSlotPlan::build(classes, &WasmRepresentation { classifiers })
}

/// Reject, by name, a class this form does not lay out.
fn check_supported(ir: &IrFile, id: ClassId) -> Result<(), Unsupported> {
    let class = &ir.classes[id as usize];
    let name = class.fq_name();
    // An annotation class that is only applied needs nothing at run time beyond an ordinary
    // layout. An annotation INSTANCE compares, hashes, and prints by its members, which this
    // object form does not provide, so a file constructing one is still declined.
    let construct = if class.annotation_impl_of.is_some()
        || (class.is_annotation && constructs(ir, class.fq_name))
    {
        "an annotation instance"
    } else if class.prop_ref.is_some() || class.func_ref.is_some() {
        "a callable reference"
    } else if class.lambda.is_some() || class.sam_wrapper.is_some() {
        "a function value's class"
    // An inner class's enclosing instance is already an ordinary constructor-prefix argument and
    // field in checked IR, which this object form can carry like every other reference field.
    // Anonymous superclass operands are also ordinary parameters, without capture storage.
    // Stored by-value captures use the same checked pre-super field transport.
    } else if !constructor_prefix::supported(ir, id) {
        "a class capturing its surroundings"
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

/// Reject a class overriding a runtime exception's property. Its message and cause are fields the
/// runtime reads directly, so an override has no slot that reading would reach.
fn check_exception_storage(
    ir: &IrFile,
    classifiers: &dyn BackendClassifierSource,
    id: ClassId,
) -> Result<(), Unsupported> {
    let class = &ir.classes[id as usize];
    let overrides = ir
        .property_overrides
        .get(&class.fq_name)
        .is_some_and(|edges| {
            edges
                .iter()
                .any(|edge| is_runtime_exception(classifiers, edge.overridden_owner))
        });
    if overrides {
        return Err(format!(
            "an override of a runtime exception's property (`{}`)",
            class.fq_name()
        ));
    }
    Ok(())
}

/// Whether a body `ir` emits constructs the class named `name`. Orphan expressions and
/// cloning-only inline templates are not emitted, so a construction there is no obligation.
fn constructs(ir: &IrFile, name: TypeName) -> bool {
    krusty::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| krusty::ir::value_namespace_expressions(ir, root))
        .any(|expression| {
            matches!(ir.expr(expression), IrExpr::New { internal, .. } if *internal == name)
        })
}

/// Lay out every class of `ir` in `module`: types, constructors' and accessors' indices, and the
/// dispatch tables, whose entries name `functions` (the file's function indices, by `FunId`). A
/// superclass no class of `ir` declares is read from `laid_out`, the classes earlier files of the
/// source module laid out.
/// What the source module says about classes beyond the file being declared.
#[derive(Clone, Copy)]
pub(super) struct BeyondFile<'a> {
    pub(super) classifiers: &'a dyn BackendClassifierSource,
    /// The module-wide numbering of every class's table and every interface member.
    pub(super) plan: &'a ModuleSlotPlan,
    /// The classes earlier files of the module laid out.
    pub(super) laid_out: &'a LaidOutClasses,
}

pub(super) fn declare(
    ir: &IrFile,
    module: &mut Module,
    runtime: &mut Runtime,
    functions: &[Option<u32>],
    properties: &RealizedProperties,
    beyond: BeyondFile<'_>,
) -> Result<FileClasses, Unsupported> {
    let BeyondFile {
        classifiers,
        plan,
        laid_out,
    } = beyond;
    for class in 0..ir.classes.len() {
        check_supported(ir, class as ClassId)?;
        check_exception_storage(ir, classifiers, class as ClassId)?;
    }
    let tables = class_tables::build_numbered(
        &WasmRepresentation { classifiers },
        ir,
        plan,
        &|superclass| laid_out.get(superclass).map(|record| record.table.clone()),
    )?;
    let superclasses: HashMap<TypeName, LaidOutClass> = tables
        .tables
        .iter()
        .filter_map(|table| table.laid_out_superclass)
        .map(|name| {
            let record = laid_out
                .get(name)
                .expect("the tables extend only a recorded class")
                .clone();
            match &record.refusal {
                Some(reason) => Err(format!("a class extending one of another file: {reason}")),
                None => Ok((name, record)),
            }
        })
        .collect::<Result<_, _>>()?;
    let laid_out_superclass = |class: ClassId| {
        tables
            .table(class)
            .laid_out_superclass
            .map(|name| &superclasses[&name])
    };
    let slot_types = slot_types(ir, module, &runtime.objects, &tables, &laid_out_superclass)?;

    let mut bases = Vec::with_capacity(ir.classes.len());
    for class in 0..ir.classes.len() {
        let mut root = class as ClassId;
        while let Some(parent) = tables.table(root).superclass {
            root = parent;
        }
        let superclass = ir.classes[root as usize].superclass;
        bases.push(match laid_out_superclass(root) {
            Some(superclass) => superclass.base,
            None if superclass == krusty::types::wk::kotlin_enum() => OwnedBase::Enum,
            None if is_runtime_exception(classifiers, superclass) => {
                OwnedBase::Exception(runtime.exception_class(module, classifiers, superclass)?)
            }
            None => OwnedBase::None,
        });
    }
    let objects = &mut runtime.objects;
    let mut forms: Vec<ClassForm> = ir.classes.iter().map(|_| ClassForm::default()).collect();
    for &id in &tables.order {
        let class = &ir.classes[id as usize];
        if class.is_interface {
            continue;
        }
        let table = tables.table(id);
        let parent = table.superclass.map(|parent| &forms[parent as usize]);
        let laid_out_parent = match table.superclass {
            None => laid_out_superclass(id),
            Some(_) => None,
        };
        let mut fields = vec![
            immutable(ValType::reference(objects.any_table)),
            immutable(ValType::I32),
        ];
        match (laid_out_superclass(id), bases[id as usize]) {
            (Some(superclass), _) => fields.extend(superclass.fields.iter().copied()),
            (None, OwnedBase::None) => {}
            (None, OwnedBase::Enum) => fields.extend([immutable(STRING), immutable(ValType::I32)]),
            (None, OwnedBase::Exception(_)) => {
                fields.extend(ExceptionTransport::base_fields(STRING));
            }
        }
        if let Some(parent) = table.superclass {
            fields.extend(inherited_fields(module, ir, &tables, parent)?);
        }
        fields.extend(own_fields(module, ir, id)?);
        let root = match bases[id as usize] {
            OwnedBase::None => objects.identified,
            OwnedBase::Enum => objects.enum_base,
            OwnedBase::Exception(structure) => structure,
        };
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(fields),
            supertype: Some(match (parent, laid_out_parent) {
                (Some(parent), _) => parent.structure.expect("a superclass has a struct"),
                (None, Some(superclass)) => superclass.structure,
                (None, None) => root,
            }),
            open: true,
        });
        let table_type = module.subtype(SubType {
            composite: table_struct(
                &slot_types[id as usize],
                objects.itable,
                objects.type_infos.ty,
            ),
            supertype: Some(match (parent, laid_out_parent) {
                (Some(parent), _) => parent.table_type.expect("a superclass has a table type"),
                (None, Some(superclass)) => superclass.table_type,
                (None, None) => objects.any_table,
            }),
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

    // What every class's table holds, and one table instance per class that has instances of its
    // own. An abstract class's entries are what a later file extending it inherits.
    let mut accessors: Vec<(Accessor, u32, u32)> = Vec::new();
    let mut bridges: Vec<(OverrideBridge, u32, u32)> = Vec::new();
    let mut realized = vec![[true; ANY_SLOTS as usize]; ir.classes.len()];
    let mut entries: Vec<Vec<Option<u32>>> = vec![Vec::new(); ir.classes.len()];
    let mut ancestry: Vec<Vec<u32>> = vec![Vec::new(); ir.classes.len()];
    let mut refusals: Vec<Option<Unsupported>> = vec![None; ir.classes.len()];
    for &id in &tables.order {
        let class = &ir.classes[id as usize];
        if class.is_interface {
            continue;
        }
        let mut ancestors = Vec::new();
        let mut ancestor = Some(id);
        let mut root = id;
        while let Some(current) = ancestor {
            ancestors.push(
                forms[current as usize]
                    .structure
                    .expect("a class has a struct"),
            );
            root = current;
            ancestor = tables.table(current).superclass;
        }
        if let Some(superclass) = laid_out_superclass(root) {
            ancestors.extend(superclass.ancestors.iter().copied());
        }
        let table = tables.table(id);
        let resolved = TableEntries {
            ir,
            functions,
            tables: &tables,
            slot_types: &slot_types,
            laid_out: laid_out_superclass(id),
            base: bases[id as usize],
        }
        .resolve(
            id,
            (module, runtime),
            &mut realized[id as usize],
            Synthesized {
                accessors: &mut accessors,
                bridges: &mut bridges,
            },
        );
        let instantiable = !(class.is_abstract || class.is_sealed);
        let functions_here = match resolved {
            Ok(functions_here) => functions_here,
            // A class with no instances of its own needs its entries only for a later file
            // extending it, which declines with the reason then.
            Err(reason) if !instantiable => {
                refusals[id as usize] = Some(reason);
                Vec::new()
            }
            Err(reason) => return Err(reason),
        };
        if instantiable {
            let (record, itable) =
                runtime
                    .objects
                    .interfaces
                    .class(module, runtime.objects.itable, ancestors.clone());
            let info = runtime.objects.type_infos.global(module, class.fq_name);
            runtime.objects.type_infos.name(
                module,
                class.fq_name,
                &krusty::backend::class_names::declared_class(ir, id),
            );
            let mut init = Code::default();
            for (slot, function) in functions_here.iter().enumerate() {
                if slot as u32 == ITABLE_FIELD {
                    init.global_get(itable).global_get(info);
                }
                match function {
                    Some(function) => {
                        module.reference_function(*function);
                        init.ref_func(*function);
                    }
                    None => {
                        init.ref_null(HeapType::Func);
                    }
                }
            }
            if table.vtable.len() as u32 <= ITABLE_FIELD {
                init.global_get(itable).global_get(info);
            }
            let table_type = forms[id as usize]
                .table_type
                .expect("a class has a table type");
            init.struct_new(table_type);
            let form = &mut forms[id as usize];
            form.table = Some(module.constant_global(ValType::reference(table_type), init));
            form.record = Some(record);
        }
        entries[id as usize] = functions_here;
        ancestry[id as usize] = ancestors;
    }
    let classes = ir.classes.len();
    Ok(FileClasses {
        tables,
        forms,
        slot_types,
        accessors,
        bridges,
        properties: properties
            .iter()
            .filter(|access| access.direct_member)
            .map(|access| (access.operation, access.target))
            .collect(),
        bases,
        realized,
        laid_out: superclasses,
        entries,
        ancestors: ancestry,
        interface_bodies: vec![HashMap::default(); classes],
        dependency_implementations: vec![Vec::new(); classes],
        refusals,
    })
}

/// The functions a table names that the file defines with its bodies, declared as tables first
/// name them.
struct Synthesized<'a> {
    accessors: &'a mut Vec<(Accessor, u32, u32)>,
    bridges: &'a mut Vec<(OverrideBridge, u32, u32)>,
}

/// What one class's table entries are resolved against.
struct TableEntries<'a> {
    ir: &'a IrFile,
    /// The file's function indices, by `FunId`.
    functions: &'a [Option<u32>],
    tables: &'a NumberedClassTables,
    slot_types: &'a [Vec<Option<u32>>],
    /// The nearest superclass an earlier file laid out, if the class has one.
    laid_out: Option<&'a LaidOutClass>,
    /// The base no file declares that the class extends.
    base: OwnedBase,
}

impl TableEntries<'_> {
    /// The function each slot of `id`'s table holds, declaring the accessors and bridges the
    /// file defines with its bodies; `None` for an untyped slot. `realized` learns which of
    /// `kotlin.Any`'s members have no body here and trap.
    fn resolve(
        &self,
        id: ClassId,
        (module, runtime): (&mut Module, &mut Runtime),
        realized: &mut [bool; ANY_SLOTS as usize],
        synthesized: Synthesized<'_>,
    ) -> Result<Vec<Option<u32>>, Unsupported> {
        let TableEntries {
            ir,
            functions,
            tables,
            slot_types,
            laid_out,
            base,
        } = *self;
        let Synthesized { accessors, bridges } = synthesized;
        let class = &ir.classes[id as usize];
        let table = tables.table(id);
        let mut resolved = Vec::with_capacity(table.vtable.len());
        for (slot, entry) in table.vtable.iter().enumerate() {
            let Some(ty) = slot_types[id as usize][slot] else {
                resolved.push(None);
                continue;
            };
            let function = match entry {
                Slot::AnyMember(member) => {
                    match inherited_any_member(ir, runtime, module, id, *member, base) {
                        Some(function) => function,
                        None => {
                            realized[slot] = false;
                            runtime.objects.trap(module, ty)
                        }
                    }
                }
                Slot::Abstract => runtime.objects.trap(module, ty),
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
                    accessor_function(module, accessors, accessor, ty)
                }
                Slot::Bridge { .. } | Slot::AccessorBridge { .. } | Slot::FunctionBridge { .. } => {
                    let member = BridgedMember::of(entry).ok_or_else(|| {
                        format!(
                            "an override carried differently from the member it overrides (`{}`)",
                            class.fq_name()
                        )
                    })?;
                    // The class that introduced the bridge: every receiver reaching this entry
                    // is an instance of it, so its table type is the one to cast to, whichever
                    // subclass's table this happens to be built for first. It is part of the
                    // bridge's identity, because two unrelated hierarchies can bridge accessors
                    // of the same types at the same slot.
                    let mut introducer = id;
                    while let Some(parent) = tables.table(introducer).superclass {
                        if tables.table(parent).vtable.get(slot) != Some(entry) {
                            break;
                        }
                        introducer = parent;
                    }
                    let bridge = OverrideBridge {
                        class: introducer,
                        member,
                    };
                    match bridges.iter().find(|(known, ..)| *known == bridge) {
                        Some((_, function, _)) => *function,
                        None => {
                            let function = module.declare();
                            bridges.push((bridge, function, ty));
                            function
                        }
                    }
                }
                Slot::Inherited { slot } => laid_out
                    .and_then(|superclass| {
                        superclass.entries.get(*slot as usize).copied().flatten()
                    })
                    .ok_or_else(|| {
                        format!(
                            "an inherited slot the superclass of another file holds nothing at \
                             (`{}`)",
                            class.fq_name()
                        )
                    })?,
            };
            resolved.push(Some(function));
        }
        Ok(resolved)
    }
}

/// The body the table of `class` holds for `kotlin.Any`'s `member`, which the class inherits
/// through `base`; `None` when this backend has none (see the module documentation).
fn inherited_any_member(
    ir: &IrFile,
    runtime: &mut Runtime,
    module: &mut Module,
    class: ClassId,
    member: AnyMember,
    base: OwnedBase,
) -> Option<u32> {
    Some(match (member, base) {
        (AnyMember::Equals, _) => runtime.objects.identity_equals,
        (AnyMember::HashCode, _) => runtime.identity.hash_code,
        (AnyMember::ToString, OwnedBase::Enum) => runtime.object_strings.enum_name,
        (AnyMember::ToString, OwnedBase::None) => {
            let names = source_class_names(ir, class)?;
            runtime.identity_to_string(module, ir.classes[class as usize].fq_name_id(), &names)
        }
        (AnyMember::ToString, OwnedBase::Exception(_)) => {
            let names = source_class_names(ir, class)?;
            runtime.throwable_to_string(module, ir.classes[class as usize].fq_name_id(), &names)
        }
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
/// and the interface table and the type information after `kotlin.Any`'s slots.
fn table_struct(slots: &[Option<u32>], itable: u32, type_info: u32) -> CompositeType {
    let mut fields = Vec::with_capacity(slots.len() + 2);
    for (slot, ty) in slots.iter().enumerate() {
        if slot as u32 == ITABLE_FIELD {
            fields.push(immutable(ValType::nullable(itable)));
            fields.push(immutable(ValType::nullable(type_info)));
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
        fields.push(immutable(ValType::nullable(type_info)));
    }
    CompositeType::Struct(fields)
}

/// The struct fields `class` declares, each mutable: a field is written by the constructor after
/// the object exists. A shared capture is the cell reference, not the element's carrier.
fn own_fields(
    module: &mut Module,
    ir: &IrFile,
    class: ClassId,
) -> Result<Vec<FieldType>, Unsupported> {
    let declared = ir.classes[class as usize]
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| (u32::try_from(index).expect("too many fields"), field.ty))
        .collect::<Vec<_>>();
    declared
        .into_iter()
        .map(|(index, ty)| {
            let value = field_storage(module, ir, class, index, ty)?.ok_or_else(|| {
                let declaration = &ir.classes[class as usize];
                format!(
                    "a `Unit`-typed field (`{}.{}`)",
                    declaration.fq_name(),
                    declaration.fields[index as usize].name
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
    module: &mut Module,
    ir: &IrFile,
    tables: &NumberedClassTables,
    class: ClassId,
) -> Result<Vec<FieldType>, Unsupported> {
    let mut fields = match tables.table(class).superclass {
        Some(parent) => inherited_fields(module, ir, tables, parent)?,
        None => Vec::new(),
    };
    fields.extend(own_fields(module, ir, class)?);
    Ok(fields)
}

/// The function type of every typed slot of every class's table.
///
/// A slot is typed by what introduced it: `kotlin.Any`'s member, the method or synthesized
/// accessor standing in it, or — for an abstract one — the member its key names. Every class
/// repeats its superclass's types, which is what makes its table type a subtype of the
/// superclass's; a disagreement is an override this form cannot carry, and declines. A superclass
/// an earlier file laid out (`laid_out_superclass`) contributes its recorded types.
fn slot_types<'r>(
    ir: &IrFile,
    module: &mut Module,
    objects: &ObjectRoot,
    tables: &NumberedClassTables,
    laid_out_superclass: &dyn Fn(ClassId) -> Option<&'r LaidOutClass>,
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
                    let value = field_carrier(module, ir, *class, *field)?;
                    Some(module.func_type(vec![REFERENCE], vec![value]))
                }
                Slot::FieldSetter { class, field } => {
                    let value = field_carrier(module, ir, *class, *field)?;
                    Some(module.func_type(vec![REFERENCE, value], Vec::new()))
                }
                Slot::Abstract => match keys.get(&(slot as u32)) {
                    Some(key) => Some(key_type(ir, module, objects, key)?),
                    None => None,
                },
                Slot::Bridge { declared, .. } => Some(method_type(module, ir, *declared)?),
                Slot::AccessorBridge {
                    declared, setter, ..
                } => Some(accessor_type(module, *declared, *setter)?),
                Slot::FunctionBridge { .. } | Slot::Inherited { .. } => None,
            };
            own[slot] = ty;
        }
        let inherited = match table.superclass {
            Some(parent) => Some(&types[parent as usize]),
            None => laid_out_superclass(id).map(|superclass| &superclass.slot_types),
        };
        if let Some(inherited) = inherited {
            for (slot, inherited) in inherited.iter().enumerate() {
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
            return accessor_type(module, *ty, setter);
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

/// How field `field` of `class` is carried. A shared capture is the cell, not its element.
pub(super) fn field_carrier(
    module: &mut Module,
    ir: &IrFile,
    class: ClassId,
    field: u32,
) -> Result<ValType, Unsupported> {
    let declaration = &ir.classes[class as usize];
    let declared = &declaration.fields[field as usize];
    field_storage(module, ir, class, field, declared.ty)?.ok_or_else(|| {
        format!(
            "a `Unit`-typed field (`{}.{}`)",
            declaration.fq_name(),
            declared.name
        )
    })
}

/// The carrier of constructor argument `index`, a cell when that argument fills a shared capture.
pub(super) fn constructor_argument_carrier(
    module: &mut Module,
    ir: &IrFile,
    class: ClassId,
    index: usize,
    declared: Ty,
) -> Result<ValType, Unsupported> {
    let field = ir.classes[class as usize]
        .ctor_args
        .get(index)
        .and_then(|argument| argument.field_index);
    match field {
        Some(field) if ir.shared_class_capture_fields.contains_key(&(class, field)) => {
            field_storage(module, ir, class, field, declared)?.ok_or_else(|| {
                format!(
                    "a `Unit` shared capture (`{}`)",
                    ir.classes[class as usize].fq_name()
                )
            })
        }
        _ => carrier(declared)?.ok_or_else(|| "a `Unit` constructor parameter".to_string()),
    }
}

/// A shared capture field stores the cell. Every other field stores its declared carrier.
fn field_storage(
    module: &mut Module,
    ir: &IrFile,
    class: ClassId,
    field: u32,
    declared: Ty,
) -> Result<Option<ValType>, Unsupported> {
    match ir.shared_class_capture_fields.get(&(class, field)) {
        Some(&element) => cell_reference(module, element).map(Some),
        None => carrier(declared),
    }
}
