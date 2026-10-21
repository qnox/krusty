//! JVM method names of declarations whose signatures mention value classes.
//!
//! kotlinc appends a hash of the value-class-bearing signature (`base-<hash>`) and names a value
//! class's own members' static implementations `name-impl`. Every site that names such a method --
//! the declaring file's realization, a sibling file's call, a bridge or a reference -- derives the
//! spelling here from the declared signature, so the declaration and its uses cannot disagree.

use super::Under;
use crate::fx_hash::HashSet;
use crate::types::{Ty, TyKind, TypeName};

/// kotlinc's `erasedUpperBound`: the classifier a type stands for once erased. A type parameter
/// stands for whatever its bound erases to, whatever the nullability along the way.
fn erased_upper_bound(t: &Ty) -> Option<TypeName> {
    let mut current = t.non_null();
    let mut seen = HashSet::default();
    loop {
        match current.kind() {
            TyKind::Obj(fq_name, _) => return Some(fq_name),
            TyKind::TyParam(name, bound) if seen.insert(name) => current = bound.non_null(),
            _ => return None,
        }
    }
}

/// A type-parameter occurrence whose erased upper bound is a value class, as the value-class type
/// kotlinc's type mapper erases it to: the bound, made nullable when the occurrence is (marked, or
/// through a nullable bound along its chain). `T : IC?` is represented exactly as `IC?` is. Any
/// other type is returned unchanged.
pub(super) fn value_class_bound_occurrence(t: Ty, under: &Under) -> Ty {
    if !t.non_null().is_ty_param() {
        return t;
    }
    let mut bound = t.non_null();
    let mut seen = HashSet::default();
    while let TyKind::TyParam(name, next) = bound.kind() {
        if !seen.insert(name) {
            return t;
        }
        bound = next.non_null();
    }
    match bound.kind() {
        TyKind::Obj(fq_name, _) if under.contains_key(&fq_name) => {
            if t.is_nullable() || t.non_null().upper_bound_admits_null() {
                Ty::nullable(bound)
            } else {
                bound
            }
        }
        _ => t,
    }
}

/// The value classes a signature is mangled against: the value-class pass's underlying map, or,
/// once it has run, every value class the file knows, natively carried ones included.
pub(super) trait ValueClassNames {
    fn names_value_class(&self, classifier: TypeName) -> bool;
}

impl ValueClassNames for Under {
    fn names_value_class(&self, classifier: TypeName) -> bool {
        self.contains_key(&classifier)
    }
}

impl ValueClassNames for crate::ir::IrFile {
    fn names_value_class(&self, classifier: TypeName) -> bool {
        self.is_value_class_name(classifier)
    }
}

/// kotlinc's inline-class mangling info for an IR type, against `value_classes`.
///
/// kotlinc (`InlineClassAbi.asInfoForMangling`) reads a type through its erased upper bound, so
/// `T : IC?` contributes `LIC?;` exactly as `IC?` does. A type parameter is nullable when it is
/// marked so or any bound along its chain is (`IrType.isNullable`).
fn mangling_info(
    t: &Ty,
    value_classes: &(impl ValueClassNames + ?Sized),
) -> crate::jvm::inline_class::InfoForMangling {
    let is_nullable = t.is_nullable() || t.is_ty_param() && t.upper_bound_admits_null();
    let (fq_name, is_value, is_nullable) = match erased_upper_bound(t) {
        Some(fq_name) => (
            fq_name.render(),
            value_classes.names_value_class(fq_name),
            is_nullable,
        ),
        None => (String::new(), false, false),
    };
    crate::jvm::inline_class::InfoForMangling {
        is_value,
        // kotlinc hashes the declared Kotlin FqName (`pkg.Outer.Inner` — dots throughout), never the
        // JVM internal spelling, so a NESTED value class converts its `$` separator too: `I$V` must
        // hash as `I.V` or every member mentioning it gets a different `-<hash>` than kotlinc's.
        fq_name: fq_name.replace(['/', '$'], "."),
        is_nullable,
    }
}

