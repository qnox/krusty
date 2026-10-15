//! A C enum as Kotlin/Native binds it.
//!
//! A *strict* enum — one whose enumerators all count on implicitly, or one the `.def` file names in
//! `strictEnums` — is a Kotlin `enum class` implementing `CEnum`, with a `Var` class for a variable
//! holding it. Any other named enum is two type aliases, `E` for its integer type and `EVar` for
//! that integer's variable, and its enumerators are constants of type `E`. An anonymous enum's
//! enumerators are constants of its integer type.

use super::super::model::EnumId;
use super::kotlin_type::{cinterop_class, named, Argument, KotlinType, Primitive};
use super::mirror::{pointed_of, Mapper, Mirror, Unsupported};
use super::structs::{private_constructor, raw_pointer_constructor};
use super::stub::{
    flags, Annotation, Class, Constructor, EnumEntry, Function, IntegerKind, Parameter, Property,
    TypeAlias, Value,
};

/// A strict enum's classes: the enum, its `Var` and that class's companion, and its companion.
pub(super) fn enum_classes(mapper: &Mapper<'_>, id: EnumId) -> Result<Vec<Class>, Unsupported> {
    let base = mapper.enum_base(id)?;
    let base_type = base.value_type();
    let name = mapper.enum_class(id);
    let enum_type = KotlinType::class(name, Vec::new());
    let enumerators = mapper
        .declarations
        .enumeration(id)
        .enumerators
        .as_ref()
        .ok_or(Unsupported)?;

    let mut class = Class::new(name, Some(flags::ENUM_CLASS));
    class.companion = Some("Companion");
    class.supertypes = vec![
        KotlinType::class(
            named("kotlin/Enum"),
            vec![Argument::Invariant(enum_type.clone())],
        ),
        cinterop_class("CEnum"),
    ];
    class.nested = vec!["Var", "Companion"];
    class.constructors = vec![Constructor {
        flags: Some(flags::PRIVATE_CONSTRUCTOR),
        parameters: vec![Parameter::new("value", base_type.clone())],
    }];
    class.properties = vec![Property {
        name: "value".to_string(),
        flags: 534,
        ty: base_type.clone(),
        getter_flags: 86,
        setter_flags: None,
        annotations: Vec::new(),
        getter_annotations: Vec::new(),
        setter_annotations: Vec::new(),
        constant: None,
    }];
    class.entries = enumerators
        .iter()
        .map(|enumerator| EnumEntry {
            name: enumerator.name.to_string(),
            annotations: vec![Annotation::new(
                &format!("kotlinx/cinterop/internal/ConstantValue.{}", base.name()),
                vec![("value", constant(base, enumerator.value))],
            )],
        })
        .collect();
    class.annotations = vec![Annotation::experimental()];

    let var_name = name.nested_child("Var");
    let mut var = Class::new(var_name, None);
    var.companion = Some("Companion");
    var.supertypes = vec![cinterop_class("CEnumVar")];
    var.nested = vec!["Companion"];
    var.constructors = vec![raw_pointer_constructor()];
    var.properties = vec![Property {
        name: "value".to_string(),
        flags: flags::MEMBER_VAR,
        ty: enum_type.clone(),
        getter_flags: flags::EXTERNAL_ACCESSOR,
        setter_flags: Some(flags::EXTERNAL_ACCESSOR),
        annotations: Vec::new(),
        getter_annotations: Vec::new(),
        setter_annotations: Vec::new(),
        constant: None,
    }];

    let mut var_companion = Class::new(var_name.nested_child("Companion"), Some(flags::COMPANION));
    var_companion.supertypes = vec![cinterop_class("CPrimitiveVar.Type")];
    var_companion.constructors = vec![private_constructor()];
    var_companion.annotations = vec![
        Annotation::new(
            "kotlinx/cinterop/internal/CEnumVarTypeSize",
            vec![("size", Value::int(base_size(base)))],
        ),
        Annotation::deprecated("Use sizeOf<T>() or alignOf<T>() instead."),
    ];

    let mut companion = Class::new(name.nested_child("Companion"), Some(flags::PLAIN_COMPANION));
    companion.constructors = vec![private_constructor()];
    companion.functions = vec![Function {
        name: "byValue".to_string(),
        flags: flags::FUNCTION,
        result: enum_type,
        parameters: vec![Parameter::new("value", base_type)],
        annotations: vec![Annotation::deprecated("Will be removed.")],
    }];
    Ok(vec![class, var, var_companion, companion])
}

/// A named non-strict enum's aliases: `EVar` and `E`.
pub(super) fn enum_aliases(mapper: &Mapper<'_>, id: EnumId) -> Option<Vec<TypeAlias>> {
    let name = mapper.non_strict_enum_name(id)?;
    let Ok(Mirror::ByValue { value, info, .. }) = mapper.enum_mirror(id) else {
        return None;
    };
    let base = mapper.enum_base(id).ok()?;
    Some(vec![
        TypeAlias {
            name: format!("{name}Var"),
            underlying: pointed_of(&info, value),
        },
        TypeAlias {
            name: name.to_string(),
            underlying: base.value_type(),
        },
    ])
}

/// A non-strict or anonymous enum's enumerators, as constants.
pub(super) fn enum_constants(mapper: &Mapper<'_>, id: EnumId) -> Vec<Property> {
    if mapper.is_strict(id) {
        return Vec::new();
    }
    let Ok(base) = mapper.enum_base(id) else {
        return Vec::new();
    };
    let Ok(mirror) = mapper.enum_mirror(id) else {
        return Vec::new();
    };
    let ty = mirror.argument();
    let Some(enumerators) = &mapper.declarations.enumeration(id).enumerators else {
        return Vec::new();
    };
    enumerators
        .iter()
        .map(|enumerator| {
            constant_property(
                &enumerator.name,
                ty.clone(),
                constant(base, enumerator.value),
            )
        })
        .collect()
}

/// A top-level `const val`.
pub(super) fn constant_property(name: &str, ty: KotlinType, value: Value) -> Property {
    Property {
        name: name.to_string(),
        flags: flags::CONST_VAL,
        ty,
        getter_flags: flags::DEFAULT_ACCESSOR,
        setter_flags: None,
        annotations: vec![Annotation::experimental()],
        getter_annotations: Vec::new(),
        setter_annotations: Vec::new(),
        constant: Some(value),
    }
}

/// `value` as a constant of the integer primitive `base`.
pub(super) fn constant(base: Primitive, value: i128) -> Value {
    let (kind, bits) = match base {
        Primitive::Byte | Primitive::UByte => (IntegerKind::Byte, 8),
        Primitive::Short | Primitive::UShort => (IntegerKind::Short, 16),
        Primitive::Int | Primitive::UInt => (IntegerKind::Int, 32),
        _ => (IntegerKind::Long, 64),
    };
    // A signed value is sign-extended from its width; an unsigned one keeps its magnitude (an
    // unsigned 64-bit value above `Long.MAX_VALUE` wraps, as its `Long` carrier does).
    let unsigned = base.is_unsigned();
    let shift = 64 - bits;
    let value = if unsigned {
        ((value as u64) << shift >> shift) as i64
    } else {
        (value as i64) << shift >> shift
    };
    Value::Integer {
        kind,
        value,
        unsigned,
    }
}

fn base_size(base: Primitive) -> u64 {
    match base {
        Primitive::Byte | Primitive::UByte | Primitive::Boolean => 1,
        Primitive::Short | Primitive::UShort => 2,
        Primitive::Int | Primitive::UInt | Primitive::Float => 4,
        _ => 8,
    }
}
