//! JVM realization of `companion { … }` block members, which are static members of the class
//! that declares the block.
//!
//! A block function is a `public static final` method of the class; a block property is a static
//! field of the class behind `public static` accessors, laid out exactly like a top-level property
//! on its facade. Both are recorded in the class's `@Metadata` with the companion flag. Field
//! storage and initialization are [`static_fields`]'s; this module owns only what distinguishes a
//! block member.

use super::*;
use krusty::ir::IrStaticAccessor;
use krusty::jvm::names::type_descriptor;
use krusty::metadata::class_builder::{JvmFieldSignature, JvmPropertySignature};

/// Whether static `index` is block-property storage, whose field is private behind its accessors
/// exactly as a top-level property's is on the facade; only a `const` keeps its declared
/// visibility.
pub(super) fn storage_is_private(ir: &IrFile, index: u32) -> bool {
    ir.companion_blocks.is_storage(index) && !ir.statics[index as usize].is_const
}

/// Whether other classes read block property `index` through its class's compiler-default
/// public `getX` (see `SourceOrderedMember::StaticDefaultAccessor`).
pub(super) fn getter_owned(ir: &IrFile, index: u32) -> bool {
    ir.companion_blocks.is_storage(index) && ir.has_jvm_default_static_getter(index)
}

/// Whether other classes write block property `index` through its class's compiler-default
/// public `setX`; each accessor is decided on its own.
pub(super) fn setter_owned(ir: &IrFile, index: u32) -> bool {
    ir.companion_blocks.is_storage(index) && ir.has_jvm_default_static_setter(index)
}

/// How the JVM realizes block property `property` (`@Metadata`'s property signature): its static
/// storage behind public static accessors, exactly as a top-level property's on its facade.
pub(super) fn property_signature(
    ir: &IrFile,
    class: &krusty::ir::IrClass,
    property: &krusty::ir::IrCompanionBlockProperty,
) -> JvmPropertySignature {
    let local_classifiers = krusty::metadata::local_classifiers::names(ir);
    let accessor_sig = |fid: u32| {
        ir.functions.get(fid as usize).map(|function| {
            (
                function.name.clone(),
                ir_method_desc(&function.params, &function.ret),
            )
        })
    };
    let storage_index = property.storage;
    let storage = property
        .storage
        .map(|storage| &ir.statics[storage as usize]);
    let default_getter = || {
        (
            storage_index.map_or_else(
                || property_getter_name(&property.name),
                |storage| ir.jvm_static_getter_name(storage),
            ),
            format!("(){}", type_descriptor(property.ty)),
        )
    };
    let default_setter = || {
        (
            storage_index.map_or_else(
                || property_setter_name(&property.name),
                |storage| ir.jvm_static_setter_name(storage),
            ),
            format!("({})V", type_descriptor(property.ty)),
        )
    };
    JvmPropertySignature {
        getter: (!property.is_const && !property.visibility.is_private())
            .then(|| {
                property
                    .getter
                    .map_or_else(|| Some(default_getter()), accessor_sig)
            })
            .flatten(),
        // A bodiless `private set` leaves the default setter private, so it is never generated
        // (its storage records the setter `Absent`) and has no signature.
        setter: (property.is_var && !property.visibility.is_private())
            .then(|| match property.setter {
                Some(setter) => accessor_sig(setter),
                None => storage
                    .is_some_and(|storage| storage.accessors.setter == IrStaticAccessor::Default)
                    .then(default_setter),
            })
            .flatten(),
        field: storage.map(|storage| JvmFieldSignature {
            name: None,
            desc: Some(type_descriptor(storage.ty)).filter(|physical| {
                super::super::metadata_method_signatures::requires_field_signature(
                    property.ty,
                    physical,
                    &local_classifiers,
                )
            }),
        }),
        synthetic_method: property_marker_signature(ir, class, property.source_order),
        delegate_method: None,
        moved_from_interface_companion: false,
    }
}

