//! JVM method names of declarations whose signatures mention value classes.
//!
//! kotlinc appends a hash of the value-class-bearing signature (`base-<hash>`) and names a value
//! class's own members' static implementations `name-impl`. Every site that names such a method --
//! the declaring file's realization, a sibling file's call, a bridge or a reference -- derives the
//! spelling here from the declared signature, so the declaration and its uses cannot disagree.

use super::Under;
use crate::types::{Ty, TypeName};
use std::collections::HashSet;

/// kotlinc's `erasedUpperBound`: the classifier a type stands for once erased. A type parameter
/// stands for whatever its bound erases to, whatever the nullability along the way.
fn erased_upper_bound(t: &Ty) -> Option<TypeName> {
    let mut current = t.non_null();
    let mut seen = HashSet::new();
    loop {
        match current {
            Ty::Obj(fq_name, _) => return Some(fq_name),
            Ty::TyParam(name, bound) if seen.insert(name) => current = bound.non_null(),
            _ => return None,
        }
    }
}

/// A type-parameter occurrence whose erased upper bound is a value class, as the value-class type
/// kotlinc's type mapper erases it to: the bound, made nullable when the occurrence is (marked, or
/// through a nullable bound along its chain). `T : IC?` is represented exactly as `IC?` is. Any
/// other type is returned unchanged.
pub(super) fn value_class_bound_occurrence(t: Ty, under: &Under) -> Ty {
    if !matches!(t.non_null(), Ty::TyParam(..)) {
        return t;
    }
    let mut bound = t.non_null();
    let mut seen = HashSet::new();
    while let Ty::TyParam(name, next) = bound {
        if !seen.insert(name) {
            return t;
        }
        bound = next.non_null();
    }
    match bound {
        Ty::Obj(fq_name, _) if under.contains_key(&fq_name) => {
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
/// once it has run, the value classes the file names callables by, natively carried ones included.
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
        self.names_callable_value_class(classifier)
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
    let is_nullable =
        t.is_nullable() || matches!(t, Ty::TyParam(..)) && t.upper_bound_admits_null();
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
