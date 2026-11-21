//! PLATFORM-SPECIFIC: `@ThreadLocal` is an optional expectation that only Native declares, so the
//! JVM reference compiler rejects the producer and the program is not a portable box regression.
//! A library object it marks keeps one instance per thread; the thread reads its own instance.

use super::{assert_official_dependency_sources, DependencyConsumer};

const LIBRARY: &str = r#"package fixture

import kotlin.native.concurrent.ThreadLocal

@ThreadLocal
internal object Counter {
    private var count = 0

    fun next(): Int = ++count
}

@ThreadLocal
object Registry {
    var names = "first"
    var size = 1
}

fun count(): Int = Counter.next()

fun register(name: String): Int {
    Registry.names += ", $name"
    return ++Registry.size
}
"#;

const CONSUMER: &str = r#"package fixture

fun box(): String {
    if (count() != 1 || count() != 2 || count() != 3) return "fail: count"
    if (register("second") != 2 || register("third") != 3) return "fail: registry"
    return if (Registry.names == "first, second, third") "OK" else "fail: names"
}
"#;

#[test]
fn dependency_thread_local_objects_keep_their_instance_on_the_thread() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    assert_official_dependency_sources(
        LIBRARY,
        &[("Consumer", CONSUMER)],
        "DependencyThreadLocalObjects",
        &DependencyConsumer::Native,
    );
}
