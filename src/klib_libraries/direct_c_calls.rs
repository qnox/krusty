//! Native C entries authenticated at the KLIB provider boundary.
//!
//! A cinterop archive owns both the declaration and its qualified `CCall.Direct` application.
//! The provider joins those facts once and publishes a physical target with the selected callable
//! identity. Neither common lowering nor a backend searches annotations or declaration spellings.

use crate::libraries::{CFunctionAbi, CallablePlacement, DirectCFunction};
use crate::metadata::semantic::{AnnotationApplication, AnnotationArgument, KotlinFunction};
use crate::types::type_name;

use super::declaration_signatures::{unsignable, KlibLibraryError};
use super::target_intrinsics::TargetIntrinsicPlatform;

const CCALL_DIRECT: &str = "kotlinx/cinterop/internal/CCall$Direct";
const GC_UNSAFE_CALL: &str = "kotlin/native/internal/GCUnsafeCall";

pub(super) fn authenticate(
    package: &[String],
    native_interop_library: bool,
    placement: CallablePlacement,
    declaration: &KotlinFunction,
) -> Result<Option<DirectCFunction>, KlibLibraryError> {
    let identity = type_name(CCALL_DIRECT);
    let mut matches = declaration
        .annotation_applications
        .iter()
        .filter(|annotation| annotation.identity == identity);
    let Some(annotation) = matches.next() else {
        return Ok(None);
    };
    let fail = |detail: &str| unsignable(package, &declaration.name, detail);
    if matches.next().is_some() {
        return Err(fail("duplicate `@CCall.Direct` applications"));
    }
    if !native_interop_library {
        return Err(fail(
            "`@CCall.Direct` is present outside a Native interop KLIB",
        ));
    }
    if !matches!(placement, CallablePlacement::Package(_))
        || !declaration.is_external
        || declaration.receiver.is_some()
        || declaration.context_count != 0
        || !declaration.context_kinds.is_empty()
        || declaration.is_suspend
        || declaration.is_inline
        || declaration.is_expect
        || declaration.is_static
        || declaration.vararg.is_some()
        || declaration
            .param_defaults
            .iter()
            .any(|defaulted| *defaulted)
    {
        return Err(fail(
            "`@CCall.Direct` does not annotate a receiver-less external C function",
        ));
    }
    if !declaration.formals.is_empty() {
        // One C symbol has one ABI; a type parameter would let each call site substitute another.
        return Err(fail("`@CCall.Direct` annotates a generic function"));
    }
    let symbol = exact_name(annotation)
        .ok_or_else(|| fail("`@CCall.Direct` has no single string `name` argument"))?;
    Ok(Some(DirectCFunction::new(symbol)))
}

/// The declaration facts that decide whether a Kotlin/Native standard library function is bound
/// to a runtime entry by `@GCUnsafeCall("callee")`. Metadata and serialized IR describe the same
/// declaration in different shapes; each fills this one record, and [`Self::callee`] is the single
/// definition of the binding.
pub(crate) struct RuntimeEntryShape<'a> {
    pub external: bool,
    pub suspend: bool,
    pub inline: bool,
    pub expect: bool,
    pub vararg: bool,
    pub context_parameters: bool,
    pub type_parameters: bool,
    pub defaulted_parameters: bool,
    /// The `callee` string of the declaration's one `@GCUnsafeCall`; `None` when there is not
    /// exactly one application or it does not pass exactly that string.
    pub callee: Option<&'a str>,
}

impl<'a> RuntimeEntryShape<'a> {
    fn of_metadata(declaration: &'a KotlinFunction) -> Self {
        let identity = type_name(GC_UNSAFE_CALL);
        let mut matches = declaration
            .annotation_applications
            .iter()
            .filter(|annotation| annotation.identity == identity);
        let callee = matches
            .next()
            .filter(|_| matches.next().is_none())
            .and_then(|annotation| match annotation.arguments.as_slice() {
                [(parameter, AnnotationArgument::String(symbol))] if parameter == "callee" => {
                    Some(symbol.as_str())
                }
                _ => None,
            });
        Self {
            external: declaration.is_external,
            suspend: declaration.is_suspend,
            inline: declaration.is_inline,
            expect: declaration.is_expect,
            vararg: declaration.vararg.is_some(),
            context_parameters: declaration.context_count != 0
                || !declaration.context_kinds.is_empty(),
            type_parameters: !declaration.formals.is_empty(),
            defaulted_parameters: declaration
                .param_defaults
                .iter()
                .any(|defaulted| *defaulted),
            callee,
        }
    }

