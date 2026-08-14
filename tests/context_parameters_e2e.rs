//! Context parameters (`context(a: A) fun f()`): the leading context receivers are supplied IMPLICITLY
//! at the call site — from the enclosing `with`-block receiver, or an in-scope local / enclosing context
//! parameter — rather than positionally. The checker resolves each context parameter to an in-scope
//! source and the lowerer prepends the loaded values (matching kotlinc's leading-value-parameter ABI).
//! Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn context_from_with_receiver() {
    // The context `a: A` is filled from the enclosing `with(A("OK"))` receiver.
    const SRC: &str = "class A(var x: String) { fun foo(): String = x }\n\
        var result = \"\"\n\
        context(a: A)\n\
        fun test1() { result = a.foo() }\n\
        fun box(): String {\n\
        \x20 with(A(\"OK\")) { test1() }\n\
        \x20 return result\n\
        }\n";
    assert_eq!(run(SRC).expect("context from with receiver"), "OK");
}

#[test]
fn contextual_inline_extension_maps_defaults_after_contexts() {
    const LIB: &str = "// LANGUAGE: +ContextParameters\n\
        package lib\n\
        class Offset(val value: Int)\n\
        class Marker\n\
        context(offset: Offset, marker: Marker)\n\
        inline fun Int.total(left: Int = 1, right: Int = 2) = this + left + right + offset.value\n";
    const MAIN: &str = "// LANGUAGE: +ContextParameters\n\
        import lib.*\n\
        fun box(): String = with(Offset(3)) {\n\
            with(Marker()) { if (4.total() == 10) \"OK\" else \"fail\" }\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("lib.kt", LIB), ("main.kt", MAIN)], "Main");
}

#[test]
fn context_operator_get_and_set_receive_the_implicit_context() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        class Box(var value: String)\n\
        context(context: Int) operator fun Box.get(index: Int): String =\n\
            if (context == 42 && index == 0) value else \"fail\"\n\
        context(context: Int) operator fun Box.set(index: Int, value: String) {\n\
            if (context == 42 && index == 0) this.value = value\n\
        }\n\
        fun box(): String = with(42) {\n\
            val value = Box(\"fail\")\n\
            value[0] = \"OK\"\n\
            value[0]\n\
        }\n";
    let output = run(SRC).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        );
        panic!("context operators failed: {diagnostics:?}")
    });
    assert_eq!(output, "OK");
}

#[test]
fn ordinary_operator_arguments_are_unchanged_without_context_parameters() {
    const SRC: &str = "class Box(var value: String) {\n\
        operator fun get(index: Int): String = if (index == 0) value else \"fail\"\n\
        operator fun set(index: Int, value: String) { if (index == 0) this.value = value }\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = Box(\"fail\")\n\
        \x20 value[0] = \"OK\"\n\
        \x20 return value[0]\n\
        }\n";
    assert_eq!(run(SRC).expect("ordinary operators"), "OK");
}

#[test]
fn context_parameter_declarations_follow_the_language_feature_mode() {
    const SOURCE: &str = "class Context\n\
        context(context: Context) fun use(): String = \"OK\"\n\
        fun box(): String = with(Context()) { use() }\n";
    common::assert_language_feature_gate(SOURCE, "ContextParameters");
}

#[test]
fn context_from_local_value() {
    // The context `a: A` is filled from an in-scope local of the matching type.
    const SRC: &str = "class A(val x: String) { fun foo(): String = x }\n\
        var result = \"\"\n\
        context(a: A)\n\
        fun test1() { result = a.foo() }\n\
        fun box(): String {\n\
        \x20 val a = A(\"OK\")\n\
        \x20 test1()\n\
        \x20 return result\n\
        }\n";
    assert_eq!(run(SRC).expect("context from local"), "OK");
}

#[test]
fn context_forwarded_through_enclosing_context() {
    // A context parameter is forwarded to a callee that needs the same context.
    const SRC: &str = "class A(val x: String)\n\
        context(a: A) fun leaf(): String = a.x\n\
        context(a: A) fun mid(): String = leaf()\n\
        fun box(): String = with(A(\"OK\")) { mid() }\n";
    assert_eq!(run(SRC).expect("context forwarded"), "OK");
}

