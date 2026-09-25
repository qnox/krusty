//! JVM method names of declarations whose signatures mention value classes.
//!
//! kotlinc appends a hash of the value-class-bearing signature (`base-<hash>`) and names a value
//! class's own members' static implementations `name-impl`. Every site that names such a method --
//! the declaring file's realization, a sibling file's call, a bridge or a reference -- derives the
//! spelling here from the declared signature, so the declaration and its uses cannot disagree.

use super::Under;
use crate::types::Ty;

/// kotlinc's inline-class mangling info for an IR type, against the value classes in `under`.
fn mangling_info(t: &Ty, under: &Under) -> crate::jvm::inline_class::InfoForMangling {
    let (fq_name, is_value, is_nullable) = match t.non_null().obj_internal() {
        Some(fq_name) => (
            fq_name.render(),
            under.contains_key(&fq_name),
            t.is_nullable(),
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

/// [`vc_mangle`] that leaves an ALREADY-mangled name alone: if `base` is exactly what this signature
/// would produce from its own stem, it is returned unchanged. A JVM method name a Kotlin declaration
/// produces never contains `-` unless kotlinc's value-class mangle put it there, so splitting at the
/// last `-` and re-mangling the stem is an exact test for "this name is already the answer".
pub(super) fn vc_mangle_once(
    base: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_file_class: bool,
    is_suspend: bool,
) -> String {
    if let Some((stem, _)) = base.rsplit_once('-') {
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
    let mangled = vc_mangle(source_name, params, ret, under, false, is_suspend);
    if mangled == source_name {
        format!("{source_name}-impl")
    } else {
        mangled
    }
}

/// kotlinc's name for a function whose JVM signature mentions a value class: `base-<hash>` (a
/// value-class parameter, or a value-class return, triggers it). Plain `base` otherwise.
pub(super) fn vc_mangle(
    base: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_file_class: bool,
    is_suspend: bool,
) -> String {
    // PARAM mangling (kotlinc `IrType.getRequiresMangling`) EXEMPTS `kotlin.Result`
    // (`!isClassWithFqName(RESULT_FQ_NAME)`), so a `Result` parameter never triggers a mangle.
    let mut pinfo: Vec<_> = params
        .iter()
        .map(|t| {
            let mut info = mangling_info(t, under);
            if info.fq_name == "kotlin.Result" {
                info.is_value = false;
            }
            info
        })
        .collect();
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
    crate::jvm::inline_class::mangled_name(base, &pinfo, ret_opt)
}
