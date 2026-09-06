use super::*;
use crate::diag::{Diagnostic, Span};
use crate::libraries::{
    CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig, LibraryCallable,
    LibraryType, PropKind, PropertyInfo, PropertySet, ResolvedSymbols, TypeKind,
};
use crate::source::SourceInput;
use crate::types::{Ty, TypeName, TypeNameList, Visibility};

mod analysis;
mod retention;
mod streaming;

#[test]
fn anonymous_defaults_receive_enclosing_classifier_identities() {
    fn anonymous_name(source: &str, facade: &str) -> String {
        let mut diagnostics = crate::diag::DiagSink::new();
        let mut file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
        assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
        name_anonymous_classes(&mut file, facade);
        let declaration = *file
            .anonymous_object_classes
            .values()
            .next()
            .expect("source must contain an anonymous object");
        let crate::ast::Decl::Class(class) = file.decl(declaration) else {
            panic!("anonymous object must map to a classifier")
        };
        class.name.clone()
    }

    assert_eq!(
        anonymous_name(
            "open class FooA\nclass BarA(val foo: FooA? = object : FooA() {})",
            "AKt",
        ),
        "BarA$1",
    );
    assert_eq!(
        anonymous_name(
            "open class FooC\nclass BarC(val foo: FooC? = object : FooC() {})",
            "CKt",
        ),
        "BarC$1",
    );
}