#[test]
fn nearest_context_wins() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        context(value: String) fun current() = value
        context(value: String) fun one(): String {
            context(value: String) fun local() = current()
            return with("OK") { local() }
        }
        context(number: Int, value: String) fun two(): String {
            context(number: Int, value: String) fun local() = value + number
            return with("OK") { local() }
        }
        fun box(): String {
            if (with("wrong") { one() } != "OK") return "one"
            return with(1) { with("wrong") { two() } }
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK1");
}

#[test]
fn local_overloads_include_context_candidates() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class A
        fun box(): String {
            fun pick(value: A) = "value"
            fun A.pick() = "extension"
            context(value: A) fun pick() = "context"
            return with(A()) { pick() } + A().pick() + pick(A())
        }
    "#;
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "Main"),
        "contextextensionvalue"
    );
}

#[test]
fn contextual_local_extension_uses_scope() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Prefix(val first: String)
        class Target(val second: String)
        fun box(): String {
            context(prefix: Prefix) fun Target.read() = prefix.first + second
            return with(Prefix("O")) { with(Target("K")) { read() } }
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn explicit_context_argument_selects_the_contextual_member_overload() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        class Token
        interface Service {
            fun read() = "ordinary"
            context(token: Token) fun read(): String
        }
        class Both : Service {
            override fun read() = "O"
            context(token: Token) override fun read() = "K"
        }
        fun box(): String {
            val service = Both()
            return service.read() + service.read(token = Token())
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn explicit_context_arguments_follow_the_language_feature_mode() {
    const SOURCE: &str = r#"
        class Token
        context(token: Token) fun read() = "OK"
        fun box() = read(token = Token())
    "#;
    common::assert_language_feature_gate(SOURCE, "ExplicitContextArguments");
}

#[test]
fn explicit_context_argument_maps_a_cross_file_declaration() {
    const DECLARATION: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        context(token: String) fun read() = token
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        fun box() = read(token = "OK")
    "#;
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[
            ("Declaration", DECLARATION),
            ("Main", CALLER)
        ])
        .expect("cross-file explicit context argument"),
        "OK"
    );
}

#[test]
fn explicit_context_arguments_map_dependency_metadata() {
    const LIBRARY: &str = r#"
        // LANGUAGE: +ContextParameters
        package dependency
        context(first: String, second: String)
        fun combine() = first + second
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        import dependency.combine
        fun box() = combine(first = "O", second = "K")
    "#;
    let diagnostics = common::diagnostics_against_ref("explicitcontextdependency", LIBRARY, CALLER)
        .expect("reference compiler unavailable");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn package_qualified_context_call_maps_dependency_defaults() {
    const LIBRARY: &str = r#"
        // LANGUAGE: +ContextParameters
        package dependency
        context(prefix: String)
        fun choose(useValue: Boolean, value: String = "K") = if (useValue) value else prefix
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextParameters
        fun box() = with("O") { dependency.choose(false) + dependency.choose(true) }
    "#;
    let diagnostics = common::diagnostics_against_ref("contextdependencydefaults", LIBRARY, CALLER)
        .expect("reference compiler unavailable");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn package_qualified_context_call_runs_with_dependency_defaults() {
    const LIBRARY: &str = r#"
        // LANGUAGE: +ContextParameters
        package dependency
        context(prefix: String)
        fun choose(useValue: Boolean, value: String = "K") = if (useValue) value else prefix
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextParameters
        fun box() = with("O") { dependency.choose(false) + dependency.choose(true) }
    "#;
    common::expect_box_ok_against_ref("context_dependency_defaults_run", LIBRARY, CALLER);
}

#[test]
fn package_qualified_suspend_context_call_runs() {
    const LIBRARY: &str = r#"
        // LANGUAGE: +ContextParameters
        package dependency
        context(value: String)
        suspend fun read() = value
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextParameters
        suspend fun probe() = with("OK") { dependency.read() }
    "#;
    common::expect_suspend_result_against_ref(
        "suspend_context_dependency_run",
        LIBRARY,
        CALLER,
        "probe(continuation)",
        "OK",
    );
}

#[test]
fn explicit_and_implicit_context_arguments_can_be_mixed() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        context(number: Int, text: String)
        fun combine() = text + number
        fun box() = with(1) { combine(text = "OK") }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK1");
}

#[test]
fn implicit_context_argument_contributes_to_generic_inference() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        context(first: T, second: R)
        fun <T, R> choose() = second
        fun box() = with(1) { choose(second = "OK") }
    "#;
    assert_eq!(
        common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        Vec::<String>::new()
    );
}

