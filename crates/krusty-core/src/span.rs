//! Source positions shared by every phase.

/// A byte range into the source file. `u32` offsets keep this 8 bytes (data-oriented).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }
}
