//! A metadata function. Context parameters, `@OnlyInputTypes` formals, and a strict-equality
//! bound are absent on almost every declaration, so the common record keeps one pointer and
//! allocates that side record only when one of those facts is present.

use std::sync::Arc;

use crate::fir::ResolvedParameterIdentity;
use crate::language_version::LanguageVersion;
use crate::libraries::{CallSig, GenericSig};
use crate::types::{ContextParameterKind, ReturnValueStatus, Ty, TypeName, Visibility};

use super::{MetaValueParam, MfnFlags};

/// A function decoded from a `Class`/`Package` `@Metadata` message — the *metadata-truth* signature
/// kotlinc resolves against (`JvmProtoBufUtil.getJvmMethodSignature`): the Kotlin name, the JVM method
/// name + descriptor (from the `method_signature` extension when present), Kotlin visibility/`inline`/
/// `suspend`/`operator`, and the extension-receiver class. For an `inline` function the bytecode is
/// `private`/synthetic, so these flags differ from the access flags — metadata is primary, bytecode is
/// fallback.
#[derive(Clone, Debug)]
pub struct MetaFn {
    pub kotlin_name: String,
    pub jvm_name: String,
    /// The JVM descriptor from the `method_signature` extension; `None` when metadata omits it (the
    /// caller may then fall back to a bytecode method of the same name, or compute it from proto types).
    pub jvm_desc: Option<&'static str>,
    pub visibility: Visibility,
    /// Bit-packed `is_inline`/`is_suspend`/`is_extension`/`is_operator`/`ret_nullable` (read via the
    /// accessors below).
    /// `is_extension` — whether this is an EXTENSION (a receiver of any kind, class or
    /// type parameter) vs a true top-level function; lets the classpath ext index avoid mis-indexing a
    /// top-level generic as an extension on its first parameter's type. `ret_nullable` — whether the
    /// Kotlin return type is nullable (`T?`, `Type.nullable`); the JVM descriptor/`Signature` erase this,
    /// only `@Metadata` carries it, and it drives the elvis null-check for a nullable-returning scope fn.
    pub flags: MfnFlags,
    /// Extension-receiver Kotlin class name (`kotlin/Result` for `Result.getOrThrow`), if any. `None` for a
    /// top-level fn AND for an extension on a type PARAMETER — use [`MetaFn::is_extension`] to disambiguate.
    pub receiver_class: Option<TypeName>,
    /// The Kotlin return-type class name (`kotlin/UInt` for `UInt.coerceAtMost`), if it is a class type.
    pub ret_class: Option<TypeName>,
    /// SOURCE value parameters in declaration order. The LENGTH is the source arity: it excludes
    /// synthetic JVM descriptor params such as suspend `Continuation` or Compose `Composer`/masks.
    pub value_params: Vec<MetaValueParam>,
    /// The metadata-primary generic signature (type parameters + parameter/return gsig nodes), decoded
    /// straight from `@Metadata` rather than the JVM `Signature` attribute — a JVM-agnostic, Kotlin-faithful
    /// source (nullability, variance, Kotlin type identities). `None` when the return type won't decode.
    pub generic_sig: Option<GenericSig>,
    /// The function's declared contract (`Function.contract`, field 32), decoded into the shared
    /// contract IR — the effects the checker applies at call sites (`returns(…) implies …`,
    /// `callsInPlace`). `None` when the function declares no contract.
    pub contract: Option<Arc<crate::contracts::Contract>>,
    pub return_value_status: ReturnValueStatus,
    /// Annotation class identities declared on this function (from `Function.annotation`).
    /// The decoder keeps these as interned `TypeName`s so consumers — overload resolution,
    /// deprecation handling, etc. — can check for the annotations they care about without the
    /// metadata layer hard-coding any one annotation's semantics.
    pub annotations: Vec<TypeName>,
    extras: Option<Box<FunctionExtras>>,
}