/// Which of an interface's block-property storage classes holds a stored block property.
///
/// A JVM interface cannot declare a non-public or non-final field, so kotlinc keeps the storage
/// of an interface's `companion { … }` block properties in package-private
/// `<Interface>$$PrivateFieldsN` classes, each initialized by its own `<clinit>`. The interface's
/// `<clinit>` calls each class's `init()` at that group's place in declaration order: the blocks
/// declared before the companion object are initialized before it, the rest after it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StorageGroup {
    BeforeCompanion,
    AfterCompanion,
}

impl StorageGroup {
    const ALL: [StorageGroup; 2] = [StorageGroup::BeforeCompanion, StorageGroup::AfterCompanion];

    fn number(self) -> u8 {
        match self {
            StorageGroup::BeforeCompanion => 1,
            StorageGroup::AfterCompanion => 2,
        }
    }
}

/// Whether block-property storage `index` lives in its interface's storage class.
pub(super) fn in_interface_storage_class(ir: &IrFile, index: u32) -> bool {
    let storage = &ir.statics[index as usize];
    ir.companion_blocks.is_storage(index)
        && !storage.is_const
        && owning_interface(ir, index).is_some()
}

/// The storage class `group` of interface `interface`'s block properties.
fn interface_storage_class(interface: &str, group: StorageGroup) -> String {
    format!("{interface}$$PrivateFields{}", group.number())
}

/// The storage group of interface `class`'s stored block property `index`.
fn storage_group(ir: &IrFile, class: &IrClass, index: u32) -> StorageGroup {
    let companion_order = class
        .companion_class
        .and_then(|companion| ir.class_id_by_name(companion))
        .and_then(|companion| ir.class_source_order(companion));
    match companion_order {
        Some(order) if ir.statics[index as usize].source_order > order => {
            StorageGroup::AfterCompanion
        }
        _ => StorageGroup::BeforeCompanion,
    }
}

/// The JVM class whose field stores static `index`, given `owner`, the class that owns it.
pub(super) fn storage_class(ir: &IrFile, index: u32, owner: &str) -> String {
    match owning_interface(ir, index) {
        Some(class) if in_interface_storage_class(ir, index) => {
            interface_storage_class(owner, storage_group(ir, class, index))
        }
        _ => owner.to_string(),
    }
}

/// The block-property storage interface `class` keeps in storage class `group`, in declaration
/// order.
fn interface_storage(ir: &IrFile, class: &IrClass, group: StorageGroup) -> Vec<u32> {
    let mut storage: Vec<u32> = (0..ir.statics.len() as u32)
        .filter(|&index| {
            ir.statics[index as usize].owner == Some(class.fq_name_id())
                && in_interface_storage_class(ir, index)
                && storage_group(ir, class, index) == group
        })
        .collect();
    storage.sort_by_key(|&index| {
        let s = &ir.statics[index as usize];
        (s.source_order, index)
    });
    storage
}

/// Whether interface `class` keeps any block-property storage in storage classes.
pub(super) fn has_interface_storage(ir: &IrFile, class: &IrClass) -> bool {
    StorageGroup::ALL
        .iter()
        .any(|&group| !interface_storage(ir, class, group).is_empty())
}

/// The JVM interface of this file that owns static `index`, if one does.
fn owning_interface(ir: &IrFile, index: u32) -> Option<&IrClass> {
    let owner = ir.statics[index as usize].owner?;
    ir.classes
        .iter()
        .find(|class| class.is_interface && class.fq_name_id() == owner)
}

/// The access flags of static `index`'s compiler-default accessor: `public static final`, or
/// `public static` on an interface, where a method cannot be final.
pub(super) fn default_accessor_access(ir: &IrFile, index: u32) -> u16 {
    if owning_interface(ir, index).is_some() {
        0x0009
    } else {
        0x0019
    }
}

/// A reference to a compiler-default accessor of block property storage `index` on `owner`: an
/// interface's static accessor is an interface method.
pub(super) fn accessor_methodref(
    ir: &IrFile,
    cw: &mut ClassWriter,
    index: u32,
    owner: &str,
    name: &str,
    descriptor: &str,
) -> u16 {
    if owning_interface(ir, index).is_some() {
        cw.interface_methodref(owner, name, descriptor)
    } else {
        cw.methodref(owner, name, descriptor)
    }
}