#[test]
fn explicit_context_parameter_does_not_take_a_positional_value_slot() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        context(transform: (T) -> T)
        fun <T> apply(value: T) = transform(value)
        fun box() = apply<String>("OK", transform = { it })
    "#;
    assert_eq!(
        common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        Vec::<String>::new()
    );
}

#[test]
fn explicit_generic_context_argument_keeps_numeric_result_type() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters +ExplicitContextArguments
        context(value: T) fun <T> identity() = value
        fun box() = if (identity<Long>(value = 1) == 1L) "OK" else "fail"
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn contextual_property_supports_compound_assignment() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Counter { var writes = 0 }
        class Value {
            context(counter: Counter)
            var number: Int
                get() = 1
                set(value) { counter.writes++ }
        }
        fun box() = with(Counter()) {
            Value().number += 1
            if (writes == 1) "OK" else "fail"
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn contextual_property_supports_safe_assignment() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Counter { var writes = 0 }
        class Value {
            context(counter: Counter)
            var number: Int
                get() = 0
                set(value) { counter.writes++ }
        }
        fun value(): Value? = Value()
        fun box() = with(Counter()) {
            value()?.number = 1
            if (writes == 1) "OK" else "fail"
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn one_scope_value_can_supply_multiple_context_types_and_a_member_context() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Payload(val text: String)
        interface Logging {
            fun decorate(value: String): String
        }
        interface Repository<T> {
            context(logging: Logging) fun save(value: T): String
        }
        class Environment : Logging, Repository<Payload> {
            override fun decorate(value: String) = value
            context(logging: Logging)
            override fun save(value: Payload) = logging.decorate(value.text)
        }
        context(logging: Logging, repository: Repository<Payload>)
        fun execute() = repository.save(Payload("OK"))
        fun box() = with(Environment()) { execute() }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_property_accessors_receive_the_selected_scope_value() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        var stored = ""
        context(prefix: String)
        var message: String
            get() = stored
            set(value) { stored = prefix + value }
        fun box() = with("O") {
            message = "K"
            message
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn member_context_property_uses_the_regular_property_path() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Holder {
            private var stored = ""
            context(prefix: String)
            var message: String
                get() = stored
                set(value) { stored = prefix + value }
        }
        fun box() = with("O") {
            val holder = Holder()
            holder.message = "K"
            holder.message
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn extension_context_property_uses_the_regular_property_path() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Holder(var stored: String)
        context(prefix: String)
        var Holder.message: String
            get() = stored
            set(value) { stored = prefix + value }
        fun box() = with("O") {
            val holder = Holder("")
            holder.message = "K"
            holder.message
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_property_getter_may_declare_its_return_type() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        context(value: String)
        val message: String
            get(): String = value
        fun box() = with("OK") { message }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_property_type_parameters_scope_its_context_clause() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Box<T>(val value: T)
        context(box: Box<T>)
        val <T> current: T
            get() = box.value
        fun box() = with(Box("OK")) { current }
    "#;
    assert_eq!(
        common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        Vec::<String>::new()
    );
}

#[test]
fn function_valued_context_property_can_be_invoked() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        context(outer: String)
        val action get() = context(inner: String) fun(): String = inner
        fun box() = with("wrong") { action("OK") }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_function_property_beats_inapplicable_local() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        val action: context(String) () -> String = { "OK" }
        fun box(): String {
            context(value: String) fun action() = "local"
            return action("unused")
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_function_property_through_alias() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        typealias Action = context(String) () -> String
        val action: Action = { "OK" }
        fun box() = action("unused")
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_invoke_uses_scope() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Action {
            context(value: String) operator fun invoke() = value
        }
        class Owner {
            val action = Action()
            fun run() = with("OK") { action() }
        }
        fun box() = Owner().run()
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_function_value_uses_scope() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        val action: context(String) () -> String = { substring(0) }
        fun box() = with("OK") { action() }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn missing_context_names_the_callable() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class C
        context(c: C) fun use(): String = "OK"
        fun box() = use()
    "#;
    assert_eq!(
        common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        vec!["No context argument for 'context(c: C) fun use(): String' found."]
    );
}

#[test]
fn functional_interface_member_context_is_implicit() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        class C\n\
        fun interface Action {\n\
        \x20 context(c: C) fun accept(value: String): String\n\
        }\n\
        fun consume(action: Action): String = with(C()) { action.accept(\"OK\") }\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(SRC),
        Vec::<String>::new()
    );
}

#[test]
fn implicit_receiver_member_shadows_top_level() {
    // Inside `with(Scope)`, an unqualified call to a name that is BOTH a member of the receiver and a
    // top-level function binds the MEMBER (kotlinc scoping: the receiver is the nearer scope). Outside
    // the block, the top-level function is called.
    const SRC: &str = "class Scope { fun tag(): String = \"member\" }\n\
        fun tag(): String = \"top-level\"\n\
        fun box(): String {\n\
        \x20 val inside = with(Scope()) { tag() }\n\
        \x20 val outside = tag()\n\
        \x20 return if (inside == \"member\" && outside == \"top-level\") \"OK\" else \"no: \" + inside + \"/\" + outside\n\
        }\n";
    assert_eq!(run(SRC).expect("member shadows top-level"), "OK");
}

#[test]
fn nearer_dispatch_member_extension_beats_inapplicable_outer_member() {
    const SRC: &str = "class Scope {\n\
        \x20 fun Token.select(): String = \"extension\"\n\
        \x20 fun Token.choose(): String = \"OK\"\n\
        \x20 fun Token.defaulted(): String = \"extension\"\n\
        \x20 fun Token.named(value: String): String = \"extension\"\n\
        \x20 fun Token.spread(vararg values: String): String = \"extension\"\n\
        \x20 fun Token.block(action: () -> String): String = \"extension\"\n\
        }\n\
        class Token {\n\
        \x20 fun select(): String = \"member\"\n\
        \x20 fun choose(scope: Scope): String = \"wrong\"\n\
        \x20 fun defaulted(value: String = \"D\"): String = value\n\
        \x20 fun named(prefix: String = \"N\", value: String): String = prefix + value\n\
        \x20 fun spread(vararg values: String): String = \"vararg\"\n\
        \x20 fun block(prefix: String = \"T\", action: () -> String): String = prefix + action()\n\
        }\n\
        fun Token.forward(scope: Scope): String = with(scope) {\n\
        \x20 select() + choose() + defaulted() + named(value = \"K\") +\n\
        \x20     spread(\"x\", \"y\") + block { \"L\" }\n\
        }\n\
        fun box(): String = Token().forward(Scope())\n";
    assert_eq!(
        run(SRC).expect("nearer dispatch member extension"),
        "memberOKDNKvarargTL"
    );
}

#[test]
fn member_extension_uses_nearest_implicit_extension_receiver() {
    const SRC: &str = "class Token\n\
        class Host {\n\
        \x20 fun Token.choose(): String = \"OK\"\n\
        \x20 fun run(token: Token): String = with(token) { choose() }\n\
        }\n\
        fun box(): String = Host().run(Token())\n";
    assert_eq!(run(SRC).expect("nearest extension receiver"), "OK");
}

#[test]
fn one_receiver_can_supply_member_extension_and_context() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Node(val text: String) {
            context(context: Node)
            fun Node.render() = context.text + this@Node.text + text
        }
        fun box() = with(Node("D")) { render() + Node("E").render() }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "DDDDDE");
}

#[test]
fn one_receiver_can_supply_member_extension_property_and_context() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Node(val text: String) {
            context(context: Node)
            val Node.rendered: String
                get() = context.text + this@Node.text + this.text
        }
        fun box() = with(Node("D")) { rendered + Node("E").rendered }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "DDDDDE");
}

