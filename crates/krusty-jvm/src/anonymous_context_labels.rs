//! kotlinc's JVM spelling of an anonymous context parameter (`context(_: Box)`).
//!
//! The same label names the parameter in a function's `LocalVariableTable`, `MethodParameters` and
//! null assertions, and — prefixed with `$` — the field a local or anonymous class stores the
//! captured receiver in. Repeated labels in one parameter list are numbered.

use krusty::types::{Ty, TyKind};

/// What separates a repeated anonymous context label from its ordinal: `$context-String$1`, or
/// `$context-String#1` ([`krusty::kotlin_version::Quirk::AnonymousContextOrdinalAfterHash`]).
fn ordinal_separator(quirks: krusty::kotlin_version::Quirks) -> char {
    if quirks.has(krusty::kotlin_version::Quirk::AnonymousContextOrdinalAfterHash) {
        '#'
    } else {
        '$'
    }
}

fn label(ty: Ty) -> String {
    let stem = {
        let unexpected = ty.non_null();
        match unexpected.kind() {
            TyKind::Obj(name, _) => name.nested_segment_ref(),
            // A type parameter is labelled after its upper bound: `context(_: T)` is `$context-Any`,
            // and with `T : CharSequence` it is `$context-CharSequence`.
            TyKind::TyParam(_, bound) => return label(bound),
            TyKind::Unit => "Unit",
            TyKind::Fun(_) => "Function",
            TyKind::Nothing => "Nothing",
            _ => panic!("anonymous context parameter has no JVM type label: {unexpected:?}"),
        }
    };
    format!("$context-{stem}")
}

/// The labels of one parameter list, `None` where a parameter is not an anonymous context
/// parameter. A label that occurs more than once is numbered by occurrence.
pub(super) fn disambiguated(
    quirks: krusty::kotlin_version::Quirks,
    anonymous: Vec<Option<Ty>>,
) -> Vec<Option<String>> {
    let bases = anonymous
        .into_iter()
        .map(|ty| ty.map(label))
        .collect::<Vec<_>>();
    let mut totals = krusty::fx_hash::HashMap::<&str, usize>::default();
    for base in bases.iter().flatten() {
        *totals.entry(base).or_default() += 1;
    }
    let mut seen = krusty::fx_hash::HashMap::<&str, usize>::default();
    bases
        .iter()
        .map(|base| {
            let base = base.as_deref()?;
            if totals[base] == 1 {
                Some(base.to_owned())
            } else {
                let ordinal = seen.entry(base).or_default();
                *ordinal += 1;
                Some(format!("{base}{}{ordinal}", ordinal_separator(quirks)))
            }
        })
        .collect()
}

/// The label of the `index`th of a callable's anonymous context parameters.
pub(super) fn label_at(
    quirks: krusty::kotlin_version::Quirks,
    anonymous: &[Ty],
    index: usize,
) -> String {
    disambiguated(quirks, anonymous.iter().copied().map(Some).collect())
        .swap_remove(index)
        .expect("every anonymous context parameter has a label")
}
