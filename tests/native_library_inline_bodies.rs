//! Library inline bodies the native backend meets once a KLIB template is expanded at its call.
//!
//! An expanded `TODO()`, `assertFailsWith`, `toUInt()` or `Byte.and` no longer reaches the backend
//! as the call the program wrote: it arrives as the body the library declares, which constructs a
//! defaulted `NotImplementedError`, a `Result`, an unsigned value, or calls a primitive operator as
//! a member. Each test settles the answer on the JVM first and then requires the Native lane, which
//! analyzes against the Kotlin/Native stdlib KLIB and so expands its templates, to run the same
//! program to `OK`.

use super::super::common::{expect_box_run, jdk_modules, kotlin_test_jar, stdlib_jar};
use super::{host, Lane, NativeLane, Outcome};

fn expect_both(src: &str, stem: &str) {
    let kotlin_test = kotlin_test_jar()
        .expect("no kotlin-test jar found; provision the reference toolchain (`just` fetches it)");
    let jdk = jdk_modules();
    assert_eq!(
        expect_box_run(src, stem, &[stdlib_jar(), kotlin_test], Some(jdk.as_path())),
        "OK",
        "{stem}: the JVM oracle"
    );
    let Some(target) = host() else {
        if std::env::var_os("KRUSTY_REQUIRE_NATIVE_CONFORMANCE").is_some() {
            panic!("{stem}: required Native target/KLIB setup is unavailable");
        }
        return;
    };
    let lane = NativeLane {
        target: Some(target),
    };
    let outcome = lane.compile_and_run(
        &std::env::temp_dir(),
        stem,
        &[(format!("{stem}.kt"), src.to_string())],
        src,
    );
    assert_eq!(outcome, Outcome::Pass, "{stem}: the Native lane");
}

#[test]
fn a_todo_carries_its_declared_default_message() {
    expect_both(
        "fun box(): String = try {\n\
         \x20   TODO()\n\
         } catch (e: NotImplementedError) {\n\
         \x20   if (e.message == \"An operation is not implemented.\") \"OK\" else \"fail: ${e.message}\"\n\
         }\n",
        "TodoDefaultMessage",
    );
}

#[test]
fn assert_fails_with_answers_the_exception_its_block_threw() {
    expect_both(
        "import kotlin.test.*\n\
         fun box(): String {\n\
         \x20   val thrown = assertFailsWith<IllegalStateException> { error(\"boom\") }\n\
         \x20   if (thrown.message != \"boom\") return \"fail: ${thrown.message}\"\n\
         \x20   val any = assertFails { throw IllegalArgumentException(\"any\") }\n\
         \x20   return if (any.message == \"any\") \"OK\" else \"fail: ${any.message}\"\n\
         }\n",
        "AssertFailsWithBlock",
    );
}

#[test]
fn assert_fails_with_fails_a_block_that_completes() {
    expect_both(
        "import kotlin.test.*\n\
         fun box(): String = try {\n\
         \x20   assertFailsWith<IllegalStateException> { }\n\
         \x20   \"fail: no assertion error\"\n\
         } catch (e: AssertionError) {\n\
         \x20   \"OK\"\n\
         }\n",
        "AssertFailsWithCompletedBlock",
    );
}

#[test]
fn unsigned_values_are_constructed_and_read_as_their_bits() {
    expect_both(
        "fun box(): String {\n\
         \x20   val big = (-1).toUInt()\n\
         \x20   if (big / 2u != 2147483647u) return \"fail: div ${big / 2u}\"\n\
         \x20   if (big % 10u != 5u) return \"fail: rem ${big % 10u}\"\n\
         \x20   if (big.toLong() != 4294967295L) return \"fail: widen ${big.toLong()}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedConstructionAndData",
    );
}

#[test]
fn narrow_bitwise_operators_reach_the_int_operators_they_are_declared_by() {
    expect_both(
        "import kotlin.experimental.*\n\
         fun box(): String {\n\
         \x20   val a: Byte = 0x0F\n\
         \x20   val b: Byte = 0x3C\n\
         \x20   if ((a and b) != 0x0C.toByte()) return \"fail: and\"\n\
         \x20   if ((a or b) != 0x3F.toByte()) return \"fail: or\"\n\
         \x20   if (a.inv() != (-16).toByte()) return \"fail: inv\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "NarrowBitwiseOperators",
    );
}

#[test]
fn a_narrow_element_is_found_in_a_range_held_under_its_interface() {
    expect_both(
        "fun box(): String {\n\
         \x20   val range = 1..3\n\
         \x20   val inside: Byte = 2\n\
         \x20   val outside: Byte = 4\n\
         \x20   return if (inside in range && outside !in range) \"OK\" else \"fail\"\n\
         }\n",
        "NarrowElementInRange",
    );
}

#[test]
fn a_built_string_is_the_text_its_builder_holds() {
    expect_both(
        "fun box(): String = buildString { append(\"O\"); append('K') }\n",
        "BuiltString",
    );
}
