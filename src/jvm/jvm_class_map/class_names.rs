//! `JavaToKotlinClassMap`'s name mapping between Kotlin classifiers and JVM classes: the JVM class
//! a Kotlin classifier is on the platform (`mapKotlinToJava`), and the Kotlin classifier a JVM
//! class is read as (`mapJavaToKotlin`). Unlike erasure, a primitive maps to its box, a companion
//! with an intrinsic mapping to its `CompanionObject` class, and `kotlin.Nothing` only one way.

use super::{wrapper_to_kotlin_prim, ERASURE_GROUPS, NOTHING_JVM};
use crate::fx_hash::HashMap as FxHashMap;
use crate::jvm::names::MAX_NUMBERED_FUNCTION_ARITY;
use crate::types::{type_name, type_name_child, TypeName, KFUNCTION_INTERNAL};

/// The JVM class kotlinc's class map gives the Kotlin classifier `classifier`, or `None` when it
/// maps none.
pub(crate) fn java_class(classifier: TypeName) -> Option<TypeName> {
    if let Some(class) = class_map().kotlin_to_java.get(&classifier) {
        return Some(*class);
    }
    // A function classifier beyond the numbered runtime interfaces is the arity-erased one.
    let function = crate::jvm::function_classifiers::classifier(classifier)
        .filter(|function| function.identity() == classifier)?;
    if function.is_reflective() {
        Some(type_name(KFUNCTION_INTERNAL))
    } else if function.arity() > MAX_NUMBERED_FUNCTION_ARITY {
        Some(type_name("kotlin/jvm/functions/FunctionN"))
    } else {
        None
    }
}

/// The Kotlin classifier kotlinc's class map reads the JVM class `class` as, or `None` when it
/// maps none.
pub(crate) fn kotlin_class(class: TypeName) -> Option<TypeName> {
    class_map().java_to_kotlin.get(&class).copied()
}

struct ClassMap {
    kotlin_to_java: FxHashMap<TypeName, TypeName>,
    java_to_kotlin: FxHashMap<TypeName, TypeName>,
}

/// The pairs `JavaToKotlinClassMap`'s `init` adds, in its order: the mapped builtins and
/// collections, the primitive boxes, the intrinsic companions and the numbered function
/// interfaces.
fn class_map() -> &'static ClassMap {
    static MAP: std::sync::OnceLock<ClassMap> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = ClassMap {
            kotlin_to_java: FxHashMap::default(),
            java_to_kotlin: FxHashMap::default(),
        };
        for group in ERASURE_GROUPS {
            let java = type_name(group.jvm_name);
            for kotlin in group.kotlin_names {
                map.kotlin_to_java.insert(type_name(kotlin), java);
            }
            if let Some(kotlin) = group.kotlin_names.first() {
                if group.jvm_name != NOTHING_JVM {
                    map.java_to_kotlin.insert(java, type_name(kotlin));
                }
            }
        }
        let mut both = |kotlin: TypeName, java: TypeName| {
            map.kotlin_to_java.insert(kotlin, java);
            map.java_to_kotlin.insert(java, kotlin);
        };
        let internal = type_name("kotlin/jvm/internal");
        for wrapper in [
            "java/lang/Boolean",
            "java/lang/Character",
            "java/lang/Byte",
            "java/lang/Short",
            "java/lang/Integer",
            "java/lang/Float",
            "java/lang/Long",
            "java/lang/Double",
        ] {
            let primitive = type_name(wrapper_to_kotlin_prim(wrapper).expect("a JVM box"));
            both(primitive, type_name(wrapper));
            // `CompanionObjectMapping`: the number types, `String` and `Enum` below.
            if wrapper != "java/lang/Boolean" {
                both(
                    primitive.nested_child("Companion"),
                    type_name_child(
                        internal,
                        &format!("{}CompanionObject", primitive.segment_ref()),
                    ),
                );
            }
        }
        for owner in ["kotlin/String", "kotlin/Enum"] {
            let owner = type_name(owner);
            both(
                owner.nested_child("Companion"),
                type_name_child(internal, &format!("{}CompanionObject", owner.segment_ref())),
            );
        }
        let kotlin = type_name("kotlin");
        let functions = type_name("kotlin/jvm/functions");
        for arity in 0..=MAX_NUMBERED_FUNCTION_ARITY {
            both(
                type_name_child(kotlin, &format!("Function{arity}")),
                type_name_child(functions, &format!("Function{arity}")),
            );
        }
        map
    })
}
