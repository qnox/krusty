//! Context parameters (`context(a: A) fun f()`): the leading context receivers are supplied IMPLICITLY
//! at the call site — from the enclosing `with`-block receiver or an enclosing context parameter —
//! rather than positionally. The checker resolves each context parameter to an in-scope
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
fn contextual_member_extension_retains_default_type_after_unnamed_context() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        class Context\n\
        class Extended\n\
        class Containing {\n\
        \x20 context(_: Context) fun Extended.foo(value: String = \"OK\") = value\n\
        }\n\
        fun box(): String = with(Containing()) {\n\
        \x20 with(Context()) { Extended().foo() }\n\
        }\n";

    assert_eq!(
        run(SRC).expect("default after unnamed context parameter"),
        "OK"
    );
}

#[test]
fn named_and_anonymous_context_parameters_keep_their_declaration_positions() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        context(_: String, count: Int) fun inspect(): String = count.toString()\n\
        fun box(): String = with(\"unused\") { with(42) {\n\
        \x20 if (inspect() == \"42\") \"OK\" else \"fail\"\n\
        } }\n";
    let output = run(SRC).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        );
        panic!("mixed context parameter kinds failed: {diagnostics:?}")
    });
    assert_eq!(output, "OK");
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
fn ordinary_local_value_is_not_an_implicit_context_argument() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        class A(val x: String) { fun foo(): String = x }\n\
        var result = \"\"\n\
        context(a: A)\n\
        fun test1() { result = a.foo() }\n\
        fun box(): String {\n\
        \x20 val a = A(\"OK\")\n\
        \x20 test1()\n\
        \x20 return result\n\
        }\n";
    let (reference, _) = common::kotlinc_source_result("ContextFromOrdinaryLocal", SRC);
    assert_ne!(
        reference, 0,
        "kotlinc accepted an ordinary local as context"
    );
    assert_eq!(
        common::front_end_diagnostics(
            SRC,
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        ),
        vec!["no context argument for 'a: A' found."]
    );
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
            val token = Token()
            return service.read() + service.read(token = token)
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
    let out = common::expect_box_run_against_ref("explicitcontextdependency", LIBRARY, CALLER)
        .expect("reference compiler unavailable");
    assert_eq!(out, "OK");
}

#[test]
fn legacy_context_receivers_keep_their_metadata_shape_across_a_dependency() {
    const LIBRARY: &str = r#"
        // LANGUAGE: +ContextReceivers
        package dependency
        context(String)
        fun read(): String = this@String
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextReceivers
        import dependency.read
        fun box(): String = with("OK") { read() }
    "#;
    let out = common::expect_box_run_against_ref("legacycontextmetadata", LIBRARY, CALLER)
        .expect("reference compiler unavailable");
    assert_eq!(out, "OK");
}

