//! The omission masks of a default-argument call: bit `k % 32` of word `k / 32` marks parameter
//! `k` as left to its default.

/// The number of mask words a default-argument entry of `parameters` parameters takes.
pub(crate) fn mask_words(parameters: usize) -> usize {
    parameters.div_ceil(32)
}

/// The mask words of a call to a declaration of `parameters` parameters that leaves `omitted` out.
pub(crate) fn omission_masks(
    parameters: usize,
    omitted: impl IntoIterator<Item = usize>,
) -> Vec<u32> {
    let mut masks = vec![0_u32; mask_words(parameters)];
    for ordinal in omitted {
        masks[ordinal / 32] |= 1 << (ordinal % 32);
    }
    masks
}

/// The bit that marks parameter `ordinal` in its mask word, as the `Int` the word is compared in.
pub(super) fn omission_bit(ordinal: usize) -> i32 {
    // Bit 31 is the sign bit of an `Int` word.
    (1_u32 << (ordinal % 32)) as i32
}
