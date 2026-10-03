//! Descriptor-aligned Kotlin callable facts published as one provider-owned record.

use crate::language_version::LanguageVersion;
use crate::libraries::{CallSig, ReturnInfo};
use crate::types::{Ty, TypeName, Visibility};

#[derive(Clone)]
pub struct MetadataCallFacts {
    /// Kotlin/source spelling of the descriptor-aligned declaration. The physical JVM spelling is
    /// the provider query key and must not leak back into source lookup or diagnostics.
    pub source_name: Option<String>,
    pub kept_params: Option<usize>,
    /// Kotlin declaration visibility when metadata owns this callable. `None` means there is no
    /// Kotlin declaration and the provider must use the Java/classfile access flags.
    pub visibility: Option<Visibility>,
    pub call_sig: CallSig,
    pub ret: ReturnInfo,
    /// The full source-declared return type selected from the SAME descriptor-aligned metadata
    /// callable as every other fact in this record. Unlike [`Self::ret`], this retains nested type
    /// arguments, so consumers do not repeat overload alignment merely to recover semantic
    /// classifiers erased by a JVM signature (`MutableList<MutableSet<T>>` → `List<Set<T>>`).
    pub declared_ret: Option<Ty>,
    /// Full Kotlin source parameter types from the same descriptor-aligned metadata declaration.
    /// An extension receiver, when present, is the leading entry. These are resolution facts; the
    /// descriptor-derived JVM parameters remain a separate realization shape in the provider.
    pub declared_params: Option<Vec<Ty>>,
    /// Metadata-primary generic signature of that exact declaration. Keeping it in this aggregate
    /// prevents consumers from aligning the overload a second time (and from accidentally reading a
    /// synthetic `$default` bridge's erased JVM signature instead of its source declaration).
    pub generic_sig: Option<crate::libraries::GenericSig>,
    /// Whether the descriptor-aligned declaration carries Kotlin's `suspend` modifier. Keeping this
    /// beside the other facts selected from the SAME callable prevents a name-wide flag from leaking
    /// across overloads, and lets consumers ignore whether a provider exposes source and JVM names
    /// separately.
    pub suspend: bool,
    /// Kotlin's source-level `inline` modifier from the same selected declaration.
    pub is_inline: bool,
    /// Whether the declaration has at least one reified type parameter.
    pub has_reified_type_params: bool,
    /// Kotlin's source-level `operator` modifier. The JVM descriptor/name cannot encode it.
    pub is_operator: bool,
    /// Kotlin's source-level `infix` modifier. The JVM descriptor/name cannot encode it.
    pub is_infix: bool,
    /// Annotation class identities declared on the descriptor-aligned declaration. Consumers decide
    /// which annotations affect resolution/emission; the classpath layer only records their
    /// qualified identities.
    pub annotations: Vec<TypeName>,
    /// The callable's declared contract, decoded from `@Metadata` (`None` when it has none).
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// Leading context parameters (supplied implicitly by the caller, not positionally).
    pub context_count: usize,
    /// `@Deprecated(level = HIDDEN)` on the selected declaration: binary-compatibility-only,
    /// never an overload-resolution candidate (kotlinc removes it from the candidate set).
    pub deprecated_hidden: bool,
    /// `@SinceKotlin` on the selected declaration. Newer than the compilation API level means the
    /// callable is not a candidate.
    pub since_kotlin: Option<LanguageVersion>,
    /// Per DESCRIPTOR parameter position, the VALUE CLASS `@Metadata` declares there when the JVM
    /// descriptor carries its erased underlying (`timeout: kotlin.time.Duration` ↔ `J`).
    ///
    /// The descriptor is the emit token and stays erased; resolution needs the Kotlin type, or a call
    /// passing a `Duration` is checked against `Long` and no overload is applicable. `None` at a
    /// position whose declared type is not a value class (the overwhelming majority).
    pub value_class_params: Vec<Option<Ty>>,
    /// The VALUE CLASS `@Metadata` declares as the RETURN when the JVM descriptor carries its erased
    /// underlying (`fun make(): K` ↔ `()Ljava/lang/String;`).
    ///
    /// The parameter facet above restores a Kotlin type resolution cannot otherwise see; this one
    /// additionally carries a CODEGEN fact — that the physical result is ALREADY the unboxed carrier.
    /// Without it a call site that knows the Kotlin return is `K` boxes as kotlinc does at a genuine
    /// box boundary and casts a `String` to `K`. `None` when the return is not a value class, or when
    /// a nullable value class stays boxed; a nullable value class erased to a reference carrier is
    /// recorded here as its nullable source type.
    pub value_class_ret: Option<Ty>,
}

impl MetadataCallFacts {
    pub(super) fn fallback(call_sig: CallSig) -> Self {
        MetadataCallFacts {
            source_name: None,
            kept_params: None,
            visibility: None,
            call_sig,
            ret: ReturnInfo::default(),
            declared_ret: None,
            declared_params: None,
            generic_sig: None,
            suspend: false,
            is_inline: false,
            has_reified_type_params: false,
            is_operator: false,
            is_infix: false,
            annotations: Vec::new(),
            contract: None,
            context_count: 0,
            deprecated_hidden: false,
            since_kotlin: None,
            value_class_params: Vec::new(),
            value_class_ret: None,
        }
    }
}
