//! Object layout and virtual-dispatch tables for the classes of one IR file.
//!
//! Everything here is a pure function over an [`IrFile`]; nothing emits code. The code generator
//! asks two questions of the result — "where is this field" and "which vtable slot is this member"
//! — and this module answers both from the IR's own hierarchy and override tables
//! (`IrFile::classifier_hierarchies`, `function_overrides`, `property_overrides`) rather than by
//! matching names.
//!
//! **Layout** is the classic single-inheritance one: the object header, then the superclass's
//! fields as a prefix in the superclass's order, then this class's fields in declaration order,
//! each aligned to its size, and the whole rounded up to 8. A subclass's first field follows the
//! superclass's last one directly, not the superclass's rounded size. The same offsets feed both
//! the loads and stores the generator emits and the `reference_offsets` table in the class's
//! descriptor, so the program and the collector cannot disagree about where a reference is.
//!
//! **Vtables** are the classic single-inheritance ones too: a class's table is its superclass's
//! with overridden slots replaced and newly declared members appended. Slot numbers are assigned
//! at the declaring class and stay valid down the hierarchy, which is what lets a call through an
//! `A`-typed value reach `B`'s override with one load. Every table begins with `kotlin.Any`'s
//! three slots — `equals`, `hashCode`, `toString`, in that order — which the runtime defines.
//!
//! Property accessors are members too. A property that is `open` (or overrides one) dispatches
//! through a getter (and, for a `var`, a setter) slot; when the source declares no accessor body,
//! the slot is a synthesized field access, so `x.v` through an `A`-typed `x` reads `B`'s `v` when
//! `B` overrides it. A property that is neither open nor an override is a direct field access.
//!
//! **Refusals.** Constructs not lowered yet — interfaces, data classes, `inner` classes, enums,
//! secondary constructors, constructor defaults, a superclass declared in another file, an override
//! that changes a parameter's machine representation (which would need a bridge) — are reported by
//! name from [`check_supported`] so the file declines with a diagnostic instead of emitting
//! something unverified.

use std::collections::{HashMap, HashSet};

use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrClass, IrFile, IrFunction};
use crate::types::{Ty, TypeName};

/// The construct a lowering declined, phrased for a diagnostic.
pub(super) type Unsupported = String;

/// How a Kotlin type is carried in memory, named by the runtime's `kt_*` typedef for the scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CKind {
    /// Kotlin `Unit` in return position: no value.
    Void,
    /// A machine scalar, named by the runtime's `kt_*` typedef.
    Scalar(&'static str),
    /// A `KRef`: every reference, including a boxed `Int?` and an erased type parameter.
    Ref,
}

impl CKind {
    /// The carrier's size in bytes, which is also its alignment on every supported target.
    pub(super) fn size(self) -> u32 {
        match self {
            Self::Void => 0,
            Self::Scalar("kt_boolean" | "kt_byte") => 1,
            Self::Scalar("kt_short" | "kt_char") => 2,
            Self::Scalar("kt_int" | "kt_float") => 4,
            Self::Scalar(_) | Self::Ref => 8,
        }
    }
}

/// The carrier for a Kotlin type. A nullable primitive is deliberately NOT a scalar: `Int?` has to
/// represent `null`, so it boxes, exactly as it does on the JVM. A type parameter erases to a
/// reference, as it does there too.
pub(super) fn c_kind(ty: Ty) -> CKind {
    match ty {
        Ty::Unit => CKind::Void,
        Ty::Boolean => CKind::Scalar("kt_boolean"),
        Ty::Byte => CKind::Scalar("kt_byte"),
        Ty::Short => CKind::Scalar("kt_short"),
        Ty::Int => CKind::Scalar("kt_int"),
        Ty::Long => CKind::Scalar("kt_long"),
        Ty::Char => CKind::Scalar("kt_char"),
        Ty::Float => CKind::Scalar("kt_float"),
        Ty::Double => CKind::Scalar("kt_double"),
        _ => CKind::Ref,
    }
}

/// The size of an object header: one pointer to the type.
pub(super) const HEADER_SIZE: u32 = 8;

/// Where one of a class's OWN fields lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FieldLayout {
    pub offset: u32,
    pub kind: CKind,
}

/// What a vtable slot holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Slot {
    /// One of `kotlin.Any`'s defaults, by runtime symbol.
    Runtime(&'static str),
    /// An emitted method.
    Function(FunId),
    /// An abstract member: the runtime's loud failure, never a jump through NULL.
    Abstract,
    /// A synthesized getter for a property with a backing field and no source getter.
    FieldGetter { class: ClassId, field: u32 },
    /// A synthesized setter, likewise.
    FieldSetter { class: ClassId, field: u32 },
}

