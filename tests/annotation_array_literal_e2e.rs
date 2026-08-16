//! `@A(v = ["x"])` — the array-literal spelling of an annotation argument.
//!
//! The literal cannot be desugared where it is parsed: which array a `[1, 2]` denotes follows the
//! element's DECLARED type (`intArrayOf` for `int[]`, `arrayOf` for `String[]`), and kotlinc
//! rejects the mismatched factory, so the parser has no way to choose. The elements are kept as
//! parsed and folded by the checker against that type.
use super::common;

fn library() -> Option<std::path::PathBuf> {
    let java = [(
        "Arr.java".into(),
        "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.TYPE})\n\
         public @interface Arr {\n\
         \x20   String[] ss() default {};\n\
         \x20   int[] xs() default {};\n\
         \x20   byte[] bs() default {};\n\
         \x20   char[] cs() default {};\n\
         \x20   boolean[] zs() default {};\n\
         }\n"
        .into(),
    )];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

#[test]
fn an_array_literal_argument_compiles_and_runs() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = library().expect("javac must compile the annotation fixture");
    let classpath = vec![library, stdlib];
    const SRC: &str = "import jl.Arr\n\
        @Arr(ss = [\"a\", \"b\"], xs = [1, 2])\n\
        class Tagged\n\
        fun box(): String = \"OK\"\n";
    let classes = common::compile_in_process(SRC, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}

#[test]
fn an_array_literal_element_takes_its_declared_array_type() {
    // The whole point of folding in the checker: `[1, 2]` for an `int[]` element must emit `I`
    // tags. `arrayOf(1, 2)` is a genuine type error there — in kotlinc too — so a parse-time
    // desugar to a fixed factory would have miscompiled or wrongly rejected this.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = library().expect("javac must compile the annotation fixture");
    let classpath = vec![library, stdlib];
    const SRC: &str = "import jl.Arr\n\
        @Arr(ss = [\"a\"], xs = [7], bs = [3], cs = ['x'], zs = [true])\n\
        class Tagged\n\
        fun box(): String = \"OK\"\n";
    let Some(classes) = common::compile_in_process(SRC, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!("krusty rejected the array-literal fixture");
    };
    let work = std::env::temp_dir().join(format!("krusty_arr_lit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("scratch dir");
    for (internal, bytes) in &classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, bytes).expect("write class");
    }
    let dumped = common::javap(&["-v", "-cp", &work.to_string_lossy(), "Tagged"])
        .expect("javap unavailable");
    let line = dumped
        .lines()
        .find(|line| line.contains("(#") && line.contains('='))
        .unwrap_or_default()
        .to_string();
    for tag in ["[s#", "[I#", "[B#", "[C#", "[Z#"] {
        assert!(
            line.contains(tag),
            "each element takes its DECLARED array type, {tag} missing: {line}"
        );
    }
}

#[test]
fn an_array_literal_in_an_unemitted_position_reports_nothing() {
    // krusty emits no annotation on a value parameter or a local and does not check one either,
    // so this pins the absence of a SPURIOUS error, not the feature: reporting the unmodelled
    // shape at parse time made these positions hard errors. It would pass with the array-literal
    // support removed, and would fail if the report moved back to the parser.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str =
        "@Target(AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.LOCAL_VARIABLE)\n\
        annotation class Meta(val tags: Array<String> = [])\n\
        fun f(@Meta(tags = [\"a\"]) x: Int): Int {\n\
        \x20   @Meta(tags = [\"b\"]) val y = x\n\
        \x20   return y\n\
        }\n\
        fun box(): String = if (f(1) == 1) \"OK\" else \"fail\"\n";
    let diagnostics = common::front_end_diagnostics(SRC, std::slice::from_ref(&stdlib), Some(&jdk));
    assert!(
        diagnostics.is_empty(),
        "an array literal in a position krusty does not emit must not be an error: {diagnostics:?}"
    );
}

#[test]
fn a_positional_vararg_element_rejects_an_array_literal() {
    // A vararg element passed POSITIONALLY expects the element type, not an array — kotlinc
    // reports "actual type is 'Array<Int>', but 'Byte' was expected" for `@V([1, 2])`. Folding the
    // literal without checking it accepted this and wrote `I` tags into a `byte[]` element, which
    // throws AnnotationTypeMismatchException when the annotation is read back.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [(
        "V.java".into(),
        "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.TYPE})\n\
         public @interface V { byte[] value(); }\n"
            .into(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        panic!("javac must compile the annotation fixture");
    };
    let classpath = vec![library, stdlib];
    const SRC: &str = "import jl.V\n\
        @V([1, 2])\n\
        class Tagged\n\
        fun box(): String = \"OK\"\n";
    let diagnostics = common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("but 'Byte' was expected")),
        "a positional vararg element must demand its ELEMENT type: {diagnostics:?}"
    );
}

#[test]
fn class_literals_inside_an_array_literal_resolve() {
    // Nothing else visits a literal's elements, so a class literal inside one stayed unresolved
    // and the whole argument was rejected as "not a supported compile-time constant" — while the
    // `arrayOf(...)` spelling of the same thing worked. This is the `classes = [X::class]` idiom.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let java = [(
        "Cls.java".into(),
        "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.TYPE})\n\
         public @interface Cls { Class<?>[] ks(); }\n"
            .into(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        panic!("javac must compile the annotation fixture");
    };
    let classpath = vec![library, stdlib];
    const SRC: &str = "import jl.Cls\n\
        @Cls(ks = [String::class, Int::class])\n\
        class Tagged\n\
        fun box(): String = \"OK\"\n";
    let classes = common::compile_in_process(SRC, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}

#[test]
fn a_nested_array_literal_is_rejected_like_kotlinc() {
    // The check must recurse exactly as far as the fold does. Checking only the outer literal let
    // `xs = [[1, 2]]` reach the fold unchecked and emit an ARRAY where a scalar element belongs,
    // which throws AnnotationTypeMismatchException on read-back; kotlinc rejects the source.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = library().expect("javac must compile the annotation fixture");
    let classpath = vec![library, stdlib];
    const SRC: &str = "import jl.Arr\n\
        @Arr(xs = [[1, 2]])\n\
        class Tagged\n\
        fun box(): String = \"OK\"\n";
    let diagnostics = common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("but 'Int' was expected")),
        "a literal nested inside a literal must be checked too: {diagnostics:?}"
    );
}

#[test]
fn a_nullable_array_element_keeps_its_declared_width() {
    // The checker strips nullability before taking the element type; the fold must strip it the
    // same way, or a nullable element type loses the width and writes `I` for a `ByteArray`.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    const SRC: &str = "annotation class M(val bs: ByteArray?)\n\
        @M(bs = [3])\n\
        class Tagged\n\
        fun box(): String = \"OK\"\n";
    let Some(classes) =
        common::compile_in_process(SRC, "Main", std::slice::from_ref(&stdlib), Some(&jdk))
    else {
        return; // kotlinc rejects a nullable annotation parameter; krusty does not model that yet
    };
    let work = std::env::temp_dir().join(format!("krusty_arr_null_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("scratch dir");
    for (internal, bytes) in &classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, bytes).expect("write class");
    }
    let dumped = common::javap(&["-v", "-cp", &work.to_string_lossy(), "Tagged"])
        .expect("javap unavailable");
    if let Some(line) = dumped
        .lines()
        .find(|line| line.contains("(#") && line.contains('='))
    {
        assert!(
            line.contains("[B#"),
            "a nullable ByteArray element still writes `B` tags: {line}"
        );
    }
}
