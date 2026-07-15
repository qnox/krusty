//! Type model: Kotlin scalar, object, array, function, nullable, and type-parameter shapes.
//! Backend-specific names and descriptors are kept out of this module.

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

/// Identity comparison of two INTERNED names. Both operands MUST come from [`intern`] (or [`wk`]); then
/// equal content ⇒ one shared allocation ⇒ this is an O(1) pointer compare instead of a byte-wise `==`.
/// Do NOT use on a raw (un-interned) string — it would report distinct for equal content.
#[inline]
pub fn same(a: &str, b: &str) -> bool {
    std::ptr::eq(a.as_ptr(), b.as_ptr())
}

/// Well-known class internal names, each interned ONCE on first use and reused for identity comparison
/// ([`same`]) against other interned names — so a hot check like "is this `Continuation`?" is a pointer
/// compare, and the literal is interned a single time rather than re-scanned per call.
pub mod wk {
    use super::intern;
    use std::sync::OnceLock;
    macro_rules! names {
        ($($f:ident => $lit:literal),* $(,)?) => { $(
            #[inline]
            pub fn $f() -> &'static str {
                static S: OnceLock<&'static str> = OnceLock::new();
                S.get_or_init(|| intern($lit))
            }
        )* };
    }
    names! {
        continuation => "kotlin/coroutines/Continuation",
        any => "kotlin/Any",
        java_object => "java/lang/Object",
    }
}

/// Intern a `Ty` to a canonical `&'static Ty` so a wrapped inner type (a `Nullable`/`TyParam` bound)
/// compares by value — the derived `Eq`/`Hash` follow the reference, so equal inner types must share
/// one pointer.
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
    /// A `suspend` function type (`suspend (A) -> R`).
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

/// The element type of a primitive specialized array class (`kotlin/IntArray` → `Int`), or `None` for a
/// non-primitive-array name. The unsigned arrays (`UIntArray`, …) keep their unsigned element so their
/// value-class identity survives; they still erase to the signed primitive array descriptor (`[I`).
/// The single canonical table — [`Ty::array_elem`], the constructor, and the backend descriptor logic
/// all route through it rather than each carrying their own copy.
pub fn prim_array_element(internal: &str) -> Option<Ty> {
    Some(match internal {
        "kotlin/IntArray" => Ty::Int,
        "kotlin/LongArray" => Ty::Long,
        "kotlin/ShortArray" => Ty::Short,
        "kotlin/ByteArray" => Ty::Byte,
        "kotlin/BooleanArray" => Ty::Boolean,
        "kotlin/CharArray" => Ty::Char,
        "kotlin/FloatArray" => Ty::Float,
        "kotlin/DoubleArray" => Ty::Double,
        "kotlin/UIntArray" => Ty::UInt,
        "kotlin/ULongArray" => Ty::ULong,
        _ => return None,
    })
}

/// The primitive specialized array class name for a primitive element (`Int` → `kotlin/IntArray`), or
/// `None` for a reference element (which lives in a boxed `Array<T>`). Inverse of [`prim_array_element`].
pub fn prim_array_name(elem: Ty) -> Option<&'static str> {
    Some(match elem {
        Ty::Int => "kotlin/IntArray",
        Ty::Long => "kotlin/LongArray",
        Ty::Short => "kotlin/ShortArray",
        Ty::Byte => "kotlin/ByteArray",
        Ty::Boolean => "kotlin/BooleanArray",
        Ty::Char => "kotlin/CharArray",
        Ty::Float => "kotlin/FloatArray",
        Ty::Double => "kotlin/DoubleArray",
        Ty::UInt => "kotlin/UIntArray",
        Ty::ULong => "kotlin/ULongArray",
        _ => return None,
    })
}

