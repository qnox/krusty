//! Type model: the primitive/String/Unit set plus `Obj` (a JVM reference type by internal name,
//! e.g. a Kotlin class `demo/Point`). No generics or nullability yet.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// Intern a class internal-name to a `&'static str` so `Ty` stays `Copy`. The compiler is
/// short-lived and the number of distinct class names is small, so leaking interned names is fine.
pub fn intern(name: &str) -> &'static str {
    static I: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(name) {
        return v;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// Intern a `Ty` to a canonical `&'static Ty` so array element types compare by value (the derived
/// `Eq`/`Hash` on `Ty::Array` follow the reference, so equal elements must share one pointer).
pub fn intern_ty(t: Ty) -> &'static Ty {
    static I: OnceLock<Mutex<HashSet<&'static Ty>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(&t) {
        return v;
    }
    let leaked: &'static Ty = Box::leak(Box::new(t));
    set.insert(leaked);
    leaked
}

/// Intern a generic type-argument list to a canonical `&'static [Ty]` so equal instantiations share a
/// pointer (the derived `Eq`/`Hash` on `Ty::Obj` compares the slice by reference). Empty → `&[]`.
pub fn intern_tys(ts: &[Ty]) -> &'static [Ty] {
    if ts.is_empty() {
        return &[];
    }
    static I: OnceLock<Mutex<HashSet<&'static [Ty]>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(ts) {
        return v;
    }
    let leaked: &'static [Ty] = Box::leak(ts.to_vec().into_boxed_slice());
    set.insert(leaked);
    leaked
}

/// A function type's signature: parameter types and return type. Interned (`intern_fnsig`) so
/// `Ty::Fun` stays `Copy`. Lets a `Fun`-typed call recover its real return type (not erased `Object`).
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct FnSig {
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// A `suspend` function type (`suspend (A) -> R`). Its JVM representation is `Function{n+1}` with a
    /// trailing `kotlin/coroutines/Continuation` parameter and an `Object`-erased result.
    pub suspend: bool,
}

