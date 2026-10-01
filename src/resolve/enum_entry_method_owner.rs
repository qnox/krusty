//! Stable classifier ownership for methods visible through an enclosing enum entry.

use crate::types::TypeName;

pub(super) fn for_entry(enum_owner: TypeName, entry: &str) -> TypeName {
    enum_owner.nested_child(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dollar_in_the_entry_name_does_not_become_an_owner_boundary() {
        let enum_owner = crate::types::type_name("enum_entry_owner_regression/Choice_1629");
        let entry_owner = for_entry(enum_owner, "ENTRY$PART_1629");

        assert!(entry_owner.matches("enum_entry_owner_regression/Choice_1629$ENTRY$PART_1629"));
        assert_eq!(entry_owner.nested_owner(), Some(enum_owner));
    }
}
