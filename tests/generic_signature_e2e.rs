//! Generic `Signature` attribute emission: kotlinc emits a JVM generic `Signature` for a
//! type-parameterized function (the descriptor erases the type params; the Signature preserves them).
//! krusty must too, for bytecode parity. A non-generic function gets no Signature. The exact strings
//! are verified byte-identical to kotlinc in the differential harness; here we assert krusty's output.

use super::common;

fn classes(src: &str) -> Vec<(String, Vec<u8>)> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::expect_compile_in_process(src, "G", &[stdlib], Some(jdk.as_path()))
}

fn method_signature(cs: &[(String, Vec<u8>)], facade: &str, name: &str) -> Option<String> {
    let ci = cs
        .iter()
        .find(|(n, _)| n.ends_with(facade))
        .map(|(_, b)| krusty::jvm::classreader::parse_class(b).expect("parse"))?;
    ci.methods
        .iter()
        .find(|m| m.name == name)
        .and_then(|m| m.signature.clone())
}

fn kotlinc_class(src: &str, name: &str) -> krusty::jvm::classreader::ClassInfo {
    let build = common::compile_libs_build("generic_signature_reference", &[("G.kt", src)])
        .expect("reference compiler unavailable");
    let output = build
        .reference_out()
        .expect("reference compiler output unavailable");
    let bytes = std::fs::read(output.join(format!("{name}.class"))).expect("reference class");
    krusty::jvm::classreader::parse_class(&bytes).expect("parse reference class")
}

fn kotlinc_class_signature(src: &str, name: &str) -> String {
    kotlinc_class(src, name)
        .signature
        .expect("reference generic signature")
}

#[test]
fn generic_function_emits_signature() {
    let src = "fun <T> id(t: T): T = t\nfun plain(x: Int): Int = x\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "GKt", "id").as_deref(),
        Some("<T:Ljava/lang/Object;>(TT;)TT;")
    );
    // A non-generic function must NOT carry a Signature attribute.
    assert_eq!(method_signature(&cs, "GKt", "plain"), None);
}

#[test]
fn function_parameter_keeps_its_complete_signature() {
    let src = "fun <T> transform(block: (String) -> T): T = block(\"x\")\n\
               fun consume(block: (String) -> Long): Long = block(\"x\")\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "GKt", "transform").as_deref(),
        Some(
            "<T:Ljava/lang/Object;>(Lkotlin/jvm/functions/Function1<-Ljava/lang/String;+TT;>;)TT;"
        )
    );
    assert_eq!(
        method_signature(&cs, "GKt", "consume").as_deref(),
        Some("(Lkotlin/jvm/functions/Function1<-Ljava/lang/String;Ljava/lang/Long;>;)J")
    );
}

