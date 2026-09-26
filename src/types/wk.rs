//! Well-known Kotlin declaration identities the compiler itself must name.
//!
//! Everything else about these declarations (members, their types, accessors) comes from the
//! providers; this module only fixes which classifier a compiler rule is about.

use super::{type_name, TypeName};
use std::sync::OnceLock;

macro_rules! names {
    ($($f:ident => $lit:literal),* $(,)?) => { $(
        #[inline]
        pub fn $f() -> TypeName {
            static S: OnceLock<TypeName> = OnceLock::new();
            *S.get_or_init(|| type_name($lit))
        }
    )* };
}

names! {
    kotlin_package => "kotlin",
    kotlin_coroutines_package => "kotlin/coroutines",
    kotlin_reflect_package => "kotlin/reflect",
    kotlin_ranges_package => "kotlin/ranges",
    kotlin_internal_package => "kotlin/internal",
    continuation => "kotlin/coroutines/Continuation",
    any => "kotlin/Any",
    java_object => "java/lang/Object",
    java_enum => "java/lang/Enum",
    kotlin_enum => "kotlin/Enum",
    int_range => "kotlin/ranges/IntRange",
    long_range => "kotlin/ranges/LongRange",
    char_range => "kotlin/ranges/CharRange",
    uint_range => "kotlin/ranges/UIntRange",
    ulong_range => "kotlin/ranges/ULongRange",
    int_progression => "kotlin/ranges/IntProgression",
    long_progression => "kotlin/ranges/LongProgression",
    char_progression => "kotlin/ranges/CharProgression",
    uint_progression => "kotlin/ranges/UIntProgression",
    ulong_progression => "kotlin/ranges/ULongProgression",
}

/// How kotlinc's `ForLoopsLowering` treats a `kotlin.ranges` progression class. Its constructors
/// are internal, so no other class is a subtype of one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressionClass {
    /// A `*Range`: its step is 1, so a loop over it increases and never reads `step`.
    Range,
    /// Any other progression, whose direction a loop learns from its `step` at run time.
    Progression,
}

/// Every progression class of `kotlin.ranges`.
pub fn progression_classes() -> [TypeName; 10] {
    [
        int_range(),
        long_range(),
        char_range(),
        uint_range(),
        ulong_range(),
        int_progression(),
        long_progression(),
        char_progression(),
        uint_progression(),
        ulong_progression(),
    ]
}

/// The progression class `name` identifies, if any. The element type and the `first`, `last` and
/// `step` members are the declaration's own and are never implied by this identity.
pub fn progression_class(name: TypeName) -> Option<ProgressionClass> {
    let ranges = [
        int_range(),
        long_range(),
        char_range(),
        uint_range(),
        ulong_range(),
    ];
    let progressions = [
        int_progression(),
        long_progression(),
        char_progression(),
        uint_progression(),
        ulong_progression(),
    ];
    if ranges.contains(&name) {
        Some(ProgressionClass::Range)
    } else if progressions.contains(&name) {
        Some(ProgressionClass::Progression)
    } else {
        None
    }
}

/// A `kotlin.ranges` function kotlinc's `ForLoopsLowering` reads as part of a loop's progression
/// (`ProgressionHandlers`) rather than as a call. Its receiver, parameters and result are checked
/// against the declaration by the provider that normalizes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressionBuilder {
    DownTo,
    Until,
    Step,
    Reversed,
}

/// The progression builder the top-level function `name` in `package` identifies, if any.
pub fn progression_builder(package: TypeName, name: &str) -> Option<ProgressionBuilder> {
    if package != kotlin_ranges_package() {
        return None;
    }
    match name {
        "downTo" => Some(ProgressionBuilder::DownTo),
        "until" => Some(ProgressionBuilder::Until),
        "step" => Some(ProgressionBuilder::Step),
        "reversed" => Some(ProgressionBuilder::Reversed),
        _ => None,
    }
}

/// `kotlin.internal.getProgressionLastElement`, the runtime function kotlinc calls for the last
/// element a stepped progression reaches (`getProgressionLastElementByReturnType`). Its overloads
/// come from the provider's declarations.
pub const PROGRESSION_LAST_ELEMENT: &str = "getProgressionLastElement";
