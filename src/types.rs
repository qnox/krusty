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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Ty {
    Int,
    Long,
    Double,
    Boolean,
    String,
    Unit,
    /// A JVM reference type identified by its internal name (e.g. `demo/Point`).
    Obj(&'static str),
    /// The type of the `null` literal — assignable to any reference type.
    Null,
    /// Placeholder after a type error, suppresses cascading diagnostics.
    Error,
}

impl Ty {
    /// A class reference type from an internal name.
    pub fn obj(internal: &str) -> Ty {
        Ty::Obj(intern(internal))
    }

    pub fn from_name(name: &str) -> Option<Ty> {
        Some(match name {
            "Int" => Ty::Int,
            "Long" => Ty::Long,
            "Double" => Ty::Double,
            "Boolean" => Ty::Boolean,
            "String" => Ty::String,
            "Unit" => Ty::Unit,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Ty::Int => "Int",
            Ty::Long => "Long",
            Ty::Double => "Double",
            Ty::Boolean => "Boolean",
            Ty::String => "String",
            Ty::Unit => "Unit",
            Ty::Obj(n) => n,
            Ty::Null => "Null",
            Ty::Error => "<error>",
        }
    }

    /// Internal name if this is a reference type.
    pub fn obj_internal(self) -> Option<&'static str> {
        match self {
            Ty::Obj(n) => Some(n),
            _ => None,
        }
    }

    /// True for JVM reference types (where `null` is a valid value).
    pub fn is_reference(self) -> bool {
        matches!(self, Ty::String | Ty::Obj(_) | Ty::Null)
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::Long | Ty::Double)
    }

    /// JVM type descriptor for ABI (`I`, `J`, `D`, `Z`, `Ljava/lang/String;`, `V`, `Lpkg/Name;`).
    pub fn descriptor(self) -> String {
        match self {
            Ty::Int => "I".into(),
            Ty::Long => "J".into(),
            Ty::Double => "D".into(),
            Ty::Boolean => "Z".into(),
            Ty::String => "Ljava/lang/String;".into(),
            Ty::Unit => "V".into(),
            Ty::Obj(n) => format!("L{n};"),
            Ty::Null => "Ljava/lang/Object;".into(),
            Ty::Error => "Ljava/lang/Object;".into(),
        }
    }

    /// Numeric promotion rank for binary arithmetic (Int < Long < Double).
    fn rank(self) -> u8 {
        match self {
            Ty::Int => 1,
            Ty::Long => 2,
            Ty::Double => 3,
            _ => 0,
        }
    }

    /// Result type of numeric promotion, or `None` if either side isn't numeric.
    pub fn promote(a: Ty, b: Ty) -> Option<Ty> {
        if a.is_numeric() && b.is_numeric() {
            Some(if a.rank() >= b.rank() { a } else { b })
        } else {
            None
        }
    }
}
