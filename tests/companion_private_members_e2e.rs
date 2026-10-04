//! Private members of a companion object, used from the class that contains it.
//!
//! Kotlin's private visibility is lexical: a declaration that is private inside a companion object
//! is visible everywhere in the companion's containing class, so a private class nested in a private
//! companion can be constructed and read from the outer class's own methods. A private or protected
//! companion also keeps that visibility on the outer class's `Companion` field.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! class file is compared byte for byte.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// The reported shape: a private data class inside a private companion, built and updated from a
/// method of the containing class.
const PRIVATE_NESTED_IN_PRIVATE_COMPANION: &str = r#"
class Raster {
    fun scan(): Float {
        val e = Edge(1f, 0.5f, 3, -1)
        e.x += e.invSlope
        return e.x + e.zMax + e.winding
    }
    private companion object {
        private data class Edge(var x: Float, val invSlope: Float, val zMax: Int, val winding: Int)
    }
}
"#;

#[test]
fn a_private_class_of_a_private_companion_is_usable_from_the_containing_class() {
    for class in ["Raster", "Raster$Companion", "Raster$Companion$Edge"] {
        byte_identical(
            "private_nested_in_private_companion",
            PRIVATE_NESTED_IN_PRIVATE_COMPANION,
            class,
        );
    }
}

const COMPANION_VISIBILITIES: &str = r#"
class PrivateHolder {
    fun read() = twice(2)
    private companion object {
        private fun twice(value: Int) = value * 2
    }
}
open class ProtectedHolder {
    protected companion object
}
"#;

#[test]
fn the_companion_field_keeps_the_companion_visibility() {
    for class in ["PrivateHolder", "ProtectedHolder"] {
        byte_identical("companion_visibilities", COMPANION_VISIBILITIES, class);
    }
}