/// Intern a `FnSig` to a canonical `&'static FnSig` (leaked; the compiler is short-lived).
pub fn intern_fnsig(s: FnSig) -> &'static FnSig {
    static I: OnceLock<Mutex<HashSet<&'static FnSig>>> = OnceLock::new();
    let set = I.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap();
    if let Some(&v) = set.get(&s) {
        return v;
    }
    let leaked: &'static FnSig = Box::leak(Box::new(s));
    set.insert(leaked);
    leaked
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Ty {
    Int,
    Byte,
    Short,
    Long,
    Float,
    Double,
    Boolean,
    Char,
    /// Unsigned integers — Kotlin inline classes over the matching signed primitive (`UInt` over
    /// `Int`, `ULong` over `Long`). Unboxed they ARE that JVM primitive (`I`/`J`); the unsignedness
    /// shows only in operation choice (unsigned `/`/`%`/compare/`toString`) and boxing (`kotlin/UInt`).
    /// Kept distinct from the signed types so those operations and widening (`toLong` = zero-extend)
    /// are selected correctly.
    UInt,
    ULong,
    String,
    Unit,
    /// A JVM reference type by internal name (e.g. `demo/Point`), with its generic type arguments
    /// (`List<Int>` → `Obj("kotlin/collections/List", [Int])`). Arguments are interned (`intern_tys`)
    /// so equal instantiations share a pointer; they erase to nothing in JVM descriptors (kotlinc's
    /// erasure) but let the front end recover element/member types. Empty for a non-generic class.
    Obj(&'static str, &'static [Ty]),
    /// A JVM array type with the given element type (`IntArray` → `Array(&Int)`, `Array<String>` →
    /// `Array(&String)`). Element `Ty`s are interned (`intern_ty`) so equal arrays share a pointer.
    Array(&'static Ty),
    /// The type of the `null` literal — assignable to any reference type.
    Null,
    /// The bottom type (`Nothing`): the type of `throw`/`return` expressions. Assignable to every
    /// type; an expression of this type never yields a value (it always diverges).
    Nothing,
    /// Placeholder after a type error, suppresses cascading diagnostics.
    Error,
    /// A Kotlin function type `(A, B) -> R` — lowered to `kotlin/jvm/functions/FunctionN` (N = arity)
    /// by the JVM backend, but the front end keeps the real parameter/return types (interned `FnSig`)
    /// so a call through a `Fun` value recovers its return type instead of erasing to `Object`.
    Fun(&'static FnSig),
    /// A nullable type `T?`. Wraps the interned non-null type. Kotlin has no `T??`, so the inner type
    /// is never itself `Nullable` (the [`Ty::nullable`] constructor enforces this). Nullability is a
    /// Kotlin-level fact: a nullable primitive (`Int?`) is a JVM *reference* (boxed `java/lang/Integer`)
    /// — that representation choice lives in the JVM backend / [`Ty::descriptor`], not in the checker.
    Nullable(&'static Ty),
}

impl Ty {
    /// A class reference type from an internal name (no generic arguments).
    pub fn obj(internal: &str) -> Ty {
        Ty::Obj(intern(internal), &[])
    }

    /// A generic class reference type — `internal<args…>`.
    pub fn obj_args(internal: &str, args: &[Ty]) -> Ty {
        Ty::Obj(intern(internal), intern_tys(args))
    }

    /// The generic type arguments of a reference type (empty for non-generic / non-`Obj`).
    pub fn type_args(self) -> &'static [Ty] {
        match self {
            Ty::Obj(_, args) => args,
            _ => &[],
        }
    }

    /// An array type with the given element type.
    pub fn array(elem: Ty) -> Ty {
        Ty::Array(intern_ty(elem))
    }

    /// The element type if this is an array — a primitive specialized array (`IntArray` → `Int`) or a
    /// Kotlin `Array<T>` carried as `Obj("kotlin/Array", [T])` (its *logical* element, e.g. `Int` for
    /// `Array<Int>`; the wrapper boxing is the backend's concern, not the type's).
    pub fn array_elem(self) -> Option<Ty> {
        match self {
            Ty::Array(e) => Some(*e),
            Ty::Obj("kotlin/Array", args) => args.first().copied(),
            _ => None,
        }
    }

    /// The nullable form `T?` of a type. Idempotent (Kotlin has no `T??`), and degenerate inputs
    /// collapse: `Null?` = `Null`, `Error?` = `Error`. `Nothing?` is kept — it's the real type of the
    /// `null` literal.
    pub fn nullable(inner: Ty) -> Ty {
        match inner {
            Ty::Nullable(_) | Ty::Null | Ty::Error => inner,
            _ => Ty::Nullable(intern_ty(inner)),
        }
    }

    /// Whether this type is nullable (`T?`).
    pub fn is_nullable(self) -> bool {
        matches!(self, Ty::Nullable(_))
    }

    /// The non-null form: strips a `?` if present, else returns `self`.
    pub fn non_null(self) -> Ty {
        match self {
            Ty::Nullable(inner) => *inner,
            _ => self,
        }
    }

    /// The unboxed primitive of a nullable primitive (`Int?` → `Int`), else `None`. Replaces the old
    /// "is this a boxed-wrapper `Obj`?" probe (`t.obj_internal().and_then(prim_of_wrapper)`).
    pub fn nullable_primitive(self) -> Option<Ty> {
        match self {
            Ty::Nullable(inner) if inner.is_primitive() => Some(*inner),
            _ => None,
        }
    }

    /// The nullable form `T?` of a primitive that krusty can box (`Int` → `Int?`). `None` for a
    /// non-primitive (already a reference) or the unsigned/value primitives, whose nullable boxing
    /// isn't supported yet (those stay rejected — never miscompiled).
    pub fn nullable_boxed(self) -> Option<Ty> {
        (self.is_primitive() && !self.is_unsigned()).then(|| Ty::nullable(self))
    }

    /// A function type `(params) -> ret`.
    pub fn fun(params: Vec<Ty>, ret: Ty) -> Ty {
        Ty::Fun(intern_fnsig(FnSig {
            params,
            ret,
            suspend: false,
        }))
    }

    /// A `suspend` function type `suspend (params) -> ret`.
    pub fn fun_suspend(params: Vec<Ty>, ret: Ty) -> Ty {
        Ty::Fun(intern_fnsig(FnSig {
            params,
            ret,
            suspend: true,
        }))
    }

    /// Whether this is a `suspend` function type.
    pub fn is_suspend_fun(self) -> bool {
        matches!(self, Ty::Fun(s) if s.suspend)
    }

    /// Arity of a function type.
    pub fn fun_arity(self) -> Option<u8> {
        match self {
            Ty::Fun(s) => Some(s.params.len() as u8),
            _ => None,
        }
    }

    /// Return type of a function type.
    pub fn fun_ret(self) -> Option<Ty> {
        match self {
            Ty::Fun(s) => Some(s.ret),
            _ => None,
        }
    }

    /// Parameter types of a function type.
    pub fn fun_params(self) -> Option<&'static [Ty]> {
        match self {
            Ty::Fun(s) => Some(&s.params),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<Ty> {
        Some(match name {
            "Int" => Ty::Int,
            "Byte" => Ty::Byte,
            "Short" => Ty::Short,
            "Long" => Ty::Long,
            "Float" => Ty::Float,
            "Double" => Ty::Double,
            "Boolean" => Ty::Boolean,
            "Char" => Ty::Char,
            "UInt" => Ty::UInt,
            "ULong" => Ty::ULong,
            "String" => Ty::String,
            "Unit" => Ty::Unit,
            "Any" => Ty::obj("kotlin/Any"),
            _ => return None,
        })
    }

    /// The element type of a specialized primitive array type name (`IntArray` → `Int`, …).
    /// `Array<T>` is handled separately (it carries its element as a type argument).
    pub fn primitive_array_element(name: &str) -> Option<Ty> {
        Some(match name {
            "IntArray" => Ty::Int,
            "LongArray" => Ty::Long,
            "DoubleArray" => Ty::Double,
            "FloatArray" => Ty::Float,
            "BooleanArray" => Ty::Boolean,
            "CharArray" => Ty::Char,
            "ByteArray" => Ty::Byte,
            "ShortArray" => Ty::Short,
            _ => return None,
        })
    }

    /// The BOXED reference form of a primitive, used as the element type of a `Array<Int>` (a
    /// `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`). Carried in the front end
    /// as the Kotlin primitive name (`kotlin/Int`); it erases to the JVM wrapper only at emit (see
    /// `jvm_class_map::to_jvm_internal`). `None` for a non-primitive (already a reference) or for the
    /// unsigned inline-class primitives (their boxing is handled by the value-class path).
    pub fn boxed_ref(self) -> Option<Ty> {
        Some(Ty::obj(match self {
            Ty::Int => "kotlin/Int",
            Ty::Byte => "kotlin/Byte",
            Ty::Short => "kotlin/Short",
            Ty::Long => "kotlin/Long",
            Ty::Float => "kotlin/Float",
            Ty::Double => "kotlin/Double",
            Ty::Boolean => "kotlin/Boolean",
            Ty::Char => "kotlin/Char",
            _ => return None,
        }))
    }

    /// Inverse of [`boxed_ref`]: if `self` is the boxed-reference form of a primitive (the element of a
    /// `Array<Int>`, carried as `Obj("kotlin/Int")`), the underlying primitive `Ty`. `None` otherwise.
    pub fn unboxed_primitive(self) -> Option<Ty> {
        Some(match self {
            Ty::Obj("kotlin/Int", _) => Ty::Int,
            Ty::Obj("kotlin/Byte", _) => Ty::Byte,
            Ty::Obj("kotlin/Short", _) => Ty::Short,
            Ty::Obj("kotlin/Long", _) => Ty::Long,
            Ty::Obj("kotlin/Float", _) => Ty::Float,
            Ty::Obj("kotlin/Double", _) => Ty::Double,
            Ty::Obj("kotlin/Boolean", _) => Ty::Boolean,
            Ty::Obj("kotlin/Char", _) => Ty::Char,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Ty::Int => "Int",
            Ty::Byte => "Byte",
            Ty::Short => "Short",
            Ty::Long => "Long",
            Ty::Float => "Float",
            Ty::Double => "Double",
            Ty::Boolean => "Boolean",
            Ty::Char => "Char",
            Ty::UInt => "UInt",
            Ty::ULong => "ULong",
            Ty::String => "String",
            Ty::Unit => "Unit",
            Ty::Obj(n, _) => n,
            Ty::Null => "Null",
            Ty::Nothing => "Nothing",
            Ty::Array(_) => "Array",
            Ty::Error => "<error>",
            Ty::Fun(_) => "Function",
            Ty::Nullable(inner) => inner.name(),
        }
    }

    /// Internal name if this is a reference type.
    pub fn obj_internal(self) -> Option<&'static str> {
        match self {
            Ty::Obj(n, _) => Some(n),
            _ => None,
        }
    }

    /// True for JVM reference types (where `null` is a valid value). Any nullable type is a
    /// reference: a nullable primitive (`Int?`) boxes to its wrapper.
    pub fn is_reference(self) -> bool {
        matches!(
            self,
            Ty::String | Ty::Obj(..) | Ty::Null | Ty::Array(_) | Ty::Fun(_) | Ty::Nullable(_)
        )
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double
        )
    }

    pub fn is_primitive(self) -> bool {
        matches!(
            self,
            Ty::Int
                | Ty::Byte
                | Ty::Short
                | Ty::Long
                | Ty::Float
                | Ty::Double
                | Ty::Boolean
                | Ty::Char
                | Ty::UInt
                | Ty::ULong
        )
    }

    /// True for the unsigned integer types (inline classes over a signed primitive).
    pub fn is_unsigned(self) -> bool {
        matches!(self, Ty::UInt | Ty::ULong)
    }

    /// A primitive whose generic upper bound (`fun <T: Int>`) krusty specializes a FUNCTION type
    /// parameter to (descriptor uses the primitive, like kotlinc). Restricted to the INTEGRAL JVM
    /// primitives: floating types (`Double`/`Float`) have boxed-vs-primitive `==` (−0.0/NaN) semantics
    /// that differ, and the unsigned/value primitives aren't representable, so those bounds stay
    /// rejected (the file skips) rather than risk a miscompile.
    pub fn is_specializable_bound(self) -> bool {
        matches!(
            self,
            Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Boolean | Ty::Char
        )
    }

    /// The signed primitive an unsigned type is represented by on the JVM (`UInt` → `Int`).
    pub fn unsigned_repr(self) -> Option<Ty> {
        match self {
            Ty::UInt => Some(Ty::Int),
            Ty::ULong => Some(Ty::Long),
            _ => None,
        }
    }

    /// JVM type descriptor for ABI (`I`, `J`, `D`, `Z`, `String`, `V`, `Lpkg/Name;`). Reference
    /// descriptors run the (Kotlin) internal name through the JVM name mapping, so the `java/lang/…`
    /// realization lives in the JVM part, not here — this method is the Ty→bytecode boundary.
    pub fn descriptor(self) -> String {
        use crate::jvm::jvm_class_map::to_jvm_internal;
        let obj_desc = |internal: &str| format!("L{};", to_jvm_internal(internal));
        match self {
            Ty::Int => "I".into(),
            Ty::Byte => "B".into(),
            Ty::Short => "S".into(),
            Ty::Long => "J".into(),
            Ty::Float => "F".into(),
            Ty::Double => "D".into(),
            Ty::Boolean => "Z".into(),
            Ty::Char => "C".into(),
            // Unboxed inline-class erasure: `UInt` is a JVM `int`, `ULong` a `long`.
            Ty::UInt => "I".into(),
            Ty::ULong => "J".into(),
            Ty::String => obj_desc("kotlin/String"),
            Ty::Unit => "V".into(),
            Ty::Obj(n, _) => obj_desc(n),
            Ty::Null => obj_desc("kotlin/Any"),
            Ty::Nothing => obj_desc("kotlin/Any"),
            Ty::Array(elem) => format!("[{}", elem.descriptor()),
            Ty::Error => obj_desc("kotlin/Any"),
            // A `suspend` function type carries a trailing `Continuation` parameter, so its arity is one
            // greater than the logical parameter count (`suspend () -> R` → `Function1`).
            Ty::Fun(s) => format!(
                "Lkotlin/jvm/functions/Function{};",
                s.params.len() + usize::from(s.suspend)
            ),
            // `T?` is a reference: a nullable primitive boxes to its wrapper (`Int?` →
            // `Ljava/lang/Integer;`, `UInt?` → `Lkotlin/UInt;`); a nullable reference keeps the same
            // descriptor.
            Ty::Nullable(inner) => match *inner {
                Ty::UInt => obj_desc("kotlin/UInt"),
                Ty::ULong => obj_desc("kotlin/ULong"),
                other => other.boxed_ref().unwrap_or(other).descriptor(),
            },
        }
    }

    /// The `kotlin/jvm/functions/FunctionN` interface internal name for a `Fun(n)` type.
    pub fn fun_interface(n: u8) -> String {
        format!("kotlin/jvm/functions/Function{n}")
    }

    /// Numeric promotion rank for binary arithmetic (Int < Long < Double).
    fn rank(self) -> u8 {
        match self {
            // Byte/Short share Int's rank: Kotlin arithmetic on them produces `Int`.
            Ty::Byte | Ty::Short | Ty::Int => 1,
            Ty::Long => 2,
            Ty::Float => 3,
            Ty::Double => 4,
            _ => 0,
        }
    }

    /// Result type of numeric promotion, or `None` if either side isn't numeric. `Byte`/`Short`
    /// promote to `Int` (Kotlin has no byte/short arithmetic — operands widen to `Int`).
    pub fn promote(a: Ty, b: Ty) -> Option<Ty> {
        if a.is_numeric() && b.is_numeric() {
            let r = if a.rank() >= b.rank() { a } else { b };
            Some(if matches!(r, Ty::Byte | Ty::Short) {
                Ty::Int
            } else {
                r
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nullable_wraps_inner_and_reports_nullable() {
        let t = Ty::nullable(Ty::Int);
        assert!(t.is_nullable());
        assert_eq!(t.non_null(), Ty::Int);
    }

    #[test]
    fn non_null_type_is_not_nullable() {
        assert!(!Ty::Int.is_nullable());
        assert_eq!(Ty::Int.non_null(), Ty::Int);
    }

    #[test]
    fn nullable_is_idempotent_no_double_wrap() {
        // Kotlin has no `T??`: wrapping an already-nullable type is a no-op.
        let once = Ty::nullable(Ty::obj("demo/Point"));
        assert_eq!(Ty::nullable(once), once);
    }

    #[test]
    fn nullable_primitive_is_a_reference_so_null_is_valid() {
        // `Int?` boxes — it accepts `null`, unlike the unboxed `Int`.
        assert!(!Ty::Int.is_reference());
        assert!(Ty::nullable(Ty::Int).is_reference());
    }

    #[test]
    fn nullable_signed_primitive_descriptor_boxes_to_jvm_wrapper() {
        assert_eq!(Ty::nullable(Ty::Int).descriptor(), "Ljava/lang/Integer;");
        assert_eq!(Ty::nullable(Ty::Boolean).descriptor(), "Ljava/lang/Boolean;");
    }

    #[test]
    fn nullable_unsigned_primitive_descriptor_boxes_to_inline_class() {
        // `UInt?`/`ULong?` box to their Kotlin inline-class type, NOT the unboxed `I`/`J`.
        assert_eq!(Ty::nullable(Ty::UInt).descriptor(), "Lkotlin/UInt;");
        assert_eq!(Ty::nullable(Ty::ULong).descriptor(), "Lkotlin/ULong;");
    }

    #[test]
    fn nullable_reference_descriptor_matches_non_null() {
        assert_eq!(Ty::nullable(Ty::String).descriptor(), Ty::String.descriptor());
        let p = Ty::obj("demo/Point");
        assert_eq!(Ty::nullable(p).descriptor(), p.descriptor());
    }

    #[test]
    fn nullable_idempotent_over_a_primitive() {
        let once = Ty::nullable(Ty::Int);
        assert_eq!(Ty::nullable(once), once);
    }

    #[test]
    fn nullable_of_null_or_error_collapses() {
        // `Null?`/`Error?` are degenerate — wrapping them is meaningless.
        assert_eq!(Ty::nullable(Ty::Null), Ty::Null);
        assert_eq!(Ty::nullable(Ty::Error), Ty::Error);
    }

    #[test]
    fn nullable_of_nothing_is_a_real_distinct_type() {
        // Kotlin's `Nothing?` is the type of the `null` literal — a real nullable type, kept.
        assert!(Ty::nullable(Ty::Nothing).is_nullable());
        assert_eq!(Ty::nullable(Ty::Nothing).non_null(), Ty::Nothing);
    }

    #[test]
    fn nullable_primitive_recovers_the_unboxed_primitive() {
        assert_eq!(Ty::nullable(Ty::Int).nullable_primitive(), Some(Ty::Int));
        // Not a nullable primitive → None.
        assert_eq!(Ty::Int.nullable_primitive(), None);
        assert_eq!(Ty::nullable(Ty::String).nullable_primitive(), None);
        assert_eq!(Ty::obj("demo/Point").nullable_primitive(), None);
    }

    #[test]
    fn nullable_boxed_is_the_supported_nullable_form_of_a_primitive() {
        assert_eq!(Ty::Int.nullable_boxed(), Some(Ty::nullable(Ty::Int)));
        assert_eq!(Ty::Char.nullable_boxed(), Some(Ty::nullable(Ty::Char)));
        // Unsigned/value primitives: nullable boxing not supported yet.
        assert_eq!(Ty::UInt.nullable_boxed(), None);
        // Already a reference → not a primitive to box.
        assert_eq!(Ty::String.nullable_boxed(), None);
    }
}
