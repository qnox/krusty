//! Java result enhancement from overridden declarations.
//!
//! kotlinc enhances a Java member's signature with the nullability of the declarations it overrides
//! (`FirSignatureEnhancement.enhanceReturnType`, whose qualifiers come from
//! `AbstractSignatureParts.computeIndexedQualifiers` and `computeQualifiersForOverride`). A Java
//! result with no nullability qualifier of its own (`T!`) that overrides a result fixed not-null
//! becomes that rigid type: `StringBuilder.toString()` overriding `Any.toString(): String` returns
//! `String`, and `ArrayList<String>.iterator()` overriding `Iterable<E>.iterator(): Iterator<E>`
//! returns `MutableIterator<String>`.
//!
//! The override relation is a fact of the complete member family, so the enhancement happens here,
//! where the core hierarchy has collected that family; providers publish only the declaration's own
//! flexible result ([`ResultEnhancement::Flexible`]).

use super::{override_parameter_types_match, resolution_subtype};
use crate::libraries::{FunctionInfo, FunctionSet, ResultEnhancement};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// The head nullability qualifier one overridden declaration contributes to an override's result
/// (`AbstractSignatureParts.nullabilityQualifier`): a nullable result is `NULLABLE`, a flexible one
/// contributes none, and any other result is `NOT_NULL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NullabilityQualifier {
    NotNull,
    Nullable,
}

fn result_qualifier(declaration: &FunctionInfo) -> Option<NullabilityQualifier> {
    match declaration.ret.apply(declaration.callable.ret) {
        Ty::PlatformNullable(_) | Ty::Error | Ty::Pending => None,
        Ty::Nullable(_) => Some(NullabilityQualifier::Nullable),
        _ if matches!(
            declaration.call_sig.result_enhancement,
            ResultEnhancement::Flexible(_) | ResultEnhancement::MappedFlexible(_)
        ) =>
        {
            None
        }
        _ => Some(NullabilityQualifier::NotNull),
    }
}

/// Record the rigid result a flexible declared result (`T!`) is enhanced to, specialized for the
/// receiver like the declared result.
pub(super) fn publish_flexible_result(enhancement: &mut ResultEnhancement, rigid: Ty) {
    *enhancement = match *enhancement {
        ResultEnhancement::None => ResultEnhancement::Flexible(rigid),
        ResultEnhancement::MappedRealization => ResultEnhancement::MappedFlexible(rigid),
        other => other,
    };
}

/// Whether `inherited` is a declaration `implementation` overrides in this receiver-ranked family.
fn overrides(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    inherited: &FunctionInfo,
) -> bool {
    let formals = |function: &FunctionInfo| {
        function
            .generic_sig
            .as_ref()
            .map(|signature| signature.formals.clone())
            .unwrap_or_default()
    };
    inherited.receiver_rank > implementation.receiver_rank
        && inherited.kind == implementation.kind
        && inherited.context_count == implementation.context_count
        && inherited.flags.suspend == implementation.flags.suspend
        && override_parameter_types_match(
            source,
            &inherited.semantic_params(),
            &formals(inherited),
            &implementation.semantic_params(),
            &formals(implementation),
        )
        && resolution_subtype(
            source,
            implementation.ret.apply(implementation.callable.ret),
            inherited.ret.apply(inherited.callable.ret),
        )
}

/// Enhance each flexible Java result that an overridden declaration fixes not-null. The
/// override's own qualifier is absent (its result is flexible), so kotlinc's covariant selection
/// picks `NOT_NULL` whenever any overridden declaration supplies it.
pub(super) fn enhance_overriding_flexible_results(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
) {
    let flexible = |enhancement| match enhancement {
        ResultEnhancement::Flexible(rigid) => Some((rigid, ResultEnhancement::NotNull)),
        // The builtin declaration's own result carries no enhancement.
        ResultEnhancement::MappedFlexible(rigid) => Some((rigid, ResultEnhancement::None)),
        _ => None,
    };
    if !functions
        .overloads
        .iter()
        .any(|function| flexible(function.call_sig.result_enhancement).is_some())
    {
        return;
    }
    let declarations = functions.overloads.clone();
    for implementation in &mut functions.overloads {
        let Some((enhanced, enhancement)) = flexible(implementation.call_sig.result_enhancement)
        else {
            continue;
        };
        if !matches!(
            implementation.ret.apply(implementation.callable.ret),
            Ty::PlatformNullable(_)
        ) {
            continue;
        }
        let fixed_not_null = declarations.iter().any(|inherited| {
            overrides(source, implementation, inherited)
                && result_qualifier(inherited) == Some(NullabilityQualifier::NotNull)
        });
        if !fixed_not_null {
            continue;
        }
        crate::trace_compiler!(
            "resolve",
            "enhanced overriding result owner={:?} name={} result={enhanced:?}",
            implementation.callable.owner,
            implementation.callable.name,
        );
        implementation.callable.ret = enhanced;
        if let Some(signature) = &mut implementation.generic_sig {
            signature.ret = enhanced;
        }
        implementation.call_sig.result_enhancement = enhancement;
    }
}