/// Facts present on a small minority of metadata functions.
#[derive(Clone, Debug)]
struct FunctionExtras {
    /// Leading context parameters. Named context parameters retain the same metadata shape as ordinary
    /// value parameters; legacy unnamed context receivers have an empty name.
    context_params: Vec<MetaValueParam>,
    /// Typed context roles decoded at the metadata boundary. Consumers never inspect the encoded
    /// empty/`<unused var>` spellings to distinguish legacy, anonymous, and named contexts.
    context_parameter_kinds: Vec<ContextParameterKind>,
    /// Function formals carrying Kotlin's internal `@OnlyInputTypes` inference policy.
    only_input_type_formals: Vec<String>,
    /// Declaration type parameters carrying Kotlin's `reified` modifier, in generic-signature
    /// order. Keeping the complete bitmap preserves mixed ordinary/reified declarations.
    reified_type_parameter_ordinals: Vec<u32>,
    /// Compiler-known strict-equality parameter refinement decoded from fields 9/10.
    equality_bound: Option<Ty>,
    /// `@SinceKotlin` version. A callable newer than the compilation's API level is not a candidate.
    since_kotlin: Option<LanguageVersion>,
}

/// The decoded function before its rare facts are packed into [`FunctionExtras`].
pub(super) struct DecodedFunction {
    pub kotlin_name: String,
    pub jvm_name: String,
    pub jvm_desc: Option<&'static str>,
    pub visibility: Visibility,
    pub flags: MfnFlags,
    pub receiver_class: Option<TypeName>,
    pub ret_class: Option<TypeName>,
    pub value_params: Vec<MetaValueParam>,
    pub generic_sig: Option<GenericSig>,
    pub contract: Option<Arc<crate::contracts::Contract>>,
    pub equality_bound: Option<Ty>,
    pub return_value_status: ReturnValueStatus,
    pub only_input_type_formals: Vec<String>,
    pub reified_type_parameter_ordinals: Vec<u32>,
    pub context_params: Vec<MetaValueParam>,
    pub context_parameter_kinds: Vec<ContextParameterKind>,
    pub annotations: Vec<TypeName>,
    pub since_kotlin: Option<LanguageVersion>,
}

fn pack_extras(
    context_params: Vec<MetaValueParam>,
    context_parameter_kinds: Vec<ContextParameterKind>,
    only_input_type_formals: Vec<String>,
    reified_type_parameter_ordinals: Vec<u32>,
    equality_bound: Option<Ty>,
    since_kotlin: Option<LanguageVersion>,
) -> Option<Box<FunctionExtras>> {
    if context_params.is_empty()
        && context_parameter_kinds.is_empty()
        && only_input_type_formals.is_empty()
        && reified_type_parameter_ordinals.is_empty()
        && equality_bound.is_none()
        && since_kotlin.is_none()
    {
        None
    } else {
        Some(Box::new(FunctionExtras {
            context_params,
            context_parameter_kinds,
            only_input_type_formals,
            reified_type_parameter_ordinals,
            equality_bound,
            since_kotlin,
        }))
    }
}

impl MetaFn {
    pub(super) fn from_decoded(decoded: DecodedFunction) -> Self {
        Self {
            kotlin_name: decoded.kotlin_name,
            jvm_name: decoded.jvm_name,
            jvm_desc: decoded.jvm_desc,
            visibility: decoded.visibility,
            flags: decoded.flags,
            receiver_class: decoded.receiver_class,
            ret_class: decoded.ret_class,
            value_params: decoded.value_params,
            generic_sig: decoded.generic_sig,
            contract: decoded.contract,
            return_value_status: decoded.return_value_status,
            annotations: decoded.annotations,
            extras: pack_extras(
                decoded.context_params,
                decoded.context_parameter_kinds,
                decoded.only_input_type_formals,
                decoded.reified_type_parameter_ordinals,
                decoded.equality_bound,
                decoded.since_kotlin,
            ),
        }
    }

