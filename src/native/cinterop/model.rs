//! The C declarations a header set declares, as cinterop reads them: an index-based arena of
//! types, the records and enums they name, and the typedefs, functions and variables declared at
//! file scope.
//!
//! Every type is a [`TypeId`] into [`Declarations::types`]; records, enums and typedefs are ids
//! into their own tables, so a self-referential struct (`struct node { struct node *next; }`) is a
//! pointer to a record id, never a cycle of owned values. Identical types share one id.

use std::collections::HashMap;
use std::rc::Rc;

/// A type, as an index into [`Declarations::types`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct TypeId(pub(super) u32);

/// A struct or union, as an index into [`Declarations::records`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct RecordId(pub(super) u32);

/// An enum, as an index into [`Declarations::enums`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct EnumId(pub(super) u32);

/// A typedef, as an index into [`Declarations::typedefs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct TypedefId(pub(super) u32);

/// The integer types by conversion rank (C11 §6.3.1.1), which also fixes their LP64 widths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum IntRank {
    Bool,
    Char,
    Short,
    Int,
    Long,
    LongLong,
    Int128,
}

impl IntRank {
    /// The width in bytes on every LP64 target krusty supports.
    pub(super) fn size(self) -> u64 {
        match self {
            Self::Bool | Self::Char => 1,
            Self::Short => 2,
            Self::Int => 4,
            Self::Long | Self::LongLong => 8,
            Self::Int128 => 16,
        }
    }
}

/// Whether a `char` written without `signed` or `unsigned` is signed depends on the target, and
/// it is a distinct type from both, so plain `char` is its own signedness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Signedness {
    Signed,
    Unsigned,
    /// Plain `char`.
    Plain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum FloatKind {
    Float,
    Double,
    LongDouble,
}

/// `const`, `volatile` and `restrict`, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Qualifiers(pub(super) u8);

impl Qualifiers {
    pub(super) const CONST: Self = Self(1);
    pub(super) const VOLATILE: Self = Self(2);
    pub(super) const RESTRICT: Self = Self(4);

