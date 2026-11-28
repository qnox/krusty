//! The KLIB route as a second, measured mode of a runnable box lane.
//!
//! The lane's ratchet holds the source route: every `// MODULE:` of a case is lowered from source
//! into the one program. kotlinc instead links every module but the last from its KLIB. Setting
//! `KRUSTY_<infix>_BOX_KLIB_ROUTE=1` also runs each multi-module case through
//! [`super::box_lane::compile_modules_from_klibs`], which builds those KLIBs and links the final
//! module against them, and reports how the two routes compare on this lane. The KLIB route never
//! feeds the ratchet: it is measured beside it until it passes everything the source route
//! passes, and only then replaces it.
//!
//! `KRUSTY_<infix>_BOX_KLIB_ROUTE_REPORT=<path>` also writes one tab-separated row per
//! multi-module case: its key, both outcomes and both compile-and-run times.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use super::box_lane::Outcome;

/// One multi-module case, run through both routes.
struct RouteCase {
    file: PathBuf,
    source: Outcome,
    source_time: Duration,
    klib: Outcome,
    klib_time: Duration,
}

/// The two routes' outcomes for one lane run, when the KLIB route is enabled.
pub struct KlibRoute {
    enabled: bool,
    cases: Mutex<Vec<RouteCase>>,
}

impl KlibRoute {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            cases: Mutex::new(Vec::new()),
        }
    }

    pub fn from_env(env_infix: &str) -> Self {
        Self::new(
            std::env::var(format!("KRUSTY_{env_infix}_BOX_KLIB_ROUTE"))
                .is_ok_and(|value| value == "1"),
        )
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn record(
        &self,
        file: PathBuf,
        (source, source_time): (&Outcome, Duration),
        (klib, klib_time): (Outcome, Duration),
    ) {
        let case = RouteCase {
            file,
            source: copied(source),
            source_time,
            klib,
            klib_time,
        };
        self.cases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(case);
    }

    /// The parity report for `lane`, keyed by `key`; `None` when the route was not enabled.
    pub fn report(&self, lane: &str, key: impl Fn(&std::path::Path) -> String) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let mut cases = std::mem::take(
            &mut *self
                .cases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        cases.sort_by(|a, b| a.file.cmp(&b.file));
        Some(parity_report(lane, &cases, key))
    }

    /// The tab-separated rows of every recorded case, in key order.
    pub fn rows(&self, key: impl Fn(&std::path::Path) -> String) -> String {
        let cases = self
            .cases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut rows = cases
            .iter()
            .map(|case| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    key(&case.file),
                    one_line(&case.source),
                    one_line(&case.klib),
                    case.source_time.as_millis(),
                    case.klib_time.as_millis()
                )
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows.concat()
    }
}

/// Only an applicable case enters the comparison; both routes share the applicability checks, so
/// a not-applicable case is one on both.
fn parity_report(
    lane: &str,
    cases: &[RouteCase],
    key: impl Fn(&std::path::Path) -> String,
) -> String {
    let mut both = 0usize;
    let mut neither = 0usize;
    let mut regressions = Vec::new();
    let mut gains = Vec::new();
    let mut regression_reasons = BTreeMap::<String, usize>::new();
    let mut source_time = Duration::ZERO;
    let mut klib_time = Duration::ZERO;
    let mut compared = 0usize;
    for case in cases {
        if matches!(case.source, Outcome::NotApplicable(_)) {
            continue;
        }
        compared += 1;
        source_time += case.source_time;
        klib_time += case.klib_time;
        match (case.source == Outcome::Pass, case.klib == Outcome::Pass) {
            (true, true) => both += 1,
            (false, false) => neither += 1,
            (true, false) => {
                *regression_reasons.entry(one_line(&case.klib)).or_default() += 1;
                regressions.push((key(&case.file), one_line(&case.klib)));
            }
            (false, true) => gains.push((key(&case.file), one_line(&case.source))),
        }
    }
    let mut report = format!(
        "{lane} klib-route parity: modules {compared}, both-pass {both}, source-only {}, \
         klib-only {}, neither {neither} (source {:.1}s, klib {:.1}s)\n",
        regressions.len(),
        gains.len(),
        source_time.as_secs_f64(),
        klib_time.as_secs_f64(),
    );
    let mut ranked = regression_reasons.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (reason, count) in ranked.iter().take(40) {
        let _ = writeln!(report, "  klib-route source-only {count:>5}  {reason}");
    }
    for (case, reason) in &regressions {
        let _ = writeln!(report, "  klib-route regression {case}: {reason}");
    }
    for (case, reason) in &gains {
        let _ = writeln!(report, "  klib-route gain {case}: source route {reason}");
    }
    report
}

