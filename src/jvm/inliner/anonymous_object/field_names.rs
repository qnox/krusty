//! The field names a regenerated object's class owns. kotlinc numbers a new field after the count
//! of fields already given the same base name (`addUniqueField`), which can pick a full name the
//! class already declares: `this$0$inline_fun$1` beside a declared field of that name. Two fields of
//! one name and descriptor are not a class, so a candidate is taken only when no field of the class
//! holds it, whether the original declared it or this regeneration already allocated it.

use crate::fx_hash::HashSet;
use crate::jvm::class_node::ClassNode;

/// Every field name of one regenerated class: the original's, then each one allocated for it.
pub(super) struct FieldNames {
    occupied: HashSet<String>,
}

impl FieldNames {
    /// The names `class` declares.
    pub(super) fn of(class: &ClassNode) -> Self {
        Self {
            occupied: class
                .fields
                .iter()
                .map(|field| field.name.clone())
                .collect(),
        }
    }

    /// `base` when the class holds no field of that name, otherwise the first of `base$1`,
    /// `base$2`, … it does not hold; the name is reserved before it is returned.
    pub(super) fn allocate(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut number = 0usize;
        while self.occupied.contains(&name) {
            number += 1;
            name = format!("{base}${number}");
        }
        self.occupied.insert(name.clone());
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::class_node::FieldNode;

    fn class_with_fields(names: &[&str]) -> ClassNode {
        ClassNode {
            major: 52,
            access: 0,
            name: "Owner".to_string(),
            signature: None,
            super_name: Some("java/lang/Object".to_string()),
            interfaces: Vec::new(),
            source_file: None,
            source_debug: None,
            outer_class: None,
            inner_classes: Vec::new(),
            visible_annotations: Vec::new(),
            invisible_annotations: Vec::new(),
            deprecated: false,
            fields: names
                .iter()
                .map(|name| FieldNode {
                    access: 0x12,
                    name: (*name).to_string(),
                    desc: "LOwner;".to_string(),
                    signature: None,
                    value: None,
                    visible_annotations: Vec::new(),
                    invisible_annotations: Vec::new(),
                    deprecated: false,
                })
                .collect(),
            methods: Vec::new(),
        }
    }

    #[test]
    fn a_free_base_is_taken_as_it_is() {
        let mut names = FieldNames::of(&class_with_fields(&["this$0"]));
        assert_eq!(names.allocate("this$0$inline_fun"), "this$0$inline_fun");
        assert_eq!(names.allocate("this$0$inline_fun"), "this$0$inline_fun$1");
    }

    #[test]
    fn a_numbered_candidate_the_class_declares_is_skipped() {
        let mut names = FieldNames::of(&class_with_fields(&[
            "this$0",
            "this$0$inline_fun",
            "this$0$inline_fun$1",
        ]));
        assert_eq!(names.allocate("this$0$inline_fun"), "this$0$inline_fun$2");
        assert_eq!(names.allocate("this$0$inline_fun"), "this$0$inline_fun$3");
    }

    #[test]
    fn a_name_allocated_earlier_is_not_allocated_again() {
        let mut names = FieldNames::of(&class_with_fields(&["x$inlined"]));
        assert_eq!(names.allocate("x"), "x");
        assert_eq!(names.allocate("x$inlined"), "x$inlined$1");
        assert_eq!(names.allocate("x$inlined$1"), "x$inlined$1$1");
        assert_eq!(names.allocate("x$inlined"), "x$inlined$2");
    }
}
