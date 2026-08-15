//! A multi-argument index operator (`m[i, j]` → `m.get(i, j)`, `m[i, j] = v` → `m.set(i, j, v)`) on a
//! user class with member `operator fun get`/`set`. Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn user_class_member_get_and_set() {
    const SRC: &str = "class Matrix {\n\
        \x20 val data: IntArray = IntArray(9)\n\
        \x20 operator fun get(i: Int, j: Int): Int = data[i * 3 + j]\n\
        \x20 operator fun set(i: Int, j: Int, v: Int) { data[i * 3 + j] = v }\n\
        }\n\
        fun box(): String {\n\
        \x20 val m = Matrix()\n\
        \x20 m[1, 2] = 42\n\
        \x20 m[2, 0] = 7\n\
        \x20 return if (m[1, 2] == 42 && m[2, 0] == 7 && m[0, 0] == 0) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("multi-index member get/set"), "OK");
}

#[test]
fn extension_get_set_operator() {
    // `get`/`set` are same-module EXTENSION operators on a user class.
    const SRC: &str = "class Grid { val d: IntArray = IntArray(4) }\n\
        operator fun Grid.get(i: Int, j: Int): Int = d[i * 2 + j]\n\
        operator fun Grid.set(i: Int, j: Int, v: Int) { d[i * 2 + j] = v }\n\
        fun box(): String {\n\
        \x20 val g = Grid()\n\
        \x20 g[1, 1] = 9\n\
        \x20 return if (g[1, 1] == 9 && g[0, 0] == 0) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("extension get/set"), "OK");
}

#[test]
fn extension_vararg_get_operator() {
    const SRC: &str = "operator fun String.get(vararg values: Any): String =\n\
        if (values[0] == 44 && values[1] == \"example\") this else \"fail\"\n\
        fun box(): String = \"OK\"[44, \"example\"]\n";

    assert_eq!(run(SRC).expect("extension vararg get"), "OK");
}

#[test]
fn extension_vararg_get_and_set_operators() {
    const SRC: &str = "class Store(var value: Int)
        operator fun Store.get(vararg indices: Int): Int = value + indices.size
        operator fun Store.set(vararg indices: Int, value: Int) {
            this.value = value + indices.size
        }
        fun box(): String {
            val store = Store(10)
            store[1, 2] = 20
            return if (store.value == 22 && store[3, 4, 5] == 25) \"OK\" else \"fail\"
        }
        ";

    assert_eq!(run(SRC).expect("extension vararg get/set"), "OK");
}

