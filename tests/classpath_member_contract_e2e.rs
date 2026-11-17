//! A `callsInPlace` contract declared by a class or object member of a compiled dependency is
//! applied at the call site like one declared by a top-level function: a deferred local assigned
//! inside the in-place lambda is definitely initialized afterwards. The dependency is built by the
//! reference kotlinc, and both compilers must accept the consumer.

use super::common;

const LIBRARY: &str = r#"@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
package neutral

import kotlin.contracts.*

object Source {
    inline fun fill(size: Int, action: (ByteArray, Int, Int) -> Int): Int {
        contract { callsInPlace(action, InvocationKind.EXACTLY_ONCE) }
        return action(ByteArray(size), 0, size)
    }
}

class Holder {
    inline fun once(action: () -> Int): Int {
        contract { callsInPlace(action, InvocationKind.EXACTLY_ONCE) }
        return action()
    }
}
"#;

const CONSUMER: &str = r#"import neutral.Holder
import neutral.Source

fun fromObject(): Int {
    var result: Int
    Source.fill(3) { _, _, end ->
        result = end
        result
    }
    return result
}

fun fromInstance(): Int {
    val result: Int
    Holder().once {
        result = 4
        result
    }
    return result
}

fun box(): String = if (fromObject() == 3 && fromInstance() == 4) "OK" else "FAIL"
"#;

#[test]
fn a_dependency_members_calls_in_place_contract_initializes_deferred_locals() {
    let library = common::kotlinc_lib_out(&[("Neutral.kt", LIBRARY)])
        .expect("reference kotlinc builds the contract dependency");
    common::ModuleClassPair::compile_with_classpath(
        &[("Consumer.kt", CONSUMER)],
        &[library],
        "ConsumerKt",
    );
}