/// Emit interface `class`'s block-property storage classes (see [`StorageGroup`]): for each
/// group, one package-private static field per stored property, a `<clinit>` that runs their
/// initializers in declaration order, and the empty `init()` the interface's own `<clinit>`
/// calls. Returns each class's name and bytes.
pub(super) fn emit_interface_storage_classes(
    ir: &IrFile,
    class: &IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<(String, Vec<u8>)> {
    StorageGroup::ALL
        .iter()
        .filter_map(|&group| emit_interface_storage_class(ir, class, group, facade, env, opts))
        .collect()
}

/// Emit interface `class`'s storage class `group`, if the group stores any property.
fn emit_interface_storage_class(
    ir: &IrFile,
    class: &IrClass,
    group: StorageGroup,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Option<(String, Vec<u8>)> {
    let storage = interface_storage(ir, class, group);
    if storage.is_empty() {
        return None;
    }
    let interface = class.fq_name();
    let holder = interface_storage_class(&interface, group);
    let mut cw = new_writer(&holder, "java/lang/Object", opts);
    cw.set_access(0x0010 | 0x0020 | 0x1000); // FINAL | SUPER | SYNTHETIC
    let mut init = CodeBuilder::new(0);
    init.ret_void();
    init.link();
    cw.add_method(0x1019, INTERFACE_STORAGE_INIT, "()V", &init); // PUBLIC | STATIC | FINAL | SYNTHETIC
    let initializers: Vec<(u32, krusty::ir::ExprId)> = storage
        .iter()
        .filter_map(|&index| {
            Some((
                index,
                super::static_fields::stored_clinit_initializer(ir, &ir.statics[index as usize])?,
            ))
        })
        .collect();
    if !initializers.is_empty() {
        let mut e = Emitter::new(
            ir,
            &mut cw,
            env,
            EmitterOwner {
                static_owner: Some(StaticOwner::Class(class.fq_name)),
                owner: &holder,
                facade,
            },
            Ty::Unit,
            initializers.iter().map(|&(_, init)| init),
        );
        e.enter_class_initializer(env.signature_symbols);
        let mut clinit = CodeBuilder::new(0);
        let mut lines: Vec<(u16, u32)> = Vec::new();
        for &(index, init) in &initializers {
            let first = clinit.line_marks().len();
            e.emit_clinit_static_store(&holder, index, init, &mut clinit);
            lines.extend(
                clinit.line_marks()[first..]
                    .iter()
                    .map(|&(pc, line)| (pc, u32::from(line))),
            );
            lines.dedup();
        }
        clinit.ret_void();
        clinit.ensure_locals(e.frame.max());
        clinit.link();
        e.cw.add_method(0x0008, "<clinit>", "()V", &clinit);
        if !lines.is_empty() {
            e.cw.set_method_lines("<clinit>", "()V", &lines);
        }
    }
    for &index in &storage {
        let s = &ir.statics[index as usize];
        let final_flag = if s.is_var { 0 } else { 0x0010 };
        cw.add_field_late(
            0x0008 | final_flag,
            ir.static_field_jvm_name(index),
            &ir_type_desc(&s.ty),
            None,
            super::declared_static_annotation(s),
        );
    }
    let xi = synthetic_class_xi(super::metadata_policy::SYNTHETIC_PRIVATE);
    cw.set_kotlin_metadata(3, opts.metadata_stamp(), xi, &[], &[]);
    Some((holder, env.run.finish_class(cw)))
}

/// The storage class's initialization trigger.
const INTERFACE_STORAGE_INIT: &str = "init";

/// In interface `class`'s `<clinit>`, initialize its block-property storage class `group`.
pub(super) fn emit_interface_storage_initialization(
    ir: &IrFile,
    class: &IrClass,
    group: StorageGroup,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
) {
    if interface_storage(ir, class, group).is_empty() {
        return;
    }
    let holder = interface_storage_class(&class.fq_name(), group);
    let init = cw.methodref(&holder, INTERFACE_STORAGE_INIT, "()V");
    code.invokestatic(init, 0, 0);
}