#[test]
fn member_extension_uses_nearest_applicable_dispatch_receiver() {
    const SRC: &str = "class Token\n\
        class Scope {\n\
        \x20 fun Token.choose(): String = \"OK\"\n\
        }\n\
        class Host {\n\
        \x20 fun Token.choose(): String = \"outer\"\n\
        \x20 fun run(token: Token, scope: Scope): String = with(token) {\n\
        \x20     with(scope) { choose() }\n\
        \x20 }\n\
        }\n\
        fun box(): String = Host().run(Token(), Scope())\n";
    assert_eq!(run(SRC).expect("nearest dispatch receiver"), "OK");
}

#[test]
fn inapplicable_nearest_member_does_not_hide_member_extension() {
    const SRC: &str = "class Scope { fun choose(value: Int): String = \"member\" }\n\
        class Host {\n\
        \x20 fun Scope.choose(): String = \"OK\"\n\
        \x20 fun run(scope: Scope): String = with(scope) { choose() }\n\
        }\n\
        fun box(): String = Host().run(Scope())\n";
    assert_eq!(run(SRC).expect("applicable member extension"), "OK");
}

#[test]
fn context_top_level_function_maps_reordered_named_arguments() {
    const SRC: &str = "class C\n\
        context(c: C) fun combine(a: String, b: String): String = a + b\n\
        fun box(): String = with(C()) { combine(b = \"K\", a = \"O\") }\n";
    assert_eq!(run(SRC).expect("context named arguments"), "OK");
}