#[test]
fn generic_member_method_compiles_runs_and_signs() {
    // A member method with its OWN type parameter (`fun <U> wrap(u: U): U`) — previously rejected with
    // "unresolved reference 'U'" because the method's type params weren't in scope for its return type.
    let src = "class Box(val n: Int) {\n  fun <U> wrap(u: U): U = u\n}\nfun box(): String = if (Box(1).wrap(\"OK\") == \"OK\") \"OK\" else \"no\"\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "Box", "wrap").as_deref(),
        Some("<U:Ljava/lang/Object;>(TU;)TU;")
    );
    if let Some(box_class) = common::find_box_class(&cs) {
        let stdlib = common::stdlib_jar();
        assert_eq!(
            common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn nested_generic_member_declares_its_type_parameter() {
    let src = "class D {\n  fun <T> same(xs: List<T>): List<T> = xs\n}\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "D", "same").as_deref(),
        // The PARAMETER realizes `List`'s declaration-site `out` as a wildcard; the RETURN spells it
        // invariantly. Verified against kotlinc 2.4.10 for this exact source.
        Some("<T:Ljava/lang/Object;>(Ljava/util/List<+TT;>;)Ljava/util/List<TT;>;")
    );
}

#[test]
fn generic_class_emits_class_signature() {
    // `class Box<T>` gets a class-level generic Signature; a non-generic class gets none.
    let src = "class Box<T>(val n: Int)\nclass Plain(val n: Int)\n";
    let cs = classes(src);
    let class_sig = |name: &str| -> Option<String> {
        cs.iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
            .and_then(|ci| ci.signature)
    };
    assert_eq!(
        class_sig("Box").as_deref(),
        Some("<T:Ljava/lang/Object;>Ljava/lang/Object;")
    );
    assert_eq!(class_sig("Plain"), None);
}

#[test]
fn parameterized_base_class_emits_class_signature() {
    let src = "open class Parent<T>\nclass Child : Parent<String>()\n";
    let cs = classes(src);
    let signature = cs
        .iter()
        .find(|(name, _)| name == "Child")
        .and_then(|(_, bytes)| krusty::jvm::classreader::parse_class(bytes).ok())
        .and_then(|class| class.signature);

    assert_eq!(signature.as_deref(), Some("LParent<Ljava/lang/String;>;"));
}

#[test]
fn type_parameter_fields_get_field_signatures() {
    // A field declared with a bare type parameter (`val a: A`) carries a field `Signature` (`TA;`).
    let src = "class Pair2<A, B>(val a: A, val b: B)\n";
    let cs = classes(src);
    let ci = cs
        .iter()
        .find(|(n, _)| n == "Pair2")
        .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
        .expect("Pair2");
    let field_sig = |name: &str| {
        ci.fields
            .iter()
            .find(|f| f.name == name)
            .unwrap()
            .signature
            .clone()
    };
    assert_eq!(field_sig("a").as_deref(), Some("TA;"));
    assert_eq!(field_sig("b").as_deref(), Some("TB;"));
}

/// Declaration-site variance becomes a JVM wildcard in a PARAMETER position only. A return type, a
/// field type and a getter's return spell every argument invariantly, at EVERY nesting depth —
/// krusty wildcarded them everywhere, so each such signature diverged from the reference bytes.
///
/// An explicit `in` projection is the user's own and survives in either position; an explicit `out`
/// on an already-`out` parameter is redundant and Kotlin normalizes it away before it reaches here.
#[test]
fn declaration_site_wildcards_appear_in_parameter_positions_only() {
    let src = "class Node<T>\n\
               fun <U> deep(a: Map<String, List<U>>): Map<String, List<U>> = a\n\
               fun <U> deeper(): List<Map<String, Node<U>>> = emptyList()\n\
               fun keepsIn(c: Comparator<in Number>): Comparator<in Number> = c\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "GKt", "deep").as_deref(),
        Some(concat!(
            "<U:Ljava/lang/Object;>",
            "(Ljava/util/Map<Ljava/lang/String;+Ljava/util/List<+TU;>;>;)",
            "Ljava/util/Map<Ljava/lang/String;Ljava/util/List<TU;>;>;"
        )),
        "the parameter wildcards at every level, the return at none"
    );
    assert_eq!(
        method_signature(&cs, "GKt", "deeper").as_deref(),
        Some("<U:Ljava/lang/Object;>()Ljava/util/List<Ljava/util/Map<Ljava/lang/String;LNode<TU;>;>;>;"),
        "a return type carries no wildcard at any depth"
    );
    // One declaration, three positions: kotlinc wildcards the CONSTRUCTOR parameter and spells the
    // backing FIELD and the GETTER's return invariantly.
    let three = classes("class Container14<out T>\nclass Box14(val c: Container14<Number>)\n");
    assert_eq!(
        method_signature(&three, "Box14", "<init>").as_deref(),
        Some("(LContainer14<+Ljava/lang/Number;>;)V"),
        "a constructor parameter keeps the wildcard"
    );
    assert_eq!(
        method_signature(&three, "Box14", "getC").as_deref(),
        Some("()LContainer14<Ljava/lang/Number;>;"),
        "the getter's return drops it"
    );
    let field_sig = three
        .iter()
        .find(|(n, _)| n == "Box14")
        .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
        .and_then(|ci| ci.fields.iter().find(|f| f.name == "c")?.signature.clone());
    assert_eq!(
        field_sig.as_deref(),
        Some("LContainer14<Ljava/lang/Number;>;"),
        "the backing field drops it too"
    );
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "GKt", "keepsIn").as_deref(),
        Some(concat!(
            "(Ljava/util/Comparator<-Ljava/lang/Number;>;)",
            "Ljava/util/Comparator<-Ljava/lang/Number;>;"
        )),
        "an explicit `in` projection is not declaration-site variance and survives in both"
    );
}

