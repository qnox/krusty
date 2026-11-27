//! A spread argument to a signature-polymorphic call (`MethodHandle.invokeExact`, `VarHandle.get`)
//! is an error at the `*`: the call's descriptor comes from the argument types at the call site,
//! so there is no array to spread into it.

use super::common;

const FILE: &str = "Handles.kt";
const MESSAGE: &str = "spread operator is prohibited for arguments to signature-polymorphic calls.";

fn diagnostic(line: usize, column: usize) -> common::CompilerError {
    common::CompilerError {
        file: FILE.to_string(),
        line,
        column,
        message: MESSAGE.to_string(),
    }
}

fn assert_diagnostics(source: &str, errors: Vec<common::CompilerError>) {
    assert_diagnostics_with_args(source, &[], errors);
}

fn assert_diagnostics_with_args(source: &str, args: &[String], errors: Vec<common::CompilerError>) {
    let result = common::compiler_diagnostics_with_shared_args(
        &[(FILE, source)],
        &[common::stdlib_jar()],
        args,
    );
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), errors);
    assert_eq!(common::compiler_errors(&result.reference_stderr), errors);
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), []);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const HANDLES: &str = "import java.lang.invoke.MethodHandle\n\
     import java.lang.invoke.VarHandle\n\
     fun test(mh: MethodHandle, vh: VarHandle) {\n\
     \x20   mh.invokeExact(\"1\", \"2\")\n\
     \x20   mh.invokeExact(*emptyArray(), \"X\")\n\
     \x20   mh.invokeExact(*arrayOf(\"A\", \"B\"), \"C\", *arrayOf(\"D\", \"E\"))\n\
     \x20   mh.invoke(*arrayOf(\"A\"))\n\
     \x20   vh.get()\n\
     \x20   vh.get(*emptyArray())\n\
     \x20   vh.set(*emptyArray())\n\
     }\n";

fn handle_errors() -> Vec<common::CompilerError> {
    vec![
        diagnostic(5, 20),
        diagnostic(6, 20),
        diagnostic(6, 45),
        diagnostic(7, 15),
        diagnostic(9, 12),
        diagnostic(10, 12),
    ]
}

#[test]
fn spread_in_method_handle_and_var_handle_calls_is_rejected_at_the_star() {
    assert_diagnostics(HANDLES, handle_errors());
}

#[test]
fn spread_is_rejected_against_the_release_11_api() {
    assert_diagnostics_with_args(HANDLES, &["-Xjdk-release=11".to_string()], handle_errors());
}

#[test]
fn spread_in_an_ordinary_vararg_call_is_accepted() {
    assert_diagnostics(
        "fun test(values: Array<String>) = java.lang.String.format(\"%s\", *values)\n",
        vec![],
    );
}
