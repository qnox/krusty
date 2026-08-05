use super::common;

#[test]
fn property_type_from_a_classpath_static_call() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Item.java".into(),
            r#"
                package fixtures;
                public final class Item {
                    private final String value;
                    public Item(String value) { this.value = value; }
                    public String getValue() { return value; }
                }
            "#
            .into(),
        ),
        (
            "Provider.java".into(),
            r#"
                package fixtures;
                public final class Provider {
                    public static Item create(String value) { return new Item(value); }
                    public static Item create(Class<?> type) { return new Item(type.getSimpleName()); }
                }
            "#
            .into(),
        ),
        (
            "LocalProvider.java".into(),
            r#"
                package fixtures;
                public final class LocalProvider {
                    public String create(String value) { return "local:" + value; }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.LocalProvider
        import fixtures.Provider

        val top = Provider.create("top")
        val qualified = fixtures.Provider.create("qualified")

        class Holder {
            val member = Provider.create("member")
        }

        class LiteralHolder {
            val product = Provider.create(LiteralHolder::class.java)
        }

        class Shadowed {
            val Provider = LocalProvider()
            val member = Provider.create("member")
        }

        fun box(): String {
            if (top.value != "top") return "top"
            if (qualified.value != "qualified") return "qualified"
            if (Holder().member.value != "member") return "member"
            if (LiteralHolder().product.value != "LiteralHolder") return "literal"
            if (Shadowed().member != "local:member") return "shadowed"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn class_literal_binds_nested_java_generic_returns() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Record.java".into(),
            r#"
                package fixtures;
                import java.util.Map;
                public interface Record {
                    String getLabel();
                    Map<String, Object> getData();
                }
            "#
            .into(),
        ),
        (
            "Store.java".into(),
            r#"
                package fixtures;
                import java.util.Map;
                import java.util.Optional;
                public final class Store {
                    public <T> Optional<T> getValue(String key, Class<T> type) {
                        Record record = new Record() {
                            public String getLabel() { return "sample"; }
                            public Map<String, Object> getData() {
                                return Map.of("code", "value");
                            }
                        };
                        return Optional.of(type.cast(record));
                    }
                    public Optional<Object> anyValue() {
                        return Optional.of("value");
                    }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Record
        import fixtures.Store

        val topRecord =
            Store()
                .getValue("record", Record::class.java)
                .orElse(null)

        fun box(): String {
            val record =
                Store()
                    .getValue("record", Record::class.java)
                    .orElse(null)

            if (record != null) {
                if (record.label != "sample") return "label"
                if (record.data["code"] != "value") return "data"
            }
            if (topRecord?.label != "sample") return "top-level"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    let invalid_source = r#"
        import fixtures.Store

        fun bad(): Int =
            Store()
                .anyValue()
                .orElse("fallback")
                .length
    "#;
    let diagnostics =
        common::front_end_diagnostics(invalid_source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved reference 'length'")),
        "Optional<Any>.orElse(String) must remain Any, got {diagnostics:?}"
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn generic_extension_property_keeps_nullability_and_kotlin_collection_type() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = common::compile_lib(
        "generic_extension_property",
        r#"
            package fixtures

            class Box<T>(val value: T?)

            val <T> Box<T>.maybe: T?
                get() = value

            val <T : Any> Box<T>.items: List<T>
                get() = listOfNotNull(value)
        "#,
    ) else {
        return;
    };
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Box
        import fixtures.items
        import fixtures.maybe

        fun box(): String {
            val empty = Box<Int>(null)
            if ((empty.maybe ?: 7) != 7) return "nullable"

            val full = Box(3)
            if (full.items.sum() != 3) return "collection"
            return "OK"
        }
    "#;
    let output = common::compile_and_run_box(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(output.trim(), "OK");
}

#[test]
fn java_generic_member_binds_lambda_param_from_receiver_type_argument() {
    // The intellij `ActionContextElement` shape: a lambda passed to a Java generic member
    // (`JBIterable<T>.filterMap(Function<? super T, ? extends R>)`) must type `it` from the
    // RECEIVER's type argument (`JBIterable<Component>` → `it: Component`), or an overload like
    // `ClientProperty.get(Component, Key<T>)` is unreachable inside it. The member's generic
    // signature spelled the wildcard argument as the CLASS formal `T`, but only the return type
    // was bound from the receiver — the parameter side fell back to `Any`, and the call failed
    // with "unresolved Java static 'ClientProperty.get' for given argument types".
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Component.java".into(),
            r#"
                package fixtures;
                public class Component {
                    private final String name;
                    public Component(String name) { this.name = name; }
                    public String getName() { return name; }
                }
            "#
            .into(),
        ),
        (
            "Key.java".into(),
            r#"
                package fixtures;
                public final class Key<T> {
                    private final String name;
                    private Key(String name) { this.name = name; }
                    public static <T> Key<T> create(String name) { return new Key<T>(name); }
                }
            "#
            .into(),
        ),
        (
            "ClientProperty.java".into(),
            r#"
                package fixtures;
                public final class ClientProperty {
                    public static Object get(Component component, Object key) { return null; }
                    public static <T> T get(Component component, Key<T> key) { return null; }
                }
            "#
            .into(),
        ),
        (
            "JBIterable.java".into(),
            r#"
                package fixtures;
                import java.util.function.Function;
                public class JBIterable<T> {
                    public <R> JBIterable<R> filterMap(Function<? super T, ? extends R> fun) { return new JBIterable<R>(); }
                    public T first() { return null; }
                    public <R> R convert(T value, R fallback) { return fallback; }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.ClientProperty
        import fixtures.Component
        import fixtures.JBIterable
        import fixtures.Key

        class ActionContextElement(val name: String)

        private val KEY = Key.create<ActionContextElement>("K")

        fun create(component: Component?, items: JBIterable<Component>): ActionContextElement? {
            // Next iteration: `R` is not yet inferred from the lambda BODY through the Java SAM
            // parameter (the wildcard decodes as `Obj(java/util/function/Function, …)`, which never
            // unifies with the lambda's function type), so `.first()?.name` on the chained result
            // still reports `unresolved reference 'name'.` — kotlinc infers `R` there.
            val parent = items
                .filterMap { ClientProperty.get(it, KEY) }
                .first()
            return parent
        }

        // A method whose params mix the CLASS formal (`T value`) and a METHOD formal (`R fallback`)
        // must still infer `R` from the argument — binding the class formal from the receiver must
        // not erase the method formal to `Any` (`label.length` would not resolve).
        fun convertLabel(items: JBIterable<Component>): Int {
            val label = items.convert(Component("c"), "OK")
            return label.length
        }

        fun box(): String {
            if (create(null, JBIterable<Component>()) != null) return "create"
            if (convertLabel(JBIterable<Component>()) != 2) return "convert"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn null_literal_args_into_a_classpath_static_call() {
    // The intellij `ActionUtil` shape: `EmptyAction.createEmptyAction(null, null, true)` against
    // `public static AnAction createEmptyAction(@Nullable String, @Nullable Icon, boolean)`. Every
    // reference argument is a `null` literal; the property type must still infer to the declared
    // return type, exactly as the same call into an INSTANCE method does.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "AnAction.java".into(),
            r#"
                package fixtures;
                public abstract class AnAction {
                    public String label() { return "action"; }
                }
            "#
            .into(),
        ),
        (
            "Icon.java".into(),
            r#"
                package fixtures;
                public class Icon { }
            "#
            .into(),
        ),
        (
            "EmptyAction.java".into(),
            r#"
                package fixtures;
                public final class EmptyAction extends AnAction {
                    public static AnAction createEmptyAction(String name, Icon icon, boolean enabled) {
                        return new EmptyAction();
                    }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.EmptyAction

        private val dummy = EmptyAction.createEmptyAction(null, null, true)

        fun box(): String {
            if (dummy.label() != "action") return "label"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn null_mixed_with_non_null_args_and_overload_ambiguity() {
    // A `null` literal mixed with non-null arguments must not disturb overload selection: only the
    // arity-matching overload applies. A bare `null` between two single-reference-parameter
    // overloads is ambiguous in kotlinc ("overload resolution ambiguity between candidates"), so it
    // must NOT silently pick one — krusty reports the call unresolved, its existing shape for a
    // failed static call.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Icon.java".into(),
            r#"
                package fixtures;
                public class Icon { }
            "#
            .into(),
        ),
        (
            "Factory.java".into(),
            r#"
                package fixtures;
                public final class Factory {
                    public static String create(String name) { return "str"; }
                    public static String create(Icon icon) { return "icon"; }
                    public static String create(String name, boolean enabled) { return "str:" + enabled; }
                    public static String create(String name, Icon icon) { return "str:icon"; }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Factory

        private val mixed = Factory.create(null, true)
        private val second = Factory.create("n", null)

        fun box(): String {
            if (mixed != "str:true") return "mixed"
            if (second != "str:icon") return "second"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    let ambiguous_source = r#"
        import fixtures.Factory

        fun bad(): String = Factory.create(null)
    "#;
    let diagnostics =
        common::front_end_diagnostics(ambiguous_source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static 'Factory.create'")),
        "a bare null between single-reference-parameter overloads must stay an error, got {diagnostics:?}"
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn null_literal_into_a_java_interface_parameter() {
    // A `null` literal for a Java INTERFACE parameter (`Face`) of a classpath static resolves the
    // same as for a class parameter.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Face.java".into(),
            r#"
                package fixtures;
                public interface Face { }
            "#
            .into(),
        ),
        (
            "Faces.java".into(),
            r#"
                package fixtures;
                public final class Faces {
                    public static String describe(Face face) { return "face"; }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Faces

        private val described = Faces.describe(null)

        fun box(): String {
            if (described != "face") return "describe"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn null_literal_into_a_primitive_parameter_still_fails() {
    // Negative pin: `null` into a primitive `boolean`/`int` parameter stays an error (kotlinc:
    // "null cannot be a value of a non-null type 'Boolean'"); krusty reports it with its existing
    // unresolved-static message — message formats are unchanged.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [(
        "Prims.java".into(),
        r#"
                package fixtures;
                public final class Prims {
                    public static String prim(boolean flag) { return "b"; }
                    public static String primInt(int value) { return "i"; }
                }
            "#
        .into(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Prims

        fun badBoolean(): String = Prims.prim(null)
        fun badInt(): String = Prims.primInt(null)
    "#;
    let diagnostics = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static 'Prims.prim'")),
        "null into a primitive boolean parameter must stay an error, got {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("unresolved Java static 'Prims.primInt'")),
        "null into a primitive int parameter must stay an error, got {diagnostics:?}"
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn java_typevar_return_keeps_platform_nullability() {
    // The intellij `ActionUtil` shape: a Java method returning its own TYPE VARIABLE
    // (`public <T> T getClientProperty(Key<T> key)`) binds `T` to the Kotlin NON-NULL type the
    // call site supplies (`Key<Boolean>` → `Boolean`). kotlinc types the result as the PLATFORM
    // `T!`, so a null check on it is legal and smart-casts inside the branch; krusty substituted
    // the binding's exact non-null type and rejected `was != null` with
    // "operator '!=' cannot be applied to 'Boolean' and 'Null'.". A plain Java reference return
    // (`String getName()`) already carries platform nullability — only the type-variable
    // substitution lost it.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "Key.java".into(),
            r#"
                package fixtures;
                public final class Key<T> {
                    private final String name;
                    private Key(String name) { this.name = name; }
                    public static <T> Key<T> create(String name) { return new Key<T>(name); }
                }
            "#
            .into(),
        ),
        (
            "Presentation.java".into(),
            r#"
                package fixtures;
                public final class Presentation {
                    @SuppressWarnings("unchecked")
                    public <T> T getClientProperty(Key<T> key) { return null; }
                    public <T> void putClientProperty(Key<T> key, T value) { }
                    public String getName() { return null; }
                    public Boolean getFlag() { return null; }
                }
            "#
            .into(),
        ),
        (
            "Lists.java".into(),
            r#"
                package fixtures;
                public final class Lists {
                    public <T> T first(java.util.List<T> list) { return list.get(0); }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Key
        import fixtures.Lists
        import fixtures.Presentation

        private val KEY: Key<Boolean> = Key.create("K")

        fun box(): String {
            val p = Presentation()
            // Java returns null here: the platform-typed result must compile the null check AND
            // skip the branch (the smart-cast `!was` inside the branch must type as Boolean).
            val was = p.getClientProperty(KEY)
            if (was != null && !was) return "typevar"
            // The declared-reference controls keep their existing platform behavior.
            val n = p.getName()
            if (n != null && n.isEmpty()) return "name"
            val f = p.getFlag()
            if (f != null && !f) return "flag"
            // Call-site inference from ARGUMENTS (`List<String>` → `T = String`): the result
            // stays a String inside the branch.
            val s = Lists().first(listOf("x"))
            if (s != null && s.length != 1) return "first"
            // The same argument inference with a PRIMITIVE binding (`List<Boolean>` → `T = Boolean`)
            // gets the platform treatment too.
            val b = Lists().first(listOf(true))
            if (b != null && !b) return "firstBool"
            // A platform result also compares against a Boolean/Int LITERAL (kotlinc unboxes the
            // `T!` operand): `getClientProperty(...) == false` / `first(...) == 1`. The Java side
            // returns null/`1`, so neither branch is taken.
            if (p.getClientProperty(KEY) == false) return "eqFalse"
            if (Lists().first(listOf(1)) != 1) return "eqInt"
            // kotlinc also admits a null argument INTO an unannotated Java type-variable
            // parameter (the platform `T!` parameter of `putClientProperty`).
            p.putClientProperty(KEY, null)
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn java_static_typevar_return_keeps_platform_nullability() {
    // The static twin of `java_typevar_return_keeps_platform_nullability`: a Java STATIC generic
    // method (`public static <T> T identity(T t)`) binds its type variable through the
    // companion-static return substitution (and the package top-level static index), which carried
    // no platform fact — `Statics.identity(true)` typed as the exact non-null `Boolean` and
    // rejected `b != null` with "operator '!=' cannot be applied to 'Boolean' and 'Null'.".
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [(
        "Statics.java".into(),
        r#"
                package fixtures;
                public final class Statics {
                    public static <T> T identity(T t) { return t; }
                }
            "#
        .into(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Statics

        fun box(): String {
            // Primitive binding (`T = Boolean`): the platform result null-checks and smart-casts.
            // Java returns the argument (`true`), so the branch is skipped.
            val b = Statics.identity(true)
            if (b != null && !b) return "prim"
            // Reference binding (`T = String`): unchanged — still a String inside the branch.
            val s = Statics.identity("x")
            if (s != null && s.length != 1) return "ref"
            return "OK"
        }
    "#;
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}

#[test]
fn kotlin_non_null_return_still_rejects_null_check() {
    // Negative pin: a KOTLIN (non-platform) non-null return must STILL reject `!= null` with the
    // existing message — the platform treatment above applies to Java type-variable returns only.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classpath = vec![stdlib];
    let source = r#"
        fun nonNull(): Boolean = true

        fun bad(): Boolean = nonNull() != null
    "#;
    let diagnostics = common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message
                .contains("operator '!=' cannot be applied to 'Boolean' and 'Null'.")),
        "a Kotlin non-null Boolean return must keep rejecting `!= null`, got {diagnostics:?}"
    );
}

// The lightweight signature inferer's `null` literal arm also admits a bare
// `val x = null` (typed `Nothing?`, as kotlinc accepts) where it previously
// reported "cannot infer the type of property 'x'".
#[test]
fn null_literal_property_initializer_compiles() {
    let source = r#"
        val x = null

        fun box(): String = if (x == null) "OK" else "F"
    "#;
    let outcome = common::compile_and_run_with_stdlib(source, "Main");
    assert_eq!(outcome.as_deref(), Some("OK"));
}
