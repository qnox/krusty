//! kotlinc's JVM spelling of an anonymous context parameter (`context(_: Box)`).
//!
//! The same label names the parameter in a function's `LocalVariableTable`, `MethodParameters` and
//! null assertions, and — prefixed with `$` — the field a local or anonymous class stores the
//! captured receiver in. Repeated labels in one parameter list are numbered.

use crate::types::Ty;

/// What separates a repeated anonymous context label from its ordinal: `$context-String$1` since
/// Kotlin 2.4.20, `$context-String#1` before it.
fn ordinal_separator() -> char {
    if crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20) {
        '$'
    } else {
        '#'
    }
}

fn label(ty: Ty) -> String {
    let stem = match ty.non_null() {
        Ty::Obj(name, _) => name.nested_segment_ref(),
        // A type parameter is labelled after its upper bound: `context(_: T)` is `$context-Any`,
        // and with `T : CharSequence` it is `$context-CharSequence`.
        Ty::TyParam(_, bound) => return label(*bound),
        Ty::Unit => "Unit",
        Ty::Fun(_) => "Function",
        Ty::Nothing => "Nothing",
        unexpected => panic!("anonymous context parameter has no JVM type label: {unexpected:?}"),
    };
    format!("$context-{stem}")
}

/// The labels of one parameter list, `None` where a parameter is not an anonymous context
/// parameter. A label that occurs more than once is numbered by occurrence.
pub(super) fn disambiguated(anonymous: Vec<Option<Ty>>) -> Vec<Option<String>> {
    let bases = anonymous
        .into_iter()
        .map(|ty| ty.map(label))
        .collect::<Vec<_>>();
    let mut totals = std::collections::HashMap::<&str, usize>::new();
    for base in bases.iter().flatten() {
        *totals.entry(base).or_default() += 1;
    }
    let mut seen = std::collections::HashMap::<&str, usize>::new();
    bases
        .iter()
        .map(|base| {
            let base = base.as_deref()?;
            if totals[base] == 1 {
                Some(base.to_owned())
            } else {
                let ordinal = seen.entry(base).or_default();
                *ordinal += 1;
                Some(format!("{base}{}{ordinal}", ordinal_separator()))
            }
        })
        .collect()
}

/// The label of the `index`th of a callable's anonymous context parameters.
pub(super) fn label_at(anonymous: &[Ty], index: usize) -> String {
    disambiguated(anonymous.iter().copied().map(Some).collect())
        .swap_remove(index)
        .expect("every anonymous context parameter has a label")
}