/// The identity of a virtual member, independent of which class's implementation fills it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum SlotKey {
    /// `kotlin.Any`'s slots, by number.
    Any(u32),
    /// A method, by its implementation in some class; an override registers under its own id too,
    /// so a further override finds the slot through either.
    Function(FunId),
    /// A property's getter, by the declaring class and the property name.
    Getter(ClassId, String),
    Setter(ClassId, String),
}

#[derive(Clone, Debug)]
pub(super) struct ClassLayout {
    /// The superclass in this file, or `None` for a class extending `kotlin.Any`.
    pub superclass: Option<ClassId>,
    /// Parallel to `IrClass::fields`.
    pub fields: Vec<FieldLayout>,
    /// The first byte after the last field, before any rounding — where a subclass's own fields
    /// begin, exactly as C packs the members of a struct that spells the superclass's fields out
    /// as a prefix.
    pub fields_end: u32,
    /// Bytes including the header, rounded up to 8.
    pub instance_size: u32,
    /// Every reference field, inherited ones first — what the collector traces.
    pub reference_offsets: Vec<u32>,
    pub vtable: Vec<Slot>,
    pub slots: HashMap<SlotKey, u32>,
}

/// The layouts of every class in a file, indexed by `ClassId`, plus the order in which a
/// superclass precedes its subclasses.
#[derive(Debug)]
pub(super) struct ClassModel {
    pub layouts: Vec<ClassLayout>,
    pub order: Vec<ClassId>,
}

impl ClassModel {
    pub(super) fn layout(&self, class: ClassId) -> &ClassLayout {
        &self.layouts[class as usize]
    }

    /// The slot a member dispatches through, looked up on the class the call names.
    pub(super) fn slot(&self, class: ClassId, key: &SlotKey) -> Option<u32> {
        self.layout(class).slots.get(key).copied()
    }
}

/// `kotlin.Any`'s vtable, the prefix of every class's.
fn any_vtable() -> (Vec<Slot>, HashMap<SlotKey, u32>) {
    let vtable = vec![
        Slot::Runtime("kt_any_equals"),
        Slot::Runtime("kt_any_hash_code"),
        Slot::Runtime("kt_any_to_string"),
    ];
    let slots = (0..3).map(|slot| (SlotKey::Any(slot), slot)).collect();
    (vtable, slots)
}

/// The `kotlin.Any` slot a method overrides, decided by name and arity — the frontend does not
/// record an override of a dependency member in `function_overrides`, and Kotlin admits no other
/// member with these names and arities in a class.
fn any_slot(function: &IrFunction) -> Option<u32> {
    match (function.name.as_str(), function.params.len()) {
        ("equals", 1) => Some(0),
        ("hashCode", 0) => Some(1),
        ("toString", 0) => Some(2),
        _ => None,
    }
}

/// The expected carriers of `kotlin.Any`'s slots, so a same-named member with another signature
/// is not mistaken for an override.
fn any_slot_signature(slot: u32) -> (&'static [CKind], CKind) {
    match slot {
        0 => (&[CKind::Ref], CKind::Scalar("kt_boolean")),
        1 => (&[], CKind::Scalar("kt_int")),
        _ => (&[], CKind::Ref),
    }
}

/// Reject, by name, a class this step does not lower.
pub(super) fn check_supported(ir: &IrFile, class: &IrClass) -> Result<(), Unsupported> {
    let name = class.fq_name();
    let construct = if class.is_interface {
        "an interface"
    } else if !class.interfaces.is_empty()
        || class.supertypes.iter().any(|supertype| {
            supertype
                .obj_internal()
                .and_then(|internal| ir.class_id_by_name(internal))
                .is_some_and(|id| ir.classes[id as usize].is_interface)
        })
    {
        "an interface supertype"
    } else if class.is_data {
        "a data class"
    } else if class.is_inner_class || class.constructor_prefix_count > 0 {
        "an inner class"
    } else if !class.enum_entries.is_empty() || class.enum_entry_of.is_some() {
        "an enum class"
    } else if class.is_value {
        // A `value class` is not a one-field class: Kotlin gives it equality, hashing and rendering
        // by its underlying value, and the JVM erases it to that value entirely. Common IR carries
        // it as an ordinary class, and treating it as one compiles — and answers `IC(1) == IC(1)`
        // with identity, which is false. Declined until the generator realizes those members.
        "a value class"
    } else if !class.secondary_ctors.is_empty() {
        "a secondary constructor"
    } else if ir
        .class_ctor_defaults_name(class.fq_name_id())
        .is_some_and(|defaults| defaults.iter().any(Option::is_some))
        || class.ctor_args.iter().any(|argument| argument.has_default)
    {
        "a constructor default argument"
    } else if class.is_annotation || class.annotation_impl_of.is_some() {
        "an annotation class"
    } else if class.is_anonymous_object
        || class.enclosing_function.is_some()
        || class.is_local_class
    {
        "a local or anonymous class"
    } else if class.prop_ref.is_some() || class.func_ref.is_some() {
        "a callable reference"
    } else if !class.pre_super_param_fields.is_empty() {
        "a field stored before the superclass constructor"
    } else if class.ctor_args.iter().any(|argument| argument.is_vararg) {
        "a vararg constructor parameter"
    } else {
        return Ok(());
    };
    Err(format!("{construct} (`{name}`)"))
}

