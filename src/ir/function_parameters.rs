//! Function-parameter names, defaults, and compiler-generated provenance.

use super::ExprId;

#[derive(Clone, Default, Debug)]
pub struct FnParamInfo {
    pub names: Vec<String>,
    pub defaults: Option<Vec<Option<ExprId>>>,
    /// The registered `defaults` serve only the `$default` stub. A call site must not reuse them to
    /// fill an omitted argument. This is set for extensions whose defaults are not all constant.
    pub stub_only: bool,
    /// Parameter provenance parallel to `names`; absent tail entries are source-declared. JVM
    /// lowering uses this semantic fact when choosing `MethodParameters` access flags.
    compiler_generated: Vec<bool>,
}

impl FnParamInfo {
    pub fn names(names: Vec<String>) -> Self {
        Self {
            names,
            defaults: None,
            stub_only: false,
            compiler_generated: Vec::new(),
        }
    }

    pub fn defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        Self {
            names,
            defaults: Some(defaults),
            stub_only: false,
            compiler_generated: Vec::new(),
        }
    }

    /// [`Self::defaults`] with the stub-only marker set — see [`Self::stub_only`].
    pub fn stub_only_defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        Self {
            names,
            defaults: Some(defaults),
            stub_only: true,
            compiler_generated: Vec::new(),
        }
    }

    pub(crate) fn prepend_compiler_generated(&mut self, name: String) {
        self.compiler_generated.resize(self.names.len(), false);
        self.names.insert(0, name);
        self.compiler_generated.insert(0, true);
    }

    pub(crate) fn mark_compiler_generated(&mut self, parameter: usize) {
        assert!(
            parameter < self.names.len(),
            "parameter provenance needs a name"
        );
        self.compiler_generated.resize(self.names.len(), false);
        self.compiler_generated[parameter] = true;
    }

    pub(crate) fn is_compiler_generated(&self, parameter: usize) -> bool {
        self.compiler_generated
            .get(parameter)
            .copied()
            .unwrap_or(false)
    }
}