/// The JVM name of a suspend lambda class's typed `invoke`, over the lambda's declared parameters
/// (context parameters and receiver first) in order. kotlinc mangles it like any function taking a
/// value class, its continuation contributing the trailing `_`. Its result, returned through that
/// continuation, is `Any?`, so the name never carries a result hash: `suspend (IC) -> IC` and
/// `suspend (IC) -> Unit` both take `invoke-hHQ9REg`.
pub(crate) fn suspend_lambda_invoke_name(ir: &crate::ir::IrFile, parameters: &[Ty]) -> String {
    vc_mangle(
        "invoke",
        parameters,
        &Ty::nullable(Ty::obj("kotlin/Any")),
        ir,
        false,
        true,
    )
}

/// [`vc_mangle`] that leaves an ALREADY-mangled name alone: if `base` is exactly what this signature
/// would produce from its own stem, it is returned unchanged. The mangle appends a fixed-width
/// suffix, `-` plus seven base64url characters, so the stem is everything before the last eight
/// characters. The hash itself may contain `-` (`getS-C-fiWsc`, `memberFun--ndakOA`), so splitting
/// at the last `-` would take a piece of the hash for the stem and mangle the name a second time.
pub(super) fn vc_mangle_once(
    base: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_file_class: bool,
    is_suspend: bool,
) -> String {
    let stem = base
        .len()
        .checked_sub(crate::jvm::inline_class::MANGLE_SUFFIX_LEN)
        .filter(|&at| base.is_char_boundary(at) && base[at..].starts_with('-'))
        .map(|at| &base[..at]);
    if let Some(stem) = stem {
        if vc_mangle(stem, params, ret, under, is_file_class, is_suspend) == base {
            return base.to_string();
        }
    }
    vc_mangle(base, params, ret, under, is_file_class, is_suspend)
}

/// JVM name of a value-class member realized as a static implementation over its carrier. A
/// signature that independently requires Kotlin's value-class hash keeps that hash
/// (`same-iUtXLc0`); otherwise kotlinc uses the structural `-impl` suffix. The declaring file's
/// realization and a sibling file's call site both derive the name here, from the same declared
/// signature, so the two cannot disagree.
pub(super) fn vc_member_impl_name(
    source_name: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_suspend: bool,
) -> String {
    let mangled = vc_member_entry_name(source_name, params, ret, under, is_suspend);
    if mangled == source_name {
        format!("{source_name}-impl")
    } else {
        mangled
    }
}

/// The name a value-class member answers to on the box: its declared name, mangled when its
/// signature mentions a value class (`f-<hash>`). Its static implementation takes this name too,
/// or `name-impl` when there is nothing to mangle.
pub(super) fn vc_member_entry_name(
    source_name: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_suspend: bool,
) -> String {
    vc_mangle(source_name, params, ret, under, false, is_suspend)
}

/// JVM name and value-class-bound parameter replacements of a same-module call. Mangling sees the
/// class bound (`UInt`), which is the value class the hash includes. Parameters whose class bound
/// is not a value class retain the already-selected physical call shape; this boundary must not
/// remap unrelated generic, function, `Unit`, or reference parameters.
pub(super) fn module_call_jvm_name(
    name: &str,
    parameters: &[Ty],
    type_parameters: &[crate::ir::IrCallableTypeParameter],
    result: &Ty,
    owner_is_value_class_member: bool,
    owner_is_file: bool,
    module_default_call: bool,
    semantic_default: bool,
    under: &Under,
    suspend: bool,
) -> (String, Vec<(usize, Ty)>) {
    let base = if module_default_call {
        name.strip_suffix("$default").unwrap_or(name)
    } else {
        name
    };
    let erased =
        crate::jvm::generic_erasure::erased_callable_parameters(parameters, type_parameters);
    let replacements = parameters
        .iter()
        .copied()
        .zip(erased)
        .enumerate()
        .filter_map(|(index, (semantic, erased))| {
            let classifier = erased.non_null().kotlin_class_internal()?;
            semantic
                .non_null()
                .is_ty_param()
                .then_some(())
                .filter(|_| under.contains_key(&classifier))
                .map(|_| (index, erased))
        })
        .collect::<Vec<_>>();
    let mut mangling_parameters = parameters.to_vec();
    for &(index, replacement) in &replacements {
        mangling_parameters[index] = replacement;
    }
    let mangled = if owner_is_value_class_member {
        vc_member_impl_name(base, &mangling_parameters, result, under, suspend)
    } else {
        vc_mangle_once(
            base,
            &mangling_parameters,
            result,
            under,
            owner_is_file,
            suspend,
        )
    };
    let name = if module_default_call && !semantic_default {
        format!("{mangled}$default")
    } else {
        mangled
    };
    (name, replacements)
}

