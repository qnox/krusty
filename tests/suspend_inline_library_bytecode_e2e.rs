//! A library `suspend inline fun` is inlined from its compiled body, as kotlinc does.
//!
//! kotlinc copies the library method's bytecode for every suspend inline call: the `$default`
//! bridge's mask handling for an omitted argument, the callee's locals and `$i$f` markers, the
//! suspension markers the coroutine transform keys on, and the SMAP line mapping. The calling
//! method's continuation then spills exactly the locals that copy keeps alive.
//!
//! krusty used to rebuild `Mutex.withLock` and `Semaphore.withPermit` from a structural plan of
//! their enter/cleanup calls, which wrote neither the default bridge's locals nor the copied
//! markers, so the caller and its continuation both differed from kotlinc.
use super::common;

const SOURCE: &str = r#"
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.sync.withPermit

class Store {
    private val mutex = Mutex()
    private val semaphore = Semaphore(1)
    private var state = 0

    suspend fun update(delta: Int): Int = mutex.withLock {
        state += delta
        state
    }

    suspend fun owned(delta: Int): Int = mutex.withLock(this) {
        state += delta
        state
    }

    suspend fun permit(delta: Int): Int = semaphore.withPermit {
        state += delta
        state
    }
}
"#;

/// The caller and each continuation are kotlinc's, byte for byte.
#[test]
fn library_suspend_inline_calls_match_kotlinc() {
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "SuspendInlineLibrary",
        SOURCE,
        &["Store", "Store$update$1", "Store$owned$1", "Store$permit$1"],
        &[common::coroutines_jar()],
    );
}
