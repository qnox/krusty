//! Invocation-owned JVM emission settings.

use super::{InnerClassResolver, JvmDefaultMode, LambdaModes};

/// Per-file emission configuration passed explicitly down the emit callgraph and stamped onto every
/// `ClassWriter` so synthetic serializer/companion/DefaultImpls classes inherit it too. The
/// `Default` is v52 with no `SourceFile`; shipping paths build their options through
/// [`crate::jvm::backend::shipping_emit_options`].
#[derive(Clone)]
pub struct EmitOptions {
    /// Class-file major version to emit (default v52; `-jvm-target 25` ⇒ v69).
    pub class_major: Option<u16>,
    /// Source-file simple name for the `SourceFile` attribute (e.g. `Foo.kt`); `None` ⇒ no attribute.
    pub source_file: Option<String>,
    /// `-module-name` recorded in each class's `@Metadata`; `None` matches kotlinc's default `main`.
    pub module_name: Option<String>,
    /// Emit computed `@kotlin.Metadata` for supported class shapes.
    pub emit_class_metadata: bool,
    /// `-jvm-default`: the JVM shape of interface members with bodies.
    pub jvm_default: JvmDefaultMode,
    /// Emit `Intrinsics.checkNotNullParameter` guards (`-Xno-param-assertions` clears this).
    pub param_assertions: bool,
    /// Independent `-Xlambdas` / `-Xsam-conversions` strategies for this invocation.
    pub lambda_modes: LambdaModes,
    pub inner_class_resolver: Option<InnerClassResolver>,
    /// The file's value classes, for the redundant-boxing pass; the emitter fills it per file.
    pub(crate) value_classes:
        std::rc::Rc<crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors>,
    /// `-java-parameters`: name each declared parameter in a `MethodParameters` attribute.
    pub java_parameters: bool,
    /// `-Xstring-concat=inline`: always use `StringBuilder`, regardless of class-file version.
    pub inline_string_concat: bool,
    /// `-language-version X.Y`: the `@kotlin.Metadata` and `.kotlin_module` version stamp.
    pub metadata_version: Option<[i32; 3]>,
    /// Whether language settings enable declaration annotation records in Kotlin metadata.
    pub annotations_in_metadata: bool,
    /// Whether emitted Kotlin metadata carries kotlinc's pre-release flag.
    pub pre_release_metadata: bool,
}

/// The `mv` written without `-language-version`: kotlinc's default-language-version stamp.
pub const DEFAULT_METADATA_VERSION: [i32; 3] = [2, 4, 0];

impl EmitOptions {
    pub fn metadata_version(&self) -> [i32; 3] {
        self.metadata_version.unwrap_or(DEFAULT_METADATA_VERSION)
    }

    pub fn metadata_stamp(&self) -> crate::jvm::classfile::MetadataStamp {
        crate::jvm::classfile::MetadataStamp {
            version: self.metadata_version(),
            pre_release: self.pre_release_metadata,
        }
    }

    pub fn with_pre_release_metadata(mut self, pre_release: bool) -> Self {
        self.pre_release_metadata = pre_release;
        self
    }

    pub fn with_metadata_version(mut self, version: Option<[i32; 3]>) -> Self {
        self.metadata_version = version;
        self
    }

    pub fn with_annotations_in_metadata(mut self, enabled: bool) -> Self {
        self.annotations_in_metadata = enabled;
        self
    }

    pub fn with_jvm_default(mut self, mode: JvmDefaultMode) -> Self {
        self.jvm_default = mode;
        self
    }

    pub fn with_java_parameters(mut self, enabled: bool) -> Self {
        self.java_parameters = enabled;
        self
    }

    pub fn with_lambda_modes(mut self, modes: LambdaModes) -> Self {
        self.lambda_modes = modes;
        self
    }

    pub fn with_inline_string_concat(mut self, inline: bool) -> Self {
        self.inline_string_concat = inline;
        self
    }

    /// `-Xno-param-assertions` passes `false`.
    pub fn with_param_assertions(mut self, enabled: bool) -> Self {
        self.param_assertions = enabled;
        self
    }
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            class_major: None,
            source_file: None,
            module_name: None,
            emit_class_metadata: true,
            jvm_default: JvmDefaultMode::Enable,
            param_assertions: true,
            java_parameters: false,
            inline_string_concat: false,
            lambda_modes: LambdaModes::default(),
            inner_class_resolver: None,
            value_classes: std::rc::Rc::default(),
            metadata_version: None,
            annotations_in_metadata: true,
            pre_release_metadata: false,
        }
    }
}