/// kotlinc's name for a function whose JVM signature mentions a value class: `base-<hash>` (a
/// value-class parameter, or a value-class return, triggers it). Plain `base` otherwise.
pub(super) fn vc_mangle(
    base: &str,
    params: &[Ty],
    ret: &Ty,
    under: &(impl ValueClassNames + ?Sized),
    is_file_class: bool,
    is_suspend: bool,
) -> String {
    let mut pinfo: Vec<_> = params.iter().map(|t| mangling_info(t, under)).collect();
    let requires_param_mangling = params.iter().any(|t| requires_mangling(t, under));
    // kotlinc mangles the ORIGINAL (pre-CPS) signature, which for a suspend fun includes the trailing
    // `Continuation` value parameter — a non-inline type, so it contributes the `_` placeholder. Without
    // it a suspend `f(Id): Int` would hash identically to the non-suspend overload. (A lone non-value
    // `_` never triggers mangling on its own — `requires_param_mangling` checks `is_value`.)
    if is_suspend {
        pinfo.push(crate::jvm::inline_class::InfoForMangling {
            fq_name: String::new(),
            is_value: false,
            is_nullable: false,
        });
    }
    // RETURN mangling (kotlinc `hasMangledReturnType`) does NOT exempt `Result`, but applies only when the
    // function is NOT in a file class (a top-level fn returning a value class keeps its plain name).
    let rinfo = mangling_info(ret, under);
    let ret_opt = (rinfo.is_value && !is_file_class).then_some(&rinfo);
    crate::jvm::inline_class::mangled_name(base, &pinfo, requires_param_mangling, ret_opt)
}

/// kotlinc's `IrType.getRequiresMangling` for a value parameter: its erased upper bound is a value
/// class other than `kotlin.Result` (`!isClassWithFqName(RESULT_FQ_NAME)`). The exemption decides
/// only whether the parameter TRIGGERS a mangle; once another parameter or the return does, a
/// `Result` parameter is spelled `Lkotlin.Result;` in the hashed signature like any value class.
/// The same predicate hides an ordinary class's constructor behind its `DefaultConstructorMarker`
/// accessor (`hasMangledParameters`).
pub(super) fn requires_mangling(t: &Ty, value_classes: &(impl ValueClassNames + ?Sized)) -> bool {
    erased_upper_bound(t).is_some_and(|classifier| {
        classifier != crate::types::type_name("kotlin/Result")
            && value_classes.names_value_class(classifier)
    })
}

/// Whether kotlinc mangles a property accessor over a property of type `t`: the property's type,
/// read through its erased upper bound, is a value class. A type parameter bounded by a value class
/// (`val z: T` with `T : Z?`) mangles its accessors exactly as the bound itself would.
pub(super) fn accessor_type_is_value_class(t: &Ty, under: &Under) -> bool {
    mangling_info(t, under).is_value
}

#[cfg(test)]
mod tests {
    use super::{module_call_jvm_name, Under};
    use crate::ir::IrCallableTypeParameter;
    use crate::types::{type_name, Ty};

    #[test]
    fn module_call_replaces_only_value_class_bounded_type_parameters() {
        let mut under = Under::default();
        under.insert(type_name("kotlin/UInt"), Ty::Int);
        let parameters = [
            Ty::ty_param("U", Ty::UInt),
            Ty::Unit,
            Ty::ty_param("T", Ty::obj("kotlin/Any")),
            Ty::fun(vec![Ty::String], Ty::Int),
        ];
        let type_parameters = [
            IrCallableTypeParameter {
                semantic_name: "U".to_string(),
                bounds: Box::new([(Ty::UInt, false)]),
            },
            IrCallableTypeParameter {
                semantic_name: "T".to_string(),
                bounds: Box::new([(Ty::obj("kotlin/Any"), false)]),
            },
        ];

        let (_, replacements) = module_call_jvm_name(
            "accept",
            &parameters,
            &type_parameters,
            &Ty::Unit,
            false,
            true,
            false,
            false,
            &under,
            false,
        );

        assert_eq!(replacements, vec![(0, Ty::UInt)]);
    }
}