/// The JVM functional-interface internal name for each arity, `kotlin/jvm/functions/Function0..22`
/// (the arities the Kotlin stdlib declares). Indexed by arity; higher arities have no fixed interface.
pub const FUNCTION_N_INTERNAL: [&str; 23] = [
    "kotlin/jvm/functions/Function0",
    "kotlin/jvm/functions/Function1",
    "kotlin/jvm/functions/Function2",
    "kotlin/jvm/functions/Function3",
    "kotlin/jvm/functions/Function4",
    "kotlin/jvm/functions/Function5",
    "kotlin/jvm/functions/Function6",
    "kotlin/jvm/functions/Function7",
    "kotlin/jvm/functions/Function8",
    "kotlin/jvm/functions/Function9",
    "kotlin/jvm/functions/Function10",
    "kotlin/jvm/functions/Function11",
    "kotlin/jvm/functions/Function12",
    "kotlin/jvm/functions/Function13",
    "kotlin/jvm/functions/Function14",
    "kotlin/jvm/functions/Function15",
    "kotlin/jvm/functions/Function16",
    "kotlin/jvm/functions/Function17",
    "kotlin/jvm/functions/Function18",
    "kotlin/jvm/functions/Function19",
    "kotlin/jvm/functions/Function20",
    "kotlin/jvm/functions/Function21",
    "kotlin/jvm/functions/Function22",
];

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
    /// Unsigned integers — Kotlin inline-class types with unsigned operation semantics. Kept distinct
    /// from signed types so those operations and widening (`toLong` = zero-extend) are selected correctly.
    UInt,
    ULong,
    String,
    Unit,
    /// A reference type by internal name (e.g. `demo/Point`), with its generic type arguments
    /// (`List<Int>` → `Obj("kotlin/collections/List", [Int])`). Arguments are interned (`intern_tys`)
    /// so equal instantiations share a pointer and the front end can recover element/member types.
    /// Empty for a non-generic class.
    Obj(&'static str, &'static [Ty]),
    /// The type of the `null` literal — assignable to any reference type.
    Null,
    /// The bottom type (`Nothing`): the type of `throw`/`return` expressions. Assignable to every
    /// type; an expression of this type never yields a value (it always diverges).
    Nothing,
    /// Placeholder after a type error, suppresses cascading diagnostics.
    Error,
    /// A Kotlin function type `(A, B) -> R`. The front end keeps the real parameter/return types
    /// (interned `FnSig`) so a call through a `Fun` value recovers its return type.
    Fun(&'static FnSig),
    /// A nullable type `T?`. Wraps the interned non-null type. Kotlin has no `T??`, so the inner type
    /// is never itself `Nullable` (the [`Ty::nullable`] constructor enforces this).
    Nullable(&'static Ty),
    /// A generic type-parameter reference (`T`), carrying its name and declared upper bound
    /// (`<T : CharSequence>` → bound `CharSequence`; unbounded `<T>` → bound `kotlin/Any`). The checker
    /// reasons about `T` as `T` (subtyping against the bound, substitution at instantiation); runtime
    /// erasure is a backend concern.
    TyParam(&'static str, &'static Ty),
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
            Ty::TyParam(_, b) => b.type_args(),
            _ => &[],
        }
    }

    /// An array whose element is `elem`, choosing the array *kind* the way Kotlin does: a primitive
    /// element yields the specialized primitive array (`Int` → `IntArray` = `Obj("kotlin/IntArray")`,
    /// `[I`), any reference element yields the boxed `Array<T>` (`String` → `Obj("kotlin/Array", [String])`,
    /// `[Ljava/lang/String;`). To force a boxed `Array<Int>` (`[Ljava/lang/Integer;`) construct it
    /// directly as `Ty::obj_args("kotlin/Array", &[Ty::Int])`.
    pub fn array(elem: Ty) -> Ty {
        match prim_array_name(elem) {
            Some(n) => Ty::obj(n),
            None => Ty::obj_args("kotlin/Array", &[elem]),
        }
    }

    /// The element type if this is an array — a primitive specialized array (`IntArray` → `Int`) or a
    /// Kotlin `Array<T>` carried as `Obj("kotlin/Array", [T])` (its *logical* element, e.g. `Int` for
    /// `Array<Int>`; the wrapper boxing is the backend's concern, not the type's).
    pub fn array_elem(self) -> Option<Ty> {
        match self {
            Ty::Obj("kotlin/Array", args) => args.first().copied(),
            Ty::Obj(n, _) => prim_array_element(n),
            Ty::TyParam(_, b) => b.array_elem(),
            _ => None,
        }
    }

    /// Whether this type is any array — a primitive specialized array (`kotlin/IntArray`, …) or a boxed
    /// `Array<T>` (`Obj("kotlin/Array", [T])`). The single array-ness predicate; consumers must use this
    /// instead of pattern-matching a specific spelling so the representation can migrate under them.
    pub fn is_array(self) -> bool {
        matches!(self, Ty::Obj(n, _) if n == "kotlin/Array" || prim_array_element(n).is_some())
    }

    /// Whether this is a boxed `Array<T>` (`aaload`/`aastore`, elements stored as objects) as opposed to
    /// a primitive specialized array (`IntArray` → `iaload`/`iastore`). The only bit the backend needs to
    /// pick array opcodes; a reference array boxes primitive [`array_elem`]s at the store boundary.
    pub fn is_reference_array(self) -> bool {
        matches!(self, Ty::Obj("kotlin/Array", _))
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

    /// Kotlin class identity for types that have one in source-level member/subtype lookup.
    ///
    /// This is not a JVM descriptor mapping: it returns Kotlin internal names (`kotlin/Int`,
    /// `kotlin/String`, user class names), and deliberately ignores backend wrapper/internal names.
    /// Nullable and type-parameter forms delegate to their non-null/bound class identity.
    pub fn kotlin_class_internal(self) -> Option<&'static str> {
        match self {
            Ty::Obj(i, _) => Some(i),
            Ty::String => Some("kotlin/String"),
            Ty::Boolean => Some("kotlin/Boolean"),
            Ty::Byte => Some("kotlin/Byte"),
            Ty::Short => Some("kotlin/Short"),
            Ty::Int => Some("kotlin/Int"),
            Ty::Long => Some("kotlin/Long"),
            Ty::Char => Some("kotlin/Char"),
            Ty::Float => Some("kotlin/Float"),
            Ty::Double => Some("kotlin/Double"),
            Ty::UInt => Some("kotlin/UInt"),
            Ty::ULong => Some("kotlin/ULong"),
            Ty::Nullable(inner) => inner.kotlin_class_internal(),
            Ty::TyParam(_, bound) => bound.kotlin_class_internal(),
            _ => None,
        }
    }

    /// Whether this is Kotlin's semantic top reference type, including a physical JVM spelling that may
    /// arrive from classpath metadata.
    pub fn is_erased_top(self) -> bool {
        self.non_null()
            .obj_internal()
            .is_some_and(|n| same(n, wk::any()) || same(n, wk::java_object()))
    }

    /// The JVM functional-interface internal name (`kotlin/jvm/functions/FunctionN`) a function type
    /// implements — used for subtype/assignability tests against a user class that declares a
    /// function-type supertype. Kept SEPARATE from [`kotlin_class_internal`] (which returns `None` for a
    /// `Ty::Fun`): a function value is not, in general, interchangeable with its `FunctionN` class in the
    /// backend, so only the assignability checks that want the interface identity opt in here.
    pub fn function_interface_internal(self) -> Option<&'static str> {
        match self {
            Ty::Fun(s) => FUNCTION_N_INTERNAL.get(s.params.len()).copied(),
            _ => None,
        }
    }

    /// The canonical **extension-receiver key**: the `Ty` two receivers must share for an extension
    /// declared on one to resolve on the other. A Kotlin-level erasure that reproduces the equivalence
    /// the old JVM descriptor key gave for *reference* receivers, without referencing the backend — it
    /// drops a nullable reference's `?` (`String?` and `String` take the same extensions), generic
    /// arguments (`List<Int>` and `List<String>` share `List`'s extensions, recursively through
    /// arrays), and a type parameter to its (also-erased) bound (`fun T.f()` keys under the bound).
    /// `null`/`Nothing`/error key under `Any` (a `null` receiver reaches an `Any?` extension). Replaces
    /// a computed JVM descriptor string, which leaked the backend representation and allocated on every
    /// insert and lookup.
    ///
    /// It is deliberately NOT a faithful descriptor clone in two corners the descriptor folded only by
    /// accident of JVM erasure: signed vs unsigned primitives stay distinct (the descriptor merged
    /// `Int`/`UInt` because both erase to `I`), and function-type receivers stay distinct by full
    /// signature (the descriptor merged every arity-N `Fun` to `FunctionN`; that merge let an
    /// `((Int)->Int).f()` extension resolve on an `((String)->String)` receiver, which kotlinc rejects).
    /// A nullable *primitive* IS kept distinct from the unboxed primitive (`Int?` boxes — same key as an
    /// already-boxed `Array<Int>` element — while `Int` does not), matching the descriptor.
    pub fn erased_recv(self) -> Ty {
        match self {
            // A nullable primitive boxes to a distinct wrapper, so it keys apart from the unboxed
            // primitive: fold it to the primitive's boxed Kotlin class (the same key the already-boxed
            // `Array<Int>` element type carries). A nullable reference shares the non-null form's key.
            Ty::Nullable(inner) => match *inner {
                Ty::UInt => Ty::obj("kotlin/UInt"),
                Ty::ULong => Ty::obj("kotlin/ULong"),
                p if p.boxed_ref().is_some() => p.boxed_ref().unwrap_or(p),
                other => other.erased_recv(),
            },
            Ty::TyParam(_, b) => b.erased_recv(),
            // `Array<T>` keeps its array-ness but erases the ELEMENT's own generics (`Array<List<Int>>` →
            // `Array<List>`) — an array receiver keys per element class. Use `obj_args` (NOT `Ty::array`,
            // which collapses a bare-primitive element to a `IntArray` = `[I`, breaking the boxed
            // `Array<Int>` = `[Integer;` receiver) so the boxed array form is preserved.
            Ty::Obj("kotlin/Array", args) => {
                let e = args
                    .first()
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                Ty::obj_args("kotlin/Array", &[e.erased_recv()])
            }
            Ty::Obj(n, _) => Ty::Obj(n, &[]),
            // `null`/`Nothing` (and the error placeholder) are subtypes of every reference type, so a
            // receiver of one of these can invoke an `Any`/`Any?`-receiver extension — key them under
            // `Any` (`null.unsafeCast()` reaches `fun <T> Any?.unsafeCast()`).
            Ty::Null | Ty::Nothing | Ty::Error => Ty::obj("kotlin/Any"),
            _ => self,
        }
    }

    /// Candidate extension-receiver lookup keys, most-specific first. Generic receivers such as
    /// `val <T> T.p` or `val <T> Array<T>.p` register under `Any`/`Array<Any>`, while concrete receivers
    /// keep their precise erased key first so concrete overloads still win.
    pub fn erased_recv_candidates(self) -> Vec<Ty> {
        let mut keys = vec![self.erased_recv()];
        if let Ty::Obj("kotlin/Array", _) = keys[0] {
            keys.push(Ty::obj_args("kotlin/Array", &[Ty::obj("kotlin/Any")]));
        }
        keys.push(Ty::obj("kotlin/Any"));
        keys.dedup();
        keys
    }

    /// A generic type-parameter type `T` with the given declared upper bound (`kotlin/Any` if unbounded).
    pub fn ty_param(name: &str, bound: Ty) -> Ty {
        Ty::TyParam(intern(name), intern_ty(bound))
    }

    /// Whether this is a generic type-parameter reference (`T`).
    pub fn is_ty_param(self) -> bool {
        matches!(self, Ty::TyParam(..))
    }

    /// The name of a type-parameter type (`T`), else `None`.
    pub fn ty_param_name(self) -> Option<&'static str> {
        match self {
            Ty::TyParam(n, _) => Some(n),
            _ => None,
        }
    }

    /// The declared upper bound of a type-parameter type, else `None`.
    pub fn ty_param_bound(self) -> Option<Ty> {
        match self {
            Ty::TyParam(_, b) => Some(*b),
            _ => None,
        }
    }

    /// The unboxed primitive of a nullable primitive (`Int?` → `Int`), else `None`. Replaces the old
    /// "is this a boxed-wrapper `Obj`?" probe (`t.obj_internal().and_then(prim_of_wrapper)`).
    pub fn nullable_primitive(self) -> Option<Ty> {
        match self {
            Ty::Nullable(inner) if inner.boxed_ref().is_some() => Some(*inner),
            _ => None,
        }
    }

    /// The nullable form `T?` of a primitive that krusty can box (`Int` → `Int?`, `UInt` → `UInt?` boxed
    /// as `kotlin/UInt`). `None` for a non-primitive (already a reference). Unsigned boxes via its wrapper,
    /// so it is supported (parallel to [`Ty::nullable_primitive`], which already admits unsigned).
    pub fn nullable_boxed(self) -> Option<Ty> {
        self.boxed_ref().is_some().then(|| Ty::nullable(self))
    }

    /// Source-level nullable form for non-reference values that still have a valid reference
    /// representation. `Unit?` and `Nothing?` are real source types; primitive `T?` is represented as
    /// `Nullable(T)` until the backend picks its boxed carrier.
    pub fn nullable_non_ref(self) -> Option<Ty> {
        match self {
            Ty::Nothing | Ty::Unit => Some(Ty::nullable(self)),
            _ => self.nullable_boxed(),
        }
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
            "Nothing" => Ty::Nothing,
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
            // Unsigned size constructors: `UIntArray(n) { … }` / `ULongArray(n) { … }`. The element is
            // `UInt`/`ULong`; the physical array is the unboxed `[I`/`[J` (see `ir_lower`).
            "UIntArray" => Ty::UInt,
            "ULongArray" => Ty::ULong,
            _ => return None,
        })
    }

    /// The BOXED reference form of a primitive, used as the element type of a `Array<Int>` (a
    /// `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`). Carried in the front end
    /// as the Kotlin primitive name (`kotlin/Int`); it erases to the JVM wrapper only at emit (see
    /// `jvm_class_map::to_jvm_internal`). Unsigned primitives box to their own inline-class wrappers.
    /// `None` for a non-primitive (already a reference).
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
            // Unsigned types box to their OWN inline-class wrapper (`UInt` → `kotlin/UInt`), not a
            // `java/lang/*`; `kotlin_prim_to_wrapper` maps the wrapper to itself.
            Ty::UInt => "kotlin/UInt",
            Ty::ULong => "kotlin/ULong",
            _ => return None,
        }))
    }

    /// Boxed JVM wrapper for a primitive (`Int` -> `kotlin/Int`), excluding unsigned inline classes.
    pub fn jvm_boxed_ref(self) -> Option<Ty> {
        self.boxed_ref().filter(|_| !self.is_unsigned())
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
            Ty::Error => "<error>",
            Ty::Fun(_) => "Function",
            Ty::Nullable(inner) => inner.name(),
            Ty::TyParam(n, _) => n,
        }
    }

    /// Internal class name if this is an object type.
    pub fn obj_internal(self) -> Option<&'static str> {
        match self {
            Ty::Obj(n, _) => Some(n),
            // A type parameter follows its bound for object identity queries.
            Ty::TyParam(_, b) => b.obj_internal(),
            _ => None,
        }
    }

    /// True for values that can carry `null` in the language model. Any nullable type is reference-like;
    /// a type parameter follows its bound.
    pub fn is_reference(self) -> bool {
        match self {
            Ty::TyParam(_, b) => b.is_reference(),
            _ => matches!(
                self,
                Ty::String | Ty::Obj(..) | Ty::Null | Ty::Fun(_) | Ty::Nullable(_)
            ),
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double
        )
    }

    pub fn is_numeric_or_char(self) -> bool {
        self.is_numeric() || self == Ty::Char
    }

    /// True for a member/property read result that can be used as an expression value in the current
    /// lowering model. `Unit`/`Error` entries are ignored when resolving zero-arg property-like reads.
    pub fn is_read_value_result(self) -> bool {
        !matches!(self, Ty::Unit | Ty::Error)
    }

    /// True for the signed integral types whose Kotlin range overload yields `IntRange`.
    pub fn is_int_range_operand(self) -> bool {
        matches!(self, Ty::Byte | Ty::Short | Ty::Int)
    }

    /// Loop counter type for a same-typed Kotlin range bound, if krusty can lower it as counted.
    pub fn range_counter_type(self) -> Option<Ty> {
        Some(match self {
            Ty::Byte | Ty::Short => Ty::Int,
            Ty::Int | Ty::Long | Ty::UInt | Ty::ULong | Ty::Char => self,
            _ => return None,
        })
    }

    /// Kotlin range value type for `lo..hi`/`lo..<hi`, if the operand pair is supported.
    pub fn range_value_type(lo: Ty, hi: Ty) -> Option<Ty> {
        Some(match (lo, hi) {
            (Ty::Char, Ty::Char) => Ty::obj("kotlin/ranges/CharRange"),
            (Ty::UInt, Ty::UInt) => Ty::obj("kotlin/ranges/UIntRange"),
            (Ty::ULong, Ty::ULong) => Ty::obj("kotlin/ranges/ULongRange"),
            (Ty::Double, Ty::Double) | (Ty::Float, Ty::Float) => {
                Ty::obj("kotlin/ranges/ClosedFloatingPointRange")
            }
            (l, r) if l.is_int_range_operand() && r.is_int_range_operand() => {
                Ty::obj("kotlin/ranges/IntRange")
            }
            (l, r)
                if (l.is_int_range_operand() || l == Ty::Long)
                    && (r.is_int_range_operand() || r == Ty::Long) =>
            {
                Ty::obj("kotlin/ranges/LongRange")
            }
            _ => return None,
        })
    }

    /// Scalar type used while evaluating Kotlin operations that widen small integral values to `Int`.
    pub fn int_arithmetic_repr(self) -> Ty {
        match self {
            Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
            t => t,
        }
    }

    /// Whether a numeric `actual` can be assigned to this numeric target in source checking.
    pub fn accepts_numeric(self, actual: Ty) -> bool {
        match self {
            Ty::Byte | Ty::Short => matches!(actual, Ty::Int | Ty::Byte | Ty::Short),
            Ty::Long => matches!(actual, Ty::Int | Ty::Byte | Ty::Short | Ty::Char),
            Ty::Float | Ty::Double => matches!(
                actual,
                Ty::Int | Ty::Long | Ty::Byte | Ty::Short | Ty::Char | Ty::Float
            ),
            _ => false,
        }
    }

    /// True for the unsigned integer types (inline classes over a signed primitive).
    pub fn is_unsigned(self) -> bool {
        matches!(self, Ty::UInt | Ty::ULong)
    }

    /// True for Kotlin scalar values that the JVM backend carries in primitive slots.
    pub fn is_jvm_scalar(self) -> bool {
        self.scalar_value_repr().is_some()
    }

    /// The primitive representation used for built-in scalar values.
    pub fn scalar_value_repr(self) -> Option<Ty> {
        Some(match self {
            Ty::Int
            | Ty::Byte
            | Ty::Short
            | Ty::Long
            | Ty::Float
            | Ty::Double
            | Ty::Boolean
            | Ty::Char => self,
            Ty::UInt => Ty::Int,
            Ty::ULong => Ty::Long,
            _ => return None,
        })
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
            Some(r.int_arithmetic_repr())
        } else {
            None
        }
    }
}

