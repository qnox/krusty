//! Declaration facts a JVM provider derives from a classfile member's own flags and owner, where
//! Kotlin metadata does not state them.

use crate::types::TypeName;

/// Kotlin's JVM delegation filter, applied at the provider boundary: an implemented Java method
/// (no Kotlin `@Metadata`, such as a JDK default method or one admitted onto a mapped builtin) and
/// an implemented builtin mapped to a Java default (`@PlatformDependent`, such as
/// `Map.getOrDefault`) stay inherited instead of being forwarded to a delegate.
pub(crate) fn inherited_by_delegation(
    is_abstract: bool,
    has_kotlin_metadata: bool,
    annotations: &[TypeName],
) -> bool {
    !is_abstract
        && (!has_kotlin_metadata || annotations.contains(&crate::types::wk::platform_dependent()))
}

/// JLS 15.12.3: a method is signature-polymorphic when `MethodHandle` or `VarHandle` declares it
/// `native`, with a single variable-arity `Object[]` parameter. The JDK's own
/// `@PolymorphicSignature` marker is not consulted: a `ct.sym` release view omits it.
pub(super) fn is_signature_polymorphic_method(
    owner: TypeName,
    method: &crate::jvm::classreader::MethodSig,
) -> bool {
    const ACC_NATIVE: u16 = 0x0100;
    (owner.matches("java/lang/invoke/MethodHandle") || owner.matches("java/lang/invoke/VarHandle"))
        && method.access & ACC_NATIVE != 0
        && method.is_vararg()
        && method.descriptor.starts_with("([Ljava/lang/Object;)")
}
