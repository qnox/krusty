//! An override of a generic declaration from a Kotlin dependency carries the dependency's own
//! unapplied signature into bridge planning. The applied source view uses `Token`, while the
//! erased bridge remains `exchange(Object): Object`; neither fact is recovered from a stdlib
//! classifier or member name.

use super::common;

const LIB: &str = "package fixture\n\
    interface Exchange<T> { fun exchange(value: T): T }\n\
    class Token(val id: Int)\n";

const SRC: &str = "import fixture.Exchange\n\
    import fixture.Token\n\
    class TokenExchange : Exchange<Token> {\n\
    \x20   override fun exchange(value: Token): Token = value\n\
    }\n";

#[test]
fn a_dependency_override_keeps_its_declared_generic_signature() {
    let classes = common::classes_against_kotlinc_lib(
        "DependencyOverrideSignature",
        &[("Fixture.kt", LIB)],
        SRC,
    )
    .expect("reference kotlinc is provisioned");
    let (reference, krusty) = classes
        .method_declarations("TokenExchange")
        .expect("both compilers emit TokenExchange");
    assert_eq!(krusty, reference);

    let reference = classes
        .reference
        .get("TokenExchange")
        .expect("kotlinc emits TokenExchange");
    let krusty = classes
        .krusty
        .get("TokenExchange")
        .expect("krusty emits TokenExchange");
    assert_eq!(
        common::member_table(krusty),
        common::member_table(reference)
    );
}
