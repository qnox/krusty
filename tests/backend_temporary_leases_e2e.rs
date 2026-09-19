//! Every slot the BACKEND owns, live at once, in one method.
//!
//! These slots — an operand spilled across a branch, a `&&` operand held across its right-hand
//! side, a vararg array under construction, a `try` result parked across a `finally`, a catch-all's
//! caught throwable, a return value parked across a finalizer, a default stub's masks and marker —
//! are named by no semantic value. They used to be registered in the emitter's semantic slot map
//! under reserved numeric keys (`1_000_000` … `5_000_000`, `9_000_001`), which meant a value id
//! that ever reached one of those ranges would silently overwrite a live temporary, and the
//! temporary itself was mixed in with values that lexical scoping removes and definite-assignment
//! analysis filters.
//!
//! They are leased now, under an identity that is not a value id at all, and released where they
//! die. The failure mode a mis-scoped lease produces is a `VerifyError` — a frame that claims a
//! slot is live when it is not, or omits one that is — so these cases are RUN, not just compiled:
//! the JVM verifier is the oracle.

use super::common;

fn run(source: &str, stem: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} must compile and run"))
}

/// One method holding several kinds at once, each nested inside another's live range: a `try`
/// result parked across a `finally`, a catch-all throwable live while that `finally` itself runs a
/// `try`, a `&&` spill inside it, and a vararg built from branchy elements. A lease released too
/// early drops a slot out of a frame the verifier then rejects; one released too late leaves a slot
/// typed on a path that never stored it.
#[test]
fn nested_backend_temporaries_stay_live_exactly_while_they_are() {
    assert_eq!(
        run(
            "fun pick(flag: Boolean, vararg parts: String): String = parts[if (flag) 0 else 1]\n\
             \n\
             fun tangle(flag: Boolean, other: Boolean): String {\n\
             \x20   val outer = try {\n\
             \x20       if (flag) throw IllegalStateException(\"x\")\n\
             \x20       \"body\"\n\
             \x20   } finally {\n\
             \x20       try {\n\
             \x20           val held = flag && other\n\
             \x20           pick(held, if (flag) \"a\" else \"b\", if (other) \"c\" else \"d\")\n\
             \x20       } finally {\n\
             \x20           pick(other, \"e\", \"f\")\n\
             \x20       }\n\
             \x20   }\n\
             \x20   return outer\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (tangle(false, true) != \"body\") return \"FAIL1\"\n\
             \x20   if (tangle(false, false) != \"body\") return \"FAIL2\"\n\
             \x20   try {\n\
             \x20       tangle(true, true)\n\
             \x20       return \"FAIL3\"\n\
             \x20   } catch (e: IllegalStateException) {\n\
             \x20       if (e.message != \"x\") return \"FAIL4\"\n\
             \x20   }\n\
             \x20   return \"OK\"\n\
             }\n",
            "NestedBackendTemporaries",
        ),
        "OK"
    );
}

/// A RETURN parked across a finalizer that itself branches, with a `&&` spill and a vararg inside
/// the finalizer. The parked return has to be typed in every frame the finalizer records, and must
/// stop being typed once the transfer completes.
#[test]
fn a_parked_return_survives_a_branchy_finalizer() {
    assert_eq!(
        run(
            "fun join(vararg parts: String): String = parts.joinToString(\"\")\n\
             \n\
             fun parked(flag: Boolean, other: Boolean): String {\n\
             \x20   try {\n\
             \x20       return join(if (flag) \"O\" else \"o\", if (other) \"K\" else \"k\")\n\
             \x20   } finally {\n\
             \x20       val held = flag && other\n\
             \x20       join(if (held) \"1\" else \"2\", \"3\")\n\
             \x20   }\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (parked(true, true) != \"OK\") return \"FAIL1\"\n\
             \x20   if (parked(false, true) != \"oK\") return \"FAIL2\"\n\
             \x20   if (parked(true, false) != \"Ok\") return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ParkedReturnFinalizer",
        ),
        "OK"
    );
}

/// A DEFAULT STUB's masks and marker, which live for the whole synthetic method, alongside a body
/// that spills. The stub is reached through every defaulted arity so each mask word is exercised.
#[test]
fn a_default_stubs_masks_are_live_for_the_whole_stub() {
    assert_eq!(
        run(
            "fun join(vararg parts: String): String = parts.joinToString(\"\")\n\
             \n\
             class Holder {\n\
             \x20   fun many(\n\
             \x20       a: String = \"a\",\n\
             \x20       b: String = \"b\",\n\
             \x20       c: Boolean = true,\n\
             \x20       d: Boolean = false,\n\
             \x20   ): String = join(a, b, if (c && d) \"1\" else \"0\")\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   val holder = Holder()\n\
             \x20   if (holder.many() != \"ab0\") return \"FAIL1\"\n\
             \x20   if (holder.many(\"x\") != \"xb0\") return \"FAIL2\"\n\
             \x20   if (holder.many(\"x\", \"y\") != \"xy0\") return \"FAIL3\"\n\
             \x20   if (holder.many(\"x\", \"y\", true, true) != \"xy1\") return \"FAIL4\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "DefaultStubMasks",
        ),
        "OK"
    );
}

/// A SUPER-CONSTRUCTOR argument list whose elements branch, so the constructor spills them to
/// temporaries before `invokespecial`. The temporaries are live across the whole argument list and
/// must be gone from the frames of anything that follows.
#[test]
fn spilled_super_constructor_arguments_are_released_after_the_call() {
    assert_eq!(
        run(
            "open class Base(val first: String, val second: String)\n\
             \n\
             class Derived(flag: Boolean, other: Boolean) :\n\
             \x20   Base(if (flag) \"O\" else \"o\", if (other) \"K\" else \"k\") {\n\
             \x20   val tail: String = if (flag && other) \"!\" else \"?\"\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   val yes = Derived(true, true)\n\
             \x20   if (yes.first + yes.second + yes.tail != \"OK!\") return \"FAIL1\"\n\
             \x20   val no = Derived(false, false)\n\
             \x20   if (no.first + no.second + no.tail != \"ok?\") return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "SpilledSuperArguments",
        ),
        "OK"
    );
}