fn copied(outcome: &Outcome) -> Outcome {
    match outcome {
        Outcome::Pass => Outcome::Pass,
        Outcome::Declined(reason) => Outcome::Declined(reason.clone()),
        Outcome::Frontend(reason) => Outcome::Frontend(reason.clone()),
        Outcome::CompilerPanic(reason) => Outcome::CompilerPanic(reason.clone()),
        Outcome::Harness(reason) => Outcome::Harness(reason.clone()),
        Outcome::NotApplicable(reason) => Outcome::NotApplicable(reason),
        Outcome::Failed(reason) => Outcome::Failed(reason.clone()),
    }
}

/// An outcome as one report line: its kind, then the first line of its reason.
fn one_line(outcome: &Outcome) -> String {
    let (kind, reason) = match outcome {
        Outcome::Pass => return "pass".to_string(),
        Outcome::Declined(reason) => ("declined", reason.as_str()),
        Outcome::Frontend(reason) => ("frontend", reason.as_str()),
        Outcome::CompilerPanic(reason) => ("compiler panic", reason.as_str()),
        Outcome::Harness(reason) => ("harness", reason.as_str()),
        Outcome::NotApplicable(reason) => ("not applicable", *reason),
        Outcome::Failed(reason) => ("failed", reason.as_str()),
    };
    let first = reason.lines().next().unwrap_or("").replace('\t', " ");
    format!("{kind}: {first}")
}

#[test]
fn parity_splits_cases_by_which_route_passes_and_ranks_klib_regressions() {
    let route = KlibRoute::new(true);
    let case = |name: &str, source: Outcome, klib: Outcome| {
        route.record(
            PathBuf::from(name),
            (&source, Duration::from_millis(1500)),
            (klib, Duration::from_millis(500)),
        );
    };
    case("b.kt", Outcome::Pass, Outcome::Pass);
    case(
        "c.kt",
        Outcome::Pass,
        Outcome::Declined("KLIB IR body: class\nmore".to_string()),
    );
    case(
        "a.kt",
        Outcome::Pass,
        Outcome::Declined("KLIB IR body: class".to_string()),
    );
    case(
        "d.kt",
        Outcome::Failed("got FAIL".to_string()),
        Outcome::Pass,
    );
    case(
        "e.kt",
        Outcome::Frontend("unresolved".to_string()),
        Outcome::Frontend("unresolved".to_string()),
    );
    case(
        "f.kt",
        Outcome::NotApplicable("muted"),
        Outcome::NotApplicable("muted"),
    );
    let key = |file: &std::path::Path| file.display().to_string();
    assert_eq!(
        route.rows(key),
        "a.kt\tpass\tdeclined: KLIB IR body: class\t1500\t500\n\
         b.kt\tpass\tpass\t1500\t500\n\
         c.kt\tpass\tdeclined: KLIB IR body: class\t1500\t500\n\
         d.kt\tfailed: got FAIL\tpass\t1500\t500\n\
         e.kt\tfrontend: unresolved\tfrontend: unresolved\t1500\t500\n\
         f.kt\tnot applicable: muted\tnot applicable: muted\t1500\t500\n"
    );
    assert_eq!(
        route.report("Native", key).as_deref(),
        Some(
            "Native klib-route parity: modules 5, both-pass 1, source-only 2, klib-only 1, \
             neither 1 (source 7.5s, klib 2.5s)\n  \
             klib-route source-only     2  declined: KLIB IR body: class\n  \
             klib-route regression a.kt: declined: KLIB IR body: class\n  \
             klib-route regression c.kt: declined: KLIB IR body: class\n  \
             klib-route gain d.kt: source route failed: got FAIL\n"
        )
    );
}

#[test]
fn a_disabled_route_reports_nothing() {
    let route = KlibRoute::new(false);
    assert!(!route.enabled());
    assert_eq!(route.report("wasm-js", |_| String::new()), None);
}
