//! A type variable solved through the declaration's own BOUND relation.
//!
//! `fun <T : Base<T>, C : T> f(self: C, subs: Iterable<T>)` states two constraints beyond the
//! parameters: `T` is a `Base<T>`, and `C` is a `T`. Both are load-bearing at a call site.
//!
//! * An argument can pin `T` to a type that its own bound forbids — `f(Auth(), listOf(Login()))`
//!   pins `T = Login`, but `Login` is a `Base<Cmd>`, not a `Base<Login>`. The recursive bound says
//!   where to look for the type the call actually means: the application of `Base` in `Login`'s
//!   hierarchy, so `T = Cmd`.
//! * With a vararg parameter (`subs: Array<out T>`) no argument reaches `T` at all; only `C` is
//!   bound. The same relation answers it from `C`'s value.
//!
//! Keeping the violating binding, or leaving `T` open, drops the candidate as violating its own
//! declared bounds — reported as "unresolved Java static …" or "argument type mismatch: … but
//! 'Iterable<Base<T>>' was expected".
use super::common;

const LIB: &str = "package lib\n\
    abstract class BaseCmd<T : BaseCmd<T>>(val label: String)\n\
    abstract class Cmd(label: String) : BaseCmd<Cmd>(label)\n\
    fun <T : BaseCmd<T>, CommandT : T> CommandT.withSubs(subs: Iterable<T>): CommandT = this\n\
    fun <T : BaseCmd<T>, CommandT : T> CommandT.withVarargSubs(vararg subs: T): CommandT = this\n";

#[test]
fn a_bound_relation_solves_a_variable_no_argument_pins_correctly() {
    const MAIN: &str = "import lib.Cmd\n\
        import lib.withSubs\n\
        import lib.withVarargSubs\n\
        class Login : Cmd(\"login\")\n\
        class Auth : Cmd(\"auth\")\n\
        fun box(): String {\n\
            val listed = Auth().withSubs(listOf(Login()))\n\
            val varargs = Auth().withVarargSubs(Login())\n\
            return if (listed.label == \"auth\" && varargs.label == \"auth\") \"OK\" else \"fail\"\n\
        }\n";
    let Some(result) = common::expect_box_run_against_kotlinc(LIB, MAIN) else {
        return;
    };
    assert_eq!(result, "OK", "bound relation solves the variable");
}

#[test]
fn a_java_declared_bound_relation_solves_the_same_way() {
    // The same signatures declared in JAVA, so only the JVM generic signature carries the relation:
    // `<T extends BaseCmd<T>, C extends T>`. Both the one-formal and two-formal shapes, and both an
    // explicit and an inferred element type, must reach the same answer.
    let java = vec![
        (
            "BaseCmd.java".into(),
            "package jlib;\n\
             public abstract class BaseCmd<T extends BaseCmd<T>> {\n\
                 public final String label;\n\
                 protected BaseCmd(String label) { this.label = label; }\n\
             }\n"
                .into(),
        ),
        (
            "Cmd.java".into(),
            "package jlib;\n\
             public abstract class Cmd extends BaseCmd<Cmd> {\n\
                 protected Cmd(String label) { super(label); }\n\
             }\n"
                .into(),
        ),
        (
            "Api.java".into(),
            "package jlib;\n\
             public final class Api {\n\
                 public static <T extends BaseCmd<T>> T only(T self, Iterable<T> subs) { return self; }\n\
                 public static <T extends BaseCmd<T>, C extends T> C both(C self, Iterable<T> subs) { return self; }\n\
             }\n"
                .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    const MAIN: &str = "import jlib.Cmd\n\
        import jlib.Api\n\
        class Login : Cmd(\"login\")\n\
        class Auth : Cmd(\"auth\")\n\
        fun box(): String {\n\
            val explicitOnly = Api.only(Auth(), listOf<Cmd>(Login()))\n\
            val inferredOnly = Api.only(Auth(), listOf(Login()))\n\
            val explicitBoth = Api.both(Auth(), listOf<Cmd>(Login()))\n\
            val inferredBoth = Api.both(Auth(), listOf(Login()))\n\
            val labels = explicitOnly.label + inferredOnly.label + explicitBoth.label + inferredBoth.label\n\
            return if (labels == \"authauthauthauth\") \"OK\" else \"fail\" + labels\n\
        }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library, stdlib];
    let output = common::compile_and_run_box(MAIN, "Main", &classpath, Some(jdk.as_path()));
    assert_eq!(
        output.as_deref(),
        Some("OK"),
        "a java-declared bound relation solves the same way"
    );
}

#[test]
fn explicit_type_arguments_still_decide() {
    // The re-solve replaces a binding the bounds check would have rejected, so it must never
    // override what the call site spelled out: a wrong explicit argument stays an error, and a
    // right one stays accepted.
    const MAIN_BAD: &str = "import lib.Cmd\n\
        import lib.withSubs\n\
        class Login : Cmd(\"login\")\n\
        class Auth : Cmd(\"auth\")\n\
        fun box(): String = Auth().withSubs<Login, Auth>(listOf(Login())).label\n";
    const MAIN_GOOD: &str = "import lib.Cmd\n\
        import lib.withSubs\n\
        class Login : Cmd(\"login\")\n\
        class Auth : Cmd(\"auth\")\n\
        fun box(): String = Auth().withSubs<Cmd, Auth>(listOf(Login())).label\n";
    let Some(library) = common::kotlinc_library(LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library, stdlib];
    assert!(
        !common::front_end_diagnostics(MAIN_BAD, &classpath, Some(jdk.as_path())).is_empty(),
        "an explicit type argument that violates the bound must stay rejected"
    );
    assert_eq!(
        common::compile_and_run_box(MAIN_GOOD, "Main", &classpath, Some(jdk.as_path())).as_deref(),
        Some("auth"),
        "an explicit type argument that satisfies the bound must still be honoured"
    );
}

#[test]
fn a_violating_call_with_no_solution_stays_rejected() {
    // `Rogue` is a `BaseCmd<Cmd>`, so no application of the bound in its hierarchy makes the call
    // well-typed. The re-solve must not rescue it.
    const MAIN: &str = "import lib.BaseCmd\n\
        import lib.Cmd\n\
        import lib.withSubs\n\
        class Rogue : BaseCmd<Cmd>(\"rogue\")\n\
        class Login : Cmd(\"login\")\n\
        fun box(): String = Rogue().withSubs(listOf(Login())).label\n";
    let Some(library) = common::kotlinc_library(LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library, stdlib];
    assert!(
        !common::front_end_diagnostics(MAIN, &classpath, Some(jdk.as_path())).is_empty(),
        "a violating call with no solution must stay rejected"
    );
}
