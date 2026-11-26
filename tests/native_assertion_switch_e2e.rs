//! The Native stdlib's assertion switches, reached through a serialized library body.
//!
//! `kotlin.native.concurrent.atomicLazy` is public and its `AtomicLazyImpl.value` body calls the
//! stdlib's internal `isAssertionArgumentEvaluationEnabled()` and
//! `isAssertionThrowingErrorEnabled()`, whose own bodies throw `NotImplementedError("Implemented as
//! intrinsic")`. The compiler answers them for the build (off, as in kotlinc-native's default
//! build), so a read must run the initializer and return its value instead of entering those bodies.
//!
//! PLATFORM-SPECIFIC: `atomicLazy` exists only in the Native stdlib, so the JVM oracle cannot
//! compile this program; kotlinc-native's own answer for it is "OK".

use super::common::expect_native_box;

#[test]
fn a_library_body_that_asks_the_assertion_switches_runs_with_them_off() {
    expect_native_box(
        "import kotlin.native.concurrent.atomicLazy\n\
         \n\
         fun box(): String {\n\
         \x20   var runs = 0\n\
         \x20   val value = atomicLazy { runs += 1; \"OK\" }\n\
         \x20   if (value.value != \"OK\") return \"fail first read: ${value.value}\"\n\
         \x20   if (value.value != \"OK\") return \"fail second read\"\n\
         \x20   return if (runs == 1) \"OK\" else \"fail: initializer ran $runs times\"\n\
         }\n",
        "AssertionSwitchesInLibraryBody",
        "OK",
    );
}