    pub(super) fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(super) fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub(super) fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Type {
    Void,
    Int {
        rank: IntRank,
        signedness: Signedness,
    },
    Float(FloatKind),
    Complex(FloatKind),
    /// The target's `__builtin_va_list`, whose shape is the target's.
    VaList,
    Pointer(TypeId),
    /// `None` is an array of unknown length (`T x[]`).
    Array {
        element: TypeId,
        length: Option<u64>,
    },
    Function(FunctionType),
    Record(RecordId),
    Enum(EnumId),
    Typedef(TypedefId),
    Qualified {
        base: TypeId,
        qualifiers: Qualifiers,
    },
    Atomic(TypeId),
    /// A GNU vector (`__attribute__((vector_size(n)))`): `size` bytes of `element`.
    Vector {
        element: TypeId,
        size: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct FunctionType {
    pub(super) result: TypeId,
    /// The parameter types after adjustment: an array or function parameter is a pointer.
    pub(super) params: Vec<TypeId>,
    pub(super) variadic: bool,
    /// `false` for an old-style `f()` declaration, which says nothing about its parameters.
    pub(super) prototyped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecordKind {
    Struct,
    Union,
}

/// Where a declaration is, so a header filter can choose by file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Origin {
    pub(super) file: Rc<str>,
    pub(super) line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Field {
    /// `None` for an anonymous struct or union member and for an unnamed bitfield.
    pub(super) name: Option<Rc<str>>,
    pub(super) ty: TypeId,
    pub(super) bit_width: Option<u64>,
    /// An `aligned` attribute or `_Alignas`, in bytes.
    pub(super) aligned: Option<u64>,
    pub(super) packed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) kind: RecordKind,
    pub(super) tag: Option<Rc<str>>,
    /// `None` until the record is defined.
    pub(super) fields: Option<Vec<Field>>,
    pub(super) packed: bool,
    pub(super) aligned: Option<u64>,
    pub(super) origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Enumerator {
    pub(super) name: Rc<str>,
    pub(super) value: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Enum {
    pub(super) tag: Option<Rc<str>>,
    /// `None` until the enum is defined.
    pub(super) enumerators: Option<Vec<Enumerator>>,
    pub(super) packed: bool,
    pub(super) aligned: Option<u64>,
    pub(super) origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Typedef {
    pub(super) name: Rc<str>,
    pub(super) ty: TypeId,
    /// An `aligned` attribute on the typedef, which replaces the type's alignment (it may lower
    /// it, as GCC and clang allow).
    pub(super) aligned: Option<u64>,
    /// `None` for a compiler-provided typedef (`__builtin_va_list`, `__int128_t`).
    pub(super) origin: Option<Origin>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Storage {
    None,
    Extern,
    Static,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Function {
    pub(super) name: Rc<str>,
    /// A [`Type::Function`].
    pub(super) ty: TypeId,
    pub(super) param_names: Vec<Option<Rc<str>>>,
    pub(super) storage: Storage,
    pub(super) inline: bool,
    pub(super) noreturn: bool,
    /// Whether the header defines it (a `static inline` helper): Kotlin/Native binds only
    /// functions with an external definition by default, so these are recorded, not bound.
    pub(super) has_body: bool,
    /// The symbol an `__asm__("name")` label binds it to, when it is not `name`.
    pub(super) asm_name: Option<Rc<str>>,
    pub(super) origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Variable {
    pub(super) name: Rc<str>,
    pub(super) ty: TypeId,
    pub(super) storage: Storage,
    pub(super) thread_local: bool,
    pub(super) aligned: Option<u64>,
    pub(super) asm_name: Option<Rc<str>>,
    pub(super) origin: Origin,
}

/// Everything a translation unit declares at file scope.
#[derive(Debug, Default)]
pub(super) struct Declarations {
    pub(super) types: Vec<Type>,
    interned: HashMap<Type, TypeId>,
    pub(super) records: Vec<Record>,
    pub(super) enums: Vec<Enum>,
    pub(super) typedefs: Vec<Typedef>,
    pub(super) functions: Vec<Function>,
    pub(super) variables: Vec<Variable>,
}

impl Declarations {
    /// The id of `ty`, adding it to the arena the first time.
    pub(super) fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(&id) = self.interned.get(&ty) {
            return id;
        }
        let id = TypeId(u32::try_from(self.types.len()).expect("fewer than 2^32 types"));
        self.types.push(ty.clone());
        self.interned.insert(ty, id);
        id
    }

    pub(super) fn ty(&self, id: TypeId) -> &Type {
        &self.types[id.0 as usize]
    }

    pub(super) fn record(&self, id: RecordId) -> &Record {
        &self.records[id.0 as usize]
    }

    pub(super) fn record_mut(&mut self, id: RecordId) -> &mut Record {
        &mut self.records[id.0 as usize]
    }

    pub(super) fn enumeration(&self, id: EnumId) -> &Enum {
        &self.enums[id.0 as usize]
    }

    pub(super) fn enumeration_mut(&mut self, id: EnumId) -> &mut Enum {
        &mut self.enums[id.0 as usize]
    }

    pub(super) fn typedef(&self, id: TypedefId) -> &Typedef {
        &self.typedefs[id.0 as usize]
    }

    pub(super) fn add_record(&mut self, record: Record) -> RecordId {
        let id = RecordId(u32::try_from(self.records.len()).expect("fewer than 2^32 records"));
        self.records.push(record);
        id
    }

    pub(super) fn add_enum(&mut self, enumeration: Enum) -> EnumId {
        let id = EnumId(u32::try_from(self.enums.len()).expect("fewer than 2^32 enums"));
        self.enums.push(enumeration);
        id
    }

    pub(super) fn add_typedef(&mut self, typedef: Typedef) -> TypedefId {
        let id = TypedefId(u32::try_from(self.typedefs.len()).expect("fewer than 2^32 typedefs"));
        self.typedefs.push(typedef);
        id
    }

    /// `ty` with typedefs and qualifiers looked through.
    pub(super) fn canonical(&self, mut ty: TypeId) -> TypeId {
        loop {
            match self.ty(ty) {
                Type::Typedef(typedef) => ty = self.typedef(*typedef).ty,
                Type::Qualified { base, .. } => ty = *base,
                _ => return ty,
            }
        }
    }

    /// The indices that reach the member `name` of `record`, through any anonymous struct and
    /// union members it is declared in (C11 §6.7.2.1p13).
    pub(super) fn member_path(&self, record: RecordId, name: &str) -> Option<Vec<usize>> {
        let fields = self.record(record).fields.as_ref()?;
        for (index, field) in fields.iter().enumerate() {
            match &field.name {
                Some(field_name) if &**field_name == name => return Some(vec![index]),
                Some(_) => {}
                None if field.bit_width.is_none() => {
                    let Type::Record(inner) = self.ty(self.canonical(field.ty)) else {
                        continue;
                    };
                    if let Some(mut path) = self.member_path(*inner, name) {
                        path.insert(0, index);
                        return Some(path);
                    }
                }
                None => {}
            }
        }
        None
    }

    /// The C spelling of `ty`, for diagnostics and for checking the parser in tests.
    pub(super) fn spell(&self, ty: TypeId) -> String {
        self.spell_around(ty, String::new())
    }

    /// `ty` declaring `inner` (a declarator spelled so far), C's inside-out syntax.
    fn spell_around(&self, ty: TypeId, inner: String) -> String {
        let with = |base: String| {
            if inner.is_empty() {
                base
            } else {
                format!("{base} {inner}")
            }
        };
        match self.ty(ty) {
            Type::Void => with("void".to_string()),
            Type::Int { rank, signedness } => with(int_spelling(*rank, *signedness).to_string()),
            Type::Float(kind) => with(float_spelling(*kind).to_string()),
            Type::Complex(kind) => with(format!("_Complex {}", float_spelling(*kind))),
            Type::VaList => with("__builtin_va_list".to_string()),
            Type::Record(record) => {
                let record = self.record(*record);
                let keyword = match record.kind {
                    RecordKind::Struct => "struct",
                    RecordKind::Union => "union",
                };
                with(format!(
                    "{keyword} {}",
                    record.tag.as_deref().unwrap_or("<anonymous>")
                ))
            }
            Type::Enum(enumeration) => with(format!(
                "enum {}",
                self.enumeration(*enumeration)
                    .tag
                    .as_deref()
                    .unwrap_or("<anonymous>")
            )),
            Type::Typedef(typedef) => with(self.typedef(*typedef).name.to_string()),
            Type::Qualified { base, qualifiers } => {
                let words = qualifier_words(*qualifiers);
                match self.ty(*base) {
                    Type::Pointer(_) => {
                        let inner = if inner.is_empty() {
                            words
                        } else {
                            format!("{words} {inner}")
                        };
                        self.spell_around(*base, inner)
                    }
                    _ => with(format!("{words} {}", self.spell(*base))),
                }
            }
            Type::Atomic(base) => with(format!("_Atomic({})", self.spell(*base))),
            Type::Vector { element, size } => with(format!(
                "{} __attribute__((vector_size({size})))",
                self.spell(*element)
            )),
            Type::Pointer(pointee) => {
                let inner = format!("*{inner}");
                let inner = match self.ty(*pointee) {
                    Type::Array { .. } | Type::Function(_) => format!("({inner})"),
                    _ => inner,
                };
                self.spell_around(*pointee, inner)
            }
            Type::Array { element, length } => {
                let length = length.map_or(String::new(), |length| length.to_string());
                self.spell_around(*element, format!("{inner}[{length}]"))
            }
            Type::Function(function) => {
                let mut params: Vec<String> = function
                    .params
                    .iter()
                    .map(|&param| self.spell(param))
                    .collect();
                if function.variadic {
                    params.push("...".to_string());
                } else if params.is_empty() && function.prototyped {
                    params.push("void".to_string());
                }
                self.spell_around(function.result, format!("{inner}({})", params.join(", ")))
            }
        }
    }
}

fn int_spelling(rank: IntRank, signedness: Signedness) -> &'static str {
    match (rank, signedness) {
        (IntRank::Bool, _) => "_Bool",
        (IntRank::Char, Signedness::Plain) => "char",
        (IntRank::Char, Signedness::Signed) => "signed char",
        (IntRank::Char, Signedness::Unsigned) => "unsigned char",
        (IntRank::Short, Signedness::Unsigned) => "unsigned short",
        (IntRank::Short, _) => "short",
        (IntRank::Int, Signedness::Unsigned) => "unsigned int",
        (IntRank::Int, _) => "int",
        (IntRank::Long, Signedness::Unsigned) => "unsigned long",
        (IntRank::Long, _) => "long",
        (IntRank::LongLong, Signedness::Unsigned) => "unsigned long long",
        (IntRank::LongLong, _) => "long long",
        (IntRank::Int128, Signedness::Unsigned) => "unsigned __int128",
        (IntRank::Int128, _) => "__int128",
    }
}

fn float_spelling(kind: FloatKind) -> &'static str {
    match kind {
        FloatKind::Float => "float",
        FloatKind::Double => "double",
        FloatKind::LongDouble => "long double",
    }
}

fn qualifier_words(qualifiers: Qualifiers) -> String {
    [
        (Qualifiers::CONST, "const"),
        (Qualifiers::VOLATILE, "volatile"),
        (Qualifiers::RESTRICT, "restrict"),
    ]
    .iter()
    .filter(|(bit, _)| qualifiers.contains(*bit))
    .map(|(_, word)| *word)
    .collect::<Vec<_>>()
    .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_types_share_one_id_and_spell_inside_out() {
        let mut declarations = Declarations::default();
        let int = declarations.intern(Type::Int {
            rank: IntRank::Int,
            signedness: Signedness::Signed,
        });
        let char_type = declarations.intern(Type::Int {
            rank: IntRank::Char,
            signedness: Signedness::Plain,
        });
        let const_char = declarations.intern(Type::Qualified {
            base: char_type,
            qualifiers: Qualifiers::CONST,
        });
        let string = declarations.intern(Type::Pointer(const_char));
        let function = declarations.intern(Type::Function(FunctionType {
            result: int,
            params: vec![string],
            variadic: true,
            prototyped: true,
        }));
        let pointer = declarations.intern(Type::Pointer(function));
        let table = declarations.intern(Type::Array {
            element: pointer,
            length: Some(4),
        });
        assert_eq!(declarations.intern(Type::Pointer(const_char)), string);
        assert_eq!(declarations.spell(table), "int (*[4])(const char *, ...)");
        let restrict_pointer = declarations.intern(Type::Qualified {
            base: string,
            qualifiers: Qualifiers::RESTRICT.union(Qualifiers::CONST),
        });
        assert_eq!(
            declarations.spell(restrict_pointer),
            "const char *const restrict"
        );
        assert!(!Qualifiers::CONST.is_empty());
        assert_eq!(declarations.canonical(restrict_pointer), string);
    }
}
