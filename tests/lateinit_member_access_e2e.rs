//! A `lateinit` member property is reached the way kotlinc reaches it from inside its class.
//!
//! A non-private one keeps its accessors in-class, in a constructor too: the getter is where its
//! uninitialized check lives, so a read calls `getX()` and a write calls `setX(…)`. A private one has
//! no accessors and reads its field, checked at the read with `dup; ifnonnull L; pop; ldc name;
//! invokestatic throwUninitializedPropertyAccessException; aconst_null; L:`, as is a read through
//! the `access$get…$p` field bridge of a nested class. `::x.isInitialized` reads the field
//! unchecked. krusty read and wrote the field of a non-private property directly and left out the
//! `pop` and `aconst_null` of the check.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

const SOURCE: &str = "class Fin {\n\
                      \x20   lateinit var pub: String\n\
                      \x20   internal lateinit var inner: String\n\
                      \x20   private lateinit var priv: String\n\
                      \x20   init { pub = \"a\"; priv = \"b\" }\n\
                      \x20   fun fill() { pub = \"c\"; inner = \"d\"; priv = \"e\" }\n\
                      \x20   fun read(): String = pub + inner + priv\n\
                      \x20   inner class In { fun r() = priv + pub }\n\
                      \x20   fun lam() = { priv }\n\
                      \x20   fun isSet() = this::pub.isInitialized\n\
                      }\n";

#[test]
fn a_lateinit_member_is_accessed_like_kotlinc_from_its_class() {
    byte_identical("LateinitMemberAccess", SOURCE, "Fin");
}

#[test]
fn a_nested_class_reads_a_lateinit_member_like_kotlinc() {
    byte_identical("LateinitMemberAccessNested", SOURCE, "Fin$In");
}

#[test]
fn lateinit_members_read_and_throw_through_their_accessors() {
    let src = "class C {\n\
               \x20   lateinit var pub: String\n\
               \x20   private lateinit var priv: String\n\
               \x20   fun fill() { pub = \"O\"; priv = \"K\" }\n\
               \x20   fun read(): String = pub + priv\n\
               \x20   fun readPriv(): String = priv\n\
               }\n\
               fun box(): String {\n\
               \x20   val c = C()\n\
               \x20   try { c.read(); return \"no throw\" } catch (e: UninitializedPropertyAccessException) {}\n\
               \x20   try { c.readPriv(); return \"no private throw\" } catch (e: UninitializedPropertyAccessException) {}\n\
               \x20   c.fill()\n\
               \x20   return c.read()\n\
               }\n";
    common::expect_box_ok_with_stdlib(src, "LateinitMemberThrows");
}