    /// The runtime symbol the declaration is bound to.
    pub(crate) fn callee(&self) -> Option<&'a str> {
        let plain = self.external
            && !self.suspend
            && !self.inline
            && !self.expect
            && !self.vararg
            && !self.context_parameters
            && !self.type_parameters
            && !self.defaulted_parameters;
        self.callee.filter(|_| plain)
    }
}

/// The Kotlin/Native runtime entry a Native standard library declaration is bound to by its
/// `@GCUnsafeCall("callee")`. The caller offers only a declaration no compiler intrinsic realizes,
/// so what remains is a call of the runtime's own C function; whether the runtime defines it is the
/// backend's check, against the runtime it links.
pub(super) fn runtime_entry(
    platform: Option<TargetIntrinsicPlatform>,
    declaration: &KotlinFunction,
) -> Option<DirectCFunction> {
    if platform != Some(TargetIntrinsicPlatform::Native) {
        return None;
    }
    let symbol = RuntimeEntryShape::of_metadata(declaration).callee()?;
    Some(DirectCFunction::with_abi(symbol, CFunctionAbi::Runtime))
}

/// The runtime entry a candidate no compiler intrinsic and no cinterop `CCall.Direct` realizes
/// falls to. Every inventory site that publishes a callable shares this order.
pub(super) fn runtime_entry_unless_realized(
    platform: Option<TargetIntrinsicPlatform>,
    declaration: &KotlinFunction,
    direct_c_function: Option<DirectCFunction>,
    target_intrinsic_realized: bool,
) -> Option<DirectCFunction> {
    if direct_c_function.is_some() || target_intrinsic_realized {
        return direct_c_function;
    }
    runtime_entry(platform, declaration)
}

