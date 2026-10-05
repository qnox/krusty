//! The type-parameter DECLARATION section of a JVM generic `Signature` (`<T:Ljava/lang/Object;>`)
//! and the class `Signature` built on it. Methods and classes share one formatter for the section,
//! so a bound is written the same way wherever its type parameter is declared.

use super::signature_formatter::{JvmSignatureFormatter, Wildcards};
use crate::jvm::names::type_descriptor;
use crate::types::Ty;

/// Format a class's generic shape into a JVM class `Signature` (`<T:Ljava/lang/Object;>Ljava/lang/Object;`).
pub(super) fn jvm_class_signature(
    formatter: &JvmSignatureFormatter<'_>,
    g: &crate::ir::IrGenericSig,
) -> Option<String> {
    let mut s = jvm_type_params(formatter, g)?;
    if g.supers.is_empty() {
        // A plain generic class with no (parameterized) supertypes: just extends `Object`.
        s.push_str("Ljava/lang/Object;");
    } else {
        // The parameterized superclass + interfaces (`Ljava/lang/Object;LOperation<Lkotlin/Result<..>;>;`),
        // formatted from the platform-agnostic `Ty`s so a reader recovers a member's concrete generic
        // return. A class header is not a method-parameter position: declaration-site variance is
        // not written on its own arguments (`interface L<E> : List<E>` implements Java `List<E>`).
        // An explicit source projection remains encoded by `ty_at` itself.
        for sup in &g.supers {
            s.push_str(&formatter.supertype(sup)?);
        }
    }
    Some(s).filter(|signature| signature.contains('<'))
}

/// The shared `<T:bound…>` type-parameter DECLARATION section, or `""` when there are no own type
/// parameters (e.g. a generic class's getter `getA()` → `()TA;` USES the class's `A` but declares none).
/// `None` if any bound can't be represented.
pub(super) fn jvm_type_params(
    formatter: &JvmSignatureFormatter<'_>,
    g: &crate::ir::IrGenericSig,
) -> Option<String> {
    if g.type_params.is_empty() {
        return Some(String::new());
    }
    let mut s = String::from("<");
    for parameter in &g.type_params {
        s.push_str(&parameter.name);
        if parameter.bounds.is_empty() {
            s.push_str(":Ljava/lang/Object;");
            continue;
        }
        let bounds = parameter.bounds.iter();
        if bounds.clone().all(|(_, is_interface)| *is_interface) {
            s.push(':');
        }
        for (bound, _) in bounds {
            s.push(':');
            s.push_str(&jvm_bound_descriptor(formatter, bound)?);
        }
    }
    s.push('>');
    Some(s)
}

/// A type-parameter upper bound as a JVM signature element: `kotlin/Any` → `Ljava/lang/Object;`, a
/// primitive → its boxed wrapper (`kotlin/Int` → `Ljava/lang/Integer;`), and anything else in
/// kotlinc's generic-argument mode, which writes every declaration-site wildcard.
fn jvm_bound_descriptor(formatter: &JvmSignatureFormatter<'_>, bound: &Ty) -> Option<String> {
    if *bound == Ty::obj("kotlin/Any") {
        return Some("Ljava/lang/Object;".to_string());
    }
    if bound.is_jvm_scalar() {
        return bound.nullable_boxed().map(type_descriptor);
    }
    formatter.ty_at(bound, Wildcards::Generic)
}
