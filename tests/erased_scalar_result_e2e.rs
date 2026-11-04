//! A generic result substituted with a primitive (`Holder<Int>.get()`) arrives in an erased
//! `Object` slot already boxed. kotlinc coerces that slot straight to a reference consumer (`Any`, a
//! type parameter, `Int?`, a return of `Any`), so the value is never unboxed and boxed again; only
//! a primitive consumer unboxes it.
//!
//! The rule does not depend on who declares the consumer. A dependency function, a dependency
//! constructor, or a member reached through a selected descriptor (`listOf(T)`, `Pair(A, B)`,
//! `KProperty0<Int>.get()` passed to `listOf`) takes the erased result unchanged too.
//!
//! When a later argument (a `try`) makes the earlier ones spill to temporaries, each is still
//! evaluated against its parameter slot: the erased result is stored and reloaded as the `Object`
//! the callee returned. kotlinc lays such a spill out differently (it spills the stack at the `try`
//! itself, reloading it inside the protected region), a separate divergence, so these shapes pin
//! krusty's complete instruction sequence instead of a kotlinc byte diff, and run the program for
//! its result.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Holder<T>(val value: T) {\n\
                      \x20   fun get(): T = value\n\
                      }\n\
                      \n\
                      fun <T> share(a: T, b: T) {}\n\
                      \n\
                      fun take(a: Any) {}\n\
                      \n\
                      fun takeNullable(a: Int?) {}\n\
                      \n\
                      fun takeInt(a: Int) {}\n\
                      \n\
                      fun shared(h: Holder<Int>) { share(42, h.get()) }\n\
                      \n\
                      fun taken(h: Holder<Int>) { take(h.get()) }\n\
                      \n\
                      fun nullable(h: Holder<Int>) { takeNullable(h.get()) }\n\
                      \n\
                      fun primitive(h: Holder<Int>) { takeInt(h.get()) }\n\
                      \n\
                      fun returned(h: Holder<Long>): Any = h.get()\n\
                      \n\
                      fun property(h: Holder<Boolean>) { share(true, h.value) }\n";

#[test]
fn erased_scalar_results_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "ErasedScalarResult",
        SOURCE,
        "store/ErasedScalarResultKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/ErasedScalarResultKt differs from kotlinc: {diff}"));
}

const DEPENDENCY_SOURCE: &str = "package store\n\
                                 \n\
                                 val answer = 42\n\
                                 \n\
                                 class Box<T>(val value: T)\n\
                                 \n\
                                 fun <T> keep(value: T): T = value\n\
                                 \n\
                                 fun propertyValue(): List<Int> {\n\
                                 \x20   val p = ::answer\n\
                                 \x20   return listOf(p.get())\n\
                                 }\n\
                                 \n\
                                 fun element(l: List<Int>): List<Int> = listOf(l[0])\n\
                                 \n\
                                 fun kept(l: List<Char>): Any = keep(l[0])\n\
                                 \n\
                                 fun paired(l: List<Int>): Pair<Int, Long> = Pair(l[0], 2L)\n\
                                 \n\
                                 fun boxed(l: List<Long>): Box<Long> = Box(keep(l[0]))\n\
                                 \n\
                                 fun put(m: MutableMap<String, Int>, l: List<Int>) {\n\
                                 \x20   m.put(\"k\", l[0])\n\
                                 }\n\
                                 \n\
                                 fun printed(m: Map<String, Boolean>) {\n\
                                 \x20   println(m.getValue(\"k\"))\n\
                                 }\n";

