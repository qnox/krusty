//! Whether a call through one of `kotlin.Any`'s slots in one file of the module could reach, in an
//! object another file creates, a slot with no body.
//!
//! Every file lowers into one module, so a receiver whose static type does not close its classes
//! within the file (`Any?`, a type parameter, an interface or an open class) can be an object
//! another file creates: an instance of a class whose table traps, or a runtime value that file
//! creates and this target cannot print (a floating box, a function value). A file proves its own
//! calls against its own classes and values while it is lowered
//! (`lower::objects::unrealized_members`). This record carries the rest across files: each file's
//! calls that could leave it, and each file's slots with no body, and the module declines when one
//! file's calls meet another file's such slots, whichever file comes first.

use krusty::backend::class_tables::{AnyMember, ANY_SLOTS};

/// What a call through a member's slot can receive from another file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Receiver {
    /// An instance of another file's class extending or implementing an open class of this file.
    Classes,
    /// Any object: an instance of any class, or a runtime value.
    Erased,
}

/// What one file contributes.
#[derive(Default)]
pub(super) struct FileReach {
    /// The first call of the file through each member's slot whose receiver can come from another
    /// file: the construct it is, and what it can receive.
    calls: [Option<(String, Receiver)>; ANY_SLOTS as usize],
    /// Whether a class of the file has no body in each member's slot.
    bodiless_classes: [bool; ANY_SLOTS as usize],
    /// Whether the file creates a runtime value with no body in each member's slot.
    bodiless_values: [bool; ANY_SLOTS as usize],
}

impl FileReach {
    /// Record a call through `member`'s slot that can receive `receiver` from another file.
    pub(super) fn call(
        &mut self,
        member: AnyMember,
        receiver: Receiver,
        construct: impl FnOnce() -> String,
    ) {
        let recorded = &mut self.calls[member.slot() as usize];
        match recorded {
            Some((_, Receiver::Erased)) => {}
            Some((_, Receiver::Classes)) if receiver == Receiver::Classes => {}
            _ => *recorded = Some((construct(), receiver)),
        }
    }

    /// Record a class of the file whose table has no body for `member`.
    pub(super) fn bodiless_class(&mut self, member: AnyMember) {
        self.bodiless_classes[member.slot() as usize] = true;
    }

    /// Record a runtime value the file creates with no body for `member`.
    pub(super) fn bodiless_value(&mut self, member: AnyMember) {
        self.bodiless_values[member.slot() as usize] = true;
    }

    /// The call of `calls` that can reach one of `slots`' bodiless slots.
    fn meets(calls: &Self, slots: &Self) -> Option<String> {
        AnyMember::ALL.iter().find_map(|member| {
            let slot = member.slot() as usize;
            let (construct, receiver) = calls.calls[slot].as_ref()?;
            let reached = slots.bodiless_classes[slot]
                || (*receiver == Receiver::Erased && slots.bodiless_values[slot]);
            reached.then(|| construct.clone())
        })
    }

    fn absorb(&mut self, other: Self) {
        for member in AnyMember::ALL {
            let slot = member.slot() as usize;
            if let Some((construct, receiver)) = &other.calls[slot] {
                let receiver = *receiver;
                self.call(member, receiver, || construct.clone());
            }
            self.bodiless_classes[slot] |= other.bodiless_classes[slot];
            self.bodiless_values[slot] |= other.bodiless_values[slot];
        }
    }
}

/// What the files of the module lowered so far contribute.
#[derive(Default)]
pub(super) struct ModuleReach {
    earlier: FileReach,
}

impl ModuleReach {
    /// Add one more file's contribution. The construct to decline, when a call of this file can
    /// reach an earlier file's bodiless slot or an earlier file's call can reach one of this
    /// file's.
    pub(super) fn add(&mut self, file: FileReach) -> Option<String> {
        let met = FileReach::meets(&file, &self.earlier)
            .or_else(|| FileReach::meets(&self.earlier, &file))
            .map(|construct| format!("{construct} reaching an object another file creates"));
        self.earlier.absorb(file);
        met
    }
}

#[cfg(test)]
mod tests {
    use super::{AnyMember, FileReach, ModuleReach, Receiver};

    fn calling(member: AnyMember, receiver: Receiver) -> FileReach {
        let mut file = FileReach::default();
        file.call(member, receiver, || format!("a call of {member:?}"));
        file
    }

    fn bodiless_class(member: AnyMember) -> FileReach {
        let mut file = FileReach::default();
        file.bodiless_class(member);
        file
    }

    fn bodiless_value(member: AnyMember) -> FileReach {
        let mut file = FileReach::default();
        file.bodiless_value(member);
        file
    }

    #[test]
    fn an_erased_call_meets_another_files_bodiless_class_or_value_in_either_order() {
        for later_calls in [false, true] {
            for slots in [
                bodiless_class(AnyMember::ToString),
                bodiless_value(AnyMember::ToString),
            ] {
                let calls = calling(AnyMember::ToString, Receiver::Erased);
                let mut module = ModuleReach::default();
                let (first, second) = if later_calls {
                    (slots, calls)
                } else {
                    (calls, slots)
                };
                assert_eq!(module.add(first), None);
                assert_eq!(
                    module.add(second),
                    Some("a call of ToString reaching an object another file creates".to_string())
                );
            }
        }
    }

    #[test]
    fn a_call_on_an_open_class_meets_only_another_files_bodiless_class() {
        let mut module = ModuleReach::default();
        assert_eq!(
            module.add(calling(AnyMember::ToString, Receiver::Classes)),
            None
        );
        assert_eq!(module.add(bodiless_value(AnyMember::ToString)), None);
        assert_eq!(
            module.add(bodiless_class(AnyMember::ToString)),
            Some("a call of ToString reaching an object another file creates".to_string())
        );
    }

    #[test]
    fn a_files_own_calls_and_slots_and_other_members_do_not_meet() {
        let mut own = calling(AnyMember::ToString, Receiver::Erased);
        own.bodiless_class(AnyMember::ToString);
        own.bodiless_value(AnyMember::ToString);
        let mut module = ModuleReach::default();
        assert_eq!(module.add(own), None);
        assert_eq!(module.add(bodiless_class(AnyMember::HashCode)), None);
        assert_eq!(
            module.add(calling(AnyMember::HashCode, Receiver::Erased)),
            Some("a call of HashCode reaching an object another file creates".to_string())
        );
    }
}
