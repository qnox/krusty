//! v0 type model: the primitive/String/Unit set, no generics or nullability yet.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    Int,
    Long,
    Double,
    Boolean,
    String,
    Unit,
    /// Placeholder after a type error, suppresses cascading diagnostics.
    Error,
}

impl Ty {
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
            Ty::Error => "<error>",
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::Long | Ty::Double)
    }

    /// JVM type descriptor for ABI (`I`, `J`, `D`, `Z`, `Ljava/lang/String;`, `V`).
    pub fn descriptor(self) -> &'static str {
        match self {
            Ty::Int => "I",
            Ty::Long => "J",
            Ty::Double => "D",
            Ty::Boolean => "Z",
            Ty::String => "Ljava/lang/String;",
            Ty::Unit => "V",
            Ty::Error => "Ljava/lang/Object;",
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
