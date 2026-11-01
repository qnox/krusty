//! Native reimplementation of the Parcelize compiler plugin (`org.jetbrains.kotlin.parcelize`).
//!
//! kotlinc's plugin makes a class annotated `@Parcelize` implement `android.os.Parcelable`: its
//! FIR declaration generator adds `describeContents()` and `writeToParcel(dest, flags)` where the
//! class does not declare them, and its IR transform fills their bodies and adds the static
//! `CREATOR` field, whose value is an instance of a local `Creator` class that reads the class
//! back from a `Parcel`.

mod facts;
mod frontend;
mod generate;
mod serializers;

pub use facts::ParcelizeFileFacts;

use crate::ir::IrFile;
use crate::plugins::runtime_declarations::PluginRuntime;
use crate::plugins::{
    FrontendCallable, FrontendClassCheckContext, FrontendClassContext, FrontendPluginDiagnostic,
    IrPlugin, PluginContext, PluginRejection, PluginSourceView,
};

/// The annotations that make a class parcelized without options: the current runtime's and the
/// deprecated Android extensions'.
const PARCELIZE_ANNOTATIONS: [&str; 2] = [
    "kotlinx/parcelize/Parcelize",
    "kotlinx/android/parcel/Parcelize",
];

/// The packages of the Parcelize runtime's annotations, current and deprecated.
const RUNTIME_PACKAGES: [&str; 2] = ["kotlinx/parcelize", "kotlinx/android/parcel"];

/// One of the Parcelize runtime's annotations.
fn is_runtime_annotation(annotation: crate::types::TypeName) -> bool {
    RUNTIME_PACKAGES
        .iter()
        .any(|&package| annotation.parent() == Some(crate::types::type_name(package)))
}

/// `android.os.Parcel`, which the generated members write to and read from.
const PARCEL: &str = "android/os/Parcel";

/// `android.os.Parcelable.Creator`, the interface of a parcelized class's `CREATOR`.
fn creator_interface() -> crate::types::TypeName {
    crate::types::type_name("android/os/Parcelable").nested_child("Creator")
}

/// The Parcelize plugin, configured for one compilation.
pub struct ParcelizePlugin {
    /// Every annotation that triggers the plugin: the built-in ones, then `additionalAnnotation`s.
    annotations: Vec<crate::types::TypeName>,
}

impl ParcelizePlugin {
    /// The plugin as `-P plugin:org.jetbrains.kotlin.parcelize:additionalAnnotation=<fq name>`
    /// options configure it.
    pub fn from_options(options: &[crate::plugins::cli::PluginOption]) -> Self {
        let annotations = PARCELIZE_ANNOTATIONS
            .iter()
            .map(|&annotation| crate::types::type_name(annotation))
            .chain(
                options
                    .iter()
                    .filter(|option| option.key == "additionalAnnotation")
                    .flat_map(|option| {
                        crate::plugins::allopen::classifier_identities(&option.value)
                    }),
            )
            .collect();
        Self { annotations }
    }
}

impl IrPlugin for ParcelizePlugin {
    fn name(&self) -> &str {
        "parcelize"
    }

    fn generate_frontend_declarations(
        &self,
        ctx: &FrontendClassContext<'_>,
        members: &mut Vec<FrontendCallable>,
    ) {
        frontend::declare_members(&self.annotations, ctx, members);
    }

    fn check_frontend_class(
        &self,
        ctx: &FrontendClassCheckContext<'_>,
        diagnostics: &mut Vec<FrontendPluginDiagnostic>,
    ) {
        frontend::check_class(&self.annotations, ctx, diagnostics);
    }

    fn record_source_facts(
        &self,
        ir: &mut IrFile,
        source: &PluginSourceView<'_>,
        runtime: &mut dyn PluginRuntime,
        rejections: &mut Vec<PluginRejection>,
    ) {
        facts::record(ir, &self.annotations, source, runtime, rejections);
    }

    fn complete_declarations(&self, ir: &mut IrFile, _ctx: &PluginContext) {
        if let Some(facts) = ir.parcelize.take() {
            generate::complete(ir, &facts);
        }
    }
}
