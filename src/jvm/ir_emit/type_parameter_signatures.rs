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
    jvm_type_parameter_declarations(
        formatter,
        g.type_params
            .iter()
            .map(|parameter| (parameter.name.as_str(), parameter.bounds.as_slice())),
    )
}

/// The `<T:bound…>` section for each `(source name, bounds)` declaration, or `""` for none. Each
/// bound carries whether it is an interface.
pub(super) fn jvm_type_parameter_declarations<'p>(
    formatter: &JvmSignatureFormatter<'_>,
    parameters: impl ExactSizeIterator<Item = (&'p str, &'p [(Ty, bool)])>,
) -> Option<String> {
    if parameters.len() == 0 {
        return Some(String::new());
    }
    let mut s = String::from("<");
    for (name, declared_bounds) in parameters {
        s.push_str(name);
        if declared_bounds.is_empty() {
            s.push_str(":Ljava/lang/Object;");
            continue;
        }
        // kotlinc's `writeFormalTypeParameter`: the class bound leads, whatever position the
        // declaration wrote it in, and the remaining bounds follow in written order. Kotlin
        // metadata keeps the written order itself.
        let class_bound = declared_bounds
            .iter()
            .position(|(bound, is_interface)| !*is_interface && !bound.non_null().is_ty_param());
        let bounds = class_bound
            .into_iter()
            .chain((0..declared_bounds.len()).filter(|&index| Some(index) != class_bound))
            .map(|index| &declared_bounds[index]);
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
