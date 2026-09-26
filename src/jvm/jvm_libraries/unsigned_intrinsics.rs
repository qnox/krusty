//! The JVM realization of the unsigned member operations the provider-boundary realization layer
//! classifies (`builtin_member_realization::unsigned_member_operation`).
//!
//! On a JDK 8+ target kotlinc does not call or inline these stdlib declarations: a call to
//! `UInt.compareTo(UInt)`, `div(UInt)`, `rem(UInt)` or `toString()` (and the `ULong` ones) is the
//! JDK's own unsigned operation on the carrier, whatever the stdlib's own implementation looks like
//! (kotlinc's `IntrinsicMethods` maps the declaration, not its body). The stdlib's inline bodies
//! (`uintCompare`, `uintDivide-*`) are what kotlinc emits only where it builds the call itself, such
//! as the counted-loop comparator. Which declarations these are is decided by that classification;
//! this module owns only the JDK owners, names and descriptors, and applies them to every classified
//! member, so a classified member can never keep its ordinary stdlib call or inline body.

use crate::libraries::builtin_member_realization::{UnsignedElement, UnsignedMemberOperation};
use crate::libraries::{InlineKind, LibraryMember, MemberRealization};
use crate::types::type_name;

/// Realizes `member`, classified as `operation` on `element`, as the JDK static method that
/// implements it on the carrier, with the receiver passed as its first argument.
pub(super) fn realize_jdk_unsigned_member(
    element: UnsignedElement,
    operation: UnsignedMemberOperation,
    member: &mut LibraryMember,
) {
    let (carrier, jdk_owner) = match element {
        UnsignedElement::UInt => ("I", "java/lang/Integer"),
        UnsignedElement::ULong => ("J", "java/lang/Long"),
    };
    let (jdk_name, descriptor) = match operation {
        UnsignedMemberOperation::CompareTo => ("compareUnsigned", format!("({carrier}{carrier})I")),
        UnsignedMemberOperation::Divide => {
            ("divideUnsigned", format!("({carrier}{carrier}){carrier}"))
        }
        UnsignedMemberOperation::Remainder => (
            "remainderUnsigned",
            format!("({carrier}{carrier}){carrier}"),
        ),
        UnsignedMemberOperation::ToString => {
            ("toUnsignedString", format!("({carrier})Ljava/lang/String;"))
        }
    };
    member.owner = Some(type_name(jdk_owner));
    member.physical_name = Some(jdk_name.to_string());
    member.descriptor = descriptor;
    member.realization = MemberRealization::Direct {
        pass_receiver: true,
    };
    member.inline = InlineKind::None;
    member.inline_body_plan = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    fn realized(
        element: UnsignedElement,
        operation: UnsignedMemberOperation,
        mut member: LibraryMember,
    ) -> (
        Option<String>,
        Option<String>,
        String,
        MemberRealization,
        bool,
    ) {
        realize_jdk_unsigned_member(element, operation, &mut member);
        (
            member.owner.map(|owner| owner.render()),
            member.physical_name,
            member.descriptor,
            member.realization,
            member.inline == InlineKind::None && member.inline_body_plan.is_none(),
        )
    }

    /// Whatever realization, descriptor or inline body the stdlib member arrived with, a classified
    /// operation becomes the JDK method: none of those shapes can keep the ordinary stdlib path.
    #[test]
    fn every_classified_member_becomes_its_jdk_operation() {
        let direct = || {
            let mut member = LibraryMember::new(
                "compareTo-WZ4Q5Ns".to_owned(),
                vec![Ty::UInt],
                Ty::Int,
                "(II)I".to_owned(),
            );
            member.realization = MemberRealization::Direct {
                pass_receiver: true,
            };
            member
        };
        let dispatched = || {
            let mut member = direct();
            member.realization = MemberRealization::Dispatch;
            member
        };
        let boxed_descriptor = || {
            let mut member = direct();
            member.descriptor = "(Lkotlin/UInt;)I".to_owned();
            member
        };
        let inline = || {
            let mut member = direct();
            member.inline = InlineKind::CanInline;
            member
        };
        let expected = (
            Some("java/lang/Integer".to_owned()),
            Some("compareUnsigned".to_owned()),
            "(II)I".to_owned(),
            MemberRealization::Direct {
                pass_receiver: true,
            },
            true,
        );
        for member in [direct(), dispatched(), boxed_descriptor(), inline()] {
            assert_eq!(
                realized(
                    UnsignedElement::UInt,
                    UnsignedMemberOperation::CompareTo,
                    member
                ),
                expected
            );
        }
        let long_member = LibraryMember::new(
            "toString-impl".to_owned(),
            Vec::new(),
            Ty::String,
            "(J)Ljava/lang/String;".to_owned(),
        );
        assert_eq!(
            realized(
                UnsignedElement::ULong,
                UnsignedMemberOperation::ToString,
                long_member
            ),
            (
                Some("java/lang/Long".to_owned()),
                Some("toUnsignedString".to_owned()),
                "(J)Ljava/lang/String;".to_owned(),
                MemberRealization::Direct {
                    pass_receiver: true,
                },
                true,
            )
        );
    }
}
