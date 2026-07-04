//! The OrgSlug shape: a `@JvmInline value class` whose `init` block validates its underlying by calling a
//! classpath `object` (`SlugRules`, carrying `const val`s) that returns a classpath SEALED type, then
//! `require(result is SlugValidation.Ok)`. This compiled and runs once its root — a `const val` inside an
//! `object` — is supported (the value-class init reads `SlugRules.MIN`/`MAX`, inlined per kotlinc). A
//! regression lock over the interaction of: value-class `init`, a classpath object with `const val`s, a
//! `when`-returning classpath sealed type, and an `is`-smart-cast `require` in the value-class constructor.
//! Built by the real kotlinc via the shared `common::run_box_against` harness.
use super::common;

const LIB: &str = "package lib\n\
     sealed class SlugValidation {\n\
     \x20 object Ok : SlugValidation()\n\
     \x20 data class TooShort(val min: Int) : SlugValidation()\n\
     \x20 data class TooLong(val max: Int) : SlugValidation()\n\
     \x20 object Empty : SlugValidation()\n\
     }\n\
     object SlugRules {\n\
     \x20 const val MIN = 3\n\
     \x20 const val MAX = 63\n\
     \x20 fun validate(s: String): SlugValidation = when {\n\
     \x20   s.isEmpty() -> SlugValidation.Empty\n\
     \x20   s.length < MIN -> SlugValidation.TooShort(MIN)\n\
     \x20   s.length > MAX -> SlugValidation.TooLong(MAX)\n\
     \x20   else -> SlugValidation.Ok\n\
     \x20 }\n\
     }\n";

#[test]
fn value_class_init_validates_via_classpath_sealed() {
    let main = "import lib.SlugRules\n\
        import lib.SlugValidation\n\
        @JvmInline value class OrgSlug(val v: String) {\n\
        \x20 init { require(SlugRules.validate(v) is SlugValidation.Ok) }\n\
        }\n\
        fun box(): String {\n\
        \x20 val s = OrgSlug(\"my-org\")\n\
        \x20 if (s.v != \"my-org\") return \"fail value: ${s.v}\"\n\
        \x20 if (SlugRules.MAX != 63) return \"fail const\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("vc_init_validate", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}
