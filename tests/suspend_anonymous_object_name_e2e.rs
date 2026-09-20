//! A `suspend fun` that also declares an anonymous object must not give the two classes one name.
//!
//! The coroutine transform names its continuation `<owner>$<fn>$1`. That is the ordinal kotlinc
//! uses, but the same `<owner>$<fn>$N` sequence also names the anonymous objects declared in the
//! body, and those are named BEFORE the transform runs. A suspend function returning an
//! `object : T` therefore already owned `<owner>$<fn>$1`, and the continuation took it again: two
//! classes, one name. The continuation's own constructor call then resolved to the anonymous
//! object's constructor and emission aborted the WHOLE FILE on the arity disagreement
//! (`supplied=1 physical=2`), so a single such function stopped its module emitting anything.
//!
//! It needs a capture from BEFORE the suspension point and one produced AFTER it: with only the
//! post-suspension value the object captures nothing that has to survive the state machine, and
//! the constructor arities happen to agree.
use super::common;

const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
    interface Holder { val first: String; val second: String }\n\
    suspend fun fetch(id: String): String = id + \"!\"\n\
    suspend fun make(before: String, id: String): Holder {\n\
    \x20   val after: String = fetch(id)\n\
    \x20   return object : Holder {\n\
    \x20       override val first: String = before\n\
    \x20       override val second: String = after\n\
    \x20   }\n\
    }\n\
    fun box(): String = runBlocking {\n\
    \x20   val h = make(\"A\", \"B\")\n\
    \x20   if (h.first == \"A\" && h.second == \"B!\") \"OK\" else h.first + \"/\" + h.second\n\
    }\n";

/// Compiles at all, and the object still carries BOTH captures across the suspension.
#[test]
fn a_suspend_function_that_declares_an_anonymous_object_names_both_classes() {
    let jdk = common::jdk_modules();
    let cp = [common::stdlib_jar(), common::coroutines_jar(), jdk.clone()];
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &cp, Some(jdk.as_path())),
        "OK"
    );
}