    #[inline]
    pub fn is_public(&self) -> bool {
        self.visibility == Visibility::Public
    }
    #[inline]
    pub fn is_inline(&self) -> bool {
        self.flags.has(MfnFlags::IS_INLINE)
    }
    #[inline]
    pub fn is_suspend(&self) -> bool {
        self.flags.has(MfnFlags::IS_SUSPEND)
    }
    #[inline]
    pub fn is_abstract(&self) -> bool {
        self.flags.has(MfnFlags::IS_ABSTRACT)
    }
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(MfnFlags::IS_FINAL)
    }
    #[inline]
    pub fn is_extension(&self) -> bool {
        self.flags.has(MfnFlags::IS_EXTENSION)
    }
    #[inline]
    pub fn is_operator(&self) -> bool {
        self.flags.has(MfnFlags::IS_OPERATOR)
    }
    #[inline]
    pub fn is_infix(&self) -> bool {
        self.flags.has(MfnFlags::IS_INFIX)
    }
    #[inline]
    pub fn has_reified_type_params(&self) -> bool {
        self.flags.has(MfnFlags::HAS_REIFIED_TYPE_PARAMS)
    }
    /// A `companion { … }` block member: a static member of the class that declares the block, called
    /// through the classifier with no receiver.
    #[inline]
    pub fn is_companion_block_member(&self) -> bool {
        self.flags.has(MfnFlags::IS_COMPANION_BLOCK_MEMBER)
    }
    #[inline]
    pub fn ret_nullable(&self) -> bool {
        self.flags.has(MfnFlags::RET_NULLABLE)
    }
    /// `@Deprecated(level = HIDDEN)`: the declaration exists for binary compatibility only and
    /// kotlinc removes it from overload resolution entirely. Stamped from the realization
    /// method's `kotlin.Deprecated` annotation after metadata decode.
    #[inline]
    pub fn deprecated_hidden(&self) -> bool {
        self.flags.has(MfnFlags::DEPRECATED_HIDDEN)
    }

    /// `@SinceKotlin` on this declaration. Absent means the callable is available at every API level.
    pub fn since_kotlin(&self) -> Option<LanguageVersion> {
        self.extras.as_ref().and_then(|extras| extras.since_kotlin)
    }

    pub fn context_params(&self) -> &[MetaValueParam] {
        match &self.extras {
            Some(extras) => extras.context_params.as_slice(),
            None => &[],
        }
    }

    pub fn context_parameter_kinds(&self) -> &[ContextParameterKind] {
        match &self.extras {
            Some(extras) => extras.context_parameter_kinds.as_slice(),
            None => &[],
        }
    }

    pub fn only_input_type_formals(&self) -> &[String] {
        match &self.extras {
            Some(extras) => extras.only_input_type_formals.as_slice(),
            None => &[],
        }
    }

    pub fn reified_type_parameter_ordinals(&self) -> &[u32] {
        match &self.extras {
            Some(extras) => extras.reified_type_parameter_ordinals.as_slice(),
            None => &[],
        }
    }

    pub fn equality_bound(&self) -> Option<Ty> {
        self.extras
            .as_ref()
            .and_then(|extras| extras.equality_bound)
    }

    pub fn context_count(&self) -> usize {
        self.context_params().len()
    }

    pub fn parameters(&self) -> impl Iterator<Item = &MetaValueParam> {
        self.context_params().iter().chain(&self.value_params)
    }

    pub fn member_call_sig(&self) -> CallSig {
        assert_eq!(
            self.context_params().len(),
            self.context_parameter_kinds().len(),
            "metadata functions must publish one typed role per context parameter"
        );
        let parameters: Vec<_> = self.parameters().collect();
        let (lambda_receivers, lambda_receiver_params) = self.lambda_receiver_shape();
        let mut sig = CallSig::metadata_function(
            parameters.len(),
            parameters.iter().map(|p| p.name.clone()).collect(),
            parameters.iter().map(|p| p.has_default()).collect(),
            lambda_receivers,
            lambda_receiver_params,
            parameters.iter().map(|p| p.inline_modifier()).collect(),
            self.vararg_index()
                .map(|index| index + self.context_count()),
        );
        for (ordinal, (parameter, kind)) in self
            .context_params()
            .iter()
            .zip(self.context_parameter_kinds())
            .enumerate()
        {
            sig.parameter_identities[ordinal] = match kind {
                ContextParameterKind::Named => ResolvedParameterIdentity::ContextValue {
                    ordinal: ordinal as u32,
                    source_name: parameter.name.as_str().into(),
                },
                ContextParameterKind::Anonymous => {
                    ResolvedParameterIdentity::AnonymousContextParameter {
                        ordinal: ordinal as u32,
                    }
                }
                ContextParameterKind::LegacyReceiver => {
                    ResolvedParameterIdentity::LegacyContextReceiver {
                        ordinal: ordinal as u32,
                    }
                }
                ContextParameterKind::None => {
                    panic!("a metadata context prefix must carry a context role")
                }
            };
        }
        sig.platform_nullable_params = parameters.iter().map(|p| p.nullable()).collect();
        sig.only_input_type_formals = self.only_input_type_formals().to_vec();
        sig.reified_type_parameter_ordinals = self.reified_type_parameter_ordinals().to_vec();
        sig.no_infer_params = parameters
            .iter()
            .map(|parameter| parameter.no_infer())
            .collect();
        sig
    }

    /// Decode the semantic receiver-function shape once for every metadata function consumer.
    /// A concrete `Recv.() -> R` carries both the receiver type and the mark; a generic
    /// `T.() -> R` carries only the mark and recovers `T` after call-site substitution.
    pub(super) fn lambda_receiver_shape(&self) -> (Vec<Option<Ty>>, Vec<bool>) {
        (
            self.parameters()
                .map(|p| p.recv_fun_receiver.map(Ty::obj_name))
                .collect(),
            self.parameters().map(|p| p.recv_fun()).collect(),
        )
    }

    pub fn vararg_index(&self) -> Option<usize> {
        self.value_params
            .iter()
            .position(|parameter| parameter.vararg())
    }

    pub fn extension_call_sig(&self) -> CallSig {
        self.member_call_sig()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> DecodedFunction {
        DecodedFunction {
            kotlin_name: String::new(),
            jvm_name: String::new(),
            jvm_desc: None,
            visibility: Visibility::Public,
            flags: MfnFlags::default(),
            receiver_class: None,
            ret_class: None,
            value_params: Vec::new(),
            generic_sig: None,
            contract: None,
            equality_bound: None,
            return_value_status: ReturnValueStatus::Unspecified,
            only_input_type_formals: Vec::new(),
            reified_type_parameter_ordinals: Vec::new(),
            context_params: Vec::new(),
            context_parameter_kinds: Vec::new(),
            annotations: Vec::new(),
            since_kotlin: None,
        }
    }

    #[test]
    fn common_function_omits_the_side_record() {
        let function = MetaFn::from_decoded(blank());
        assert!(function.context_params().is_empty());
        assert!(function.context_parameter_kinds().is_empty());
        assert!(function.only_input_type_formals().is_empty());
        assert_eq!(function.equality_bound(), None);
        // Four empty vectors and an `Option<Ty>` used to sit on every function. The side
        // record is one pointer. Pin the size so those fields cannot move back onto `MetaFn`.
        assert_eq!(size_of::<MetaFn>(), 296);
    }

    #[test]
    fn rare_function_facts_share_one_side_record() {
        let context_type = Ty::obj("sample/metadata6044/Context");
        let equality_bound = Ty::obj("sample/metadata6044/Bound");
        let mut decoded = blank();
        decoded.context_params = vec![MetaValueParam {
            ty: context_type.obj_internal(),
            name: "ctx".to_string(),
            flags: super::super::MvpFlags::default(),
            recv_fun_receiver: None,
        }];
        decoded.context_parameter_kinds = vec![ContextParameterKind::Named];
        decoded.only_input_type_formals = vec!["T".to_string()];
        decoded.reified_type_parameter_ordinals = vec![1];
        decoded.equality_bound = Some(equality_bound);
        let function = MetaFn::from_decoded(decoded);
        assert_eq!(function.context_params().len(), 1);
        assert_eq!(function.context_params()[0].name, "ctx");
        assert_eq!(
            function.context_parameter_kinds(),
            &[ContextParameterKind::Named]
        );
        assert_eq!(function.only_input_type_formals(), &["T".to_string()]);
        assert_eq!(function.reified_type_parameter_ordinals(), &[1]);
        assert_eq!(function.equality_bound(), Some(equality_bound));
        assert_eq!(function.context_count(), 1);
    }
}
