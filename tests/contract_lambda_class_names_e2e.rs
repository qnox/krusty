//! A function's `contract { … }` description is dropped from its body before kotlinc numbers the
//! generated classes, so the lambda it holds takes no position: the first lambda class declared
//! after it is `$1`, not `$2`. Only a call that selects `kotlin.contracts.contract` is one; a
//! call of any other function spelled `contract` stays in the body and is numbered. kotlinc names
//! every generated class in that one walk, anonymous objects and local classes included.
//!
//! PLATFORM-SPECIFIC: generated-class ordinals are JVM class-file names, so these compare class
//! sets with kotlinc rather than run a shared box program.

use super::common;

fn assert_classes_match_kotlinc(file: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(file, source)]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_contract_description_lambda_takes_no_class_ordinal() {
    assert_classes_match_kotlinc(
        "Contracts.kt",
        r#"
import kotlin.contracts.*

@OptIn(ExperimentalContracts::class)
fun <T> runIt(block: suspend () -> T): suspend () -> T {
    contract {
        callsInPlace(block, InvocationKind.EXACTLY_ONCE)
    }
    return { block() }
}
"#,
    );
}

#[test]
fn a_qualified_contract_description_lambda_takes_no_class_ordinal() {
    assert_classes_match_kotlinc(
        "QualifiedContracts.kt",
        r#"
import kotlin.contracts.ExperimentalContracts
import kotlin.contracts.InvocationKind

@OptIn(ExperimentalContracts::class)
fun <T> runIt(block: suspend () -> T): suspend () -> T {
    kotlin.contracts.contract {
        callsInPlace(block, InvocationKind.EXACTLY_ONCE)
    }
    return { block() }
}
"#,
    );
}

#[test]
fn a_lambda_passed_to_a_function_named_contract_takes_its_class_ordinal() {
    assert_classes_match_kotlinc(
        "UserContract.kt",
        r#"
package user

fun contract(block: () -> Unit) = block()

fun <T> runIt(block: suspend () -> T): suspend () -> T {
    contract {
        println("described")
    }
    return { block() }
}

fun objectAfter(): Any {
    contract {
        println("described")
    }
    return object {}
}
"#,
    );
}

#[test]
fn an_object_after_a_contract_description_takes_the_next_class_ordinal() {
    assert_classes_match_kotlinc(
        "ContractThenObject.kt",
        r#"
import kotlin.contracts.*

@OptIn(ExperimentalContracts::class)
fun <T> pairIt(block: suspend () -> T): Pair<Any, suspend () -> T> {
    contract {
        callsInPlace(block, InvocationKind.EXACTLY_ONCE)
    }
    return Pair(object {}, suspend { block() })
}

@OptIn(ExperimentalContracts::class)
fun <T> localIn(block: suspend () -> T): suspend () -> Any {
    contract {
        callsInPlace(block, InvocationKind.EXACTLY_ONCE)
    }
    return suspend {
        class Local(val value: Any?)
        Local(block())
    }
}
"#,
    );
}

#[test]
fn an_object_passed_to_a_function_named_contract_takes_its_class_ordinal() {
    assert_classes_match_kotlinc(
        "UserContractObjects.kt",
        r#"
package user

fun contract(task: Runnable) = task.run()

fun <T> objectArgument(block: suspend () -> T): suspend () -> T {
    contract(object : Runnable {
        override fun run() {}
    })
    return suspend { block() }
}

fun <T> objectAfter(block: suspend () -> T): Pair<Any, suspend () -> T> {
    contract(Runnable { println("described") })
    return Pair(object {}, suspend { block() })
}
"#,
    );
}
