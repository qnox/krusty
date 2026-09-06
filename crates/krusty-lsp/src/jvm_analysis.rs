//! JVM target setup for standalone, in-process editor analysis.
//!
//! Compiler analysis itself accepts only the frontend's semantic-platform contract. This adapter
//! owns JDK discovery and the JVM provider so target details do not leak into that core layer.

use krusty::features::LangFeatures;
use krusty::source::SourceInput;

use crate::compiler_analysis::{self, SourceSetAnalysis};

pub(crate) fn analyze_standalone_source_inputs(inputs: &[SourceInput<'_>]) -> SourceSetAnalysis {
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(
        krusty::toolchain::jdk_modules().into_iter().collect(),
    ));
    compiler_analysis::analyze_source_inputs_with_features(
        inputs,
        Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
    )
}
