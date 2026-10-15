//! How Kotlin/Native represents a C type in Kotlin: its *mirror*.
//!
//! A type is mirrored by value — a scalar, pointer or enum, read and written as a Kotlin value —
//! or by reference — a struct or function, reachable only through a pointer. Either way it has a
//! *pointed* type, the `kotlinx.cinterop` variable class a pointer to it points at (`IntVar` for
//! `int`, the struct's own class for a struct). A by-value mirror also has the Kotlin value type
//! (`Int`, `CPointer<IntVar>`), nullable when it is a pointer.
//!
//! A typedef keeps its name on both forms: `typedef unsigned uint` mirrors to the alias `uint` for
//! `UInt` and the alias `uintVar` for `UIntVar`.

use std::collections::HashSet;
use std::rc::Rc;

use super::super::layout::enum_underlying;
use super::super::model::{
    Declarations, EnumId, FloatKind, IntRank, RecordId, Signedness, Type, TypeId,
};
use super::kotlin_type::{
    c_array_pointer, c_array_pointer_var, c_function, c_opaque_pointer, c_opaque_pointer_var,
    c_pointer, c_pointer_var, c_pointer_var_of, c_value, forward_declared_struct, named, unit,
    KotlinType, Primitive,
};
use super::registry::Registry;
use crate::types::TypeName;

/// What kind of value a by-value mirror holds, which decides the variable class a typedef of it
/// declares.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Info {
    Primitive(Primitive),
    Pointer,
    /// A strict enum, by its class.
    Enum(TypeName),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Mirror {
    ByValue {
        pointed: KotlinType,
        value: KotlinType,
        nullable: bool,
        info: Info,
    },
    ByRef {
        pointed: KotlinType,
    },
}

impl Mirror {
    pub(super) fn pointed(&self) -> &KotlinType {
        match self {
            Self::ByValue { pointed, .. } | Self::ByRef { pointed } => pointed,
        }
    }

    /// The type a value of this C type is passed and returned as: a struct by value is a
    /// `CValue` of it.
    pub(super) fn argument(&self) -> KotlinType {
        match self {
            Self::ByValue {
                value, nullable, ..
            } => value.clone().nullable(*nullable),
            Self::ByRef { pointed } => c_value(pointed.clone()),
        }
    }
}

/// A C type Kotlin/Native has no Kotlin representation for: `long double`, a 128-bit integer, a
/// complex or atomic type, a variadic function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Unsupported;

/// Mirrors the types of one header set into one Kotlin package.
pub(super) struct Mapper<'a> {
    pub(super) declarations: &'a Declarations,
    pub(super) registry: &'a Registry,
    /// The package's path, segments joined by `/`.
    package: String,
    strict_enums: HashSet<EnumId>,
}

/// Which enums are mapped to Kotlin enum classes, from the `.def` file's `strictEnums` and
/// `nonStrictEnums`.
pub(super) struct EnumStyles<'s> {
    pub(super) strict: &'s [String],
    pub(super) non_strict: &'s [String],
}

impl<'a> Mapper<'a> {
    pub(super) fn new(
        declarations: &'a Declarations,
        registry: &'a Registry,
        package: &str,
        styles: &EnumStyles<'_>,
    ) -> Self {
        let strict_enums = registry
            .enums
            .iter()
            .copied()
            .filter(|&id| {
                let Some(name) = registry.enum_name(id) else {
                    return false;
                };
                let Some(enumerators) = &declarations.enumeration(id).enumerators else {
                    return false;
                };
                let listed = |list: &[String]| list.iter().any(|entry| **entry == **name);
                !listed(styles.non_strict)
                    && (listed(styles.strict) || enumerators.iter().all(|entry| !entry.explicit))
            })
            .collect();
        Self {
            declarations,
            registry,
            package: package.replace('.', "/"),
            strict_enums,
        }
    }

    /// A classifier the library declares, by its simple name.
    pub(super) fn own(&self, name: &str) -> TypeName {
        if self.package.is_empty() {
            named(name)
        } else {
            named(&format!("{}/{name}", self.package))
        }
    }

