//! Function-parameter identity, defaults, and compiler-generated provenance.

use super::{ExprId, FunId, IrFile};

/// What one physical common-IR parameter means to the Kotlin declaration.
///
/// This is deliberately independent of every target spelling. In particular, captures, receivers,
/// and unnamed context parameters do not acquire JVM `$...`, `<this>`, or positional names here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrParameterRole {
    Value,
    ContextValue,
    AnonymousContextParameter { ordinal: u32 },
    ContextReceiver { ordinal: u32 },
    ExtensionReceiver,
    CapturedValue { ordinal: u32 },
    CapturedReceiver { ordinal: u32 },
    PropertySetterValue,
    Generated(IrGeneratedParameterRole),
}

/// A compiler-created parameter's semantic job. Targets own any physical spelling and flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrGeneratedParameterRole {
    Positional {
        ordinal: u32,
    },
    Continuation,
    HolderReceiver,
    ValueClassCarrier,
    ValueClassEqualsOperand {
        ordinal: u8,
    },
    AccessorValue {
        ordinal: u32,
    },
    /// A value parameter of the `invoke` a callable-reference class declares for its target.
    ReferenceInvokeValue {
        ordinal: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrParameterProvenance {
    SourceDeclared,
    CompilerGenerated,
}

/// Stable identity of one physical function parameter.
///
/// `source_name` is present only when the source or a generated-declaration producer published a
/// semantic name. Its absence is a fact, not permission for a later phase to invent `pN`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrParameterIdentity {
    pub source_name: Option<String>,
    pub role: IrParameterRole,
    pub provenance: IrParameterProvenance,
}

impl IrParameterIdentity {
    pub fn source(name: impl Into<String>) -> Self {
        Self {
            source_name: Some(name.into()),
            role: IrParameterRole::Value,
            provenance: IrParameterProvenance::SourceDeclared,
        }
    }

    pub fn producer_value(name: impl Into<String>) -> Self {
        Self {
            source_name: Some(name.into()),
            role: IrParameterRole::Value,
            provenance: IrParameterProvenance::CompilerGenerated,
        }
    }

    pub fn context_value(name: impl Into<String>) -> Self {
        Self {
            source_name: Some(name.into()),
            role: IrParameterRole::ContextValue,
            provenance: IrParameterProvenance::SourceDeclared,
        }
    }

    pub fn context_receiver(ordinal: u32) -> Self {
        Self {
            source_name: None,
            role: IrParameterRole::ContextReceiver { ordinal },
            provenance: IrParameterProvenance::SourceDeclared,
        }
    }

    pub fn anonymous_context_parameter(ordinal: u32) -> Self {
        Self {
            source_name: None,
            role: IrParameterRole::AnonymousContextParameter { ordinal },
            provenance: IrParameterProvenance::SourceDeclared,
        }
    }

    pub fn extension_receiver() -> Self {
        Self {
            source_name: None,
            role: IrParameterRole::ExtensionReceiver,
            provenance: IrParameterProvenance::SourceDeclared,
        }
    }

    pub fn property_setter_value() -> Self {
        Self {
            source_name: None,
            role: IrParameterRole::PropertySetterValue,
            provenance: IrParameterProvenance::CompilerGenerated,
        }
    }

    pub fn captured_value(source_name: Option<String>, ordinal: u32) -> Self {
        Self {
            source_name,
            role: IrParameterRole::CapturedValue { ordinal },
            provenance: IrParameterProvenance::CompilerGenerated,
        }
    }

    pub fn captured_receiver(ordinal: u32) -> Self {
        Self {
            source_name: None,
            role: IrParameterRole::CapturedReceiver { ordinal },
            provenance: IrParameterProvenance::CompilerGenerated,
        }
    }