/// Build the layout and vtable of every class in `ir`, superclasses first.
pub(super) fn build(ir: &IrFile) -> Result<ClassModel, Unsupported> {
    for class in &ir.classes {
        check_supported(ir, class)?;
    }
    let order = hierarchy_order(ir)?;

    let mut layouts: Vec<Option<ClassLayout>> = vec![None; ir.classes.len()];
    for &id in &order {
        let class = &ir.classes[id as usize];
        let superclass = ir.class_id_by_name(class.superclass);
        let layout = {
            let parent = superclass.map(|parent| {
                layouts[parent as usize]
                    .as_ref()
                    .expect("superclasses are laid out first")
            });
            layout_class(ir, id, superclass, parent)?
        };
        layouts[id as usize] = Some(layout);
    }
    Ok(ClassModel {
        layouts: layouts
            .into_iter()
            .map(|layout| layout.expect("every class was laid out"))
            .collect(),
        order,
    })
}

/// Classes sorted so that a superclass precedes its subclasses. The order is read off
/// `classifier_hierarchies` — the frontend's own record of each class's supertypes — and a
/// superclass that is not in this file (other than `kotlin.Any`) is declined.
fn hierarchy_order(ir: &IrFile) -> Result<Vec<ClassId>, Unsupported> {
    let mut depths = Vec::with_capacity(ir.classes.len());
    for (id, class) in ir.classes.iter().enumerate() {
        if !class.superclass.matches("kotlin/Any")
            && ir.class_id_by_name(class.superclass).is_none()
        {
            return Err(format!(
                "a superclass declared outside this file (`{}` extends `{}`)",
                class.fq_name(),
                class.superclass.render()
            ));
        }
        let depth = ir
            .classifier_hierarchies
            .get(&class.fq_name_id())
            .map_or(0, |applied| {
                applied.iter().map(|entry| entry.depth).max().unwrap_or(0)
            });
        depths.push((depth, id as ClassId));
    }
    // A stable sort by depth: the IR order is kept within one level, so emission stays
    // deterministic for a given source.
    depths.sort_by_key(|(depth, _)| *depth);
    Ok(depths.into_iter().map(|(_, id)| id).collect())
}

fn round_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) / alignment * alignment
}

