//! The `int` masks a default-argument stub takes: one bit per source parameter, 32 to a mask, and
//! at least one mask even for a callable with no parameters.

pub(super) fn default_mask_count(param_count: usize) -> usize {
    param_count.div_ceil(32).max(1)
}

pub(super) fn default_mask_bit(param_index: usize) -> i32 {
    (1u32 << (param_index % 32)) as i32
}

/// Every mask with each of the callable's parameters marked as defaulted.
pub(super) fn full_default_masks(param_count: usize) -> Vec<i32> {
    (0..default_mask_count(param_count))
        .map(|chunk| {
            let start = chunk * 32;
            let end = ((chunk + 1) * 32).min(param_count);
            (start..end).fold(0i32, |mask, i| mask | default_mask_bit(i))
        })
        .collect()
}
