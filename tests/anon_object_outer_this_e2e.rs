use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

// Enclosing member functions must be callable from inside an anonymous object expression, both
// unqualified and via a labeled `this@Outer`. The fixtures intentionally use neutral classifier and
// member names: a regression should preserve the semantic shape without retaining source-system or
// production declaration identities.

#[test]
fn calls_outer_member_function_unqualified() {
    run_ok(
        "AnonOuterCall",
        "interface Activatable { fun invoke(): String }\n\
         abstract class Host {\n\
         var count = 0\n\
         fun install(): Activatable {\n\
         return object : Activatable {\n\
         override fun invoke(): String {\n\
         record()\n\
         return \"done\"\n\
         }\n\
         }\n\
         }\n\
         protected open fun record() { count += 1 }\n\
         }\n\
         class ConcreteHost : Host()\n\
         fun box(): String {\n\
         val a = ConcreteHost().install()\n\
         return if (a.invoke() == \"done\") \"OK\" else \"F\" }\n",
    );
}

#[test]
fn calls_outer_member_function_labeled_this() {
    run_ok(
        "AnonOuterLabeled",
        "interface Activatable { fun invoke(): String }\n\
         abstract class Host {\n\
         var count = 0\n\
         fun install(): Activatable {\n\
         return object : Activatable {\n\
         override fun invoke(): String {\n\
         this@Host.record()\n\
         return if (this@Host.count == 1) \"OK\" else \"F\"\n\
         }\n\
         }\n\
         }\n\
         protected open fun record() { count += 1 }\n\
         }\n\
         class ConcreteHost : Host()\n\
         fun box(): String {\n\
         return ConcreteHost().install().invoke() }\n",
    );
}

#[test]
fn override_dispatches_through_outer_open_function() {
    run_ok(
        "AnonOuterOverride",
        "interface Activatable { fun invoke(): String }\n\
         abstract class Host {\n\
         fun install(): Activatable {\n\
         return object : Activatable {\n\
         override fun invoke(): String = record()\n\
         }\n\
         }\n\
         protected abstract fun record(): String\n\
         }\n\
         class ConcreteHost : Host() {\n\
         override fun record(): String = \"OK\"\n\
         }\n\
         fun box(): String = ConcreteHost().install().invoke()\n",
    );
}

#[test]
fn calls_outer_member_inherited_from_dependency() {
    // The enclosing receiver's hierarchy crosses from the current module into the platform source.
    // Capture discovery must enumerate the common classifier shape; stopping at the source class would
    // miss `add`, omit the enclosing instance, and leave lowering with no receiver for the call.
    run_ok(
        "AnonOuterDependencyMember",
        "interface Callback { fun invoke(): String }\n\
         class Host : java.util.ArrayList<String>() {\n\
         fun callback(): Callback = object : Callback {\n\
         override fun invoke(): String { add(\"x\"); return if (size == 1) \"OK\" else \"F\" }\n\
         }\n\
         }\n\
         fun box(): String = Host().callback().invoke()\n",
    );
}

#[test]
fn reads_outer_property_without_an_outer_call() {
    // Capture discovery must recognize a bare property independently of method-call discovery. Using
    // only callable names would make this property-only body look capture-free and later lower `token`
    // against the anonymous receiver, even though resolution selected the enclosing `Host` instance.
    run_ok(
        "AnonOuterProperty",
        "interface Callback { fun invoke(): String }\n\
         class Host {\n\
         val token = \"OK\"\n\
         fun callback(): Callback = object : Callback {\n\
         override fun invoke(): String = token\n\
         }\n\
         }\n\
         fun box(): String = Host().callback().invoke()\n",
    );
}