fn exact_name(annotation: &AnnotationApplication) -> Option<&str> {
    let [(parameter, AnnotationArgument::String(symbol))] = annotation.arguments.as_slice() else {
        return None;
    };
    (parameter == "name").then_some(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::semantic::{KotlinFunctionTypeShape, KotlinModality, KotlinType};
    use crate::types::{TypeName, Visibility};

    fn declaration(
        identity: TypeName,
        arguments: Vec<(String, AnnotationArgument)>,
    ) -> KotlinFunction {
        KotlinFunction {
            name: "sum".to_owned(),
            receiver: None,
            params: vec![KotlinType::Class {
                internal: "kotlin/Int".to_owned(),
                args: Vec::new(),
                nullable: false,
                shape: KotlinFunctionTypeShape::default(),
            }],
            ret: KotlinType::Class {
                internal: "kotlin/Int".to_owned(),
                args: Vec::new(),
                nullable: false,
                shape: KotlinFunctionTypeShape::default(),
            },
            formals: Vec::new(),
            param_names: vec!["value".to_owned()],
            param_defaults: vec![false],
            inline_modifiers: vec![crate::types::InlineParameterModifier::None],
            vararg: None,
            visibility: Visibility::Public,
            modality: KotlinModality::Final,
            is_inline: false,
            has_reified_type_params: false,
            is_suspend: false,
            is_operator: false,
            is_infix: false,
            is_synthesized: false,
            is_expect: false,
            is_external: true,
            is_static: false,
            context_count: 0,
            context_kinds: Vec::new(),
            annotations: vec![identity],
            annotation_applications: vec![AnnotationApplication {
                identity,
                arguments,
            }],
            return_value_status: Default::default(),
            contract: None,
            equality_bound: None,
        }
    }

    fn placement() -> CallablePlacement {
        CallablePlacement::Package(type_name("fixture"))
    }

    #[test]
    fn only_the_exact_qualified_interop_application_publishes_a_c_target() {
        let exact = declaration(
            type_name(CCALL_DIRECT),
            vec![(
                "name".to_owned(),
                AnnotationArgument::String("fixture_sum".to_owned()),
            )],
        );
        assert_eq!(
            authenticate(&["fixture".to_owned()], true, placement(), &exact)
                .expect("the exact application is valid")
                .expect("it publishes a target")
                .symbol(),
            "fixture_sum"
        );

        let unrelated = declaration(
            type_name("other/CCall$Direct"),
            vec![(
                "name".to_owned(),
                AnnotationArgument::String("fixture_sum".to_owned()),
            )],
        );
        assert_eq!(
            authenticate(&["fixture".to_owned()], true, placement(), &unrelated)
                .expect("an unrelated annotation is valid"),
            None
        );
    }

    #[test]
    fn an_authoritative_application_has_one_exact_string_name() {
        let malformed = declaration(
            type_name(CCALL_DIRECT),
            vec![("value".to_owned(), AnnotationArgument::Other)],
        );
        assert_eq!(
            authenticate(&["fixture".to_owned()], true, placement(), &malformed)
                .expect_err("the malformed application rejects the declaration")
                .detail(),
            "`@CCall.Direct` has no single string `name` argument"
        );
        assert_eq!(
            authenticate(&["fixture".to_owned()], false, placement(), &malformed)
                .expect_err("a non-interop archive cannot grant the role")
                .detail(),
            "`@CCall.Direct` is present outside a Native interop KLIB"
        );
    }

    fn runtime_declaration(arguments: Vec<(String, AnnotationArgument)>) -> KotlinFunction {
        declaration(type_name(GC_UNSAFE_CALL), arguments)
    }

    fn callee(symbol: &str) -> Vec<(String, AnnotationArgument)> {
        vec![(
            "callee".to_owned(),
            AnnotationArgument::String(symbol.to_owned()),
        )]
    }

    #[test]
    fn a_native_external_publishes_the_runtime_entry_its_gc_unsafe_call_names() {
        let entry = runtime_entry(
            Some(TargetIntrinsicPlatform::Native),
            &runtime_declaration(callee("Kotlin_native_internal_GC_collect")),
        )
        .expect("the exact application names a runtime entry");
        assert_eq!(entry.symbol(), "Kotlin_native_internal_GC_collect");
        assert_eq!(entry.abi(), CFunctionAbi::Runtime);
    }

    #[test]
    fn a_runtime_entry_needs_the_native_platform_an_external_and_one_exact_callee() {
        let exact = runtime_declaration(callee("Kotlin_native_internal_GC_collect"));
        assert!(runtime_entry(Some(TargetIntrinsicPlatform::Wasm), &exact).is_none());
        assert!(runtime_entry(None, &exact).is_none());
        let mut bodied = runtime_declaration(callee("Kotlin_native_internal_GC_collect"));
        bodied.is_external = false;
        assert!(runtime_entry(Some(TargetIntrinsicPlatform::Native), &bodied).is_none());
        let renamed = runtime_declaration(vec![(
            "name".to_owned(),
            AnnotationArgument::String("Kotlin_native_internal_GC_collect".to_owned()),
        )]);
        assert!(runtime_entry(Some(TargetIntrinsicPlatform::Native), &renamed).is_none());
        let unrelated = declaration(type_name("other/GCUnsafeCall"), callee("Kotlin_x"));
        assert!(runtime_entry(Some(TargetIntrinsicPlatform::Native), &unrelated).is_none());
    }

    #[test]
    fn a_direct_c_application_never_realizes_a_member_or_associated_function() {
        let exact = declaration(
            type_name(CCALL_DIRECT),
            vec![(
                "name".to_owned(),
                AnnotationArgument::String("fixture_sum".to_owned()),
            )],
        );
        let owner = type_name("fixture/Owner");
        assert_eq!(
            authenticate(
                &["fixture".to_owned()],
                true,
                CallablePlacement::Member {
                    owner,
                    owner_is_interface: false,
                },
                &exact,
            )
            .expect_err("an instance member cannot publish a C function")
            .detail(),
            "`@CCall.Direct` does not annotate a receiver-less external C function"
        );
        assert_eq!(
            authenticate(
                &["fixture".to_owned()],
                true,
                CallablePlacement::Associated {
                    classifier: owner,
                    declaration_owner: crate::types::SemanticCallableOwner::Classifier(owner),
                    access_owner: Some(owner),
                },
                &exact,
            )
            .expect_err("an associated function cannot publish a C function")
            .detail(),
            "`@CCall.Direct` does not annotate a receiver-less external C function"
        );
    }

    #[test]
    fn a_direct_c_function_has_one_fixed_signature() {
        let mut generic = declaration(
            type_name(CCALL_DIRECT),
            vec![(
                "name".to_owned(),
                AnnotationArgument::String("fixture_sum".to_owned()),
            )],
        );
        let id = crate::metadata::semantic::KotlinTypeParameterId(0);
        generic.formals = vec![crate::metadata::semantic::KotlinTypeParameter {
            id,
            name: "T".to_owned(),
            bounds: Vec::new(),
            variance: crate::types::TypeVariance::Invariant,
            only_input: false,
            reified: false,
        }];
        generic.params = vec![KotlinType::Param {
            name: "T".to_owned(),
            id,
            nullable: false,
        }];
        assert_eq!(
            authenticate(&["fixture".to_owned()], true, placement(), &generic)
                .expect_err("a generic declaration cannot publish a C function")
                .detail(),
            "`@CCall.Direct` annotates a generic function"
        );
    }
}
