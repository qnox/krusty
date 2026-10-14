//! Physical names of an enum's generated `$VALUES` and `$ENTRIES` storage.
//!
//! A source companion property owns its declared spelling. When it uses one of these generated
//! names, kotlinc moves the synthetic field to the first unused `$N` spelling instead of renaming
//! the source declaration. Keeping that allocation here gives every enum emission site one answer.

use crate::ir::{IrClass, IrFile};

pub(super) struct EnumSyntheticFields {
    pub(super) values: String,
    pub(super) entries: String,
}

pub(super) fn names(ir: &IrFile, class: &IrClass) -> EnumSyntheticFields {
    let occupied = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(_, field)| field.owner == Some(class.fq_name))
        .map(|(index, _)| ir.static_field_jvm_name(index as u32))
        .collect::<Vec<_>>();
    EnumSyntheticFields {
        values: allocate("$VALUES", &occupied),
        entries: allocate("$ENTRIES", &occupied),
    }
}

fn allocate(base: &str, occupied: &[&str]) -> String {
    if !occupied.contains(&base) {
        return base.to_string();
    }
    (1usize..)
        .map(|suffix| format!("{base}${suffix}"))
        .find(|candidate| !occupied.contains(&candidate.as_str()))
        .expect("an unused synthetic enum field suffix always exists")
}

#[cfg(test)]
mod tests {
    use super::allocate;

    #[test]
    fn skips_source_owned_enum_storage_spellings() {
        assert_eq!(allocate("$VALUES", &["$VALUES", "$VALUES$1"]), "$VALUES$2");
        assert_eq!(allocate("$ENTRIES", &[]), "$ENTRIES");
    }
}