#[test]
fn context_local_function_maps_reordered_named_arguments() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 context(c: C) fun combine(a: String, b: String): String = a + b\n\
        \x20 return with(C()) { combine(b = \"K\", a = \"O\") }\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(SRC),
        Vec::<String>::new()
    );
    assert_eq!(run(SRC).expect("local context named arguments"), "OK");
}

#[test]
fn context_top_level_function_maps_named_argument_past_default() {
    const SRC: &str = "class C\n\
        context(c: C) fun choose(a: Int = 7, b: String): String = b\n\
        fun box(): String = with(C()) { choose(b = \"OK\") }\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(SRC),
        Vec::<String>::new()
    );
    assert_eq!(run(SRC).expect("context named argument past default"), "OK");
}

#[test]
fn context_local_function_maps_named_argument_past_default() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 context(c: C) fun choose(a: Int = 7, b: String): String = b\n\
        \x20 return with(C()) { choose(b = \"OK\") }\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(SRC),
        Vec::<String>::new()
    );
    assert_eq!(
        run(SRC).expect("local context named argument past default"),
        "OK"
    );
}

#[test]
fn context_local_default_cannot_bind_same_named_caller_local() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 val a = 5\n\
        \x20 context(c: C) fun combine(a: Int, b: Int = a): Int = a * 10 + b\n\
        \x20 val actual = with(C()) { combine(a = 1) }\n\
        \x20 return if (actual == 11) \"OK\" else actual.toString()\n\
        }\n";
    let diagnostics = common::front_end_diagnostics_with_stdlib(SRC);
    assert!(
        diagnostics.iter().any(|message| message.contains(
            "local function default argument that references another parameter is not supported"
        )),
        "{diagnostics:?}"
    );
}

#[test]
fn context_local_positional_default_cannot_bind_same_named_caller_local() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 val a = 5\n\
        \x20 context(c: C) fun combine(a: Int, b: Int = a): Int = a * 10 + b\n\
        \x20 val actual = with(C()) { combine(1) }\n\
        \x20 return if (actual == 11) \"OK\" else actual.toString()\n\
        }\n";
    let diagnostics = common::front_end_diagnostics_with_stdlib(SRC);
    assert!(
        diagnostics.iter().any(|message| message.contains(
            "local function default argument that references another parameter is not supported"
        )),
        "{diagnostics:?}"
    );
}
