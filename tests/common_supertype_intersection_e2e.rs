//! Sibling classes that share a class and an interface join to both, as kotlinc types them.

use super::common;

#[test]
fn a_join_of_siblings_keeps_every_most_specific_common_supertype() {
    const SOURCE: &str =
        include_str!("box_regressions/commonSupertypeOfSiblingsIsAnIntersection.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "common-supertype-intersection");
}

#[test]
fn a_join_of_siblings_without_collections_keeps_every_most_specific_common_supertype() {
    const SOURCE: &str = include_str!("box_regressions/commonSupertypeOfSiblingsScalarJoin.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "common-supertype-intersection-scalar");
}