/// Kotlin declaration visibility — the modifier on a `fun`/`val`/`class` (from source) or the
/// `@Metadata`/bytecode flags of a library declaration. `PRIVATE_TO_THIS` folds into `Private`;
/// `LOCAL` is not represented (locals are never surfaced as declarations). This records what a
/// declaration IS; whether a given call site may access it (`protected`/`internal`/`private`) is a
/// separate context-dependent decision made during resolution.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Visibility {
    #[default]
    Public,
    Internal,
    Protected,
    Private,
}

impl Visibility {
    /// The kotlin-metadata `Flags.VISIBILITY` enum value → `Visibility`. Order:
    /// INTERNAL=0, PRIVATE=1, PROTECTED=2, PUBLIC=3, PRIVATE_TO_THIS=4, LOCAL=5. Unknown/`LOCAL`
    /// conservatively map to `Private` (never wrongly widens access).
    pub fn from_metadata(v: u64) -> Visibility {
        match v {
            0 => Visibility::Internal,
            2 => Visibility::Protected,
            3 => Visibility::Public,
            _ => Visibility::Private,
        }
    }

    /// The source visibility modifier keyword → `Visibility`; no/unknown modifier is `public`
    /// (Kotlin's default). `PRIVATE_TO_THIS` is not a source keyword.
    pub fn from_modifier(m: &str) -> Visibility {
        match m {
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            "internal" => Visibility::Internal,
            _ => Visibility::Public,
        }
    }

