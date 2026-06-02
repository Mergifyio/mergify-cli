//! Quarantine API client for `mergify ci junit-process`.
//!
//! The API answers "for these failing tests on this branch, which
//! are currently quarantined?" — failures of quarantined tests
//! are ignored by the final CI verdict; failures of non-quarantined
//! tests still block.
//!
//! Endpoint shape:
//! `POST {api_url}/v1/ci/{owner}/repositories/{repo}/quarantines/check`
//! ```json
//! { "tests_names": [...], "branch": "..." }
//! ```
//! returns
//! ```json
//! { "quarantined_tests_names": [...], "non_quarantined_tests_names": [...] }
//! ```

use std::collections::BTreeSet;

use mergify_core::{ApiFlavor, CliError, HttpClient};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::detector;
use crate::junit_process::junit::TestCase;

/// What the quarantine API told us about a set of failing test
/// case names — sets so membership checks are O(log n) when we
/// later tag each span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuarantinedTests {
    pub quarantined: BTreeSet<String>,
    pub non_quarantined: BTreeSet<String>,
}

/// Cross-cutting view of a `junit-process` run: which case names
/// failed, which the backend says are currently quarantined, and
/// which are not. Drives the OTLP attribute tagging (a failing
/// case in the quarantined set gets `cicd.test.quarantined =
/// true`) as well as the CLI verdict (a non-zero count of
/// non-quarantined failures means the run fails).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuarantineResult {
    /// Every failing test case (status = Failed or Errored).
    pub failing: Vec<TestCase>,
    /// Subset of `failing` whose names appear in the
    /// `quarantined_tests_names` API response.
    pub quarantined: Vec<TestCase>,
    /// Subset of `failing` the API explicitly reported as
    /// non-quarantined. May be a strict subset of
    /// `failing - quarantined` when the API silently dropped some
    /// names; we trust the API's split rather than reconstructing
    /// it locally, to match Python.
    pub non_quarantined: Vec<TestCase>,
    /// Count of failing tests that are NOT quarantined. This is
    /// what determines the final exit code: zero → CI passes,
    /// non-zero → CI fails.
    pub failing_not_quarantined_count: usize,
}

#[derive(Debug, Clone)]
pub struct QuarantineFailed {
    pub message: String,
}

impl std::fmt::Display for QuarantineFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QuarantineFailed {}

/// Find every test case in `cases` whose status is a failure
/// (`Failed` or `Errored`). Mirrors Python's filter; the spans
/// inheriting this property are tagged `cicd.test.quarantined`
/// at the `spans` layer based on the result of [`check`].
fn failing_cases(cases: &[TestCase]) -> Vec<TestCase> {
    cases
        .iter()
        .filter(|c| c.status.is_failure())
        .cloned()
        .collect()
}

/// Query the Mergify CI Insights quarantine API for the names in
/// `failing_names` against the given branch. Returns the API's
/// own split — we do NOT reconstruct `non_quarantined =
/// failing - quarantined` locally, to match Python's behavior.
///
/// `failing_names` may contain duplicates if the `JUnit` input has
/// the same test name reported by multiple suites; that's fine —
/// the API treats names as a set.
pub async fn check(
    api_url: &Url,
    token: &str,
    repository: &str,
    branch: &str,
    failing_names: &[String],
) -> Result<QuarantinedTests, QuarantineFailed> {
    let (owner, repo) = detector::split_owner_repo(repository).map_err(|e| QuarantineFailed {
        message: e.to_string(),
    })?;

    let client = HttpClient::new(api_url.clone(), token, ApiFlavor::Mergify).map_err(|e| {
        QuarantineFailed {
            message: e.to_string(),
        }
    })?;

    let path = format!("/v1/ci/{owner}/repositories/{repo}/quarantines/check");
    let body = CheckRequest {
        tests_names: failing_names,
        branch,
    };

    let resp: CheckResponse = client
        .post(&path, &body)
        .await
        .map_err(|e| QuarantineFailed {
            message: e.to_string(),
        })?;

    Ok(QuarantinedTests {
        quarantined: resp.quarantined_tests_names.into_iter().collect(),
        non_quarantined: resp.non_quarantined_tests_names.into_iter().collect(),
    })
}

/// Categorize the failing test cases into quarantined and
/// non-quarantined buckets, given the API's verdict. The result
/// keeps the failing-cases list intact so the CLI can render the
/// "X/Y failures quarantined" summary without re-walking the
/// original `JUnit` input.
#[must_use]
pub fn categorize(failing: Vec<TestCase>, verdict: &QuarantinedTests) -> QuarantineResult {
    let mut quarantined = Vec::new();
    let mut non_quarantined = Vec::new();
    let mut failing_not_quarantined_count = 0;

    for case in &failing {
        let is_quarantined = verdict.quarantined.contains(&case.name);
        if is_quarantined {
            quarantined.push(case.clone());
        } else {
            failing_not_quarantined_count += 1;
            if verdict.non_quarantined.contains(&case.name) {
                non_quarantined.push(case.clone());
            }
        }
    }

    QuarantineResult {
        failing,
        quarantined,
        non_quarantined,
        failing_not_quarantined_count,
    }
}

