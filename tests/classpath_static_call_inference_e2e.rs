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
    // A lambda passed to a classpath generic member
    // (`TransformSequence<T>.mapPresent(Function<? super T, ? extends R>)`) must type `it` from the
    // RECEIVER's type argument (`TransformSequence<InputNode>` → `it: InputNode`). Otherwise the
    // typed `SlotLookup.read(InputNode, TypedSlot<T>)` overload is unreachable inside the lambda.
    // The member signature spells the wildcard argument with the OWNER formal `T`; binding only the
    // return side previously left that parameter to fall back to `Any`. All names here are synthetic:
    // the fixture preserves the generic signature shape without retaining reproduction identities.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [
        (
            "InputNode.java".into(),
            r#"
                package fixtures;
                public class InputNode {
                    private final String name;
                    public InputNode(String name) { this.name = name; }
                    public String getName() { return name; }
                }
            "#
            .into(),
        ),
        (
            "TypedSlot.java".into(),
            r#"
                package fixtures;
                public final class TypedSlot<T> {
                    private final String name;
                    private TypedSlot(String name) { this.name = name; }
                    public static <T> TypedSlot<T> create(String name) { return new TypedSlot<T>(name); }
                }
            "#
            .into(),
        ),
        (
            "SlotLookup.java".into(),
            r#"
                package fixtures;
                public final class SlotLookup {
                    public static Object read(InputNode node, Object slot) { return null; }
                    public static <T> T read(InputNode node, TypedSlot<T> slot) { return null; }
                }
            "#
            .into(),
        ),
        (
            "TransformSequence.java".into(),
            r#"
                package fixtures;
                import java.util.function.Function;
                public class TransformSequence<T> {
                    public <R> TransformSequence<R> mapPresent(Function<? super T, ? extends R> fun) { return new TransformSequence<R>(); }
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
        import fixtures.InputNode
        import fixtures.SlotLookup
        import fixtures.TransformSequence
        import fixtures.TypedSlot

        class ResultRecord(val name: String)

        private val SLOT = TypedSlot.create<ResultRecord>("result")

        fun create(node: InputNode?, items: TransformSequence<InputNode>): ResultRecord? {
            // Next iteration: `R` is not yet inferred from the lambda BODY through the Java SAM
            // parameter (the wildcard decodes as `Obj(java/util/function/Function, …)`, which never
            // unifies with the lambda's function type), so `.first()?.name` on the chained result
            // still reports `unresolved reference 'name'.` — kotlinc infers `R` there.
            val parent = items
                .mapPresent { SlotLookup.read(it, SLOT) }
                .first()
            return parent
        }

        // A method whose params mix the CLASS formal (`T value`) and a METHOD formal (`R fallback`)
        // must still infer `R` from the argument — binding the class formal from the receiver must
        // not erase the method formal to `Any` (`label.length` would not resolve).
        fun convertLabel(items: TransformSequence<InputNode>): Int {
            val label = items.convert(InputNode("node"), "OK")
            return label.length
        }

        fun box(): String {
            if (create(null, TransformSequence<InputNode>()) != null) return "create"
            if (convertLabel(TransformSequence<InputNode>()) != 2) return "convert"
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
