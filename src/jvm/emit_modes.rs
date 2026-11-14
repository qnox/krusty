//! The JVM emission strategies kotlinc's command line selects: `-Xlambdas`, `-Xsam-conversions`
//! and `-jvm-default`. They are options the driver and the conformance harness parse, so they live
//! beside the class-file model rather than inside the emitter.

/// `-Xlambdas` / `-Xsam-conversions`: how a lambda and a SAM conversion are realized on the JVM.
///
/// The two strategies produce different CLASS SETS, so this is an emitter selection rather than an
/// optimization hint: under `Class` every lambda contributes its own `.class`, and a build that asked
/// for it and got `Indy` would ship a different artifact list than it declared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LambdaMode {
    /// kotlinc's default since 2.0: an `invokedynamic` call site bootstrapped through
    /// `LambdaMetafactory`, with the body in a private static `$lambda$N` on the enclosing class.
    #[default]
    Indy,
    /// The pre-2.0 strategy, still selected explicitly by 40 of intellij-community's `BUILD.bazel`
    /// files: one synthetic class per lambda, extending `kotlin.jvm.internal.Lambda` and
    /// implementing the target interface. A lambda that captures nothing is a singleton held in a
    /// static `INSTANCE` field; a capturing one is constructed per evaluation with its captures as
    /// constructor arguments.
    Class,
}

/// JVM realization strategies for the two distinct Kotlin compiler options. Keeping the pair as one
/// value lets every CLI/backend/emitter boundary forward the same normalized configuration without
/// parallel booleans or last-option-wins behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LambdaModes {
    pub lambdas: LambdaMode,
    pub sam_conversions: LambdaMode,
}

/// kotlinc's `-jvm-default` strategy for interface members with bodies — which of the three JVM
/// shapes an interface is compiled into.
///
/// Measured against kotlinc 2.4.10 on an interface with a default method, a default property getter,
/// and a method with a default parameter value:
///
/// | | interface members | `$DefaultImpls` | implementing class | `jvmClassFlags` |
/// |---|---|---|---|---|
/// | `Enable` | default methods + `access$…$jd` bridges | forwarders to those bridges | forwarder overrides (`invokespecial`) | 3 |
/// | `NoCompatibility` | default methods | absent | nothing | 1 |
/// | `Disable` | all abstract | the real bodies, receiver as parameter 0 | forwarders (`invokestatic`) | absent |
///
/// krusty emits that full table:
///   * `NoCompatibility` — default methods on the interface, no holder, no class forwarders.
///   * `Enable` — default methods plus the compatibility surface: the `access$…$jd` bridges, holder
///     statics forwarding to them (`@Deprecated`, holder bytes differential-tested), a thin
///     `$default` holder forward, `invokespecial` forwarder overrides on implementing classes, and
///     the republished surface on sub-interfaces for inherited defaults.
///   * `Disable` emits holder bodies and implementing-class forwarders. A dependency compiled in
///     that mode publishes those holder methods as its declarations' exact physical realizations,
///     so a consumer does not reinterpret the dependency from its own mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JvmDefaultMode {
    /// kotlinc's own default since 2.2 (legacy spelling `-Xjvm-default=all-compatibility`): default
    /// methods on the interface AND a `$DefaultImpls` compatibility copy.
    #[default]
    Enable,
    /// Legacy spelling `-Xjvm-default=all`. Default methods only — no `$DefaultImpls` anywhere, and
    /// no forwarders on implementing classes. What intellij-community builds with.
    NoCompatibility,
    /// Legacy spelling `-Xjvm-default=disable`. No default methods at all: the interface is fully
    /// abstract and every body lives on `$DefaultImpls` as a static taking the receiver.
    Disable,
}

impl JvmDefaultMode {
    /// Parse a `-jvm-default` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enable" => Some(Self::Enable),
            "no-compatibility" => Some(Self::NoCompatibility),
            "disable" => Some(Self::Disable),
            _ => None,
        }
    }

    /// Parse a legacy `-Xjvm-default` value, whose names denote the SAME three shapes under
    /// different spellings. Getting this mapping backwards is silent: `all` reads as a plausible
    /// "enable" and then emits a `$DefaultImpls` class the build deliberately does not have.
    pub fn parse_legacy(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::NoCompatibility),
            "all-compatibility" => Some(Self::Enable),
            "disable" => Some(Self::Disable),
            _ => None,
        }
    }

    /// The mode one class is compiled under: `@JvmDefaultWithCompatibility` and
    /// `@JvmDefaultWithoutCompatibility` on a class or interface override the module's mode for
    /// that declaration (`Enable` and `NoCompatibility`, respectively); `Disable` modules reject
    /// both annotations, so they never reach here.
    pub fn declared(self, with_compatibility: bool, without_compatibility: bool) -> Self {
        if with_compatibility {
            Self::Enable
        } else if without_compatibility {
            Self::NoCompatibility
        } else {
            self
        }
    }

    /// The `jvmClassFlags` (`Class` JvmProtoBuf extension field 104) an interface carries under this
    /// mode: bit 0 `hasMethodBodiesInInterface`, bit 1 `isCompiledInCompatibilityMode`. `Disable`
    /// sets neither, and kotlinc then omits the field entirely.
    pub fn interface_jvm_class_flags(self) -> Option<u64> {
        match self {
            Self::Enable => Some(3),
            Self::NoCompatibility => Some(1),
            Self::Disable => None,
        }
    }
}
