//! Per-pass time budgets, read from a TOML file, and what a measurement means against one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The default budgets, which are in the executable so that the bench needs no files beside it.
pub const EMBEDDED: &str = include_str!("../../bench-budgets.toml");

/// Exit status of a bench run in which some pass is over its budget. (1 is an error, such as a
/// missing GPU or an unreadable budgets file.)
pub const EXIT_OVER_BUDGET: u8 = 2;

/// The ways of drawing that the bench runs, which `gpu_total` has budgets for.
const MODES: [&str; 2] = ["primary", "reference"];

/// Budgets in milliseconds of GPU time per frame, compared with the mean over the run.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    /// By pass name, wherever the pass runs.
    #[serde(default)]
    pub passes: BTreeMap<String, f64>,
    /// For the GPU total of one profiler frame, by mode (`primary`, `reference`). In reference
    /// mode that is one sample.
    #[serde(default)]
    pub gpu_total: BTreeMap<String, f64>,
}

impl Budgets {
    pub fn parse(text: &str) -> Result<Self, String> {
        let budgets: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Some(mode) = budgets
            .gpu_total
            .keys()
            .find(|m| !MODES.contains(&m.as_str()))
        {
            return Err(format!(
                "`gpu_total.{mode}` is not a mode; the modes are {}",
                MODES.join(" and ")
            ));
        }
        for (name, ms) in budgets.passes.iter().chain(&budgets.gpu_total) {
            if !(ms.is_finite() && *ms > 0.0) {
                return Err(format!(
                    "the budget of `{name}` is {ms}, which is not a positive number of milliseconds"
                ));
            }
        }
        Ok(budgets)
    }
}

/// Where a measurement stands against its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Within,
    Over,
    /// Nothing declares a budget for it.
    NoBudget,
    /// There is a budget but the pass has no times to hold it to: the GPU cannot time passes.
    NoData,
}

impl Status {
    /// Whether this fails the run: a pass over budget, or one that could not be checked, which
    /// must not pass the merge gate by default.
    pub fn fails(self) -> bool {
        matches!(self, Status::Over | Status::NoData)
    }
}

/// A budget is met when the mean is at most the budget.
pub fn status(budget: Option<f64>, mean_ms: Option<f64>) -> Status {
    match (budget, mean_ms) {
        (None, _) => Status::NoBudget,
        (Some(_), None) => Status::NoData,
        (Some(budget), Some(mean)) if mean <= budget => Status::Within,
        (Some(_), Some(_)) => Status::Over,
    }
}

/// The exit status of a run: non-zero if any of `statuses` fails, unless `no_fail`.
pub fn exit_code(statuses: &[Status], no_fail: bool) -> u8 {
    if !no_fail && statuses.iter().any(|s| s.fails()) {
        EXIT_OVER_BUDGET
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mean_up_to_the_budget_is_within_and_above_it_is_over() {
        assert_eq!(status(Some(1.0), Some(0.4)), Status::Within);
        assert_eq!(status(Some(1.0), Some(1.0)), Status::Within);
        assert_eq!(status(Some(1.0), Some(1.0001)), Status::Over);
    }

    #[test]
    fn a_pass_without_a_budget_is_never_over() {
        assert_eq!(status(None, Some(1000.0)), Status::NoBudget);
        assert_eq!(status(None, None), Status::NoBudget);
    }

    #[test]
    fn a_budget_with_nothing_measured_is_no_data_rather_than_a_pass() {
        assert_eq!(status(Some(1.0), None), Status::NoData);
    }

    #[test]
    fn over_budget_fails_the_run_unless_told_not_to() {
        use Status::*;
        let ok = [Within, NoBudget, Within];
        let over = [Within, Over, NoBudget];
        assert_eq!(exit_code(&ok, false), 0);
        assert_eq!(exit_code(&[], false), 0);
        assert_eq!(exit_code(&over, false), EXIT_OVER_BUDGET);
        assert_ne!(EXIT_OVER_BUDGET, 0);
        assert_eq!(exit_code(&over, true), 0);
        assert_eq!(exit_code(&ok, true), 0);
    }

    #[test]
    fn a_budget_that_could_not_be_checked_fails_the_run_too() {
        assert_eq!(exit_code(&[Status::NoData], false), EXIT_OVER_BUDGET);
        assert_eq!(exit_code(&[Status::NoData], true), 0);
    }

    #[test]
    fn a_budgets_file_is_per_pass_and_per_mode_totals() {
        let budgets = Budgets::parse(
            "# comment\n[passes]\nprimary = 1.0\nreference = 25\n[gpu_total]\nprimary = 8.6\n",
        )
        .unwrap();
        assert_eq!(budgets.passes["primary"], 1.0);
        assert_eq!(budgets.passes["reference"], 25.0);
        assert_eq!(budgets.gpu_total["primary"], 8.6);
        assert_eq!(Budgets::parse("").unwrap(), Budgets::default());
    }

    #[test]
    fn bad_budgets_files_are_errors_not_silently_ignored() {
        // A misspelt table would otherwise declare no budgets at all.
        assert!(Budgets::parse("[pass]\nprimary = 1.0").is_err());
        assert!(Budgets::parse("[passes]\nprimary = 'fast'").is_err());
        assert!(Budgets::parse("[passes]\nprimary = 0").is_err());
        assert!(Budgets::parse("[passes]\nprimary = -1.0").is_err());
        assert!(Budgets::parse("[passes]\nprimary = nan").is_err());
        assert!(Budgets::parse("[gpu_total]\nprimry = 8.6").is_err());
        assert!(Budgets::parse("not toml").is_err());
    }

    #[test]
    fn the_embedded_budgets_parse_and_hold_the_passes_that_exist() {
        let budgets = Budgets::parse(EMBEDDED).unwrap();
        assert_eq!(budgets.passes["primary"], 1.0, "Primary visibility, #10");
        assert!(budgets.passes.contains_key("reference"));
        assert!(budgets.gpu_total.contains_key("primary"));
    }
}
