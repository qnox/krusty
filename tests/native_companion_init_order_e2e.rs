//! Which companion objects a Native program initializes, and in what order, when a class is
//! constructed or its companion is reached. Native has no class initialization of its own, so the
//! order is the code generator's: with `CompanionBlocks` it is the JVM's (superclass chain and
//! default-member superinterfaces first, each with its companion); without it only the companion
//! a construction or an access names is initialized, subclass first.
// PLATFORM-SPECIFIC: the order without `CompanionBlocks` is kotlinc-native's own, pinned by the
// upstream case `nativeCompanionInitOrderLegacy`; the JVM does not share it.

use krusty::kotlin_version::{target, KotlinVersion};

use super::common;

const SUPERTYPES_FIRST: &str =
    include_str!("box_regressions/companionObjectInitOrderSupertypesFirst.kt");

const OWN_COMPANION_ONLY: &str = "\
// LANGUAGE: -CompanionBlocks -CompanionExtensions
var log = \"\"

fun note(step: String) {
    log += step
}

open class Root {
    init { note(\"r\") }
    companion object { init { note(\"R\") } }
}

open class Middle : Root()

class Leaf : Middle() {
    init { note(\"l\") }
    companion object { init { note(\"L\") } }
}

fun box(): String {
    log = \"\"
    Leaf
    if (log != \"L\") return \"companion access: $log\"

    log = \"\"
    Leaf()
    if (log != \"Rrl\") return \"construction: $log\"
    return \"OK\"
}
";

/// `CompanionBlocks` is a feature of the 2.5 release; the releases before it spell it with the
/// extensions and initialize as the legacy order does.
fn declares_companion_blocks() -> bool {
    target() >= KotlinVersion::V2_5_0_RC
}

#[test]
fn companion_blocks_initialize_supertypes_before_a_companion_and_a_construction() {
    if !declares_companion_blocks() {
        return;
    }
    common::expect_box_ok_with_stdlib(SUPERTYPES_FIRST, "NativeCompanionSupertypesFirst");
    common::expect_native_box(SUPERTYPES_FIRST, "NativeCompanionSupertypesFirst", "OK");
}

#[test]
fn without_companion_blocks_a_native_companion_initializes_only_itself() {
    if !declares_companion_blocks() {
        return;
    }
    common::expect_native_box(OWN_COMPANION_ONLY, "NativeCompanionOwnOnly", "OK");
}
