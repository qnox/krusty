//! `m["k"] = v` was rejected as "not an array (cannot index-assign)" when the map's type came from
//! Java and the assigned value was NULLABLE.
//!
//! Nothing was wrong with the store itself: the receiver's own type argument was being thrown away
//! while walking to the member's declaring supertype. Projecting `HashMap<String, Any?>` onto its
//! `Map<K, V>` template re-narrowed each argument against the formal's upper bound, and a JAVA
//! class's type parameters carry a NON-NULL bound (a Java type variable has no nullability of its
//! own). So `V` bound to `Any`, not `Any?`, the `MutableMap<K, V>.set` extension reached through
//! that supertype no longer accepted a `Long?`, no `set`/`put` was applicable, and the failure
//! surfaced as the confusing "not an array".
//!
//! The arguments were already validated against those bounds when the receiver type was FORMED, so
//! re-narrowing during a supertype projection can only lose information — which is exactly what
//! `ty_subst_applied_arguments` exists to avoid, and what the hierarchy walk now uses.
//!
//! The axis is the VALUE: the same receiver with a non-null value compiled, and a pure-Kotlin
//! `MutableMap<String, Any?>` compiled with a nullable one (its formals' bounds are `Any?`, so
//! nothing narrowed). It takes a Java-declared classifier to surface, which is why every
//! `MutableMap` fixture missed it.
//!
//! STILL BROKEN, separate root cause: a Java class that EXTENDS a generic Java class
//! (`class Doc extends LinkedHashMap<String, Object>`). There the argument comes from the Java
//! supertype signature rather than from the use site, and it is read as a plain non-null `Any`
//! instead of the platform `Any!` it should be — so a nullable value is still rejected. That is a
//! fix in the Java signature reader, not here.
use super::common;

#[test]
fn nullable_value_index_assign_on_a_platform_map() {
    let src = "fun box(): String {\n\
        \x20   val m = java.util.HashMap<String, Any?>()\n\
        \x20   val absent: Long? = null\n\
        \x20   m[\"a\"] = absent\n\
        \x20   m[\"b\"] = 7L\n\
        \x20   if (m.size != 2) return \"size:${m.size}\"\n\
        \x20   if (m[\"a\"] != null) return \"a:${m[\"a\"]}\"\n\
        \x20   if (m[\"b\"] != 7L) return \"b:${m[\"b\"]}\"\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main")
            .expect("a nullable value stores through a platform map's indexed set"),
        "OK"
    );
}

/// The non-null value on the same receiver always worked — keep it executable so a fix that widens
/// too eagerly, or narrows the receiver channel, is still caught here.
#[test]
fn non_null_value_index_assign_on_a_platform_map() {
    let src = "fun box(): String {\n\
        \x20   val m = java.util.HashMap<String, Any?>()\n\
        \x20   m[\"a\"] = 1\n\
        \x20   return if (m[\"a\"] == 1) \"OK\" else \"FAIL:${m[\"a\"]}\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main").expect("non-null indexed set still works"),
        "OK"
    );
}

/// The receiver's type argument must survive the projection for READS too, not only the indexed
/// store that surfaced it: `m["k"]` through the Java-declared `Map` supertype yields `Any?`.
#[test]
fn nullable_value_type_survives_the_supertype_projection() {
    let src = "fun box(): String {\n\
        \x20   val m = java.util.HashMap<String, Any?>()\n\
        \x20   m[\"a\"] = null\n\
        \x20   val read: Any? = m[\"a\"]\n\
        \x20   return if (read == null && m.containsKey(\"a\")) \"OK\" else \"FAIL:$read\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main")
            .expect("a nullable type argument survives the supertype projection"),
        "OK"
    );
}