fn layout_class(
    ir: &IrFile,
    id: ClassId,
    superclass: Option<ClassId>,
    parent: Option<&ClassLayout>,
) -> Result<ClassLayout, Unsupported> {
    let class = &ir.classes[id as usize];

    // ---- fields ----
    let mut end = parent.map_or(HEADER_SIZE, |parent| parent.fields_end);
    let mut reference_offsets =
        parent.map_or_else(Vec::new, |parent| parent.reference_offsets.clone());
    let mut fields = Vec::with_capacity(class.fields.len());
    for field in &class.fields {
        let kind = c_kind(field.ty);
        if kind == CKind::Void {
            return Err(format!(
                "a `Unit`-typed field (`{}.{}`)",
                class.fq_name(),
                field.name
            ));
        }
        let offset = round_up(end, kind.size());
        if kind == CKind::Ref {
            reference_offsets.push(offset);
        }
        fields.push(FieldLayout { offset, kind });
        end = offset + kind.size();
    }
    let instance_size = round_up(end, 8);

    // ---- vtable ----
    let (mut vtable, mut slots) = parent.map_or_else(any_vtable, |parent| {
        (parent.vtable.clone(), parent.slots.clone())
    });

    // Which methods are property accessors, so their slots are keyed by the property.
    let mut accessor_keys: HashMap<FunId, SlotKey> = HashMap::new();
    for property in &class.properties {
        if let Some(getter) = property.getter {
            accessor_keys.insert(getter, SlotKey::Getter(id, property.name.clone()));
        }
        if let Some(setter) = property.setter {
            accessor_keys.insert(setter, SlotKey::Setter(id, property.name.clone()));
        }
    }

    let overridden_functions = overridden_functions(ir, class)?;
    let overridden_properties = overridden_properties(ir, id, class)?;

    for &fid in &class.methods {
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() || function.is_static {
            continue;
        }
        let entry = if function.body.is_some() {
            Slot::Function(fid)
        } else {
            Slot::Abstract
        };
        let own_key = accessor_keys
            .get(&fid)
            .cloned()
            .unwrap_or(SlotKey::Function(fid));

        let replaces = match any_slot(function) {
            Some(slot) => {
                let (params, ret) = any_slot_signature(slot);
                let actual: Vec<CKind> = function.params.iter().copied().map(c_kind).collect();
                if actual == params && c_kind(function.ret) == ret {
                    Some(slot)
                } else {
                    None
                }
            }
            None => None,
        };
        let replaces = match replaces {
            Some(slot) => Some(slot),
            None => match overridden_functions.get(&fid) {
                Some(&overridden) => {
                    check_same_representation(ir, function, &ir.functions[overridden as usize])?;
                    let owner = ir.functions[overridden as usize]
                        .dispatch_receiver
                        .and_then(|owner| ir.class_id_by_name(owner))
                        .ok_or_else(|| {
                            format!(
                                "an override of a method declared outside this file (`{}`)",
                                function.name
                            )
                        })?;
                    let key = function_key(ir, owner, overridden);
                    Some(slots.get(&key).copied().ok_or_else(|| {
                        format!("an override with no slot to replace (`{}`)", function.name)
                    })?)
                }
                None => match &own_key {
                    SlotKey::Getter(_, name) | SlotKey::Setter(_, name) => {
                        let setter = matches!(own_key, SlotKey::Setter(..));
                        overridden_property_slot(
                            &overridden_properties,
                            &slots,
                            name,
                            setter,
                            &class.fq_name(),
                        )?
                    }
                    _ => None,
                },
            },
        };
        let slot = match replaces {
            Some(slot) => {
                vtable[slot as usize] = entry;
                slot
            }
            None => {
                vtable.push(entry);
                (vtable.len() - 1) as u32
            }
        };
        slots.insert(own_key, slot);
    }

    // Open or overriding properties with no source accessor still dispatch: synthesize the
    // field access as a slot.
    for property in &class.properties {
        let overrides = overridden_properties.contains_key(&property.name);
        if !property.is_open && !overrides {
            continue;
        }
        let accessors = [
            (false, property.getter.is_none()),
            (true, property.is_var && property.setter.is_none()),
        ];
        for (setter, needed) in accessors {
            if !needed {
                continue;
            }
            let entry = match property.backing_field {
                Some(field) if setter => Slot::FieldSetter { class: id, field },
                Some(field) => Slot::FieldGetter { class: id, field },
                None => Slot::Abstract,
            };
            let key = if setter {
                SlotKey::Setter(id, property.name.clone())
            } else {
                SlotKey::Getter(id, property.name.clone())
            };
            let replaces = overridden_property_slot(
                &overridden_properties,
                &slots,
                &property.name,
                setter,
                &class.fq_name(),
            )?;
            let slot = match replaces {
                Some(slot) => {
                    vtable[slot as usize] = entry;
                    slot
                }
                None => {
                    vtable.push(entry);
                    (vtable.len() - 1) as u32
                }
            };
            slots.insert(key, slot);
        }
    }

    Ok(ClassLayout {
        superclass,
        fields,
        fields_end: end,
        instance_size,
        reference_offsets,
        vtable,
        slots,
    })
}

/// The slot key of a method of `owner`: a property accessor is keyed by its property.
pub(super) fn function_key(ir: &IrFile, owner: ClassId, fid: FunId) -> SlotKey {
    let class = &ir.classes[owner as usize];
    for property in &class.properties {
        if property.getter == Some(fid) {
            return SlotKey::Getter(owner, property.name.clone());
        }
        if property.setter == Some(fid) {
            return SlotKey::Setter(owner, property.name.clone());
        }
    }
    SlotKey::Function(fid)
}

/// The slot an overriding property accessor replaces, found through the overridden property's
/// declaring class.
fn overridden_property_slot(
    overridden: &HashMap<String, (ClassId, String)>,
    slots: &HashMap<SlotKey, u32>,
    name: &str,
    setter: bool,
    class_name: &str,
) -> Result<Option<u32>, Unsupported> {
    let Some((owner, overridden_name)) = overridden.get(name) else {
        return Ok(None);
    };
    let key = if setter {
        SlotKey::Setter(*owner, overridden_name.clone())
    } else {
        SlotKey::Getter(*owner, overridden_name.clone())
    };
    // A `val` overridden by a `var` adds a setter the base never had: a new slot, not an override.
    if setter && !slots.contains_key(&key) {
        return Ok(None);
    }
    slots.get(&key).copied().map(Some).ok_or_else(|| {
        format!("a property override with no slot to replace (`{class_name}.{name}`)")
    })
}

