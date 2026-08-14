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

fn kotlinc_class_signature(src: &str, name: &str) -> String {
    let output =
        common::compile_lib_ref("generic_signature", src).expect("reference compiler unavailable");
    let bytes = std::fs::read(output.join(format!("{name}.class"))).expect("reference class");
    krusty::jvm::classreader::parse_class(&bytes)
        .expect("parse reference class")
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
        Some("<T:Ljava/lang/Object;>(Ljava/util/List<+TT;>;)Ljava/util/List<+TT;>;")
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
