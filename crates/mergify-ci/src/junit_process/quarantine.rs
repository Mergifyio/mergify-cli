//! Quarantine API client for `mergify ci junit-process`.
//!
//! The API answers "for these failing tests on this branch, which
//! are currently quarantined?" — failures of quarantined tests
//! are ignored by the final CI verdict; failures of non-quarantined
//! tests still block.
//!
//! Endpoint shape:
//! `GET {api_url}/v1/ci/{owner}/repositories/{repo}/quarantines?branch=...`
//! returns, one cursor-paginated page at a time,
//! ```json
//! { "quarantined_tests": [{ "test_name": "..." }, ...] }
//! ```

use std::collections::BTreeSet;

use mergify_core::{ApiFlavor, CliError, HttpClient};
use serde::Deserialize;
use url::Url;

use crate::detector;
use crate::junit_process::junit::TestCase;
use crate::tests_quarantine::QuarantineList;

/// Page size requested from the quarantine list endpoint.
const PER_PAGE: &str = "100";

/// Status the API answers when quarantine is not in the plan.
const PAYMENT_REQUIRED: u16 = 402;

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
    /// Subset of `failing` currently quarantined on the branch.
    pub quarantined: Vec<TestCase>,
    /// Subset of `failing` not quarantined on the branch. Empty →
    /// CI passes, non-empty → CI fails.
    pub non_quarantined: Vec<TestCase>,
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
/// at the `spans` layer based on the result of [`fetch`].
fn failing_cases(cases: &[TestCase]) -> Vec<TestCase> {
    cases
        .iter()
        .filter(|c| c.status.is_failure())
        .cloned()
        .collect()
}

/// Fetch the names of every test quarantined on `branch`, following
/// the cursor pagination until the last page. A repository without
/// the feature in its plan (402) has nothing quarantined.
pub async fn fetch(
    api_url: &Url,
    token: &str,
    repository: &str,
    branch: &str,
) -> Result<BTreeSet<String>, QuarantineFailed> {
    let failed = |message: String| QuarantineFailed { message };
    let (owner, repo) =
        detector::split_owner_repo(repository).map_err(|e| failed(e.to_string()))?;
    let client = HttpClient::new(api_url.clone(), token, ApiFlavor::Mergify)
        .map_err(|e| failed(e.to_string()))?;

    let path = format!("/v1/ci/{owner}/repositories/{repo}/quarantines");
    let mut quarantined = BTreeSet::new();
    let mut seen_cursors = BTreeSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut query = vec![("branch", branch), ("per_page", PER_PAGE)];
        if let Some(cursor) = &cursor {
            query.push(("cursor", cursor));
        }
        let Some(page) = client
            .get_page_unless::<QuarantineList<QuarantinedTestName>>(&path, &query, PAYMENT_REQUIRED)
            .await
            .map_err(|e| failed(e.to_string()))?
        else {
            return Ok(BTreeSet::new());
        };
        quarantined.extend(page.body.quarantined_tests.into_iter().map(|t| t.test_name));

        let Some(next) = page.next_cursor else {
            return Ok(quarantined);
        };
        // A cursor pointing back to a fetched page would loop forever;
        // a partial list would silently un-quarantine tests.
        if !seen_cursors.insert(next.clone()) {
            return Err(failed(
                "quarantine pagination cycled back to a fetched page".to_string(),
            ));
        }
        cursor = Some(next);
    }
}