    pub fn generated(role: IrGeneratedParameterRole, source_name: Option<String>) -> Self {
        Self {
            source_name,
            role: IrParameterRole::Generated(role),
            provenance: IrParameterProvenance::CompilerGenerated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrParameterCheck {
    NonNull,
}

#[derive(Clone, Default, Debug)]
pub struct FnParamInfo {
    pub identities: Vec<IrParameterIdentity>,
    pub defaults: Option<Vec<Option<ExprId>>>,
    /// The registered `defaults` serve only the `$default` stub. A call site must not reuse them to
    /// fill an omitted argument. This is set for extensions whose defaults are not all constant.
    pub stub_only: bool,
}

impl FnParamInfo {
    pub fn source_names(names: Vec<String>) -> Self {
        Self {
            identities: names.into_iter().map(IrParameterIdentity::source).collect(),
            defaults: None,
            stub_only: false,
        }
    }

    pub fn identities(identities: Vec<IrParameterIdentity>) -> Self {
        Self {
            identities,
            defaults: None,
            stub_only: false,
        }
    }

    pub fn source_defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        let mut info = Self::source_names(names);
        info.defaults = Some(defaults);
        info
    }

    /// [`Self::defaults`] with the stub-only marker set — see [`Self::stub_only`].
    pub fn source_stub_only_defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        let mut info = Self::source_defaults(names, defaults);
        info.stub_only = true;
        info
    }

    pub(crate) fn prepend_generated(&mut self, identity: IrParameterIdentity) {
        assert_eq!(
            identity.provenance,
            IrParameterProvenance::CompilerGenerated,
            "a prepended generated parameter carries generated provenance"
        );
        self.identities.insert(0, identity);
    }
}

impl IrFile {
    /// The single common-IR contract for a function's complete source parameter identities.
    ///
    /// Source lowering and a generated-member producer publish through different owning records,
    /// but consumers must not branch on that origin. Presence is optional for functions that have
    /// no source/debug parameter surface; when present, the identities are complete and exactly
    /// parallel to the current semantic parameter list.
    pub(crate) fn function_parameter_identities(
        &self,
        function: FunId,
    ) -> Option<&[IrParameterIdentity]> {
        let source = self
            .fn_params
            .get(&function)
            .map(|parameters| parameters.identities.as_slice());
        let generated = self
            .generated_function_publication(function)
            .map(|publication| publication.parameter_identities.as_slice());
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
            identities
                .iter()
                .all(|identity| identity.source_name.as_deref() != Some("")),
            "published source parameter identities are never empty"
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
            FnParamInfo::source_names(["left", "right"].map(String::from).to_vec()),
        );
        let generated = add_function(&mut file, "generated", 1);
        file.publish_generated_members(
            type_name("sample/Generated"),
            IrGeneratedMemberPublication {
                metadata_scope: IrGeneratedFunctionMetadataScope::Exclusive,
                functions: vec![IrGeneratedFunctionPublication {
                    function: generated,
                    parameter_identities: vec![IrParameterIdentity::producer_value("value")],
                    metadata: None,
                    debug: IrGeneratedDeclarationDebug::LocalsOnly,
                }],
            },
        );

        assert_eq!(
            file.function_parameter_identities(source),
            Some(
                &[
                    IrParameterIdentity::source("left"),
                    IrParameterIdentity::source("right"),
                ][..]
            )
        );
        assert_eq!(
            file.function_parameter_identities(generated),
            Some(&[IrParameterIdentity::producer_value("value")][..])
        );
    }

    #[test]
    #[should_panic(expected = "parameter identities exactly match semantic function arity")]
    fn parameter_identity_view_rejects_partial_source_lists() {
        let mut file = IrFile::default();
        let function = add_function(&mut file, "partial", 2);
        file.fn_params.insert(
            function,
            FnParamInfo::source_names(vec!["onlyOne".to_string()]),
        );
        let _ = file.function_parameter_identities(function);
    }

    #[test]
    fn parameter_roles_carry_source_identity_without_target_spelling() {
        let identities = [
            IrParameterIdentity::captured_value(Some("ledger".to_string()), 0),
            IrParameterIdentity::captured_value(None, 1),
            IrParameterIdentity::captured_receiver(0),
            IrParameterIdentity::context_value("audit"),
            IrParameterIdentity::context_receiver(1),
            IrParameterIdentity::extension_receiver(),
            IrParameterIdentity::source("entry"),
            IrParameterIdentity::source("limit"),
        ];
        assert_eq!(
            identities
                .iter()
                .map(|identity| identity.source_name.as_deref())
                .collect::<Vec<_>>(),
            [
                Some("ledger"),
                None,
                None,
                Some("audit"),
                None,
                None,
                Some("entry"),
                Some("limit"),
            ]
        );
        assert_eq!(
            identities
                .iter()
                .map(|identity| identity.role)
                .collect::<Vec<_>>(),
            [
                IrParameterRole::CapturedValue { ordinal: 0 },
                IrParameterRole::CapturedValue { ordinal: 1 },
                IrParameterRole::CapturedReceiver { ordinal: 0 },
                IrParameterRole::ContextValue,
                IrParameterRole::ContextReceiver { ordinal: 1 },
                IrParameterRole::ExtensionReceiver,
                IrParameterRole::Value,
                IrParameterRole::Value,
            ]
        );
        assert!(identities.iter().all(|identity| {
            identity.source_name.as_deref().is_none_or(|name| {
                !name.starts_with('$') && name != "<this>" && !name.starts_with('p')
            })
        }));
    }
}

/// The `vararg` parameter of a declared function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrVarargParameter {
    /// SOURCE index among the value parameters (receiver excluded).
    pub index: usize,
    /// No value parameter follows it.
    pub is_last: bool,
}
