/// A member of a progression value that a counted loop reads (`first`, `last`, `step`). Common
/// lowering selects which one; the backend owns the member's physical accessor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrProgressionMember {
    First,
    Last,
    Step,
}
