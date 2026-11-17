//! JVM8 realizations of the exact atomic exchange declarations decoded from Kotlin builtins.
//! The stdlib helper implements the witness-value operation using a retrying identity CAS. The
//! similarly named JDK virtual methods require Java9, even when compiling on a newer host JDK.

use crate::libraries::{LibraryMember, MemberRealization};
use crate::types::{type_name, TypeName};

/// Attach only physical call facts. The source parameters, generic signature, result and member
/// identity remain the decoded Kotlin declaration; the existing direct-call boundary prepends its
/// dispatch receiver to the static helper's operands.
pub(super) fn realize(owner: TypeName, is_property: bool, member: &mut LibraryMember) {
    if is_property {
        return;
    }
    let rows = [
        (
            "kotlin/concurrent/atomics/AtomicInt",
            "compareAndExchange",
            "(II)I",
        ),
        (
            "kotlin/concurrent/atomics/AtomicLong",
            "compareAndExchange",
            "(JJ)J",
        ),
        (
            "kotlin/concurrent/atomics/AtomicBoolean",
            "compareAndExchange",
            "(ZZ)Z",
        ),
        (
            "kotlin/concurrent/atomics/AtomicReference",
            "compareAndExchange",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        ),
        (
            "kotlin/concurrent/atomics/AtomicIntArray",
            "compareAndExchangeAt",
            "(III)I",
        ),
        (
            "kotlin/concurrent/atomics/AtomicLongArray",
            "compareAndExchangeAt",
            "(IJJ)J",
        ),
        (
            "kotlin/concurrent/atomics/AtomicArray",
            "compareAndExchangeAt",
            "(ILjava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        ),
    ];
    let matched = rows.iter().any(|(class, name, descriptor)| {
        owner == type_name(class) && member.name == *name && member.descriptor == *descriptor
    });
    if !matched {
        return;
    }
    let receiver = super::super::names::type_descriptor(crate::types::Ty::obj_name(owner));
    member.descriptor.insert_str(1, &receiver);
    member.owner = Some(type_name("kotlin/concurrent/internal/AtomicIntrinsicsKt"));
    member.physical_name = Some("compareAndExchange".to_string());
    member.realization = MemberRealization::Direct {
        pass_receiver: true,
    };
}
