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

    /// The element type if this is an array.
    pub fn array_elem(self) -> Option<Ty> {
        match self {
            Ty::Array(e) => Some(*e),
            _ => None,
        }
    }

    /// A function type `(params) -> ret`.
    pub fn fun(params: Vec<Ty>, ret: Ty) -> Ty {
        Ty::Fun(intern_fnsig(FnSig { params, ret }))
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
            "BooleanArray" => Ty::Boolean,
            "CharArray" => Ty::Char,
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
            Ty::String => "String",
            Ty::Unit => "Unit",
            Ty::Obj(n, _) => n,
            Ty::Null => "Null",
            Ty::Nothing => "Nothing",
            Ty::Array(_) => "Array",
            Ty::Error => "<error>",
            Ty::Fun(_) => "Function",
        }
    }

    /// Internal name if this is a reference type.
    pub fn obj_internal(self) -> Option<&'static str> {
        match self {
            Ty::Obj(n, _) => Some(n),
            _ => None,
        }
    }

    /// True for JVM reference types (where `null` is a valid value).
    pub fn is_reference(self) -> bool {
        matches!(self, Ty::String | Ty::Obj(..) | Ty::Null | Ty::Array(_) | Ty::Fun(_))
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double)
    }

    pub fn is_primitive(self) -> bool {
        matches!(self, Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double | Ty::Boolean | Ty::Char)
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
            Ty::String => obj_desc("kotlin/String"),
            Ty::Unit => "V".into(),
            Ty::Obj(n, _) => obj_desc(n),
            Ty::Null => obj_desc("kotlin/Any"),
            Ty::Nothing => obj_desc("kotlin/Any"),
            Ty::Array(elem) => format!("[{}", elem.descriptor()),
            Ty::Error => obj_desc("kotlin/Any"),
            Ty::Fun(s) => format!("Lkotlin/jvm/functions/Function{};", s.params.len()),
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
            Some(if matches!(r, Ty::Byte | Ty::Short) { Ty::Int } else { r })
        } else {
            None
        }
    }
}
