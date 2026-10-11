//! The Kotlin types an interop library declares with: classes and type aliases of the Kotlin
//! standard library and `kotlinx.cinterop`, and the library's own declarations.
//!
//! A use of a type alias keeps both forms, as Kotlin metadata records them: the alias with its
//! arguments, and what it stands for. `CPointerVar<ByteVar>` is the alias `CPointerVar` applied
//! to the alias `ByteVar`, standing for `CPointerVarOf<CPointer<ByteVarOf<Byte>>>`.

use crate::types::{type_name, TypeName};

/// A Kotlin type: a class or a type alias, with its arguments.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct KotlinType {
    pub(super) head: Head,
    pub(super) arguments: Vec<Argument>,
    pub(super) nullable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Head {
    Class(TypeName),
    /// A type alias, with the type it stands for at this use (its arguments substituted).
    Alias(TypeName, Box<KotlinType>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Argument {
    Star,
    Invariant(KotlinType),
    Out(KotlinType),
}

impl KotlinType {
    pub(super) fn class(name: TypeName, arguments: Vec<Argument>) -> Self {
        Self {
            head: Head::Class(name),
            arguments,
            nullable: false,
        }
    }

    /// A use of the alias `name` with `arguments`, standing for `expansion`.
    pub(super) fn alias(name: TypeName, arguments: Vec<Argument>, expansion: KotlinType) -> Self {
        Self {
            head: Head::Alias(name, Box::new(expansion)),
            arguments,
            nullable: false,
        }
    }

    pub(super) fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }
}

/// A classifier of a library the interop library is compiled against.
pub(super) fn named(path: &str) -> TypeName {
    type_name(path)
}

fn simple(path: &str) -> KotlinType {
    KotlinType::class(named(path), Vec::new())
}

fn invariant(ty: KotlinType) -> Argument {
    Argument::Invariant(ty)
}

pub(super) fn unit() -> KotlinType {
    simple("kotlin/Unit")
}

pub(super) fn string() -> KotlinType {
    simple("kotlin/String")
}

pub(super) fn any() -> KotlinType {
    simple("kotlin/Any")
}

pub(super) fn array(element: KotlinType) -> KotlinType {
    KotlinType::class(named("kotlin/Array"), vec![invariant(element)])
}

pub(super) fn native_ptr() -> KotlinType {
    KotlinType::alias(
        named("kotlinx/cinterop/NativePtr"),
        Vec::new(),
        simple("kotlin/native/internal/NativePtr"),
    )
}

/// A Kotlin primitive (or unsigned) value type a C scalar maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Primitive {
    Byte,
    Short,
    Int,
    Long,
    UByte,
    UShort,
    UInt,
    ULong,
    Float,
    Double,
    Boolean,
    Vector128,
}

impl Primitive {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Byte => "Byte",
            Self::Short => "Short",
            Self::Int => "Int",
            Self::Long => "Long",
            Self::UByte => "UByte",
            Self::UShort => "UShort",
            Self::UInt => "UInt",
            Self::ULong => "ULong",
            Self::Float => "Float",
            Self::Double => "Double",
            Self::Boolean => "Boolean",
            Self::Vector128 => "Vector128",
        }
    }

    pub(super) fn is_unsigned(self) -> bool {
        matches!(self, Self::UByte | Self::UShort | Self::UInt | Self::ULong)
    }

    /// The Kotlin value type: `kotlin.Int`, `kotlinx.cinterop.Vector128`.
    pub(super) fn value_type(self) -> KotlinType {
        match self {
            Self::Vector128 => simple("kotlinx/cinterop/Vector128"),
            other => simple(&format!("kotlin/{}", other.name())),
        }
    }

    /// The `kotlinx.cinterop` variable class holding a value of `value` (this primitive, or an
    /// alias of it): `IntVarOf<value>`.
    pub(super) fn var_of(self, value: KotlinType) -> KotlinType {
        KotlinType::class(
            named(&format!("kotlinx/cinterop/{}VarOf", self.name())),
            vec![invariant(value)],
        )
    }

    /// The `kotlinx.cinterop` alias for this primitive's variable: `IntVar`.
    pub(super) fn var(self) -> KotlinType {
        KotlinType::alias(
            named(&format!("kotlinx/cinterop/{}Var", self.name())),
            Vec::new(),
            self.var_of(self.value_type()),
        )
    }
}

