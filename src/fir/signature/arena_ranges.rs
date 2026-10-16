/// A half-open slice in the signature graph's shared operand arena.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OperandRange {
    pub(super) start: u32,
    pub(super) len: u32,
}

/// A half-open slice in the signature graph's packed call-argument arena.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CallArgumentRange {
    pub(super) start: u32,
    pub(super) len: u32,
}

/// A half-open slice in the signature graph's shared substitution arena.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SubstitutionRange {
    pub(super) start: u32,
    pub(super) len: u32,
}
