//! An inner class reads a superclass capture from the enclosing instance by closure identity.
//!
//! Two receivers may share a callable or lambda label. The super argument must follow the identity
//! the superclass captured, and the enclosing-instance parameter is selected by its recorded role.

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::DiagSink;
use crate::ir::IrFile;

fn jvm_platform() -> Box<dyn crate::libraries::SemanticPlatform> {
    Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(Rc::new(
            crate::jvm::classpath::Classpath::new(crate::toolchain::classpath_jars_for(
                "// WITH_STDLIB",
            )),
        ))
        .expect("JVM provider initialization"),
    )
}

#[derive(Clone)]
struct CaptureBackend(Rc<RefCell<Vec<IrFile>>>);

impl Backend for CaptureBackend {
    type State = ();

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        _state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        self.0.borrow_mut().push(file.ir);
        Vec::new()
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        Vec::new()
    }
}

fn capture_ir(source: &str, stem: &str) -> IrFile {
    let mut diagnostics = DiagSink::new();
    let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem(stem)];
    let stems = [stem.to_string()];
    let features = crate::features::LangFeatures::from_source(source);
    let platform = crate::frontend::PlatformProvider::from(jvm_platform()).with_native_plugins(
        crate::plugins::registry::PluginRegistry::with_builtins().every_native_extension(),
    );
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        platform,
        &features,
        &mut diagnostics,
    );
    let captured = Rc::new(RefCell::new(Vec::new()));
    let backend = CaptureBackend(Rc::clone(&captured));
    crate::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diagnostics);
    assert!(
        diagnostics.diags.is_empty(),
        "{stem}: {:?}",
        diagnostics.diags
    );
    let file = {
        let mut files = captured.borrow_mut();
        files.pop()
    };
    file.unwrap_or_else(|| panic!("{stem}: no common IR"))
}

#[test]
fn inner_super_argument_follows_the_receiver_identity_not_its_label() {
    let cases = [
        (
            "class Host(val mark: String)\n\
             fun Host.bar(): String {\n\
                 return \"OK\".run bar@{\n\
                     open class Local {\n\
                         fun read() = this@bar\n\
                     }\n\
                     class Holder {\n\
                         fun fromHost() = mark\n\
                         inner class Inner : Local()\n\
                     }\n\
                     val inner = Holder().Inner().read()\n\
                     val seen = Holder().fromHost()\n\
                     if (inner == \"OK\" && seen == \"NO\") \"OK\" else inner + seen\n\
                 }\n\
             }\n\
             fun box() = Host(\"NO\").bar()\n",
            "Callable",
        ),
        (
            "class Host(val mark: String)\n\
             fun box(): String {\n\
                 return Host(\"NO\").run label@{\n\
                     \"OK\".run label@{\n\
                         open class Local {\n\
                             fun read() = this@label\n\
                         }\n\
                         class Holder {\n\
                             fun fromHost() = mark\n\
                             inner class Inner : Local()\n\
                         }\n\
                         val seen = Holder().fromHost()\n\
                         val inner = Holder().Inner().read()\n\
                         if (inner == \"OK\" && seen == \"NO\") \"OK\" else inner + seen\n\
                     }\n\
                 }\n\
             }\n",
            "Lambda",
        ),
    ];
    for (source, stem) in cases {
        let ir = capture_ir(source, stem);
        let inner = ir
            .classes
            .iter()
            .find(|class| class.is_inner_class)
            .unwrap_or_else(|| panic!("{stem}: inner class"));
        assert!(
            inner.ctor_args.iter().any(|argument| {
                argument.provenance == crate::ir::IrCtorParameterProvenance::EnclosingInstance
            }),
            "{stem}: enclosing instance has no recorded role"
        );
        let parent = ir
            .class_id_by_name(inner.superclass)
            .unwrap_or_else(|| panic!("{stem}: superclass"));
        let parent_identity = ir.classes[parent as usize]
            .ctor_args
            .iter()
            .find_map(|argument| argument.capture_identity)
            .unwrap_or_else(|| panic!("{stem}: superclass capture identity"));
        let field_read = inner
            .super_args
            .first()
            .copied()
            .unwrap_or_else(|| panic!("{stem}: super arguments {:?}", inner.super_args));
        let crate::ir::IrExpr::GetField { class, index, .. } = ir.expr(field_read) else {
            panic!("{stem}: leading super argument {:?}", ir.expr(field_read));
        };
        let enclosing = &ir.classes[*class as usize];
        let chosen = enclosing
            .ctor_args
            .iter()
            .find(|argument| argument.field_index == Some(*index))
            .unwrap_or_else(|| panic!("{stem}: chosen field"));
        assert_eq!(
            chosen.capture_identity,
            Some(parent_identity),
            "{stem}: super argument does not read the superclass capture identity"
        );
        let other_labeled_receiver = enclosing.ctor_args.iter().any(|argument| {
            argument.field_index != Some(*index)
                && argument.capture_identity != Some(parent_identity)
                && matches!(
                    argument
                        .capture
                        .as_ref()
                        .and_then(|capture| capture.receiver.as_ref()),
                    Some(
                        crate::ir::IrCapturedReceiver::Callable(_)
                            | crate::ir::IrCapturedReceiver::Lambda(_)
                    )
                )
        });
        assert!(
            other_labeled_receiver,
            "{stem}: enclosing class did not keep a second receiver capture\n{:?}",
            enclosing
                .ctor_args
                .iter()
                .map(|argument| (&argument.capture, argument.capture_identity, argument.ty))
                .collect::<Vec<_>>()
        );
    }
}
