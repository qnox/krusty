//! Machine-readable conformance scores shared by target lanes.

/// Format one `<pct> <count> <of>` report line.
///
/// Counts stay exact until this presentation boundary. The one-decimal percentage rounds down, so
/// only `count == of` can display `100.0`. `of == 0` is a valid empty selection and reports `0.0`,
/// matching `scripts/conformance-report.sh`.
pub(crate) fn count_report(count: u64, of: u64) -> String {
    let tenths: u128 = if of == 0 {
        0
    } else {
        u128::from(count) * 1000 / u128::from(of)
    };
    format!("{}.{:01} {count} {of}\n", tenths / 10, tenths % 10)
}

#[test]
fn report_keeps_exact_counts_and_derives_one_decimal_percentage() {
    assert_eq!(count_report(2, 3), "66.6 2 3\n");
    assert_eq!(count_report(3145, 3146), "99.9 3145 3146\n");
    assert_eq!(count_report(3146, 3146), "100.0 3146 3146\n");
    assert_eq!(count_report(0, 0), "0.0 0 0\n");
}
