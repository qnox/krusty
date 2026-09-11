//! Joining `Nothing?` — the type a bare `null` generalizes to inside an inferred type argument.
//!
//! `Pair(x, null)` is `Pair<String, Nothing?>`, not `Pair<String, Null>`: the literal's own type is
//! generalized the moment it becomes a type argument. krusty's join knew `Nothing` and the `null`
//! literal but not `Nothing?`, so joining the two branches of
//! `if (c) Pair(s, s) else Pair(s, null)` failed outright and the covariant argument fell back to
//! the declared bound — `Pair<String, out Any?>` where kotlinc infers `Pair<String, String?>`.
//!
//! The fallback is what makes this expensive rather than cosmetic: every later member lookup on the
//! widened argument is unresolved (`parsed.second?.isNotBlank()`), so one bad join can fail a whole
//! file. An expected type hides it — `Pair(s, null)` checked directly against
//! `Pair<String, String?>` infers from the expectation and never joins — which is why only an
//! inferred intermediate (`val parsed = when { … }`) shows it.
use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

fn assert_kotlinc_accepts(tag: &str, source: &str) {
    let (code, diagnostics) = common::kotlinc_source_result(tag, source);
    assert_eq!(code, 0, "kotlinc rejected {tag}: {diagnostics}");
}

#[test]
fn a_null_branch_joins_into_the_other_branchs_nullable_type() {
    const SRC: &str = "fun pick(s: String, c: Boolean): Pair<String, String?> {\n\
        \x20 val p = if (c) Pair(s, s) else Pair(s, null)\n\
        \x20 return p\n\
        }\n";
    assert_kotlinc_accepts("NothingNullablePairJoin", SRC);
    assert_eq!(diagnostics(SRC), Vec::<String>::new());
}

/// The shape this came from: a `when` whose branches build the same generic type with a `null` in
/// one argument, and whose result is then MEMBER-ACCESSED. The access is the reason the widened
/// argument is not survivable — `Any?` has no `isNotBlank`.
#[test]
fn a_widened_join_argument_would_lose_its_members() {
    const SRC: &str =
        "fun split(path: String, field: String?): Triple<String, String, String?> {\n\
        \x20 val parts = path.split('/')\n\
        \x20 val parsed = when {\n\
        \x20     field != null && parts.size >= 2 -> Triple(parts[0], parts[1], field)\n\
        \x20     parts.size == 3 -> Triple(parts[0], parts[1], parts[2])\n\
        \x20     parts.size == 2 -> Triple(parts[0], parts[1], null)\n\
        \x20     else -> error(\"bad\")\n\
        \x20 }\n\
        \x20 require(parsed.third?.isNotBlank() ?: true)\n\
        \x20 return parsed\n\
        }\n";
    assert_kotlinc_accepts("NothingNullableTripleJoin", SRC);
    assert_eq!(diagnostics(SRC), Vec::<String>::new());
}

/// The joined value must also RUN as its joined type: the `null` branch stays null, the other keeps
/// its value, and the nullable argument survives a safe call.
#[test]
fn the_joined_value_keeps_both_branches_at_runtime() {
    // The return type is INFERRED: an annotation here would check each branch against it and skip
    // the join entirely, leaving the test green with or without the fix.
    const SRC: &str = "fun pick(s: String, c: Boolean) =\n\
        \x20 if (c) Pair(s, s) else Pair(s, null)\n\
        fun box(): String {\n\
        \x20 val yes = pick(\"OK\", true)\n\
        \x20 val no = pick(\"OK\", false)\n\
        \x20 if (no.second != null) return \"null branch: ${no.second}\"\n\
        \x20 return yes.second?.take(2) ?: \"missing\"\n\
        }\n";
    assert_kotlinc_accepts("NothingNullableRuntimeJoin", SRC);
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("joined pair runs"),
        "OK"
    );
}
