//! kotlinc's `AdditionalClassAnnotationLowering` mirrors `@MustBeDocumented` on an annotation class
//! as `@java.lang.annotation.Documented`, ahead of the `Retention` and `Target` mirrors, and adds
//! none of the three when the class already carries that Java meta-annotation itself.
use super::common;

const SOURCE: &str = "package docs\n\
    \n\
    @Retention(AnnotationRetention.BINARY)\n\
    @MustBeDocumented\n\
    @Target(AnnotationTarget.TYPE, AnnotationTarget.FUNCTION, AnnotationTarget.FIELD)\n\
    annotation class Safe\n\
    \n\
    @MustBeDocumented\n\
    annotation class Plain\n\
    \n\
    @MustBeDocumented\n\
    @java.lang.annotation.Documented\n\
    @java.lang.annotation.Retention(java.lang.annotation.RetentionPolicy.CLASS)\n\
    @java.lang.annotation.Target(java.lang.annotation.ElementType.METHOD)\n\
    annotation class Explicit\n";

#[test]
fn must_be_documented_mirror_is_byte_identical_to_kotlinc() {
    // One compilation per checked class: the recorded reference keeps the classes it was asked for.
    for class in ["Safe", "Plain", "Explicit"] {
        common::byte_diff_against_kotlinc_cp(
            &format!("DocumentedMirror{class}"),
            SOURCE,
            &format!("docs/{class}"),
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("docs/{class} differs from kotlinc: {diff}"));
    }
}
