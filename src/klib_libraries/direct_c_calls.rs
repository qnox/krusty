//! Native C entries authenticated at the KLIB provider boundary.
//!
//! A cinterop archive owns both the declaration and its qualified `CCall.Direct` application.
//! The provider joins those facts once and publishes a physical target with the selected callable
//! identity. Neither common lowering nor a backend searches annotations or declaration spellings.

use crate::libraries::{CallablePlacement, DirectCFunction};
use crate::metadata::semantic::{AnnotationApplication, AnnotationArgument, KotlinFunction};
use crate::types::type_name;

use super::declaration_signatures::{unsignable, KlibLibraryError};

const CCALL_DIRECT: &str = "kotlinx/cinterop/internal/CCall$Direct";

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