#[test]
fn top_level_property_field_gets_its_generic_signature() {
    // A top-level property's backing field lives on the FILE FACADE, whose field table is emitted by
    // its own path — it dropped the `Signature` a class field of the same type already carried, so a
    // consumer read `java.util.List` where kotlinc records `List<String>`. A field whose type has no
    // type arguments still carries none.
    let src = "private val xs: List<String> = listOf(\"a\")\n               private val m: Map<String, Int> = mapOf()\n               private val plain: String = \"s\"\n               fun read(): Int = xs.size + m.size + plain.length\n";
    let cs = classes(src);
    let reference = kotlinc_class(src, "GKt");
    let ci = cs
        .iter()
        .find(|(n, _)| n.ends_with("GKt"))
        .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
        .expect("GKt");
    let field_sig = |name: &str| {
        ci.fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no field {name}"))
            .signature
            .clone()
    };
    let reference_field_sig = |name: &str| {
        reference
            .fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("no reference field {name}"))
            .signature
            .clone()
    };
    for name in ["xs", "m", "plain"] {
        assert_eq!(field_sig(name), reference_field_sig(name));
    }
    assert_eq!(
        field_sig("xs").as_deref(),
        Some("Ljava/util/List<Ljava/lang/String;>;")
    );
    assert_eq!(
        field_sig("m").as_deref(),
        Some("Ljava/util/Map<Ljava/lang/String;Ljava/lang/Integer;>;")
    );
    assert_eq!(field_sig("plain"), None);
}

#[test]
fn top_level_property_accessors_get_their_generic_signatures() {
    // The facade accessors erase the property's type arguments in their descriptors, so each carries
    // the same generic `Signature` the backing field does — kotlinc signs `getPub()` as
    // `()Ljava/util/List<Ljava/lang/String;>;` and `setMut(Map)` as `(Ljava/util/Map<…>;)V`.
    let src = "val pub: List<String> = listOf(\"a\")\n               var mut: Map<String, Int> = mapOf()\n               val plain: String = \"s\"\n";
    let cs = classes(src);
    let reference = kotlinc_class(src, "GKt");
    let reference_method_signature = |name: &str| {
        reference
            .methods
            .iter()
            .find(|method| method.name == name)
            .and_then(|method| method.signature.clone())
    };
    for name in ["getPub", "getMut", "setMut", "getPlain"] {
        assert_eq!(
            method_signature(&cs, "GKt", name),
            reference_method_signature(name)
        );
    }
    assert_eq!(
        method_signature(&cs, "GKt", "getPub").as_deref(),
        Some("()Ljava/util/List<Ljava/lang/String;>;")
    );
    assert_eq!(
        method_signature(&cs, "GKt", "getMut").as_deref(),
        Some("()Ljava/util/Map<Ljava/lang/String;Ljava/lang/Integer;>;")
    );
    assert_eq!(
        method_signature(&cs, "GKt", "setMut").as_deref(),
        Some("(Ljava/util/Map<Ljava/lang/String;Ljava/lang/Integer;>;)V")
    );
    // A property whose type has no type arguments carries none.
    assert_eq!(method_signature(&cs, "GKt", "getPlain"), None);
}

#[test]
fn type_parameter_accessors_get_signatures() {
    // A generic class's synthesized accessors for a type-parameter property carry signatures:
    // `getA()` → `()TT;`, `setA(T)` → `(TT;)V` (no `<…>` prefix — they use the class's `T`, declare none).
    let src = "class Box<T>(var a: T)\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "Box", "getA").as_deref(),
        Some("()TT;")
    );
    assert_eq!(
        method_signature(&cs, "Box", "setA").as_deref(),
        Some("(TT;)V")
    );
}