#[test]
fn fixed_extension_set_keeps_normal_specificity_selection() {
    const SRC: &str = "class Store { var result = \"\" }
        operator fun Store.set(index: Any, value: Int) { result = \"any\" }
        operator fun Store.set(index: CharSequence, value: Int) { result = \"chars\" }
        fun box(): String {
            val store = Store()
            store[\"key\"] = 1
            return if (store.result == \"chars\") \"OK\" else store.result
        }
        ";

    assert_eq!(run(SRC).expect("fixed extension set specificity"), "OK");
}

#[test]
fn vararg_extension_set_uses_element_specificity() {
    const SRC: &str = "class Store { var result = \"\" }
        operator fun Store.set(vararg index: Any, value: Int) { result = \"any\" }
        operator fun Store.set(vararg index: CharSequence, value: Int) { result = \"chars\" }
        fun box(): String {
            val store = Store()
            store[\"key\", \"other\"] = 1
            return if (store.result == \"chars\") \"OK\" else store.result
        }
        ";

    assert_eq!(run(SRC).expect("vararg set specificity"), "OK");
}

#[test]
fn generic_vararg_set_infers_indices_and_value_from_their_own_operands() {
    const SRC: &str = "class Store { var result = \"\"
        operator fun <I, V> set(vararg indices: I, value: V) {
            result = indices[0].toString() + value.toString()
        }
    }
    fun box(): String {
        val store = Store()
        store[\"a\", \"b\"] = 7
        return if (store.result == \"a7\") \"OK\" else store.result
    }
    ";

    assert_eq!(run(SRC).expect("generic vararg set inference"), "OK");
}

#[test]
fn ambiguous_vararg_set_reports_ambiguity() {
    const SRC: &str = "interface Left
        interface Right
        class Both : Left, Right
        class Store {
            operator fun set(vararg indices: Left, value: Int) {}
            operator fun set(vararg indices: Right, value: Int) {}
        }
        fun box() { Store()[Both()] = 1 }
    ";

    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambiguity") && diagnostic.contains("set")),
        "expected indexed set ambiguity, got: {diagnostics:?}"
    );
}

#[test]
fn member_vararg_operators_beat_fixed_extensions() {
    const SRC: &str = "class Store {
        var result = \"\"
        operator fun get(vararg indices: Int): String = \"member-get\"
        operator fun set(vararg indices: Int, value: Int) { result = \"member-set\" }
    }
    operator fun Store.get(index: Int): String = \"extension-get\"
    operator fun Store.set(index: Int, value: Int) { result = \"extension-set\" }
    fun box(): String {
        val store = Store()
        val read = store[1]
        store[2] = 3
        return if (read == \"member-get\" && store.result == \"member-set\") \"OK\" else read + store.result
    }
    ";

    assert_eq!(run(SRC).expect("member indexed precedence"), "OK");
}

#[test]
fn ambiguous_vararg_get_reports_ambiguity() {
    const SRC: &str = "interface Left
        interface Right
        class Both : Left, Right
        class Store {
            operator fun get(vararg indices: Left): Int = 1
            operator fun get(vararg indices: Right): Int = 2
        }
        fun box() { Store()[Both()] }
    ";

    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambiguity") && diagnostic.contains("get")),
        "expected indexed get ambiguity, got: {diagnostics:?}"
    );
}

#[test]
fn fixed_generic_extension_get_infers_its_return() {
    const SRC: &str = "class Store
        operator fun <T> Store.get(index: T): T = index
        fun box(): String {
            val value: String = Store()[\"OK\"]
            return value
        }
    ";

    assert_eq!(run(SRC).expect("generic fixed extension get"), "OK");
}

#[test]
fn unavailable_context_member_does_not_drive_set_rhs_precheck() {
    const SRC: &str = "class Store { var result = \"\"
        context(token: String)
        operator fun set(vararg indices: Int, value: String) { result = token + value }
    }
    operator fun Store.set(index: Int, value: Int) { result = \"extension\" }
    fun box(): String {
        val store = Store()
        store[1] = 2
        return if (store.result == \"extension\") \"OK\" else store.result
    }
    ";

    assert_eq!(run(SRC).expect("context member exclusion"), "OK");
}

#[test]
fn reference_compiled_member_and_extension_vararg_index_operators() {
    const LIB: &str = "package lib
        class MemberStore(var value: Int = 0) {
            operator fun get(vararg indices: Int): Int = value + indices.size
            operator fun set(vararg indices: Int, value: Int) { this.value = value + indices.size }
        }
        class ExtStore(var value: Int = 0)
        operator fun ExtStore.get(vararg indices: Int): Int = value + indices.size
        operator fun ExtStore.set(vararg indices: Int, value: Int) {
            this.value = value + indices.size
        }
    ";
    const MAIN: &str = "import lib.MemberStore
        import lib.ExtStore
        import lib.get
        import lib.set
        fun box(): String {
            val member = MemberStore()
            member[1, 2] = 10
            val extension = ExtStore()
            extension[3, 4, 5] = 20
            return if (member.value == 12 && member[9] == 13 &&
                extension.value == 23 && extension[6, 7] == 25) \"OK\" else \"fail\"
        }
    ";

    let Some(output) = common::expect_box_run_against_ref("vararg_index_operators", LIB, MAIN)
    else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert_eq!(output, "OK");
}

#[test]
fn reference_compiled_member_vararg_set_preserves_receiver_first_order() {
    const LIB: &str = "package deporder
        var log: String = \"\"
        fun append(value: String) { log += value }
        fun current(): String = log
        class Store {
            operator fun set(vararg indices: Int, value: Int) { append(\"set;\") }
        }
    ";
    const MAIN: &str = "import deporder.Store
        import deporder.append
        import deporder.current
        fun receiver(): Store { append(\"receiver;\"); return Store() }
        fun index(): Int { append(\"index;\"); return 1 }
        fun value(): Int { append(\"value;\"); return 2 }
        fun box(): String {
            receiver()[index()] = value()
            return if (current() == \"receiver;index;value;set;\") \"OK\" else current()
        }
    ";

    let Some(output) = common::expect_box_run_against_ref("vararg_index_receiver_order", LIB, MAIN)
    else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert_eq!(output, "OK");
}

#[test]
fn member_vararg_get_and_set_operators() {
    const SRC: &str = "class Store {
        var value = 0
        var getSum = 0
        var setSum = 0
        operator fun get(vararg indices: Int): Int {
            for (index in indices) getSum += index
            return value
        }
        operator fun set(vararg indices: Int, value: Int) {
            for (index in indices) setSum += index
            this.value = value
        }
    }
        fun box(): String {
            val store = Store()
            store[1, 2, 3] = 40
            store[7] = 41
            val value = store[4, 5]
            val single = store[6]
            return if (value == 41 && single == 41 && store.getSum == 15 && store.setSum == 13) \"OK\" else \"fail\"
        }
    ";

    assert_eq!(run(SRC).expect("member vararg get/set"), "OK");
}

#[test]
fn member_vararg_set_evaluates_receiver_before_indices_and_value() {
    const SRC: &str = "var log = \"\"
    class Store {
        operator fun set(vararg indices: Int, value: Int) { log += \"set;\" }
    }
    fun receiver(): Store { log += \"receiver;\"; return Store() }
    fun index(): Int { log += \"index;\"; return 1 }
    fun value(): Int { log += \"value;\"; return 2 }
    fun box(): String {
        receiver()[index()] = value()
        return if (log == \"receiver;index;value;set;\") \"OK\" else log
    }
    ";

    assert_eq!(run(SRC).expect("member vararg set source order"), "OK");
}

#[test]
fn indexed_overload_selection_distinguishes_fixed_and_vararg_shapes() {
    const SRC: &str = "class Store {
        var result = \"\"
        operator fun get(vararg indices: Int): String = \"vararg:\" + indices.size
        operator fun get(first: String, second: String): String = \"fixed:\" + first + second
        operator fun set(vararg indices: Int, value: String) { result = value + indices.size }
        operator fun set(first: String, second: String, value: String) {
            result = value + first + second
        }
    }
    fun box(): String {
        val store = Store()
        val ints = store[1, 2, 3]
        val strings = store[\"a\", \"b\"]
        store[1, 2] = \"v\"
        val varargSet = store.result
        store[\"a\", \"b\"] = \"v\"
        return if (ints == \"vararg:3\" && strings == \"fixed:ab\" &&
            varargSet == \"v2\" && store.result == \"vab\") \"OK\" else \"fail\"
    }
    ";

    assert_eq!(run(SRC).expect("indexed overload selection"), "OK");
}

#[test]
fn indexed_syntax_does_not_bind_parameters_after_a_get_vararg() {
    const SRC: &str = "class Store {
        operator fun get(vararg indices: Int, tail: String): String = tail
    }
    fun box(): String = Store()[1, 2, \"tail\"]
    ";

    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("get")),
        "expected an inapplicable get diagnostic, got: {diagnostics:?}"
    );
}

#[test]
fn member_vararg_index_postfix_increment_preserves_evaluation_order() {
    const SRC: &str = "var log = \"\"
    class Store {
        var value = \"x\"
        operator fun get(vararg indices: String): String {
            log += \"get;\"
            return value
        }
        operator fun set(vararg indices: String, value: String) {
            log += \"set;\"
            this.value = value
        }
    }
    operator fun String.inc(): String {
        log += \"inc;\"
        return this + \"1\"
    }
    fun box(): String {
        val store = Store()
        fun index(value: String): String {
            log += value + \";\"
            return value
        }
        val old = store[index(\"1\"), index(\"2\"), index(\"3\")]++
        return if (old == \"x\" && store.value == \"x1\" &&
            log == \"1;2;3;get;inc;set;\") \"OK\" else log + store.value
    }
    ";

    assert_eq!(
        run(SRC).expect("member vararg indexed postfix increment"),
        "OK"
    );
}