    /// The class a struct or union is: the library's own when the headers define it, else the
    /// forward declaration `cnames.structs.<tag>`.
    pub(super) fn record_class(&self, id: RecordId) -> TypeName {
        let name = self.registry.record_name(id);
        if self.declarations.record(id).fields.is_some() {
            self.own(name)
        } else {
            forward_declared_struct(name)
        }
    }

    pub(super) fn is_strict(&self, id: EnumId) -> bool {
        self.strict_enums.contains(&id)
    }

    /// The class a strict enum is.
    pub(super) fn enum_class(&self, id: EnumId) -> TypeName {
        self.own(
            self.registry
                .enum_name(id)
                .expect("a strict enum has a name"),
        )
    }

    /// The primitive an enum's values are, from its underlying integer type.
    pub(super) fn enum_base(&self, id: EnumId) -> Result<Primitive, Unsupported> {
        let (rank, signedness) =
            enum_underlying(self.declarations.enumeration(id)).ok_or(Unsupported)?;
        integer(rank, signedness)
    }

    pub(super) fn mirror(&self, ty: TypeId) -> Result<Mirror, Unsupported> {
        let declarations = self.declarations;
        Ok(match declarations.ty(ty) {
            Type::Void | Type::Complex(_) | Type::Atomic(_) => return Err(Unsupported),
            Type::Int { rank, signedness } => primitive(integer(*rank, *signedness)?),
            Type::Float(FloatKind::Float) => primitive(Primitive::Float),
            Type::Float(FloatKind::Double) => primitive(Primitive::Double),
            Type::Float(FloatKind::LongDouble) => return Err(Unsupported),
            Type::Vector { size: 16, .. } => primitive(Primitive::Vector128),
            Type::Vector { .. } => return Err(Unsupported),
            Type::Qualified { base, .. } => self.mirror(*base)?,
            Type::Pointer(pointee) => self.pointer_to(*pointee)?,
            Type::Array { element, .. } => {
                let element_mirror = self.mirror(*element)?;
                if let Type::Array { .. } = declarations.ty(declarations.canonical(*element)) {
                    element_mirror
                } else {
                    let pointed = element_mirror.pointed().clone();
                    Mirror::ByValue {
                        pointed: c_array_pointer_var(pointed.clone()),
                        value: c_array_pointer(pointed),
                        nullable: true,
                        info: Info::Pointer,
                    }
                }
            }
            Type::Function(function) => {
                if function.variadic {
                    return Err(Unsupported);
                }
                let parameters = function
                    .params
                    .iter()
                    .map(|&param| Ok(self.mirror(param)?.argument()))
                    .collect::<Result<Vec<_>, _>>()?;
                Mirror::ByRef {
                    pointed: c_function(parameters, self.result(function.result)?),
                }
            }
            Type::Record(id) => Mirror::ByRef {
                pointed: KotlinType::class(self.record_class(*id), Vec::new()),
            },
            Type::Enum(id) => self.enum_mirror(*id)?,
            Type::Typedef(id) => {
                let typedef = declarations.typedef(*id);
                let base = self.mirror(typedef.ty)?;
                if !self.registry.has_typedef(*id) {
                    return Ok(base);
                }
                let name = &typedef.name;
                match base {
                    Mirror::ByValue {
                        pointed,
                        value,
                        nullable,
                        info,
                    } => Mirror::ByValue {
                        pointed: KotlinType::alias(
                            self.own(&format!("{name}Var")),
                            Vec::new(),
                            pointed,
                        ),
                        value: KotlinType::alias(self.own(name), Vec::new(), value),
                        nullable,
                        info,
                    },
                    Mirror::ByRef { pointed } => Mirror::ByRef {
                        pointed: KotlinType::alias(self.own(name), Vec::new(), pointed),
                    },
                }
            }
        })
    }