/// Categorize the failing test cases into quarantined and
/// non-quarantined buckets, given the branch's quarantined names. The result
/// keeps the failing-cases list intact so the CLI can render the
/// "X/Y failures quarantined" summary without re-walking the
/// original `JUnit` input.
#[must_use]
pub fn categorize(
    failing: Vec<TestCase>,
    quarantined_names: &BTreeSet<String>,
) -> QuarantineResult {
    let mut quarantined = Vec::new();
    let mut non_quarantined = Vec::new();

    for case in &failing {
        if quarantined_names.contains(&case.name) {
            quarantined.push(case.clone());
        } else {
            non_quarantined.push(case.clone());
        }
    }

    QuarantineResult {
        failing,
        quarantined,
        non_quarantined,
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
    let quarantined_names = fetch(api_url, token, repository, branch).await?;
    Ok(categorize(failing, &quarantined_names))
}

/// Lift a [`QuarantineFailed`] into the shared [`CliError`] so
/// callers can `?` it.
impl From<QuarantineFailed> for CliError {
    fn from(err: QuarantineFailed) -> Self {
        // Preserve the typed error as a transparent source instead of
        // flattening it (same Display, stays downcastable).
        Self::Source(Box::new(err))
    }
}

#[derive(Deserialize)]
struct QuarantinedTestName {
    test_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit_process::junit::{Failure, TestStatus};
    use std::time::Duration;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
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

    fn names(cases: &[TestCase]) -> Vec<&str> {
        cases.iter().map(|c| c.name.as_str()).collect()
    }

    fn page(test_names: &[&str]) -> serde_json::Value {
        let tests: Vec<_> = test_names
            .iter()
            .map(|name| serde_json::json!({ "test_name": name }))
            .collect();
        serde_json::json!({ "quarantined_tests": tests })
    }

    fn link(cursor: &str) -> String {
        format!(
            "<https://api.example/v1/ci/owner/repositories/repo/quarantines?cursor={cursor}>; rel=\"next\""
        )
    }

    #[test]
    fn categorize_buckets_quarantined_separately() {
        let failing = vec![
            case("a", TestStatus::Failed),
            case("b", TestStatus::Errored),
            case("c", TestStatus::Failed),
        ];
        let quarantined_names = ["a".to_string(), "unrelated".to_string()]
            .into_iter()
            .collect();
        let r = categorize(failing, &quarantined_names);
        assert_eq!(names(&r.quarantined), vec!["a"]);
        assert_eq!(names(&r.non_quarantined), vec!["b", "c"]);
        // 2 failures not quarantined — drives the non-zero exit code.
        assert_eq!(r.non_quarantined.len(), 2);
    }

    #[tokio::test]
    async fn fetch_follows_pagination_to_completion() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/ci/owner/repositories/repo/quarantines"))
            .and(header("Authorization", "Bearer secret"))
            .and(query_param("branch", "main"))
            .and(query_param("per_page", "100"))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", link("c2").as_str())
                    .set_body_json(page(&["t1"])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/ci/owner/repositories/repo/quarantines"))
            .and(query_param("branch", "main"))
            .and(query_param("per_page", "100"))
            .and(query_param("cursor", "c2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(&["t2"])))
            .expect(1)
            .mount(&server)
            .await;

        let api_url = Url::parse(&server.uri()).unwrap();
        let quarantined = fetch(&api_url, "secret", "owner/repo", "main")
            .await
            .expect("API call succeeds");
        assert_eq!(
            quarantined,
            ["t1".to_string(), "t2".to_string()].into_iter().collect()
        );
    }

    #[tokio::test]
    async fn fetch_surfaces_pagination_cycle_as_quarantine_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/ci/owner/repositories/repo/quarantines"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", link("loop").as_str())
                    .set_body_json(page(&["t1"])),
            )
            .expect(2)
            .mount(&server)
            .await;

        let api_url = Url::parse(&server.uri()).unwrap();
        let err = fetch(&api_url, "tok", "owner/repo", "main")
            .await
            .expect_err("a cycle must not return a partial list");
        assert_eq!(
            err.message,
            "quarantine pagination cycled back to a fetched page"
        );
    }

    #[tokio::test]
    async fn fetch_treats_payment_required_as_nothing_quarantined() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/ci/owner/repositories/repo/quarantines"))
            .respond_with(ResponseTemplate::new(402).set_body_string("upgrade your plan"))
            .expect(1)
            .mount(&server)
            .await;

        let api_url = Url::parse(&server.uri()).unwrap();
        let quarantined = fetch(&api_url, "tok", "owner/repo", "main")
            .await
            .expect("402 is not a failure");
        assert!(quarantined.is_empty());
    }

    #[tokio::test]
    async fn check_failing_short_circuits_when_no_failures() {
        // Empty failing list → no HTTP call, no QuarantineResult to
        // categorize. If the function accidentally tried to fetch,
        // the bogus URL would fail.
        let api_url = Url::parse("http://127.0.0.1:1").unwrap();
        let cases = vec![
            case("ok", TestStatus::Passed),
            case("skipped", TestStatus::Skipped),
        ];
        let r = check_failing(&api_url, "tok", "owner/repo", "main", &cases)
            .await
            .expect("must short-circuit");
        assert!(r.failing.is_empty());
        assert!(r.non_quarantined.is_empty());
    }

    #[tokio::test]
    async fn fetch_surfaces_non_200_as_quarantine_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503).set_body_string("backend down"))
            .mount(&server)
            .await;
        let api_url = Url::parse(&server.uri()).unwrap();
        let err = fetch(&api_url, "tok", "owner/repo", "main")
            .await
            .expect_err("503 must surface as QuarantineFailed");
        assert!(
            err.message.contains("503") || err.message.contains("backend down"),
            "got: {}",
            err.message,
        );
    }
}