#[test]
fn generic_class_constructor_gets_signature() {
    // The synthesized `<init>` of a generic class carries a `Signature` whose type-parameter params
    // read `T<tp>;` (`class Pair2<A, B>(val a: A, val b: B)` → `(TA;TB;)V`) — no `<…>` prefix.
    let src = "class Pair2<A, B>(val a: A, val b: B)\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "Pair2", "<init>").as_deref(),
        Some("(TA;TB;)V")
    );
}

#[test]
fn nullable_type_parameter_signature_drops_nullability() {
    // `fun <T> f(t: T?): T?` → `<T:Ljava/lang/Object;>(TT;)TT;` — `?` is not represented in the JVM
    // generic signature (kotlinc drops it; the erased descriptor is `Object`).
    let src = "fun <T> f(t: T?): T? = t\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "GKt", "f").as_deref(),
        Some("<T:Ljava/lang/Object;>(TT;)TT;")
    );
}

#[test]
fn primitive_bounded_type_param_signature_uses_wrapper() {
    // `<T: Int>` is specialized to descriptor `(I)I`, but its Signature bound is the boxed wrapper.
    let src = "fun <T : Int> idi(t: T): T = t\n";
    let cs = classes(src);
    assert_eq!(
        method_signature(&cs, "GKt", "idi").as_deref(),
        Some("<T:Ljava/lang/Integer;>(TT;)TT;")
    );
}

#[test]
fn reference_bounded_type_param_emits_class_signature() {
    // A reference bound — a user interface `T : I` or a stdlib `T : CharSequence` — must appear in the
    // class generic Signature as its erased descriptor; krusty previously emitted NO Signature at all
    // (ir_lower dropped the bound). A PARAMETERIZED bound (`T : Comparable<T>`) is still suppressed.
    let src = "interface I\n\
        class Usr<T : I>(val n: Int)\n\
        class Seq<T : CharSequence>(val n: Int)\n\
        class Cmp<T : Comparable<T>>(val n: Int)\n";
    let cs = classes(src);
    let class_sig = |name: &str| -> Option<String> {
        cs.iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
            .and_then(|ci| ci.signature)
    };
    for name in ["Usr", "Seq", "Cmp"] {
        let kotlinc = kotlinc_class_signature(src, name);
        assert_eq!(class_sig(name).as_deref(), Some(kotlinc.as_str()), "{name}");
    }
}

/// `kotlin.Array<E>` is realized as the JVM ARRAY type, and its `Signature` must spell it that way.
///
/// Writing `Lkotlin/Array<Ljava/lang/String;>;` names a class no loader can resolve, so every reader
/// of the attribute — reflection, a Java consumer, a decompiler — fails on it. Two rules follow, and
/// this pins both against kotlinc: the element goes inside `[`, and an attribute that merely repeats
/// the descriptor is omitted (which is what happens once `Array<String>` signs `[Ljava/lang/String;`).
#[test]
fn an_array_signs_as_a_jvm_array_or_not_at_all() {
    const SRC: &str = "fun a(x: Array<String>) {}\n\
                       fun <T> c(x: Array<T>) {}\n\
                       fun d(x: Array<Array<String>>) {}\n\
                       fun e(x: Array<out String>) {}\n";
    let ours = classes(SRC);
    for (name, expected) in [
        // Nothing the descriptor does not already say: no attribute, exactly as kotlinc.
        ("a", None),
        // The element is a type VARIABLE, which the descriptor erases — the attribute carries it.
        ("c", Some("<T:Ljava/lang/Object;>([TT;)V".to_string())),
        ("d", None),
        // A `out` projection on an array erases: kotlinc writes no attribute here either.
        ("e", None),
    ] {
        let signature = method_signature(&ours, "GKt", name);
        assert_eq!(
            signature, expected,
            "fun {name}'s Signature attribute must match kotlinc's"
        );
        let reference = kotlinc_class(SRC, "GKt")
            .methods
            .iter()
            .find(|m| m.name == name)
            .and_then(|m| m.signature.clone());
        assert_eq!(
            signature, reference,
            "fun {name} must sign exactly as the reference compiler does"
        );
        assert!(
            !signature.iter().any(|s| s.contains("kotlin/Array")),
            "a Signature must never name kotlin/Array — no JVM loader resolves it: {signature:?}"
        );
    }
}
