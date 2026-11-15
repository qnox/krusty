//! The class names kotlinc-wasm gives function values, which `kotlin.Any.toString` prints.
//!
//! kotlinc-wasm builds a lambda or a callable reference as an instance of a class named for its
//! shape, not for its source. The name is the function type's family and arity, then `R` for a
//! reference, then `_bound`, the count of captured values and one code per captured value when it
//! captures any: `Function0`, `SuspendFunction1_bound1_I`, `KFunction1R`,
//! `KFunction0R_bound1_A`. `toString` prints that simple name, `@` and the identity hash, as for
//! any class (`super::runtime::object_strings`). The names were measured with kotlinc-wasm 2.4.21,
//! wasm-js and wasm-wasi alike: `wasm_function_values_print_their_class_names` in
//! `tests/kotlin_box_wasm_conformance.rs` pins them.
//!
//! A value whose class kotlinc names otherwise is not given a name here: a lambda converted to a
//! `fun interface`, and a reference to a property, which prints the property instead.

use krusty::ir::{IrCallableReferenceTarget, IrExpr};
use krusty::types::{FnSig, Ty, TyKind};

/// What a function value's class name is derived from, apart from its captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NamedShape {
    /// A callable reference, whose family is `KFunction`; otherwise a lambda's `Function`.
    pub(super) reference: bool,
    /// A `suspend` function type, whose family carries the `Suspend` prefix.
    pub(super) suspend: bool,
    /// The function type's parameters, a receiver and context parameters included.
    pub(super) arity: usize,
}

/// The shape kotlinc-wasm names the function value `expression` creates, a lambda of the checked
/// type `instance` or a callable reference, or `None` for a value it names otherwise.
pub(super) fn named_shape(expression: &IrExpr, instance: Option<Ty>) -> Option<NamedShape> {
    let (reference, function_type) = match expression {
        IrExpr::Lambda { sam: None, .. } => (false, instance?),
        IrExpr::CallableReference(reference)
            if !matches!(reference.target, IrCallableReferenceTarget::Property(_)) =>
        {
            (true, reference.function_type)
        }
        _ => return None,
    };
    let signature = function_signature(function_type)?;
    Some(NamedShape {
        reference,
        suspend: signature.suspend,
        arity: signature.params.len(),
    })
}

fn function_signature(ty: Ty) -> Option<&'static FnSig> {
    match ty.non_null().kind() {
        TyKind::Fun(signature) => Some(signature),
        _ => None,
    }
}

/// How kotlinc-wasm encodes one captured value in a class name: by its declared type, a primitive
/// or an unsigned number by its own code and anything else, a shared `var` cell included, as a
/// reference.
pub(super) fn capture_code(ty: Ty, shared: bool) -> &'static str {
    if shared {
        return "A";
    }
    match ty.kind() {
        TyKind::INT => "I",
        TyKind::LONG => "J",
        TyKind::DOUBLE => "D",
        TyKind::FLOAT => "F",
        TyKind::BOOLEAN => "Z",
        TyKind::CHAR => "C",
        TyKind::SHORT => "S",
        TyKind::BYTE => "B",
        TyKind::UBYTE => "UB",
        TyKind::USHORT => "US",
        TyKind::UINT => "UI",
        TyKind::ULONG => "UJ",
        _ => "A",
    }
}

/// The simple class name of a function value of `shape` capturing values encoded as `captures`.
pub(super) fn class_name(shape: NamedShape, captures: &[&str]) -> String {
    let mut name = String::new();
    if shape.reference {
        name.push('K');
    }
    if shape.suspend {
        name.push_str("Suspend");
    }
    name.push_str("Function");
    name.push_str(&shape.arity.to_string());
    if shape.reference {
        name.push('R');
    }
    if !captures.is_empty() {
        name.push_str("_bound");
        name.push_str(&captures.len().to_string());
        name.push('_');
        name.extend(captures.iter().copied());
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(reference: bool, suspend: bool, arity: usize) -> NamedShape {
        NamedShape {
            reference,
            suspend,
            arity,
        }
    }

    /// kotlinc-wasm 2.4.21's names for these shapes, measured on wasm-js and wasm-wasi.
    #[test]
    fn names_follow_kotlinc_wasm() {
        assert_eq!(class_name(shape(false, false, 0), &[]), "Function0");
        assert_eq!(
            class_name(shape(false, false, 2), &["I"]),
            "Function2_bound1_I"
        );
        assert_eq!(
            class_name(shape(false, false, 0), &["I", "J", "A"]),
            "Function0_bound3_IJA"
        );
        assert_eq!(
            class_name(shape(false, true, 1), &["I"]),
            "SuspendFunction1_bound1_I"
        );
        assert_eq!(class_name(shape(true, false, 1), &[]), "KFunction1R");
        assert_eq!(
            class_name(shape(true, false, 0), &["A"]),
            "KFunction0R_bound1_A"
        );
        assert_eq!(class_name(shape(true, true, 0), &[]), "KSuspendFunction0R");
    }

    #[test]
    fn captures_are_coded_by_their_declared_type() {
        let codes = [
            Ty::Int,
            Ty::Long,
            Ty::Double,
            Ty::Float,
            Ty::Boolean,
            Ty::Char,
            Ty::Short,
            Ty::Byte,
            Ty::UByte,
            Ty::UShort,
            Ty::UInt,
            Ty::ULong,
            Ty::String,
        ]
        .map(|ty| capture_code(ty, false));
        assert_eq!(
            codes,
            ["I", "J", "D", "F", "Z", "C", "S", "B", "UB", "US", "UI", "UJ", "A"]
        );
        assert_eq!(capture_code(Ty::Int, true), "A");
        assert_eq!(capture_code(Ty::nullable(Ty::Int), false), "A");
    }
}
