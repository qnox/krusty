//! Function-parameter names, defaults, and compiler-generated provenance.

use super::{ExprId, FunId, IrFile};

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

impl IrFile {
    /// The single common-IR contract for a function's complete source parameter identities.
    ///
    /// Source lowering and a generated-member producer publish through different owning records,
    /// but consumers must not branch on that origin. Presence is optional for functions that have
    /// no source/debug parameter surface; when present, the identities are complete and exactly
    /// parallel to the current semantic parameter list.
    pub(crate) fn function_parameter_identities(&self, function: FunId) -> Option<&[String]> {
        let source = self
            .fn_params
            .get(&function)
            .map(|parameters| parameters.names.as_slice());
        let generated = self
            .generated_function_publication(function)
            .map(|publication| publication.parameter_names.as_slice());
        assert!(
            source.is_none() || generated.is_none(),
            "one function has one owning parameter-identity contract"
        );
        let identities = source.or(generated)?;
        let semantic_arity = self
            .functions
            .get(function as usize)
            .expect("parameter identities name an existing function")
            .params
            .len();
        assert_eq!(
            identities.len(),
            semantic_arity,
            "parameter identities exactly match semantic function arity"
        );
        assert!(
            identities.iter().all(|identity| !identity.is_empty()),
            "parameter identities are never empty"
        );
        Some(identities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        IrFunction, IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadataScope,
        IrGeneratedFunctionPublication, IrGeneratedMemberPublication,
    };
    use crate::types::{type_name, Ty};

    fn add_function(file: &mut IrFile, name: &str, arity: usize) -> FunId {
        file.add_fun(IrFunction {
            name: name.to_string(),
            params: vec![Ty::Int; arity],
            ret: Ty::Unit,
            body: None,
            is_static: false,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    #[test]
    fn source_and_generated_parameters_share_one_exact_identity_view() {
        let mut file = IrFile::default();
        let source = add_function(&mut file, "source", 2);
        file.fn_params.insert(
            source,
            FnParamInfo::names(["left", "right"].map(String::from).to_vec()),
        );
        let generated = add_function(&mut file, "generated", 1);
        file.publish_generated_members(
            type_name("sample/Generated"),
            IrGeneratedMemberPublication {
                metadata_scope: IrGeneratedFunctionMetadataScope::Exclusive,
                functions: vec![IrGeneratedFunctionPublication {
                    function: generated,
                    parameter_names: vec!["value".to_string()],
                    metadata: None,
                    debug: IrGeneratedDeclarationDebug::LocalsOnly,
                }],
            },
        );

        assert_eq!(
            file.function_parameter_identities(source),
            Some(&["left".to_string(), "right".to_string()][..])
        );
        assert_eq!(
            file.function_parameter_identities(generated),
            Some(&["value".to_string()][..])
        );
    }

    #[test]
    #[should_panic(expected = "parameter identities exactly match semantic function arity")]
    fn parameter_identity_view_rejects_partial_source_lists() {
        let mut file = IrFile::default();
        let function = add_function(&mut file, "partial", 2);
        file.fn_params
            .insert(function, FnParamInfo::names(vec!["onlyOne".to_string()]));
        let _ = file.function_parameter_identities(function);
    }
}