/// Implementation method → the method it overrides, for the methods `class` declares. Both ends
/// must be functions of this file.
fn overridden_functions(
    ir: &IrFile,
    class: &IrClass,
) -> Result<HashMap<FunId, FunId>, Unsupported> {
    let mut map = HashMap::new();
    let Some(overrides) = ir.function_overrides.get(&class.fq_name_id()) else {
        return Ok(map);
    };
    for edge in overrides {
        let implementation = match (&edge.implementation, edge.implementation_function) {
            (_, Some(fid)) => Some(fid),
            (ResolvedFunctionOverrideTarget::Module(callable), None) => {
                ir.checked_callable_functions.get(callable).copied()
            }
            (ResolvedFunctionOverrideTarget::External(_), None) => None,
        };
        let Some(implementation) = implementation else {
            continue;
        };
        let function = &ir.functions[implementation as usize];
        match &edge.overridden {
            ResolvedFunctionOverrideTarget::Module(callable) => {
                match ir.checked_callable_functions.get(callable) {
                    Some(&overridden) => {
                        // The nearest override wins when a chain records several edges.
                        map.entry(implementation).or_insert(overridden);
                    }
                    None => {
                        return Err(format!(
                            "an override of a method declared in another file (`{}.{}`)",
                            class.fq_name(),
                            function.name
                        ))
                    }
                }
            }
            ResolvedFunctionOverrideTarget::External(_) => {
                if any_slot(function).is_none() {
                    return Err(format!(
                        "an override of a dependency method (`{}.{}`)",
                        class.fq_name(),
                        function.name
                    ));
                }
            }
        }
    }
    Ok(map)
}

/// Property name → the (class, name) of the property it overrides, for `class`'s properties.
fn overridden_properties(
    ir: &IrFile,
    id: ClassId,
    class: &IrClass,
) -> Result<HashMap<String, (ClassId, String)>, Unsupported> {
    let mut map = HashMap::new();
    let Some(overrides) = ir.property_overrides.get(&class.fq_name_id()) else {
        return Ok(map);
    };
    for edge in overrides {
        let ResolvedPropertyOverrideTarget::Module(implementation) = &edge.implementation else {
            continue;
        };
        let Some(property) = ir.checked_properties.get(implementation) else {
            continue;
        };
        if property.class != Some(id) {
            continue;
        }
        let overridden = match &edge.overridden {
            ResolvedPropertyOverrideTarget::Module(overridden) => ir
                .checked_properties
                .get(overridden)
                .and_then(|base| Some((base.class?, base.name.clone()))),
            ResolvedPropertyOverrideTarget::External(_) => None,
        };
        let Some(overridden) = overridden else {
            return Err(format!(
                "an override of a property declared outside this file (`{}.{}`)",
                class.fq_name(),
                edge.name
            ));
        };
        // Depth 1 is the direct base; deeper edges describe the same slot.
        if edge.depth == 1 || !map.contains_key(&property.name) {
            map.insert(property.name.clone(), overridden);
        }
    }
    Ok(map)
}

/// An override must be callable through the overridden slot's signature. Kotlin permits a
/// covariant return (`A` → `B`, both references) and generic specialization (`T` → `Int`); the
/// second changes the machine representation and would need a bridge method, which does not
/// exist here yet.
fn check_same_representation(
    ir: &IrFile,
    implementation: &IrFunction,
    overridden: &IrFunction,
) -> Result<(), Unsupported> {
    let same = implementation.params.len() == overridden.params.len()
        && implementation
            .params
            .iter()
            .zip(&overridden.params)
            .all(|(a, b)| c_kind(*a) == c_kind(*b))
        && c_kind(implementation.ret) == c_kind(overridden.ret);
    if same {
        return Ok(());
    }
    let owner = implementation
        .dispatch_receiver
        .map_or_else(String::new, TypeName::render);
    let _ = ir;
    Err(format!(
        "an override that changes a parameter's or the result's representation (`{owner}.{}`; \
         a bridge method is needed)",
        implementation.name
    ))
}

/// A symbol-safe spelling of a Kotlin name: every non-alphanumeric character becomes `_`.
pub(super) fn c_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// Kotlin overloads share a name and C has no overloading, so a repeat gets a suffix. The suffixed
/// spelling is itself checked for collisions: a file declaring both `greet` and `greet__1` would
/// otherwise hand two functions the same symbol, and the linker would silently pick one of them.
/// Classes draw from the same pool as functions because their descriptors and constructors are
/// ordinary C objects too: a class `X` and a top-level function `type_X` must not both become
/// `kt_type_X`.
pub(super) struct Symbols {
    /// The sanitized, unique base name of each class, from which `struct kt_class_<base>`,
    /// `kt_type_<base>`, `kt_<base>__init` and the struct members `<base>_f<i>` derive.
    pub classes: Vec<String>,
    /// C symbol per IR function index.
    pub functions: Vec<String>,
}

