//! A non-generic candidate outranks a generic one only as a TIEBREAKER — after neither is more
//! specific by parameter types.
//!
//! `of(Class<T>)` and `of(Type)` are both applicable to a `Class<Resp>` argument, and Java's
//! `Class<T>` implements `Type`. Kotlin decides this by the parameters and selects the first,
//! binding `T = Resp`. krusty dropped every generic candidate as soon as any concrete one was
//! applicable, so it selected `of(Type)` — whose result is `Arg<?>` and carries no element type.
//!
//! The damage travels: `Argument.of(Resp::class.java)` typed as `Argument<out Any!>` makes a
//! surrounding `exchange(...).awaitSingle().body()` produce `Any?`, so an enclosing
//! `HttpResponse.ok(...)` yields `MutableHttpResponse<out Any>` against a declared
//! `HttpResponse<Resp>`. One overload choice cost a corpus module two separate errors.

use super::common;

const JAVA: &[(&str, &str)] = &[(
    "Arg.java",
    "package arg;\n\
     import java.lang.reflect.Type;\n\
     public final class Arg<T> {\n\
     \x20 public static <T> Arg<T> of(Class<T> type) { return null; }\n\
     \x20 public static Arg<?> of(Type type) { return null; }\n\
     }\n",
)];

fn assert_both_accept(user: &str, what: &str) {
    let java = JAVA
        .iter()
        .map(|(name, source)| ((*name).into(), (*source).into()))
        .collect::<Vec<(String, String)>>();
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must compile the overload fixture");
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(&[("Use.kt", user)], &classpath);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the {what} fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source kotlinc accepts for {what}"
    );
}

const PRELUDE: &str = "import arg.Arg\nclass Resp(val v: String = \"\")\n";

/// The generic candidate is more specific by parameter type, so genericity never gets to decide.
#[test]
fn a_generic_candidate_wins_when_its_parameter_is_more_specific() {
    assert_both_accept(
        &format!("{PRELUDE}fun use(): Arg<Resp> = Arg.of(Resp::class.java)\n"),
        "a more specific generic candidate",
    );
}

/// The selected overload really binds its type argument — the result is usable where `Arg<Resp>` is
/// required, not merely where `Arg<*>` is.
#[test]
fn the_selected_overload_binds_its_type_argument() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun take(a: Arg<Resp>): String = a.toString()\n\
             fun use(): String = take(Arg.of(Resp::class.java))\n"
        ),
        "the bound type argument",
    );
}

/// Control: with an argument that is not a `Class`, only the supertype candidate applies and it
/// must still be selected.
#[test]
fn the_supertype_candidate_is_still_selected_when_it_is_the_only_fit() {
    assert_both_accept(
        &format!("{PRELUDE}fun use(t: java.lang.reflect.Type): Arg<*> = Arg.of(t)\n"),
        "the supertype candidate alone",
    );
}

/// Control: an explicit type argument already worked and must keep working.
#[test]
fn an_explicit_type_argument_still_selects_the_generic_candidate() {
    assert_both_accept(
        &format!("{PRELUDE}fun use(): Arg<Resp> = Arg.of<Resp>(Resp::class.java)\n"),
        "an explicit type argument",
    );
}

/// Control: the tiebreaker itself survives. With parameters that are equally specific — the same
/// classifier on both sides — the NON-generic candidate is still the one selected, which is what
/// `prefer_concrete` exists to do.
#[test]
fn a_concrete_candidate_still_wins_an_equally_specific_pair() {
    let java = vec![(
        "Tie.java".to_string(),
        "package tie;\n\
         public final class Tie<T> {\n\
         \x20 public static <T> String pick(Class<T> type) { return \"generic\"; }\n\
         \x20 public static String pick(Class<?> type) { return \"concrete\"; }\n\
         }\n"
        .to_string(),
    )];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must compile the tie-break fixture");
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import tie.Tie\n\
             class Resp\n\
             fun use(): String = Tie.pick(Resp::class.java)\n",
        )],
        &classpath,
    );
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the equally-specific pair"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "an equally specific concrete candidate must still be selected"
    );
}

/// Control: INCOMPARABLE parameters are not a reason to keep the generic candidate. Two unrelated
/// SAM interfaces — the `Executable` / `ThrowingSupplier<T>` pair every assertion library has — are
/// decided by the tiebreaker, and the non-generic one wins. Being merely not-covered by a concrete
/// candidate must not promote a generic one; only being strictly more specific may.
#[test]
fn an_incomparable_generic_candidate_still_loses_to_the_concrete_one() {
    let java = vec![(
        "Sam.java".to_string(),
        "package sam;\n\
         public final class Sam {\n\
         \x20 public interface Executable { void execute() throws Throwable; }\n\
         \x20 public interface ThrowingSupplier<T> { T get() throws Throwable; }\n\
         \x20 public static String run(Executable e) { return \"plain\"; }\n\
         \x20 public static <T> T run(ThrowingSupplier<T> s) { return null; }\n\
         }\n"
        .to_string(),
    )];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac must compile the SAM fixture");
    let classpath = [library.clone(), common::stdlib_jar(), common::jdk_modules()];
    let result = common::compiler_diagnostics(
        &[(
            "Use.kt",
            "import sam.Sam\n\
             fun use(): String = Sam.run { println(1) }\n",
        )],
        &classpath,
    );
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the incomparable-SAM fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "the non-generic SAM overload must still be selected"
    );
}
