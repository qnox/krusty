//! An anonymous object declared in an interface companion initializer loads `$$INSTANCE`.
//!
//! The interface's `Companion` field is assigned only after the companion `<clinit>` returns, so a
//! `getstatic` of that field from the object is still null. A class companion publishes `Companion`
//! before its initializers run, and the object keeps that published field.
use super::common::{
    self, compare_with_kotlinc_plugin, expect_box_same_as_kotlinc, method_instructions,
};

const SRC: &str = r#"
class Payload(val text: String)
class Probe

inline fun Probe.read(block: Probe.() -> String): String = block()

interface Test {
    companion object {
        val x = Payload("O")
        val y1 = Test.x.text
        val y2 = Probe().read { x.text }
        val y3: String
        init {
            fun localFun() = x.text
            y3 = localFun()
        }
        fun method() = x.text
        val y4 = method()
        val anonObject = object {
            override fun toString() = x.text
        }
        val y5 = anonObject.toString()
    }
}

annotation class Anno {
    companion object {
        val x = Payload("K")
        val y1 = Anno.x.text
        val y2 = Probe().read { x.text }
        val y3: String
        init {
            fun localFun() = x.text
            y3 = localFun()
        }
        fun method() = x.text
        val y4 = method()
        val anonObject = object {
            override fun toString() = x.text
        }
        val y5 = anonObject.toString()
    }
}

class Holder {
    companion object {
        val x = Payload("C")
        val anonObject = object {
            override fun toString() = x.text
        }
        val y = anonObject.toString()
    }
}

fun box(): String {
    if (Test.y1 != "O" || Test.y2 != "O" || Test.y3 != "O" || Test.y4 != "O" || Test.y5 != "O") return "Test"
    if (Anno.y1 != "K" || Anno.y2 != "K" || Anno.y3 != "K" || Anno.y4 != "K" || Anno.y5 != "K") return "Anno"
    if (Holder.y != "C") return "Holder"
    return Test.x.text + Anno.x.text
}
"#;

#[test]
fn an_interface_companion_anonymous_object_reads_the_instance() {
    expect_box_same_as_kotlinc(SRC, "InterfaceCompanionAnon");
}

#[test]
fn the_anonymous_object_loads_the_companion_instance_field() {
    let cp = [common::stdlib_jar()];
    for class in [
        "Test$Companion$anonObject$1",
        "Anno$Companion$anonObject$1",
        "Holder$Companion$anonObject$1",
    ] {
        let built =
            compare_with_kotlinc_plugin("InterfaceCompanionAnon", SRC, class, &cp, "17", &[])
                .unwrap_or_else(|| panic!("{class} did not compile"));
        assert_eq!(
            method_instructions(&built.krusty, "toString();"),
            method_instructions(&built.reference, "toString();"),
            "{class}.toString"
        );
    }
}
