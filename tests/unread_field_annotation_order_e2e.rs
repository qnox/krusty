//! kotlinc visits each backing field after the methods: its name, then its descriptor, then its
//! nullability annotation. A private field no method reads or writes interns its descriptor
//! there, ahead of the annotation, rather than at the later field-table realization.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Holder {\n\
    var label: String = \"\"\n\
\n\
    private var spare: Any? = null\n\
\n\
    private var pending: List<String>? = null\n\
}\n\
";

#[test]
fn an_unread_private_field_interns_its_descriptor_before_its_annotation() {
    common::byte_diff_against_kotlinc_cp(
        "UnreadFieldAnnotationOrder",
        SOURCE,
        "store/Holder",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/Holder differs from kotlinc: {diff}"));
}
