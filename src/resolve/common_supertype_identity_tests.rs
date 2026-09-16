use super::*;
use crate::lexer::lex;
use crate::parser::parse;

#[test]
fn reflective_function_join_uses_the_declared_kfunction_supertype() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let mut diagnostics = DiagSink::new();
    let source = "package kotlin.reflect\n\
         abstract class Reflective : KFunction<String>, (String) -> String\n\
         abstract class KFunctionPretender : (String) -> String";
    let tokens = lex(source, &mut diagnostics);
    let file = parse(source, &tokens, &mut diagnostics);
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![stdlib])),
        )),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:#?}", diagnostics.diags);

    let mut probe_diagnostics = DiagSink::new();
    let checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);
    let function = Ty::fun(vec![Ty::String], Ty::String);
    let reflective = Ty::obj("kotlin/reflect/Reflective");
    assert_eq!(
        crate::assignable::applied_supertype(
            &checker,
            reflective,
            Ty::obj(crate::types::KFUNCTION_INTERNAL),
        )
        .map(|ty| ty.kotlin_class_internal()),
        Some(Some(type_name(crate::types::KFUNCTION_INTERNAL)))
    );
    assert_eq!(
        crate::symbol_resolver::classifier_callable_signature(&checker.fed_source(), reflective,),
        Some(function)
    );
    let join = |classifier| {
        semantic_common_supertype(
            &checker.fed_source(),
            &checker,
            function,
            Ty::obj(classifier),
        )
    };

    assert_eq!(
        join("kotlin/reflect/Reflective"),
        Some(function),
        "a declaration that inherits KFunction keeps its exact callable shape"
    );
    assert_eq!(
        join("kotlin/reflect/KFunctionPretender"),
        None,
        "a KFunction-looking spelling does not make an unrelated callable classifier reflective"
    );
    assert!(
        probe_diagnostics.diags.is_empty(),
        "{:#?}",
        probe_diagnostics.diags
    );
}
