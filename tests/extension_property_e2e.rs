//! `val` extension properties (`val Recv.name: T get() = …`) lower to a static getter `getName(Recv): T`
//! (like an extension function), with `this` = the receiver; a read `x.name` becomes `getName(x)`. No
//! backing field. Mutable extension properties use the corresponding static setter. Round-tripped on
//! the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn member_extension_property_resolution() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const CASES: &[(&str, &str, Option<&str>)] = &[
        (
            "both receivers in lexical scope",
            "
                class Token(val text: String)
                class Container {
                    private val Token.marker: String get() = text
                    fun read(token: Token): String = token.marker
                }
            ",
            None,
        ),
        (
            "missing dispatch receiver",
            "
                class Token(val text: String)
                class Container {
                    private val Token.marker: String get() = text
                }
                fun read(token: Token): String = token.marker
            ",
            Some("unresolved"),
        ),
        (
            "getter return inferred from extension receiver",
            "
                class Token(val text: String)
                class Container {
                    private val Token.marker get() = this.text
                    fun read(token: Token): String = token.marker
                }
            ",
            None,
        ),
        (
            "public implicit dispatch receiver",
            "
                class Token(val text: String)
                class Container {
                    val Token.marker: String get() = text
                }
                fun read(container: Container, token: Token): String =
                    container.run { token.marker }
            ",
            None,
        ),
        (
            "private implicit dispatch receiver",
            "
                class Token(val text: String)
                class Container {
                    private val Token.marker: String get() = text
                }
                fun read(container: Container, token: Token): String =
                    container.run { token.marker }
            ",
            Some("cannot access 'marker'"),
        ),
        (
            "private inherited member",
            "
                class Token(val text: String)
                open class Base {
                    private val Token.marker: String get() = text
                }
                class Derived : Base() {
                    fun read(token: Token): String = token.marker
                }
            ",
            Some("cannot access 'marker'"),
        ),
        (
            "protected inherited member",
            "
                class Token(val text: String)
                open class Base {
                    protected val Token.marker: String get() = text
                }
                class Derived : Base() {
                    fun read(token: Token): String = token.marker
                }
            ",
            None,
        ),
        (
            "protected member remains inaccessible externally",
            "
                class Token(val text: String)
                class Container {
                    protected val Token.marker: String get() = text
                }
                fun read(container: Container, token: Token): String =
                    container.run { token.marker }
            ",
            Some("cannot access 'marker'"),
        ),
        (
            "supertype extension receiver",
            "
                open class Token(val text: String)
                class SpecialToken(text: String) : Token(text)
                class Container {
                    val Token.marker: String get() = text
                    fun read(token: SpecialToken): String = token.marker
                }
            ",
            None,
        ),
        (
            "generic receiver substitutes return",
            "
                class Container {
                    val <T> T.marker: T get() = this
                    fun read(token: String): Int = token.marker.length
                }
            ",
            None,
        ),
        (
            "member property takes precedence",
            "
                class Container {
                    val String.length: String get() = this
                    fun read(token: String): Int = token.length
                }
            ",
            None,
        ),
        (
            "nested receiver shadows top-level class",
            "
                class Token
                class Container {
                    class Token
                    val Token.marker: Int get() = 1
                    fun read(token: Token): Int = token.marker
                }
            ",
            None,
        ),
        (
            "member extension setter",
            "
                class Token(var text: String)
                class Container {
                    var Token.marker: String
                        get() = text
                        set(value) { text = value }
                    fun write(token: Token, value: String) {
                        token.marker = value
                    }
                }
            ",
            None,
        ),
        (
            "extension visibility does not affect ordinary property",
            "
                class Token
                class Container {
                    val marker: String = \"OK\"
                    private val Token.marker: String get() = \"hidden\"
                }
                fun read(container: Container): String = container.marker
            ",
            None,
        ),
        (
            "ordinary setter takes precedence",
            "
                class Token(var marker: String)
                class Container {
                    var Token.marker: Int
                        get() = 1
                        set(value) {}
                    fun write(token: Token) {
                        token.marker = \"OK\"
                    }
                }
            ",
            None,
        ),
        (
            "inferred overloads keep full receiver identity",
            "
                class Container {
                    val Array<String>.marker get() = \"OK\"
                    val Array<Int>.marker get() = 1
                    fun read(values: Array<Int>): Int = values.marker
                }
            ",
            None,
        ),
        (
            "receiver inference is declaration-order independent",
            "
                class Container {
                    val Token.marker get() = text
                    fun read(token: Token) = token.marker.missing
                }
                class Token(val text: String)
            ",
            Some("unresolved reference 'missing'"),
        ),
        (
            "numeric conversion does not select extension receiver",
            "
                class Container {
                    val Long.marker: Int get() = 1
                    fun read(token: Int): Int = token.marker
                }
            ",
            Some("unresolved"),
        ),
        (
            "bounded generic receiver is more specific",
            "
                open class Token
                class SpecialToken : Token()
                class Container {
                    val <T> T.marker: String get() = \"fallback\"
                    val <T : Token> T.marker: Int get() = 1
                    fun read(token: SpecialToken): Int = token.marker
                }
            ",
            None,
        ),
        (
            "concrete nested receiver beats generic receiver",
            "
                class Container {
                    val <T> Array<T>.marker: String get() = \"generic\"
                    val Array<String>.marker: Int get() = 1
                    fun read(token: Array<String>): Int = token.marker
                }
            ",
            None,
        ),
        (
            "nearest implicit dispatch receiver wins",
            "
                class Token
                open class BaseContainer {
                    val Token.marker: Int get() = 1
                }
                class InnerContainer : BaseContainer()
                class OuterContainer {
                    val Token.marker: String get() = \"outer\"
                    fun read(inner: InnerContainer, token: Token): Int =
                        inner.run { token.marker }
                }
            ",
            None,
        ),
        (
            "class type parameter substitutes from dispatch receiver",
            "
                class Container<T>(private val value: T) {
                    val String.marker: T get() = value
                }
                fun read(container: Container<String>, token: String): Int =
                    container.run { token.marker.length }
            ",
            None,
        ),
        (
            "inferred generic return follows receiver binding",
            "
                class Container {
                    val <T> T.marker get() = this
                    fun read(token: String): Int = token.marker.length
                }
            ",
            None,
        ),
        (
            "inferred nested generic return follows receiver binding",
            "
                class Wrapper<T>(val value: T)
                class Container {
                    val <T> T.wrapped get() = Wrapper(this)
                    fun read(token: String): Int = token.wrapped.value.length
                }
            ",
            None,
        ),
        (
            "inferred nested generic return follows dispatch binding",
            "
                class Wrapper<T>(val value: T)
                class Container<T>(private val value: T) {
                    val String.wrapped get() = Wrapper(value)
                }
                fun read(container: Container<String>, token: String): Int =
                    container.run { token.wrapped.value.length }
            ",
            None,
        ),
        (
            "inferred return preserves generic member result",
            "
                class Wrapper<T>(val value: T)
                class Container<T>(private val value: T) {
                    fun current(): T = value
                    val String.wrapped get() = Wrapper(current())
                }
                fun read(container: Container<String>, token: String): Int =
                    container.run { token.wrapped.value.length }
            ",
            None,
        ),
        (
            "inferred return selects the matching generic member overload",
            "
                class Wrapper<T>(val value: T)
                class Container<T>(private val value: T) {
                    fun choose(token: String): String = token
                    fun choose(number: Int): T = value
                    val String.wrapped get() = Wrapper(choose(\"ok\"))
                }
                fun read(container: Container<Int>, token: String): Int =
                    container.run { token.wrapped.value.length }
            ",
            None,
        ),
        (
            "inferred return substitutes an inherited generic member",
            "
                class Wrapper<T>(val value: T)
                open class Base<T>(private val value: T) {
                    fun current(): T = value
                }
                class Container<T>(value: T) : Base<T>(value) {
                    val String.wrapped get() = Wrapper(current())
                }
                fun read(container: Container<String>, token: String): Int =
                    container.run { token.wrapped.value.length }
            ",
            None,
        ),
        (
            "repeated generic bindings use their common type",
            "
                class Container {
                    fun <T> choose(first: T, second: T): T = first
                    val String.marker get() = choose(\"text\", 1)
                    fun read(token: String): Int = token.marker.length
                }
            ",
            Some("unresolved reference 'length'"),
        ),
        (
            "lambda typing selects the matching member overload",
            "
                class Container {
                    fun <R> apply(value: Int, block: (Int) -> R): R = block(value)
                    fun <R> apply(value: String, block: (String) -> R): R = block(value)
                    val String.marker get() = apply(\"ok\") { it.length }
                    fun read(token: String): Int = token.marker
                }
            ",
            None,
        ),
        (
            "repeated generic bindings keep a common source supertype",
            "
                interface Sized { val size: Int }
                class First : Sized { override val size: Int = 1 }
                class Second : Sized { override val size: Int = 2 }
                class Container {
                    fun <T> choose(first: T, second: T): T = first
                    val String.marker get() = choose(First(), Second()).size
                    fun read(token: String): Int = token.marker
                }
            ",
            None,
        ),
        (
            "named arguments bind generic return slots",
            "
                class Pair<A, B>(val first: A, val second: B)
                class Container {
                    fun <A, B> pair(first: A, second: B): Pair<A, B> =
                        Pair(first, second)
                    val String.marker get() = pair(second = 1, first = \"ok\")
                    fun read(token: String): Int = token.marker.first.length
                }
            ",
            None,
        ),
        (
            "common generic supertype preserves applied arguments",
            "
                interface Holder<T> { val value: T }
                class First : Holder<String> { override val value: String = \"a\" }
                class Second : Holder<String> { override val value: String = \"b\" }
                class Container {
                    fun <T> choose(first: T, second: T): T = first
                    val String.marker get() = choose(First(), Second()).value.length
                    fun read(token: String): Int = token.marker
                }
            ",
            None,
        ),
        (
            "named arguments guide generic lambda planning",
            "
                class Wrapper<T>(val value: T)
                class Container<T>(private val item: T) {
                    fun <R> apply(count: Int, value: T, block: (T) -> R): R =
                        block(value)
                    val String.marker get() =
                        apply(value = item, count = 1) { Wrapper(it) }
                }
                fun read(container: Container<String>, token: String): Int =
                    container.run { token.marker.value.length }
            ",
            None,
        ),
        (
            "lambda planning prefers the specific overload",
            "
                open class Base
                class Specific(val text: String) : Base()
                class Container {
                    fun <R> apply(value: Base, block: (Int) -> R): R = block(1)
                    fun <R> apply(value: Specific, block: (Specific) -> R): R =
                        block(value)
                    val String.marker get() =
                        apply(Specific(\"ok\")) { it.text }
                    fun read(token: String): String = token.marker
                }
            ",
            None,
        ),
        (
            "block getter requires an explicit return type",
            "
                class Container {
                    val String.marker get() { return length }
                }
            ",
            Some("cannot infer the type of property 'marker'"),
        ),
        (
            "inherited dispatch type argument substitutes return",
            "
                open class Base<T>(private val value: T) {
                    val String.marker: T get() = value
                }
                class Derived : Base<String>(\"OK\")
                fun read(dispatch: Derived, token: String): Int =
                    dispatch.run { token.marker.length }
            ",
            None,
        ),
        (
            "unrelated extension receivers remain ambiguous",
            "
                interface Left
                interface Right
                class Both : Left, Right
                class Container {
                    val Left.marker: String get() = \"left\"
                    val Right.marker: Int get() = 1
                    fun read(token: Both) = token.marker
                }
            ",
            Some("overload resolution ambiguity"),
        ),
        (
            "read-only member blocks extension setter",
            "
                class Container {
                    var String.length: Int
                        get() = 1
                        set(value) {}
                    fun write(token: String) {
                        token.length = 1
                    }
                }
            ",
            Some("'val' cannot be reassigned"),
        ),
        (
            "property type parameter shadows class parameter",
            "
                class Container<T> {
                    val <T> T.marker: T get() = this
                    fun read(token: String): Int = token.marker.length
                }
            ",
            None,
        ),
        (
            "receiver specificity beats nearer owner",
            "
                open class BaseContainer {
                    val String.marker: Int get() = 1
                }
                class Container : BaseContainer() {
                    val Any.marker: String get() = \"fallback\"
                    fun read(token: String): Int = token.marker
                }
            ",
            None,
        ),
        (
            "generic interface dispatch argument substitutes return",
            "
                interface BaseContainer<T> {
                    fun value(): T
                    val String.marker: T get() = value()
                }
                class Container : BaseContainer<String> {
                    override fun value(): String = \"OK\"
                }
                fun read(dispatch: Container, token: String): Int =
                    dispatch.run { token.marker.length }
            ",
            None,
        ),
        (
            "transitive dispatch argument substitutes return",
            "
                open class BaseContainer<T>(private val value: T) {
                    val String.marker: T get() = value
                }
                open class MiddleContainer<U>(value: U) : BaseContainer<U>(value)
                class Container : MiddleContainer<String>(\"OK\")
                fun read(dispatch: Container, token: String): Int =
                    dispatch.run { token.marker.length }
            ",
            None,
        ),
        (
            "inferred getter sees inherited receiver property",
            "
                open class TokenBase(val text: String)
                class Token(text: String) : TokenBase(text)
                class Container {
                    val Token.marker get() = text
                    fun read(token: Token) = token.marker.missing
                }
            ",
            Some("unresolved reference 'missing'"),
        ),
        (
            "inferred getter substitutes inherited receiver property",
            "
                interface Holder<T> { val value: T }
                class Token(override val value: String) : Holder<String>
                class Container {
                    val Token.marker get() = value
                    fun read(token: Token): Int = token.marker.length
                }
            ",
            None,
        ),
    ];

    for &(case, source, expected_diagnostic) in CASES {
        let diagnostics = common::front_end_diagnostics(
            source,
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
        if let Some(expected) = expected_diagnostic {
            assert!(
                diagnostics.iter().any(|message| message.contains(expected)),
                "{case}: expected diagnostic containing {expected:?}: {diagnostics:?}"
            );
        } else {
            assert!(
                diagnostics.is_empty(),
                "{case}: unexpected diagnostics: {diagnostics:?}"
            );
        }
    }
}

#[test]
fn imported_extension_property_keeps_declaration_identity() {
    let model = "\
package sample.model

class Item
";
    let first = "\
package sample.first

import sample.model.Item

var Item.label: String
    get() = \"OK\"
    set(value) {}

val <T> T.marker: String
    get() = \"\"
";
    let second = "\
package sample.second

import sample.model.Item

val Item.label: Int
    get() = 7

val Item.marker: String
    get() = \"WRONG\"
";
    let entry = "\
package sample.second

import sample.first.label
import sample.first.marker
import sample.model.Item

fun box(): String {
    val item = Item()
    item.label = \"ignored\"
    val bound = item::label
    bound.set(\"ignored\")
    val unbound = Item::label
    unbound.set(item, \"ignored\")
    val arbitrary = Item()::label
    arbitrary.set(\"ignored\")
    return if (
        item.label == \"OK\" &&
        bound.get() == \"OK\" &&
        unbound.get(item) == \"OK\" &&
        arbitrary.get() == \"OK\" &&
        item.marker == \"\"
    ) \"OK\" else \"FAIL\"
}
";

    common::expect_front_end_ok_files_with_stdlib(
        &[model, first, second, entry],
        "imported extension property",
    );
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Model", model),
            ("First", first),
            ("Second", second),
            ("Entry", entry),
        ],
        "imported extension property",
    );
}

#[test]
fn aliased_extension_property_import_selects_declaration() {
    let model = "package sample.model\nclass Item\n";
    let first = "\
package sample.first
import sample.model.Item
val Item.label: String get() = \"OK\"
";
    let second = "\
package sample.second
import sample.model.Item
val Item.label: Int get() = 1
";
    let entry = "\
package sample.use
import sample.first.label as selectedLabel
import sample.model.Item
fun box(): String = Item().selectedLabel
";

    common::expect_box_ok_files_with_stdlib(
        &[
            ("Model", model),
            ("First", first),
            ("Second", second),
            ("Entry", entry),
        ],
        "aliased extension property",
    );
}

#[test]
fn cross_file_unit_extension_property_uses_value_descriptors() {
    let model = "package sample.model\nclass Item(var touched: Boolean)\n";
    let extension = "\
package sample.extension
import sample.model.Item
var Item.signal: Unit
    get() = Unit
    set(value) { touched = value == Unit }
";
    let entry = "\
package sample.use
import sample.extension.signal
import sample.model.Item
fun box(): String {
    val item = Item(false)
    item.signal = Unit
    val direct: Unit = item.signal
    return if (item.touched && direct == Unit) \"OK\" else \"FAIL\"
}
";

    common::expect_box_ok_files_with_stdlib(
        &[("Model", model), ("Extension", extension), ("Entry", entry)],
        "cross-file Unit extension property",
    );
}

#[test]
fn private_extension_properties_are_file_scoped() {
    let first = "\
package sample

private val String.code: String
    get() = \"O\"

fun first(): String = \"\".code
";
    let second = "\
package sample

private val String.code: String
    get() = \"K\"

fun second(): String = \"\".code
";
    let entry = "\
package sample

fun box(): String = first() + second()
";

    common::expect_front_end_ok_files_with_stdlib(
        &[first, second, entry],
        "private extension properties",
    );
    common::expect_box_ok_files_with_stdlib(
        &[("First", first), ("Second", second), ("Entry", entry)],
        "private extension properties",
    );
}

#[test]
fn same_precedence_extension_properties_are_ambiguous() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            "package one\nval String.code: String get() = \"one\"",
            "package two\nval String.code: Int get() = 2",
            "package use\nimport one.*\nimport two.*\nfun read(): Any = \"\".code",
        ],
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("overload resolution ambiguity")),
        "{diagnostics:?}"
    );
}

#[test]
fn member_extension_classpath_members() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const CASES: &[(&str, &str)] = &[
        (
            "receiver specificity",
            "
                class Container {
                    val Any.marker: String get() = \"fallback\"
                    val CharSequence.marker: Int get() = 1
                    fun read(token: String): Int = token.marker
                }
            ",
        ),
        (
            "receiver classpath properties",
            "
                import kotlin.reflect.KClass

                class Container {
                    val KClass<*>.label: String
                        get() = buildString {
                            append(simpleName)
                            if (typeParameters.isNotEmpty()) {
                                append(typeParameters.size)
                            }
                        }
                }
            ",
        ),
        (
            "function receiver survives a nested receiver lambda",
            "
                import kotlin.reflect.KClass

                class Container {
                    fun KClass<*>.label(): String =
                        buildString {
                            append(simpleName)
                            if (typeParameters.isNotEmpty()) {
                                append(typeParameters.size)
                            }
                        }
                }
            ",
        ),
        (
            "extension property beats dispatch property",
            "
                class Token(val value: String)
                class Container(val value: Int) {
                    val Token.marker: Int get() = value.length
                }
            ",
        ),
        (
            "extension property wins after a nested receiver misses",
            "
                class Token(val value: String)
                class Container(val value: Int) {
                    val Token.marker: Int
                        get() = buildString {
                            append(value.length)
                        }.length
                }
            ",
        ),
        (
            "setter writes the dispatch receiver",
            "
                class Token
                class Container(var count: Int) {
                    var Token.marker: Int
                        get() = count
                        set(value) {
                            count = value
                        }
                }
            ",
        ),
        (
            "function updates the dispatch receiver",
            "
                class Token
                class Container(var count: Int) {
                    fun Token.update() {
                        count++
                    }
                }
            ",
        ),
    ];

    for &(case, source) in CASES {
        let diagnostics = common::front_end_diagnostics(
            source,
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
        assert!(
            diagnostics.is_empty(),
            "{case}: unexpected diagnostics: {diagnostics:?}"
        );
    }
}

#[test]
fn member_extension_receiver_inference_is_cross_file_order_independent() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            "
                class Container {
                    val Token.marker get() = text
                    fun read(token: Token) = token.marker.missing
                }
            ",
            "class Token(val text: String)",
        ],
        &[],
        None,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'missing'")),
        "expected the inferred String return to expose the bad chained read: {diagnostics:?}"
    );
}

#[test]
fn extension_receiver_writes_lower_from_the_selected_implicit_receiver() {
    const CASES: &[(&str, &str)] = &[
        (
            "assignment",
            "
                class State(var amount: Int)
                fun State.replace(next: Int): Int {
                    amount = next
                    return amount
                }
                fun box(): String =
                    if (State(1).replace(7) == 7) \"OK\" else \"fail\"
            ",
        ),
        (
            "increment",
            "
                class State(var amount: Int)
                fun State.advance(): Int {
                    amount++
                    return amount
                }
                fun box(): String =
                    if (State(1).advance() == 2) \"OK\" else \"fail\"
            ",
        ),
        (
            "interface accessors",
            "
                interface State {
                    var amount: Int
                }
                class StateImpl(override var amount: Int) : State
                fun State.advance(): Int {
                    amount++
                    return amount
                }
                fun box(): String =
                    if (StateImpl(1).advance() == 2) \"OK\" else \"fail\"
            ",
        ),
    ];

    for &(case, source) in CASES {
        assert_eq!(run(source).as_deref(), Some("OK"), "{case}");
    }

    let classpath_result = common::run_box_against(
        "implicit_property_write",
        "
            package fixture
            class State(var amount: Int)
        ",
        "
            import fixture.State
            fun State.advance(): Int {
                amount++
                return amount
            }
            fun box(): String =
                if (State(1).advance() == 2) \"OK\" else \"fail\"
        ",
    );
    if let Some(result) = classpath_result {
        assert_eq!(result, "OK", "classpath accessors");
    }
}

#[test]
fn extension_property_user_class_bare_member() {
    const SRC: &str = "class A(val n: Int)\n\
val A.doubled: Int get() = n * 2\n\
fun box(): String = if (A(21).doubled == 42) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("ext property on user class compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_on_primitive_this() {
    const SRC: &str = "val Int.sq: Int get() = this * this\n\
fun box(): String = if (5.sq == 25) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("ext property on Int compiles + runs"), "OK");
}

#[test]
fn extension_property_with_own_type_param() {
    // `val <T> Array<T>.length` declares a generic type parameter on the extension property; `T`
    // scopes over the receiver type. It erases like a function's — the getter reads `size`.
    const SRC: &str = "val <T> Array<T>.length: Int get() = this.size\n\
fun box(): String = if (arrayOfNulls<Int>(10).length == 10) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("generic extension property compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_type_param_bound_scopes_accessor() {
    const SRC: &str = "val <T: String> T.first: Char get() = this[0]\n\
fun box(): String = if (\"OK\".first == 'O') \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("bounded generic extension property compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_on_bare_type_param_receiver() {
    // `val <T> T.tag` has a free type-parameter receiver; it erases to `Any` and applies to any
    // receiver (String, Int, …). Both reads resolve to the one static getter.
    const SRC: &str = "val <T> T.tag: String get() = \"K\"\n\
fun box(): String = if (\"x\".tag + 1.tag == \"KK\") \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("type-parameter-receiver extension property compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_on_string() {
    const SRC: &str = "val String.firstC: Char get() = this[0]\n\
fun box(): String = if (\"OK\".firstC == 'O') \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("ext property on String compiles + runs"),
        "OK"
    );
}

#[test]
fn nullable_and_generic_extension_properties_accept_nullable_receivers() {
    const SRC: &str = "val String?.nullableTag: String get() = if (this == null) \"O\" else this\n\
val <T> T.genericTag: String get() = \"K\"\n\
fun box(): String {\n\
  val value: String? = null\n\
  return value.nullableTag + value.genericTag\n\
}\n";
    assert_eq!(
        run(SRC).expect("nullable extension receivers compile + run"),
        "OK"
    );
}

#[test]
fn string_length_member_precedes_extension_property() {
    const SRC: &str = "val String.length: String get() = \"bad\"\n\
fun box(): String {\n\
  val s: String? = \"OK\"\n\
  val direct = \"OK\".length\n\
  val safe = s?.length ?: 0\n\
  return if (direct == 2 && safe == 2) \"OK\" else \"no\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("String.length member keeps precedence"),
        "OK"
    );
}
