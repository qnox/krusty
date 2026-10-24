//! The descriptor emitter and the C runtime agree on `KType`'s layout.

use super::ktype;

/// The C declaration pins each field the two sides share, and the record's size, with a static
/// assertion; the offsets the emitter writes at must be the same numbers.
#[test]
fn descriptor_offsets_match_the_c_declaration() {
    let header = include_str!("../../../runtime/krusty_rt.h");
    let pinned = [
        ("class_names", ktype::CLASS_NAMES as usize),
        ("collection_size", ktype::COLLECTION_SIZE as usize),
        ("itable", ktype::ITABLE as usize),
    ];
    for (field, offset) in pinned {
        let assertion = format!("_Static_assert(offsetof(KType, {field}) == {offset},");
        assert!(
            header.contains(&assertion),
            "krusty_rt.h lacks `{assertion}`"
        );
    }
    let size = format!("_Static_assert(sizeof(KType) == {},", ktype::SIZE);
    assert!(header.contains(&size), "krusty_rt.h lacks `{size}`");
}