/// Resolve the failing test cases for `cases`, hit the quarantine
/// API, and combine the two into a [`QuarantineResult`]. The CLI
/// orchestration uses the bundled fn so the happy path is a single
/// call instead of three.
pub async fn check_failing(
    api_url: &Url,
    token: &str,
    repository: &str,
    branch: &str,
    cases: &[TestCase],
) -> Result<QuarantineResult, QuarantineFailed> {
    let failing = failing_cases(cases);
    if failing.is_empty() {
        return Ok(QuarantineResult::default());
    }
    let names: Vec<String> = failing.iter().map(|c| c.name.clone()).collect();
    let verdict = check(api_url, token, repository, branch, &names).await?;
    Ok(categorize(failing, &verdict))
}

/// Lift a [`QuarantineFailed`] into the shared [`CliError`] so
/// callers can `?` it.
impl From<QuarantineFailed> for CliError {
    fn from(err: QuarantineFailed) -> Self {
        Self::Generic(err.message)
    }
}

#[derive(Serialize)]
struct CheckRequest<'a> {
    tests_names: &'a [String],
    branch: &'a str,
}

#[derive(Deserialize)]
struct CheckResponse {
    quarantined_tests_names: Vec<String>,
    non_quarantined_tests_names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit_process::junit::{Failure, TestStatus};
    use std::time::Duration;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn case(name: &str, status: TestStatus) -> TestCase {
        TestCase {
            name: name.to_string(),
            suite_name: "s".to_string(),
            duration: Some(Duration::from_secs(0)),
            file: None,
            line: None,
            status,
            failure: Failure::default(),
        }
    }

    #[test]
    fn categorize_buckets_quarantined_separately() {
        let failing = vec![
            case("a", TestStatus::Failed),
            case("b", TestStatus::Errored),
            case("c", TestStatus::Failed),
        ];
        let verdict = QuarantinedTests {
            quarantined: ["a".to_string()].into_iter().collect(),
            non_quarantined: ["b".to_string(), "c".to_string()].into_iter().collect(),
        };
        let r = categorize(failing, &verdict);
        assert_eq!(
            r.quarantined.iter().map(|c| &c.name).collect::<Vec<_>>(),
            vec!["a"]
        );
        assert_eq!(
            r.non_quarantined
                .iter()
                .map(|c| &c.name)
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        // 2 failures not quarantined — drives the non-zero exit code.
        assert_eq!(r.failing_not_quarantined_count, 2);
    }

    #[test]
    fn categorize_counts_unknown_as_not_quarantined() {
        // The API may omit names it doesn't recognize (e.g. typo,
        // never seen before). Python treats those as failures that
        // weren't quarantined → must count toward
        // `failing_not_quarantined_count` even though they're not
        // explicitly listed in `non_quarantined_tests_names`.
        let failing = vec![case("x", TestStatus::Failed)];
        let verdict = QuarantinedTests::default();
        let r = categorize(failing, &verdict);
        assert!(r.quarantined.is_empty());
        assert!(r.non_quarantined.is_empty());
        assert_eq!(r.failing_not_quarantined_count, 1);
    }

    #[tokio::test]
    async fn check_posts_to_owner_scoped_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/ci/owner/repositories/repo/quarantines/check"))
            .and(header("Authorization", "Bearer secret"))
            .and(body_json(serde_json::json!({
                "tests_names": ["t1", "t2"],
                "branch": "main",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "quarantined_tests_names": ["t1"],
                "non_quarantined_tests_names": ["t2"],
            })))
            .mount(&server)
            .await;

        let api_url = Url::parse(&server.uri()).unwrap();
        let verdict = check(
            &api_url,
            "secret",
            "owner/repo",
            "main",
            &["t1".to_string(), "t2".to_string()],
        )
        .await
        .expect("API call succeeds");
        assert_eq!(verdict.quarantined.len(), 1);
        assert!(verdict.quarantined.contains("t1"));
        assert!(verdict.non_quarantined.contains("t2"));
    }

    #[tokio::test]
    async fn check_failing_short_circuits_when_no_failures() {
        // Empty failing list → no HTTP call, no QuarantineResult to
        // categorize. Mirrors Python's early return. If the function
        // accidentally tried to POST, the bogus URL would fail.
        let api_url = Url::parse("http://127.0.0.1:1").unwrap();
        let cases = vec![
            case("ok", TestStatus::Passed),
            case("skipped", TestStatus::Skipped),
        ];
        let r = check_failing(&api_url, "tok", "owner/repo", "main", &cases)
            .await
            .expect("must short-circuit");
        assert!(r.failing.is_empty());
        assert_eq!(r.failing_not_quarantined_count, 0);
    }

    #[tokio::test]
    async fn check_surfaces_non_200_as_quarantine_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("backend down"))
            .mount(&server)
            .await;
        let api_url = Url::parse(&server.uri()).unwrap();
        let err = check(&api_url, "tok", "owner/repo", "main", &["t".to_string()])
            .await
            .expect_err("503 must surface as QuarantineFailed");
        assert!(
            err.message.contains("503") || err.message.contains("backend down"),
            "got: {}",
            err.message,
        );
    }
}
