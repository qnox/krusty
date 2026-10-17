//! A constructor call that leaves a self-bounded Java type parameter unconstrained types as the
//! bound with the recursive occurrence starred and the argument projected `out`, as kotlinc does:
//! `Builder("x")` for `class Builder<SELF extends Builder<SELF>>` is `Builder<out Builder<*>>`.
//! Every fluent member returning `SELF` therefore reads `Builder<*>`, and a chain keeps the
//! classifier's members at every step.

use super::common;

#[test]
fn a_self_bounded_constructor_result_keeps_a_fluent_chain_typed() {
    let builder = "package fixtures;\n\
        public class Builder<SELF extends Builder<SELF>> {\n\
        \x20 private final StringBuilder log;\n\
        \x20 public Builder(String name) { this.log = new StringBuilder(name); }\n\
        \x20 @SuppressWarnings(\"unchecked\")\n\
        \x20 private SELF self() { return (SELF) this; }\n\
        \x20 public SELF withPort(int port) { log.append(':').append(port); return self(); }\n\
        \x20 public SELF withEnv(String key, String value) { log.append(' ').append(key).append('=').append(value); return self(); }\n\
        \x20 public String describe() { return log.toString(); }\n\
        }\n";
    let use_src = "import fixtures.Builder\n\
        fun box(): String {\n\
        \x20 val built = Builder(\"ssh\").withPort(22).withEnv(\"USER\", \"test\").withEnv(\"SUDO\", \"false\")\n\
        \x20 if (built.describe() != \"ssh:22 USER=test SUDO=false\") return built.describe()\n\
        \x20 val exact = Builder(\"plain\")\n\
        \x20 val step: Builder<*> = exact.withPort(1)\n\
        \x20 if (step.withPort(2).withPort(3).describe() != \"plain:1:2:3\") return \"chain\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let out = common::java_interop_box(
        "java_self_bounded_constructor",
        &[("Builder.java", builder)],
        use_src,
    );
    assert_eq!(out, "OK");
}