    /// Coarse map from a legacy `is_public` bool, for synthetic/top-level callables that never carry a
    /// finer visibility (a top-level or extension can be `public`/`internal`/`private` but NEVER
    /// `protected`, so no protected information is lost here). `internal` top-levels still read back as
    /// `Private` until the finer decode reaches those arms — a deliberate interim under-approximation.
    pub fn from_public(is_public: bool) -> Visibility {
        if is_public {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }

    /// Whether this is the `public` visibility — the exact predicate the pre-context resolver used
    /// (`is_public`). Kept so the current public-only filter is expressible verbatim while the
    /// context-aware `accessible(...)` gate is introduced separately.
    pub fn is_public(self) -> bool {
        self == Visibility::Public
    }

    /// Whether this is `private` — the source `is_private` bool the parser/AST previously carried.
    pub fn is_private(self) -> bool {
        self == Visibility::Private
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ty_param_carries_name_and_bound() {
        let t = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
        assert!(t.is_ty_param());
        assert_eq!(t.ty_param_name(), Some("T"));
        assert_eq!(t.ty_param_bound(), Some(Ty::obj("kotlin/CharSequence")));
    }

    #[test]
    fn ty_param_is_reference_follows_its_bound() {
        assert!(Ty::ty_param("T", Ty::obj("kotlin/Any")).is_reference());
        // A primitive-bounded `<T : Int>` is not a reference (it specializes to the primitive).
        assert!(!Ty::ty_param("T", Ty::Int).is_reference());
    }

    #[test]
    fn non_ty_param_reports_none() {
        assert!(!Ty::Int.is_ty_param());
        assert_eq!(Ty::Int.ty_param_name(), None);
        assert_eq!(Ty::Int.ty_param_bound(), None);
    }

    #[test]
    fn kotlin_class_internal_is_source_class_identity() {
        assert_eq!(Ty::Int.kotlin_class_internal(), Some("kotlin/Int"));
        assert_eq!(Ty::String.kotlin_class_internal(), Some("kotlin/String"));
        assert_eq!(
            Ty::obj_args("demo/Box", &[Ty::Int]).kotlin_class_internal(),
            Some("demo/Box")
        );
        assert_eq!(
            Ty::nullable(Ty::UInt).kotlin_class_internal(),
            Some("kotlin/UInt")
        );
        assert_eq!(
            Ty::ty_param("T", Ty::obj("kotlin/CharSequence")).kotlin_class_internal(),
            Some("kotlin/CharSequence")
        );
        assert_eq!(Ty::Null.kotlin_class_internal(), None);
    }

    #[test]
    fn erased_recv_folds_nullability_generics_and_type_params() {
        // Nullability: `String?` and `String` resolve the same extensions.
        assert_eq!(Ty::nullable(Ty::String).erased_recv(), Ty::String);
        // Generic arguments are dropped (instantiation-independent).
        assert_eq!(
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]).erased_recv(),
            Ty::obj_args("kotlin/collections/List", &[Ty::String]).erased_recv()
        );
        assert_eq!(
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]).erased_recv(),
            Ty::obj("kotlin/collections/List")
        );
        // A type parameter keys under its (also-erased) bound.
        assert_eq!(
            Ty::ty_param("T", Ty::obj_args("kotlin/collections/List", &[Ty::Int])).erased_recv(),
            Ty::obj("kotlin/collections/List")
        );
        // Array element generics erase too, but the array-ness is kept.
        assert_eq!(
            Ty::array(Ty::obj_args("kotlin/collections/List", &[Ty::Int])).erased_recv(),
            Ty::array(Ty::obj("kotlin/collections/List"))
        );
        // Reference vs primitive and signed vs unsigned stay distinct.
        assert_ne!(Ty::Int.erased_recv(), Ty::UInt.erased_recv());
        assert_eq!(Ty::Int.erased_recv(), Ty::Int);
        // A nullable primitive boxes — distinct from the unboxed primitive, equal to a boxed element.
        assert_ne!(Ty::nullable(Ty::Int).erased_recv(), Ty::Int.erased_recv());
        assert_eq!(Ty::nullable(Ty::Int).erased_recv(), Ty::obj("kotlin/Int"));
        assert_eq!(Ty::nullable(Ty::UInt).erased_recv(), Ty::obj("kotlin/UInt"));
        // A nullable reference shares the non-null key.
        assert_eq!(Ty::nullable(Ty::String).erased_recv(), Ty::String);
        // `null`/`Nothing`/error all key under `Any` (a `null` receiver reaches an `Any?` extension).
        assert_eq!(Ty::Null.erased_recv(), Ty::obj("kotlin/Any"));
        assert_eq!(Ty::Nothing.erased_recv(), Ty::obj("kotlin/Any"));
        assert_eq!(
            Ty::nullable(Ty::obj("kotlin/Any")).erased_recv(),
            Ty::obj("kotlin/Any")
        );
    }

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
        // Unsigned boxes to its inline-class wrapper (`UInt?` → boxed `kotlin/UInt`).
        assert_eq!(Ty::UInt.nullable_boxed(), Some(Ty::nullable(Ty::UInt)));
        assert_eq!(Ty::ULong.nullable_boxed(), Some(Ty::nullable(Ty::ULong)));
        // Already a reference → not a primitive to box.
        assert_eq!(Ty::String.nullable_boxed(), None);
    }

    #[test]
    fn nullable_non_ref_keeps_source_forms() {
        assert_eq!(Ty::Unit.nullable_non_ref(), Some(Ty::nullable(Ty::Unit)));
        assert_eq!(
            Ty::Nothing.nullable_non_ref(),
            Some(Ty::nullable(Ty::Nothing))
        );
        assert_eq!(Ty::Int.nullable_non_ref(), Some(Ty::nullable(Ty::Int)));
        assert_eq!(Ty::String.nullable_non_ref(), None);
    }
}