#[test]
fn anonymous_context_parameters_keep_their_metadata_shape_across_a_dependency() {
    const LIBRARY: &str = r#"
        // LANGUAGE: +ContextParameters
        package dependency
        context(_: String)
        fun read(): String = "OK"
    "#;
    const CALLER: &str = r#"
        // LANGUAGE: +ContextParameters
        import dependency.read
        fun box(): String = with("scope") { read() }
    "#;
    let out = common::expect_box_run_against_ref("anonymouscontextmetadata", LIBRARY, CALLER)
        .expect("reference compiler unavailable");
    assert_eq!(out, "OK");
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
    let out = common::expect_box_run_against_ref("contextdependencydefaults", LIBRARY, CALLER)
        .expect("reference compiler unavailable");
    assert_eq!(out, "OK");
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
fn class_member_and_top_level_with_context() {
    // kotlinc tower: a dispatch or extension receiver (Member) beats a top-level callable, which
    // beats a context-parameter receiver. Inside `context(A) { }`, `foo` and `b` are the contextual
    // top-level declarations; `A().foo()` and `A().b` stay the class members.
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class A {
            fun foo(): String = "class fun"
            val b: String = "class val"
        }
        fun <A, R> context(context: A, block: context(A) () -> R): R = block(context)
        context(a: A)
        fun foo(): String = "context fun"
        context(a: A)
        val b: String
            get() = "context val"
        fun box(): String {
            return if (
                (A().foo() == "class fun") &&
                (A().b == "class val") &&
                (context(A()) {
                    foo() == "context fun" && b == "context val"
                })
            ) "OK" else "NOK"
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn generic_property_with_context_parameters() {
    // The property's own type parameters are not the class's. Metadata must name `X` and `Y`
    // when it records `Entries<X, Y>` and the context parameters, or emission panics.
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Entries<K, V>
        class C<Z> {
            context(x: X, y: Y)
            var <X, Y> ctx: Entries<X, Y>
                get() = Entries<X, Y>()
                set(value) {}
        }
        fun box(): String {
            val c = C<String>()
            return "OK"
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn context_interface_delegation_forwards_both_interfaces() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        data class Language(var name: String)
        interface LoggingContext {
            fun log(level: Int, message: String)
        }
        interface SaveRepository<T> {
            context(context: LoggingContext)
            fun save(content: T)
        }
        context(context: LoggingContext, repository: SaveRepository<Language>)
        fun startBusinessOperation() {
            context.log(0, "Operation has started")
            repository.save(Language("Kotlin"))
        }
        class CompositeContext(c1: LoggingContext, c2: SaveRepository<Language>): LoggingContext by c1, SaveRepository<Language> by c2
        fun box(): String {
            val loggingCtx = object : LoggingContext {
                override fun log(level: Int, message: String) {}
            }
            val saveCtx = object : SaveRepository<Language> {
                context(context: LoggingContext)
                override fun save(content: Language) {
                    context.log(message = "Saving $content", level = 123)
                }
            }
            with(CompositeContext(loggingCtx, saveCtx)) {
                startBusinessOperation()
            }
            return "OK"
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn missing_context_names_the_parameter() {
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
        vec!["no context argument for 'c: C' found."]
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
    common::expect_true_e2e("functional_interface_member_context_is_implicit", SRC, &[]);
}

#[test]
fn contextual_member_failures_keep_their_inferred_result() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(c: C)
    fun accept(value: String): String = value
}
fun inferred(api: Api) = with(C()) { api.accept(null).length }
fun missing(api: Api) = api.accept("x").length
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ContextRejected.kt".to_string(),
            line: 7,
            column: 49,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
        common::CompilerError {
            file: "ContextRejected.kt".to_string(),
            line: 8,
            column: 29,
            message: "no context argument for 'c: C' found.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the contextual failure fixtures"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the contextual failure fixtures"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_contextual_member_reapplies_its_demanded_return_binding() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class Api {
    context(c: T)
    fun <T> String.accept(value: String) = c
}
fun use(api: Api) = with(api) { with(1) { "".accept(null).inc() } }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextPending.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ContextPending.kt".to_string(),
        line: 6,
        column: 53,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected contextual call"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the rejected contextual call"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_contextual_generic_member_uses_its_context_binding() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class Api {
    context(x: T)
    fun <T : Any> accept(value: T): T = value
}
fun use(api: Api) = with(1) { api.accept(null).inc() }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextBoundRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ContextBoundRejected.kt".to_string(),
        line: 6,
        column: 42,
        message: "null cannot be a value of a non-null type 'Int'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the null argument");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_contextual_generic_member_keeps_bindings_before_a_missing_context() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(x: T, missing: C)
    fun <T : Any> accept(value: T): T = value
}
fun use(api: Api) = with(1) { api.accept(null).inc() }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("PartialContextBoundRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "PartialContextBoundRejected.kt".to_string(),
            line: 7,
            column: 35,
            message: "no context argument for 'missing: C' found.".to_string(),
        },
        common::CompilerError {
            file: "PartialContextBoundRejected.kt".to_string(),
            line: 7,
            column: 42,
            message: "null cannot be a value of a non-null type 'Int'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the missing context and null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the missing context and null argument"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_contextual_generic_member_keeps_bindings_after_a_missing_context() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(missing: C, x: T)
    fun <T : Any> accept(value: T): T = value
}
fun use(api: Api) = with(1) { api.accept(null).inc() }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("LaterContextBoundRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "LaterContextBoundRejected.kt".to_string(),
            line: 7,
            column: 35,
            message: "no context argument for 'missing: C' found.".to_string(),
        },
        common::CompilerError {
            file: "LaterContextBoundRejected.kt".to_string(),
            line: 7,
            column: 42,
            message: "null cannot be a value of a non-null type 'Int'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the missing context and null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the missing context and null argument"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn rejected_signature_recovery_seeds_explicit_type_arguments_before_context_lookup() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class Api {
    context(x: T)
    fun <T> accept(value: String): T = x
}
fun use(api: Api) = with(1) { api.accept<String>(null).length }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ExplicitContextBoundRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ExplicitContextBoundRejected.kt".to_string(),
            line: 6,
            column: 35,
            message: "no context argument for 'x: T' found.".to_string(),
        },
        common::CompilerError {
            file: "ExplicitContextBoundRejected.kt".to_string(),
            line: 6,
            column: 50,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the explicit context mismatch"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the explicit context mismatch"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn rejected_signature_recovery_preserves_explicit_binding_during_context_match() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class Api {
    context(x: T)
    fun <T> accept(value: String): T = x
}
fun use(api: Api) = with("") { api.accept<CharSequence>(null).length }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ExplicitContextSubtypeRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ExplicitContextSubtypeRejected.kt".to_string(),
        line: 6,
        column: 57,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the null argument");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn rejected_argument_diagnostic_preserves_explicit_binding_during_context_match() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class Api {
    context(x: T)
    fun <T> accept(value: T): T = value
}
fun use(api: Api) = with("") { api.accept<CharSequence>(null) }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ExplicitContextValueRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ExplicitContextValueRejected.kt".to_string(),
        line: 6,
        column: 57,
        message: "null cannot be a value of a non-null type 'CharSequence'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the null argument");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn rejected_signature_recovery_preserves_expected_binding_during_context_match() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class Api {
    context(x: T)
    fun <T> accept(value: String): T = x
}
fun use(api: Api): CharSequence = with("") { api.accept(null) }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ExpectedContextSubtypeRejected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ExpectedContextSubtypeRejected.kt".to_string(),
        line: 6,
        column: 57,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the null argument"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the null argument");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_member_extension_uses_its_generic_dispatch_receiver() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api<T>(val x: T) {
    context(c: C)
    fun String.accept(value: String) = x
}
fun use(api: Api<Int>) = with(api) { with(C()) { "".accept(null).inc() } }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextDispatchPending.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ContextDispatchPending.kt".to_string(),
        line: 7,
        column: 60,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the generic dispatch failure"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the generic dispatch failure"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_unavailable_context_retains_its_generic_dispatch_result() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api<T>(val x: T) {
    context(c: C)
    fun String.accept(value: String) = x
}
fun use(api: Api<Int>) = with(api) { "".accept(null).inc() }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextUnavailableDispatchPending.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ContextUnavailableDispatchPending.kt".to_string(),
            line: 7,
            column: 41,
            message: "no context argument for 'c: C' found.".to_string(),
        },
        common::CompilerError {
            file: "ContextUnavailableDispatchPending.kt".to_string(),
            line: 7,
            column: 48,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the unavailable context failure"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the unavailable context failure"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_unavailable_context_retains_bindings_from_available_context_parameters() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(x: T, missing: C)
    fun <T> String.accept(value: String) = x
}
fun use(api: Api) = with(api) { with(1) { "".accept(null).inc() } }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("PartialContextPending.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "PartialContextPending.kt".to_string(),
            line: 7,
            column: 46,
            message: "no context argument for 'missing: C' found.".to_string(),
        },
        common::CompilerError {
            file: "PartialContextPending.kt".to_string(),
            line: 7,
            column: 53,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the partially available context call"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the partially available context call"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_dependency_member_retains_bindings_from_available_context_parameters() {
    const LIBRARY: &str = r#"// LANGUAGE: +ContextParameters
package dependency
class C
class Api {
    context(x: T, missing: C)
    fun <T> accept(value: String): T = x
}
"#;
    const USE: &str = r#"// LANGUAGE: +ContextParameters
import dependency.Api
fun use(api: Api) = with(1) { api.accept(null).inc() }
"#;
    let Some(library) = common::kotlinc_lib_out(&[("Dependency.kt", LIBRARY)]) else {
        return;
    };
    let result = common::compiler_diagnostics_with_reference_args(
        &[("DependencyUse.kt", USE)],
        &[library, common::stdlib_jar()],
        &common::language_directives::kotlinc_args(USE),
    );
    let expected = [
        common::CompilerError {
            file: "DependencyUse.kt".to_string(),
            line: 3,
            column: 35,
            message: "no context argument for 'missing: C' found.".to_string(),
        },
        common::CompilerError {
            file: "DependencyUse.kt".to_string(),
            line: 3,
            column: 42,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the dependency context failure"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the dependency context failure"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_explicit_result_retains_bindings_from_available_context_parameters() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(x: T, missing: C)
    fun <T> String.accept(value: String): T = x
}
fun use(api: Api) = with(api) { with(1) { "".accept(null).inc() } }
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ExplicitPartialContextPending.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ExplicitPartialContextPending.kt".to_string(),
            line: 7,
            column: 46,
            message: "no context argument for 'missing: C' found.".to_string(),
        },
        common::CompilerError {
            file: "ExplicitPartialContextPending.kt".to_string(),
            line: 7,
            column: 53,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the explicit-result partial context call"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the explicit-result partial context call"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn a_rejected_direct_member_uses_its_explicit_receiver() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api<T>(val x: T) {
    context(c: C)
    fun accept(value: String) = x
    fun use(other: Api<String>) = with(C()) { other.accept(null).length }
}
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextExplicitReceiverPending.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ContextExplicitReceiverPending.kt".to_string(),
        line: 6,
        column: 60,
        message: "null cannot be a value of a non-null type 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the explicit receiver failure"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the explicit receiver failure"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_unavailable_context_is_not_an_unresolved_generic_member() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(c: C)
    fun <T> create(): T = TODO()
}
fun unfixed(api: Api) = api.create()
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextUnfixed.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ContextUnfixed.kt".to_string(),
            line: 7,
            column: 29,
            message: "cannot infer type for type parameter 'T'. Specify it explicitly.".to_string(),
        },
        common::CompilerError {
            file: "ContextUnfixed.kt".to_string(),
            line: 7,
            column: 29,
            message: "no context argument for 'c: C' found.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the unfixed call"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the unfixed call");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_expected_result_fixes_an_unavailable_contextual_generic_member() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(c: C)
    fun <T> create(): T = TODO()
}
fun expected(api: Api): String = api.create()
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextExpected.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [common::CompilerError {
        file: "ContextExpected.kt".to_string(),
        line: 7,
        column: 38,
        message: "no context argument for 'c: C' found.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the missing context"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the missing context");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_unmapped_contextual_member_is_not_unresolved() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    context(c: C)
    fun accept(value: String): String = value
}
fun missing(api: Api) = api.accept()
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextUnmapped.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ContextUnmapped.kt".to_string(),
            line: 7,
            column: 29,
            message: "no context argument for 'c: C' found.".to_string(),
        },
        common::CompilerError {
            file: "ContextUnmapped.kt".to_string(),
            line: 7,
            column: 29,
            message: "no value passed for parameter 'value'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the unmapped call"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the unmapped call");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn context_availability_does_not_split_a_rejected_overload_family() {
    const SRC: &str = r#"// LANGUAGE: +ContextParameters
class C
class Api {
    fun pick(value: String): Int = 0
    fun pick(value: CharSequence): Int = 0
    context(c: C)
    fun pick(value: Any): String = ""
}
fun mixed(api: Api) = api.pick(null).length
"#;
    let result = common::compiler_diagnostics_with_reference_args(
        &[("ContextMixedFamily.kt", SRC)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(SRC),
    );
    let expected = [
        common::CompilerError {
            file: "ContextMixedFamily.kt".to_string(),
            line: 9,
            column: 32,
            message: "null cannot be a value of a non-null type 'String'.".to_string(),
        },
        common::CompilerError {
            file: "ContextMixedFamily.kt".to_string(),
            line: 9,
            column: 38,
            message: "unresolved reference 'length' on receiver of type 'Int'.".to_string(),
        },
    ];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the rejected call"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(result.krusty_code, 0, "krusty accepted the rejected call");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
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
    common::expect_true_e2e(
        "context_local_function_maps_reordered_named_arguments",
        SRC,
        &[],
    );
    assert_eq!(run(SRC).expect("local context named arguments"), "OK");
}

#[test]
fn context_top_level_function_maps_named_argument_past_default() {
    const SRC: &str = "class C\n\
        context(c: C) fun choose(a: Int = 7, b: String): String = b\n\
        fun box(): String = with(C()) { choose(b = \"OK\") }\n";
    common::expect_true_e2e(
        "context_top_level_function_maps_named_argument_past_default",
        SRC,
        &[],
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
    common::expect_true_e2e(
        "context_local_function_maps_named_argument_past_default",
        SRC,
        &[],
    );
    assert_eq!(
        run(SRC).expect("local context named argument past default"),
        "OK"
    );
}

#[test]
fn context_local_named_default_binds_preceding_parameter() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 val a = 5\n\
        \x20 context(c: C) fun combine(a: Int, b: Int = a): Int = a * 10 + b\n\
        \x20 val actual = with(C()) { combine(a = 1) }\n\
        \x20 return if (actual == 11) \"OK\" else actual.toString()\n\
        }\n";
    common::expect_true_e2e(
        "context_local_named_default_binds_preceding_parameter",
        SRC,
        &[],
    );
    assert_eq!(run(SRC).expect("local context named default"), "OK");
}

#[test]
fn context_local_positional_default_binds_preceding_parameter() {
    const SRC: &str = "class C\n\
        fun box(): String {\n\
        \x20 val a = 5\n\
        \x20 context(c: C) fun combine(a: Int, b: Int = a): Int = a * 10 + b\n\
        \x20 val actual = with(C()) { combine(1) }\n\
        \x20 return if (actual == 11) \"OK\" else actual.toString()\n\
        }\n";
    common::expect_true_e2e(
        "context_local_positional_default_binds_preceding_parameter",
        SRC,
        &[],
    );
    assert_eq!(run(SRC).expect("local context positional default"), "OK");
}

#[test]
fn contextual_class_retains_context_for_member_bodies() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextReceivers
        class A(val value: String)
        context(A)
        class B { fun result(): String = value }
        fun box(): String = with(A("OK")) { B() }.result()
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn named_context_parameter_on_a_class_binds_its_member_body() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        context(label: String)
        class Holder { fun result(): String = label }
        fun box(): String = with("OK") { Holder() }.result()
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn contextual_class_retains_multiple_contexts_in_source_order() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextReceivers
        class A(val value: String)
        class B(val value: String)
        context(A, B)
        class C { fun result(): String = this@A.value + this@B.value }
        fun box(): String {
            val value = with(A("O")) { with(B("K")) { C() } }
            return value.result()
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn contextual_class_constructors_forward_the_same_context_prefix() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextReceivers
        class A(val value: String)
        context(A)
        class B(oValue: Boolean = true, kValue: Boolean = true) {
            var o: Boolean
            var k: Boolean
            init { o = oValue; k = kValue }
            constructor(oValue: String, kValue: String) : this(
                oValue == "O",
                kValue == "K"
            )
            fun result(): String = if (o && k) value else "fail"
        }
        fun box(): String = with(A("OK")) {
            val primary = B(true, true).result()
            val secondary = B("O", "K").result()
            val defaulted = B().result()
            if (primary == "OK" && secondary == "OK" && defaulted == "OK") "OK" else "fail"
        }
    "#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn same_type_context_dispatch_and_extension_stay_distinct() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        fun <A, R> context(context: A, block: context(A) () -> R): R = block(context)
        class A(val a: String = "d ") {
            context(a: A)
            fun A.funMember(): String {
                return a.a + this@A.a + this.a
            }
            context(a: A)
            val A.propertyMember: String
                get() = a.a + this@A.a + this.a
            fun simpleUsageInsideClass(): String {
                return funMember() + propertyMember
            }
            fun usageWithThisInsideClass(): String {
                return this.funMember() + this.propertyMember
            }
            fun usageWithExtensionInsideClass(): String {
                return A("e ").funMember() + A("e ").propertyMember
            }
            fun usageWithContextAndExtensionInsideClass(): String {
                var temp = ""
                context(A("c ")) {
                    @Suppress("RECEIVER_SHADOWED_BY_CONTEXT_PARAMETER")
                    temp = A("e ").funMember() + A("e ").propertyMember
                }
                return temp
            }
        }
        fun simpleUsageOutsideClass(): String {
            with(A("d ")) {
                return funMember() + propertyMember
            }
        }
        fun usageWithExtensionOutsideClass(): String {
            with(A("d ")) {
                return A("e ").funMember() + A("e ").propertyMember
            }
        }
        fun usageWithExtensionAndContextOutsideClass(): String {
            var temp = ""
            with(A("d ")) {
                context(A("c ")) {
                    temp = (@Suppress("RECEIVER_SHADOWED_BY_CONTEXT_PARAMETER") A("e ").funMember()) + (@Suppress("RECEIVER_SHADOWED_BY_CONTEXT_PARAMETER") A("e ").propertyMember)
                }
            }
            return temp
        }
        fun box(): String {
            return if (
                (A().simpleUsageInsideClass() == "d d d d d d ") &&
                (A().usageWithThisInsideClass() == "d d d d d d ") &&
                (A().usageWithExtensionInsideClass() == "d d e d d e ") &&
                (A().usageWithContextAndExtensionInsideClass() == "c d e c d e ") &&
                (simpleUsageOutsideClass() == "d d d d d d ") &&
                (usageWithExtensionOutsideClass() == "d d e d d e ") &&
                (usageWithExtensionAndContextOutsideClass() == "c d e c d e ")
            ) "OK" else "NOK"
        }
    "#;
    common::expect_box_ok_with_stdlib(SRC, "SameTypeContextDispatchAndExtension");
}

/// `getValue`, `setValue`, and `provideDelegate` are separate convention calls, and each one can
/// bind a different dispatch receiver. The context receiver is lexically nearer, but the ordinary
/// `with` receiver declares the same delegate operators and wins all three.
#[test]
fn ordinary_delegate_operator_beats_a_nearer_context_receiver() {
    const SRC: &str = r#"
        // LANGUAGE: +ContextParameters
        class Ordinary(val tag: String) {
            operator fun Cell.getValue(thisRef: Any?, property: Any?): String = "get-" + tag
            operator fun VarCell.getValue(thisRef: Any?, property: Any?): String = "get"
            operator fun VarCell.setValue(thisRef: Any?, property: Any?, value: String) {
                seen = "set-" + tag
            }
            operator fun Factory.provideDelegate(thisRef: Any?, property: Any?): Provided =
                Provided("provide-" + tag)
        }
        class Contextual(val tag: String) {
            operator fun Cell.getValue(thisRef: Any?, property: Any?): String = "get-" + tag
            operator fun VarCell.getValue(thisRef: Any?, property: Any?): String = "get"
            operator fun VarCell.setValue(thisRef: Any?, property: Any?, value: String) {
                seen = "set-" + tag
            }
            operator fun Factory.provideDelegate(thisRef: Any?, property: Any?): Provided =
                Provided("provide-" + tag)
        }
        class Cell
        class VarCell { var seen: String = "unset" }
        class Factory
        class Provided(val tag: String) {
            operator fun getValue(thisRef: Any?, property: Any?): String = tag
        }
        fun <A, R> context(value: A, block: context(A) () -> R): R = block(value)
        fun box(): String {
            val seen = with(Ordinary("ordinary")) {
                context(Contextual("context")) {
                    val read: String by Cell()
                    val cell = VarCell()
                    var slot: String by cell
                    slot = "x"
                    val made: String by Factory()
                    read + "|" + cell.seen + "|" + made
                }
            }
            val expected = "get-ordinary|set-ordinary|provide-ordinary"
            return if (seen == expected) "OK" else seen
        }
    "#;
    common::expect_box_same_as_kotlinc(SRC, "OrdinaryDelegateBeforeContext");
}

const INFERRED_CONTEXT_PROPERTIES: &str = "// LANGUAGE: +ContextParameters\n\
    class Prefix { val o = \"O\" }\n\
    context(prefix: Prefix)\n\
    val suffixed get() = prefix.o + \"K\"\n\
    context(prefix: Prefix)\n\
    val String.tagged get() = prefix.o + this\n\
    fun Prefix.run(): String = if (suffixed == \"OK\") \"K\".tagged else \"Fail\"\n\
    fun box(): String = Prefix().run()\n";

/// A property whose type its getter infers reads its named context parameters there, as a function
/// body reads its own; the getter's type is the one it infers from them.
#[test]
fn an_inferred_property_getter_reads_its_context_parameters() {
    common::assert_class_code_matches_kotlinc(
        "InferredContextProperties",
        INFERRED_CONTEXT_PROPERTIES,
        "InferredContextPropertiesKt",
    );
    common::expect_box_same_as_kotlinc(INFERRED_CONTEXT_PROPERTIES, "InferredContextPropertiesRun");
}

/// A context parameter is a declared parameter before the vararg, but no source argument fills
/// it: the call's arguments start at the first value parameter, so the vararg packs exactly the
/// positional arguments that reach it, whatever the call form.
const CONTEXT_VARARG_DECLARATIONS: &str = "context(tag: String)\n\
    fun tagged(vararg pairs: Pair<String, Int>, tail: Pair<String, Int>): Int =\n\
    \x20 pairs.size + tail.second + tag.length\n\
    context(tag: String) fun plain(vararg xs: Int): Int = xs.sum() + tag.length\n\
    context(tag: String) fun Int.ext(vararg xs: Int, last: Int = 0): Int = this + xs.size + last + tag.length\n\
    class Box {\n\
    \x20 context(tag: String) fun member(vararg xs: String): Int = xs.size + tag.length\n\
    \x20 context(tag: String) fun Int.memberExt(vararg xs: Int): Int = this + xs.size + tag.length\n\
    \x20 fun run(): Int = with(\"tt\") { member(\"a\", \"b\") + 1.memberExt(4, 5) }\n\
    }\n";

const CONTEXT_VARARG_CALLS: &str = "fun box(): String {\n\
    \x20 val results = with(\"t\") {\n\
    \x20   listOf(\n\
    \x20     tagged(\"a\" to 1, \"b\" to 2, tail = \"c\" to 3),\n\
    \x20     tagged(tail = \"z\" to 5),\n\
    \x20     plain(1, 2, 3),\n\
    \x20     plain(),\n\
    \x20     plain(*intArrayOf(4, 5)),\n\
    \x20     10.ext(1, 2, last = 5),\n\
    \x20     Box().member(\"x\"),\n\
    \x20   )\n\
    \x20 }\n\
    \x20 val expected = listOf(6, 6, 7, 1, 10, 18, 2)\n\
    \x20 if (results != expected) return \"fail $results\"\n\
    \x20 return if (Box().run() == 9) \"OK\" else \"fail member extension ${Box().run()}\"\n\
    }\n";

#[test]
fn varargs_after_context_parameters_take_only_the_arguments_that_reach_them() {
    let src = format!("{CONTEXT_VARARG_DECLARATIONS}{CONTEXT_VARARG_CALLS}");
    common::expect_box_ok_with_stdlib(&src, "Main");
}

#[test]
fn dependency_varargs_after_context_parameters_take_only_the_arguments_that_reach_them() {
    let lib = format!("package lib\n{CONTEXT_VARARG_DECLARATIONS}");
    let main = format!("import lib.*\n{CONTEXT_VARARG_CALLS}");
    if let Some(result) = common::expect_box_run_against("context_vararg_dependency", &lib, &main) {
        assert_eq!(result, "OK");
    }
}

/// Two more callables with a `vararg` after a context parameter: a member-extension `invoke`
/// operator applied to a value, and a local function.
#[test]
fn varargs_after_context_parameters_reach_operator_invokes_and_local_functions() {
    let src = "class Runner {\n\
        \x20 context(tag: String)\n\
        \x20 operator fun Int.invoke(vararg xs: Int): Int = this + tag.length + xs.size\n\
        \x20 fun run(): Int = with(\"t\") { val n = 1; n(2, 3) }\n\
        }\n\
        fun local(): Int {\n\
        \x20 context(tag: String)\n\
        \x20 fun inner(vararg xs: Int, last: Int = 5): Int = tag.length + xs.sum() + last\n\
        \x20 return with(\"ab\") { inner(1, 2) + inner(last = 1) }\n\
        }\n\
        fun box(): String {\n\
        \x20 if (Runner().run() != 4) return \"fail invoke ${Runner().run()}\"\n\
        \x20 return if (local() == 10 + 3) \"OK\" else \"fail local ${local()}\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "Main");
}