fn class_symbol_family(base: &str) -> [String; 6] {
    [
        format!("kt_type_{base}"),
        format!("kt_vtable_{base}"),
        format!("kt_refs_{base}"),
        format!("kt_{base}__init"),
        format!("kt_singleton_{base}"),
        format!("kt_singleton_{base}_get"),
    ]
}

pub(super) fn symbols(ir: &IrFile) -> Symbols {
    let mut taken: HashSet<String> = HashSet::new();
    let mut unique = |base: String, family: &dyn Fn(&str) -> Vec<String>| -> String {
        let mut candidate = base.clone();
        let mut ordinal = 0;
        loop {
            let names = family(&candidate);
            if names.iter().all(|name| !taken.contains(name)) {
                taken.extend(names);
                return candidate;
            }
            ordinal += 1;
            candidate = format!("{base}__{ordinal}");
        }
    };

    // Classes first: a class reserves a whole family of names, and a function only one.
    let classes: Vec<String> = ir
        .classes
        .iter()
        .map(|class| {
            unique(c_identifier(&class.fq_name()), &|base| {
                class_symbol_family(base).to_vec()
            })
        })
        .collect();

    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    let mut functions = vec![String::new(); ir.functions.len()];
    // Methods are named by their class, then everything else by the package.
    for (class, base) in ir.classes.iter().zip(&classes) {
        for &fid in &class.methods {
            let function = &ir.functions[fid as usize];
            let method = format!("kt_{base}_{}", c_identifier(&function.name));
            functions[fid as usize] = unique(method, &|name| vec![name.to_string()]);
        }
    }
    for (index, function) in ir.functions.iter().enumerate() {
        if !functions[index].is_empty() {
            continue;
        }
        let base = match &package {
            Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
            None => format!("kt_{}", c_identifier(&function.name)),
        };
        functions[index] = unique(base, &|name| vec![name.to_string()]);
    }
    Symbols { classes, functions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrField, IrProperty, IrfFlags};

    fn function(name: &str, owner: &str, params: Vec<Ty>, ret: Ty, abstract_: bool) -> IrFunction {
        IrFunction {
            name: name.to_string(),
            params,
            ret,
            body: if abstract_ { None } else { Some(0) },
            is_static: false,
            dispatch_receiver: Some(crate::types::type_name(owner)),
            param_checks: Vec::new(),
        }
    }

    fn field(name: &str, ty: Ty) -> IrField {
        IrField {
            name: name.to_string(),
            ty,
            type_param: None,
            default: None,
            flags: IrfFlags::default(),
        }
    }

    fn class(ir: &mut IrFile, name: &str, superclass: &str, depth: u32) -> ClassId {
        let mut class = IrClass::synthetic(crate::types::type_name(name));
        class.superclass = crate::types::type_name(superclass);
        class.is_open = true;
        let id = ir.classes.len() as ClassId;
        ir.classes.push(class);
        let mut applied = vec![crate::ir::IrAppliedClassifier {
            classifier: crate::types::type_name(name),
            applied: Ty::obj(name),
            depth: 0,
        }];
        if depth > 0 {
            applied.push(crate::ir::IrAppliedClassifier {
                classifier: crate::types::type_name(superclass),
                applied: Ty::obj(superclass),
                depth,
            });
        }
        ir.classifier_hierarchies
            .insert(crate::types::type_name(name), applied);
        id
    }

    fn add_method(ir: &mut IrFile, class: ClassId, function: IrFunction) -> FunId {
        let fid = ir.functions.len() as FunId;
        ir.functions.push(function);
        ir.classes[class as usize].methods.push(fid);
        fid
    }

    fn record_override(ir: &mut IrFile, class: ClassId, implementation: FunId, overridden: FunId) {
        // The frontend records overrides by callable identity; the test uses the function index
        // as that identity, which is what `checked_callable_functions` maps back.
        use crate::fir::CallableId;
        let implementation_id = CallableId::from_raw(1000 + implementation);
        let overridden_id = CallableId::from_raw(1000 + overridden);
        ir.checked_callable_functions
            .insert(implementation_id, implementation);
        ir.checked_callable_functions
            .insert(overridden_id, overridden);
        let name = ir.functions[implementation as usize].name.clone();
        let owner = ir.classes[class as usize].fq_name_id();
        ir.function_overrides
            .entry(owner)
            .or_default()
            .push(crate::ir::IrFunctionOverride {
                implementation: ResolvedFunctionOverrideTarget::Module(implementation_id),
                implementation_function: None,
                implementation_owner: owner,
                overridden: ResolvedFunctionOverrideTarget::Module(overridden_id),
                overridden_owner: ir.functions[overridden as usize]
                    .dispatch_receiver
                    .expect("instance method"),
                overridden_is_interface: false,
                name,
                declared_parameters: Vec::new(),
                declared_result: Ty::Unit,
                applied_parameters: Vec::new(),
                applied_result: Ty::Unit,
                implementation_parameters: Vec::new(),
                implementation_result: Ty::Unit,
                suspend: false,
                depth: 1,
            });
    }

    #[test]
    fn every_vtable_begins_with_kotlin_any_in_the_fixed_order() {
        let mut ir = IrFile::default();
        let point = class(&mut ir, "Point", "kotlin/Any", 0);
        let model = build(&ir).expect("layout");
        let layout = model.layout(point);
        assert_eq!(
            layout.vtable,
            vec![
                Slot::Runtime("kt_any_equals"),
                Slot::Runtime("kt_any_hash_code"),
                Slot::Runtime("kt_any_to_string"),
            ]
        );
        assert_eq!(layout.instance_size, HEADER_SIZE);
    }

    #[test]
    fn a_new_method_appends_and_an_override_replaces_the_slot() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let a_name = add_method(&mut ir, a, function("name", "A", vec![], Ty::String, false));
        let a_other = add_method(&mut ir, a, function("other", "A", vec![], Ty::Int, false));
        let b_name = add_method(&mut ir, b, function("name", "B", vec![], Ty::String, false));
        let b_extra = add_method(&mut ir, b, function("extra", "B", vec![], Ty::Int, false));
        record_override(&mut ir, b, b_name, a_name);

        let model = build(&ir).expect("layout");
        assert_eq!(model.slot(a, &SlotKey::Function(a_name)), Some(3));
        assert_eq!(model.slot(a, &SlotKey::Function(a_other)), Some(4));
        assert_eq!(model.layout(a).vtable[3], Slot::Function(a_name));

        let b_layout = model.layout(b);
        assert_eq!(
            b_layout.vtable[3],
            Slot::Function(b_name),
            "the override takes the slot its base assigned"
        );
        assert_eq!(
            b_layout.vtable[4],
            Slot::Function(a_other),
            "inherited unchanged"
        );
        assert_eq!(
            b_layout.vtable[5],
            Slot::Function(b_extra),
            "new members append"
        );
        assert_eq!(
            model.slot(b, &SlotKey::Function(a_name)),
            Some(3),
            "a call naming the base method finds the same slot on the subclass"
        );
        assert_eq!(model.slot(b, &SlotKey::Function(b_name)), Some(3));
    }

    #[test]
    fn a_three_level_chain_keeps_slot_numbers_stable() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let c = class(&mut ir, "C", "B", 2);
        let a_f = add_method(&mut ir, a, function("f", "A", vec![], Ty::Int, true));
        let b_g = add_method(&mut ir, b, function("g", "B", vec![], Ty::Int, false));
        let c_f = add_method(&mut ir, c, function("f", "C", vec![], Ty::Int, false));
        record_override(&mut ir, c, c_f, a_f);

        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(a).vtable[3], Slot::Abstract);
        assert_eq!(model.layout(b).vtable[3], Slot::Abstract);
        assert_eq!(model.layout(b).vtable[4], Slot::Function(b_g));
        assert_eq!(
            model.layout(c).vtable[3],
            Slot::Function(c_f),
            "an override two levels down replaces the slot the root declared"
        );
        assert_eq!(model.layout(c).vtable[4], Slot::Function(b_g));
        assert_eq!(model.layout(c).vtable.len(), 5);
        assert_eq!(model.order, vec![a, b, c]);
    }

    #[test]
    fn kotlin_any_members_are_matched_by_name_and_signature() {
        let mut ir = IrFile::default();
        let p = class(&mut ir, "P", "kotlin/Any", 0);
        let to_string = add_method(
            &mut ir,
            p,
            function("toString", "P", vec![], Ty::String, false),
        );
        // Same name, different arity: an overload, not an override.
        let overload = add_method(
            &mut ir,
            p,
            function("toString", "P", vec![Ty::Int], Ty::String, false),
        );
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(p).vtable[2], Slot::Function(to_string));
        assert_eq!(model.layout(p).vtable[3], Slot::Function(overload));
    }

    #[test]
    fn fields_follow_the_superclass_prefix_and_align_to_their_size() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("flag", Ty::Boolean), field("count", Ty::Int)];
        let b = class(&mut ir, "B", "A", 1);
        ir.classes[b as usize].fields = vec![
            field("next", Ty::nullable(Ty::obj("B"))),
            field("small", Ty::Short),
            field("wide", Ty::Long),
            field("text", Ty::String),
        ];

        let model = build(&ir).expect("layout");
        let a_layout = model.layout(a);
        assert_eq!(a_layout.fields[0].offset, 8);
        assert_eq!(
            a_layout.fields[1].offset, 12,
            "an Int after a Boolean aligns to 4"
        );
        assert_eq!(a_layout.instance_size, 16, "rounded up to 8");
        assert!(a_layout.reference_offsets.is_empty());

        let b_layout = model.layout(b);
        assert_eq!(
            b_layout.fields[0].offset, 16,
            "own fields start after the superclass"
        );
        assert_eq!(b_layout.fields[1].offset, 24);
        assert_eq!(
            b_layout.fields[2].offset, 32,
            "a Long after a Short aligns to 8"
        );
        assert_eq!(b_layout.fields[3].offset, 40);
        assert_eq!(b_layout.instance_size, 48);
        assert_eq!(
            b_layout.reference_offsets,
            vec![16, 40],
            "exactly the reference fields, in layout order — what the collector traces"
        );
    }

    #[test]
    fn an_inherited_reference_field_stays_in_the_subclass_trace_list() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("label", Ty::String)];
        let b = class(&mut ir, "B", "A", 1);
        ir.classes[b as usize].fields = vec![field("n", Ty::Int)];
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(b).reference_offsets, vec![8]);
        assert_eq!(model.layout(b).fields[0].offset, 16);
        assert_eq!(model.layout(b).instance_size, 24);
    }

    #[test]
    fn a_subclass_field_packs_after_the_superclass_field_not_after_its_rounded_size() {
        // `A { a: Int }` ends at 12 and is 16 bytes; `B : A { b: Int }` puts `b` at 12, where C
        // puts the next member of the flattened struct, not at 16. The generated `_Static_assert`
        // is what caught the difference.
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("a", Ty::Int)];
        let b = class(&mut ir, "B", "A", 1);
        ir.classes[b as usize].fields = vec![field("b", Ty::Int)];
        let model = build(&ir).expect("layout");
        assert_eq!(model.layout(a).instance_size, 16);
        assert_eq!(model.layout(b).fields[0].offset, 12);
        assert_eq!(model.layout(b).instance_size, 16);
    }

    #[test]
    fn a_superclass_from_another_file_is_declined() {
        let mut ir = IrFile::default();
        class(&mut ir, "B", "other/A", 1);
        let error = build(&ir).expect_err("declined");
        assert!(
            error.contains("superclass declared outside this file"),
            "{error}"
        );
    }

    #[test]
    fn an_override_that_changes_representation_is_declined() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        let b = class(&mut ir, "B", "A", 1);
        let erased = Ty::nullable(Ty::obj("kotlin/Any"));
        let a_f = add_method(
            &mut ir,
            a,
            function("f", "A", vec![erased], Ty::Unit, false),
        );
        let b_f = add_method(
            &mut ir,
            b,
            function("f", "B", vec![Ty::Int], Ty::Unit, false),
        );
        record_override(&mut ir, b, b_f, a_f);
        let error = build(&ir).expect_err("a KRef slot cannot hold a kt_int implementation");
        assert!(error.contains("representation"), "{error}");
    }

    #[test]
    fn an_open_property_without_a_source_getter_gets_a_synthesized_slot() {
        let mut ir = IrFile::default();
        let a = class(&mut ir, "A", "kotlin/Any", 0);
        ir.classes[a as usize].fields = vec![field("v", Ty::Int)];
        ir.classes[a as usize].properties = vec![IrProperty {
            name: "v".to_string(),
            context_params: Vec::new(),
            source_order: 0,
            decl_line: 1,
            ty: Ty::Int,
            visibility: crate::types::Visibility::Public,
            annotations: Box::new([]),
            initializer: None,
            storage_ty: None,
            backing_field: Some(0),
            is_var: false,
            is_open: true,
            is_private: false,
            setter_is_private: false,
            getter: None,
            setter: None,
            getter_jvm_name: None,
            setter_jvm_name: None,
            needs_access_bridge: false,
        }];
        let model = build(&ir).expect("layout");
        assert_eq!(
            model.layout(a).vtable[3],
            Slot::FieldGetter { class: a, field: 0 }
        );
        assert_eq!(model.slot(a, &SlotKey::Getter(a, "v".to_string())), Some(3));
    }

    #[test]
    fn out_of_scope_classes_are_declined_by_name() {
        let mut ir = IrFile::default();
        let id = class(&mut ir, "D", "kotlin/Any", 0);
        ir.classes[id as usize].is_data = true;
        assert!(build(&ir).expect_err("declined").contains("a data class"));
        ir.classes[id as usize].is_data = false;
        ir.classes[id as usize].is_interface = true;
        assert!(build(&ir).expect_err("declined").contains("an interface"));
        ir.classes[id as usize].is_interface = false;
        ir.classes[id as usize].is_inner_class = true;
        assert!(build(&ir).expect_err("declined").contains("an inner class"));
    }
}