/// `CPointer<pointed>`.
pub(super) fn c_pointer(pointed: KotlinType) -> KotlinType {
    KotlinType::class(named("kotlinx/cinterop/CPointer"), vec![invariant(pointed)])
}

/// `CPointerVarOf<pointer>`.
pub(super) fn c_pointer_var_of(pointer: KotlinType) -> KotlinType {
    KotlinType::class(
        named("kotlinx/cinterop/CPointerVarOf"),
        vec![invariant(pointer)],
    )
}

/// `CPointerVar<pointed>`, which stands for `CPointerVarOf<CPointer<pointed>>`.
pub(super) fn c_pointer_var(pointed: KotlinType) -> KotlinType {
    KotlinType::alias(
        named("kotlinx/cinterop/CPointerVar"),
        vec![invariant(pointed.clone())],
        c_pointer_var_of(c_pointer(pointed)),
    )
}

/// `CArrayPointer<pointed>`, which stands for `CPointer<pointed>`.
pub(super) fn c_array_pointer(pointed: KotlinType) -> KotlinType {
    KotlinType::alias(
        named("kotlinx/cinterop/CArrayPointer"),
        vec![invariant(pointed.clone())],
        c_pointer(pointed),
    )
}

/// `CArrayPointerVar<pointed>`, which stands for `CPointerVar<pointed>`.
pub(super) fn c_array_pointer_var(pointed: KotlinType) -> KotlinType {
    KotlinType::alias(
        named("kotlinx/cinterop/CArrayPointerVar"),
        vec![invariant(pointed.clone())],
        c_pointer_var(pointed),
    )
}

/// `COpaquePointer`, which stands for `CPointer<out CPointed>`.
pub(super) fn c_opaque_pointer() -> KotlinType {
    KotlinType::alias(
        named("kotlinx/cinterop/COpaquePointer"),
        Vec::new(),
        KotlinType::class(
            named("kotlinx/cinterop/CPointer"),
            vec![Argument::Out(simple("kotlinx/cinterop/CPointed"))],
        ),
    )
}

/// `COpaquePointerVar`, which stands for `CPointerVarOf<COpaquePointer>`.
pub(super) fn c_opaque_pointer_var() -> KotlinType {
    KotlinType::alias(
        named("kotlinx/cinterop/COpaquePointerVar"),
        Vec::new(),
        c_pointer_var_of(c_opaque_pointer()),
    )
}

/// `CValue<pointed>`, a struct passed by value.
pub(super) fn c_value(pointed: KotlinType) -> KotlinType {
    KotlinType::class(named("kotlinx/cinterop/CValue"), vec![invariant(pointed)])
}

/// `CValuesRef<pointed>`, what a pointer parameter accepts.
pub(super) fn c_values_ref(pointed: Option<KotlinType>) -> KotlinType {
    KotlinType::class(
        named("kotlinx/cinterop/CValuesRef"),
        vec![pointed.map_or(Argument::Star, invariant)],
    )
}

/// `CFunction<FunctionN<parameters…, result>>`.
pub(super) fn c_function(parameters: Vec<KotlinType>, result: KotlinType) -> KotlinType {
    let arity = parameters.len();
    let mut arguments: Vec<Argument> = parameters.into_iter().map(invariant).collect();
    arguments.push(invariant(result));
    let function = KotlinType::class(named(&format!("kotlin/Function{arity}")), arguments);
    KotlinType::class(
        named("kotlinx/cinterop/CFunction"),
        vec![invariant(function)],
    )
}

/// A `kotlinx.cinterop` class without arguments: `CStructVar`, `CEnum`, ….
pub(super) fn cinterop_class(name: &str) -> KotlinType {
    simple(&format!("kotlinx/cinterop/{name}"))
}

/// The type a struct declared nowhere in the headers is named by: `cnames.structs.<tag>`.
pub(super) fn forward_declared_struct(tag: &str) -> TypeName {
    named(&format!("cnames/structs/{tag}"))
}