    /// The mirror of a pointer to `pointee`.
    pub(super) fn pointer_to(&self, pointee: TypeId) -> Result<Mirror, Unsupported> {
        let declarations = self.declarations;
        Ok(match declarations.ty(declarations.canonical(pointee)) {
            Type::Void => opaque_pointer(),
            Type::Function(function) if function.variadic => opaque_pointer(),
            Type::Array { .. } => self.mirror(pointee)?,
            _ => {
                let pointed = self.mirror(pointee)?.pointed().clone();
                Mirror::ByValue {
                    pointed: c_pointer_var(pointed.clone()),
                    value: c_pointer(pointed),
                    nullable: true,
                    info: Info::Pointer,
                }
            }
        })
    }

    /// The Kotlin type a function returns a value of `ty` as: `Unit` for `void`.
    pub(super) fn result(&self, ty: TypeId) -> Result<KotlinType, Unsupported> {
        if let Type::Void = self.declarations.ty(self.declarations.canonical(ty)) {
            return Ok(unit());
        }
        Ok(self.mirror(ty)?.argument())
    }

    pub(super) fn enum_mirror(&self, id: EnumId) -> Result<Mirror, Unsupported> {
        let base = primitive(self.enum_base(id)?);
        if self.is_strict(id) {
            let class = self.enum_class(id);
            return Ok(Mirror::ByValue {
                pointed: KotlinType::class(class.nested_child("Var"), Vec::new()),
                value: KotlinType::class(class, Vec::new()),
                nullable: false,
                info: Info::Enum(class),
            });
        }
        let Some(name) = self.registry.enum_name(id) else {
            return Ok(base);
        };
        let Mirror::ByValue {
            pointed,
            value,
            nullable,
            info,
        } = base
        else {
            unreachable!("a primitive is mirrored by value")
        };
        Ok(Mirror::ByValue {
            pointed: KotlinType::alias(self.own(&format!("{name}Var")), Vec::new(), pointed),
            value: KotlinType::alias(self.own(name), Vec::new(), value),
            nullable,
            info,
        })
    }

    /// The name of a named, non-strict enum, which the library declares as two type aliases.
    pub(super) fn non_strict_enum_name(&self, id: EnumId) -> Option<&Rc<str>> {
        (!self.is_strict(id))
            .then(|| self.registry.enum_name(id))
            .flatten()
    }
}

/// The variable class a typedef of a value with `info` declares, holding `value`: `IntVarOf<T>`,
/// `CPointerVarOf<T>`, or a strict enum's `Var`.
pub(super) fn pointed_of(info: &Info, value: KotlinType) -> KotlinType {
    match info {
        Info::Primitive(primitive) => primitive.var_of(value),
        Info::Pointer => c_pointer_var_of(value),
        Info::Enum(class) => KotlinType::class(class.nested_child("Var"), Vec::new()),
    }
}

fn primitive(primitive: Primitive) -> Mirror {
    Mirror::ByValue {
        pointed: primitive.var(),
        value: primitive.value_type(),
        nullable: false,
        info: Info::Primitive(primitive),
    }
}

fn opaque_pointer() -> Mirror {
    Mirror::ByValue {
        pointed: c_opaque_pointer_var(),
        value: c_opaque_pointer(),
        nullable: true,
        info: Info::Pointer,
    }
}

/// The primitive a C integer type is: `char` and `signed char` are `Byte` whatever the target's
/// `char` signedness, `_Bool` is `Boolean`.
pub(super) fn integer(rank: IntRank, signedness: Signedness) -> Result<Primitive, Unsupported> {
    let unsigned = signedness == Signedness::Unsigned;
    Ok(match (rank, unsigned) {
        (IntRank::Bool, _) => Primitive::Boolean,
        (IntRank::Char, false) => Primitive::Byte,
        (IntRank::Char, true) => Primitive::UByte,
        (IntRank::Short, false) => Primitive::Short,
        (IntRank::Short, true) => Primitive::UShort,
        (IntRank::Int, false) => Primitive::Int,
        (IntRank::Int, true) => Primitive::UInt,
        (IntRank::Long | IntRank::LongLong, false) => Primitive::Long,
        (IntRank::Long | IntRank::LongLong, true) => Primitive::ULong,
        (IntRank::Int128, _) => return Err(Unsupported),
    })
}