#[test]
fn erased_scalar_results_passed_to_dependency_reference_slots_are_byte_identical_to_kotlinc() {
    for class in [
        "store/ErasedScalarDependencyArgumentKt",
        "store/ErasedScalarDependencyArgumentKt$propertyValue$p$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "ErasedScalarDependencyArgument",
            DEPENDENCY_SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

const SPILLED_SOURCE: &str = "package store\n\
                              \n\
                              fun spilledPair(l: List<Int>): Pair<Int, Long> =\n\
                              \x20   Pair(l[0], try { 2L } catch (_: Throwable) { 3L })\n\
                              \n\
                              fun spilledStatic(l: List<Int>): Boolean =\n\
                              \x20   java.util.Objects.equals(l[0], try { 2 } catch (_: Throwable) { 3 })\n\
                              \n\
                              fun spilledVirtual(m: MutableMap<Int, Int>, l: List<Int>) {\n\
                              \x20   m.put(l[0], try { 2 } catch (_: Throwable) { 3 })\n\
                              }\n\
                              \n\
                              fun box(): String {\n\
                              \x20   val l = listOf(5)\n\
                              \x20   if (spilledPair(l) != Pair(5, 2L)) return \"pair\"\n\
                              \x20   if (!spilledStatic(listOf(2))) return \"static\"\n\
                              \x20   val m = mutableMapOf<Int, Int>()\n\
                              \x20   spilledVirtual(m, l)\n\
                              \x20   if (m != mapOf(5 to 2)) return \"virtual\"\n\
                              \x20   return \"OK\"\n\
                              }\n";

/// The complete instruction sequence of each spilled call: the erased `List.get` result goes to an
/// `Object` temporary (`astore`) and back (`aload`), with no `Number.intValue`/`Integer.valueOf`.
#[test]
fn spilled_erased_scalar_arguments_stay_in_their_object_slot() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SPILLED_SOURCE, "ErasedScalarSpilledArgument"),
        "OK"
    );
    let classes = common::expect_classes_with_stdlib(SPILLED_SOURCE, "ErasedScalarSpilledArgument");
    let dir = common::scratch_dir().expect("scratch directory for the emitted class");
    for (internal, bytes) in &classes {
        let path = dir.join(format!("{internal}.class"));
        std::fs::create_dir_all(path.parent().expect("class path has a parent"))
            .expect("create class directory");
        std::fs::write(path, bytes).expect("write emitted class");
    }
    let classpath = dir.to_string_lossy().into_owned();
    let text = common::javap(&[
        "-p",
        "-c",
        "-l",
        "-cp",
        classpath.as_str(),
        "store.ErasedScalarSpilledArgumentKt",
    ])
    .expect("javap is provisioned");
    let check_parameter = "invokestatic # // Method kotlin/jvm/internal/Intrinsics.checkNotNullParameter:(Ljava/lang/Object;Ljava/lang/String;)V";
    let list_get =
        "invokeinterface # 2 // InterfaceMethod java/util/List.get:(I)Ljava/lang/Object;";
    let int_box = "invokestatic # // Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;";
    let expected = |rows: &[(&str, &str)]| -> Vec<String> {
        rows.iter()
            .map(|(pc, code)| format!("{pc}: {code}"))
            .collect()
    };
    assert_eq!(
        common::method_instructions(&text, "spilledPair("),
        expected(&[
            ("0", "aload_0"),
            ("1", "ldc # // String l"),
            ("3", check_parameter),
            ("6", "aload_0"),
            ("7", "iconst_0"),
            ("8", list_get),
            ("13", "astore_1"),
            ("14", "nop"),
            ("15", "ldc2_w # // long 2l"),
            ("18", "lstore_2"),
            ("19", "goto 28"),
            ("22", "astore 4"),
            ("24", "ldc2_w # // long 3l"),
            ("27", "lstore_2"),
            ("28", "lload_2"),
            ("29", "lstore_2"),
            ("30", "new # // class kotlin/Pair"),
            ("33", "dup"),
            ("34", "aload_1"),
            ("35", "lload_2"),
            (
                "36",
                "invokestatic # // Method java/lang/Long.valueOf:(J)Ljava/lang/Long;",
            ),
            (
                "39",
                "invokespecial # // Method kotlin/Pair.\"<init>\":(Ljava/lang/Object;Ljava/lang/Object;)V",
            ),
            ("42", "areturn"),
        ]),
    );
    assert_eq!(
        common::method_instructions(&text, "spilledStatic("),
        expected(&[
            ("0", "aload_0"),
            ("1", "ldc # // String l"),
            ("3", check_parameter),
            ("6", "aload_0"),
            ("7", "iconst_0"),
            ("8", list_get),
            ("13", "astore_1"),
            ("14", "nop"),
            ("15", "iconst_2"),
            ("16", "istore_2"),
            ("17", "goto 23"),
            ("20", "astore_3"),
            ("21", "iconst_3"),
            ("22", "istore_2"),
            ("23", "iload_2"),
            ("24", int_box),
            ("27", "aload_1"),
            ("28", "swap"),
            (
                "29",
                "invokestatic # // Method java/util/Objects.equals:(Ljava/lang/Object;Ljava/lang/Object;)Z",
            ),
            ("32", "ireturn"),
        ]),
    );
    assert_eq!(
        common::method_instructions(&text, "spilledVirtual("),
        expected(&[
            ("0", "aload_0"),
            ("1", "ldc # // String m"),
            ("3", check_parameter),
            ("6", "aload_1"),
            ("7", "ldc # // String l"),
            ("9", check_parameter),
            ("12", "aload_0"),
            ("13", "astore_2"),
            ("14", "aload_1"),
            ("15", "iconst_0"),
            ("16", list_get),
            ("21", "astore_3"),
            ("22", "nop"),
            ("23", "iconst_2"),
            ("24", "istore 4"),
            ("26", "goto 34"),
            ("29", "astore 5"),
            ("31", "iconst_3"),
            ("32", "istore 4"),
            ("34", "iload 4"),
            ("36", "istore 4"),
            ("38", "aload_2"),
            ("39", "aload_3"),
            ("40", "iload 4"),
            ("42", int_box),
            (
                "45",
                "invokeinterface # 3 // InterfaceMethod java/util/Map.put:(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            ),
            ("50", "pop"),
            ("51", "return"),
        ]),
    );
}
