//! A Java class's SUPERTYPE arguments carry Java's flexible nullability, exactly as its own member
//! signatures do.
//!
//! `class Properties extends Hashtable<Object, Object>` exposes `MutableMap<Any!, Any!>`, so a
//! `null` value stores fine and the class widens to `MutableMap<Any?, Any?>`. krusty read the
//! generic signature's arguments verbatim, which hardened them to non-null: every member reached
//! THROUGH the supertype then rejected a nullable argument, while the class's own `put` — whose
//! descriptor goes through the Java-nullability pass — accepted it.
//!
//! Index assignment is where it showed: `p["k"] = value` resolves through `MutableMap.set`, whose
//! `V` came from the hardened supertype argument, so a nullable value left "not an array (cannot
//! index-assign)" — a diagnostic that names neither the real cause nor the real type.
//!
//! A Java class with its OWN type parameters never showed it: `HashMap<K, V> implements Map<K, V>`
//! passes variables through, and the arguments are then whatever the USE SITE wrote. It takes a
//! supertype instantiated at a concrete Java type to harden anything.
use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

#[test]
fn a_nullable_value_stores_into_a_java_map_subclass() {
    const SRC: &str = "import java.util.Properties\n\
        fun store(p: Properties, v: String?) {\n\
        \x20 p[\"k\"] = v\n\
        }\n";
    assert_eq!(diagnostics(SRC), Vec::<String>::new());
}

#[test]
fn a_java_map_subclass_widens_to_a_nullable_kotlin_map() {
    const SRC: &str = "import java.util.Properties\n\
        fun widen(p: Properties): MutableMap<Any?, Any?> = p\n";
    assert_eq!(diagnostics(SRC), Vec::<String>::new());
}

/// The store must also RUN: the value reaches the map and reads back, and a `null` is accepted by
/// the JVM method the operator lowers to.
#[test]
fn the_stored_value_round_trips_through_the_java_map() {
    // The store goes through a PARAMETER of the Java type: a freshly constructed local carries the
    // constructor's own type, which the checker never projects onto the hardened supertype.
    const SRC: &str = "import java.util.Properties\n\
        fun store(p: Properties, v: String?) {\n\
        \x20 p[\"k\"] = v\n\
        }\n\
        fun box(): String {\n\
        \x20 val p = Properties()\n\
        \x20 store(p, \"OK\")\n\
        \x20 return p[\"k\"] as? String ?: \"missing\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("java map store runs"),
        "OK"
    );
}
