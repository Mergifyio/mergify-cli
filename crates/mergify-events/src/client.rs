//! The `/logs` fetch: explicit window, cursor pagination followed to
//! completion, newest-first guarantee.

use mergify_core::CliError;
use mergify_core::http::Client;
use serde::Deserialize;

use crate::event::Event;
use crate::window::Window;

/// The API's page-size ceiling.
const PER_PAGE_MAX: usize = 100;

/// A filtered query over a repository's activity log.
pub struct Query {
    /// Only events belonging to this pull request; `None` queries the
    /// whole repository.
    pub pull_request: Option<u64>,
    /// Only these event types (repeated `event_type` keys, OR
    /// semantics); empty selects all. Values pass through verbatim —
    /// the server owns the vocabulary, so a type this CLI has never
    /// heard of still works against a newer engine.
    pub event_types: Vec<String>,
    /// The explicit time range — never optional, see [`Window`].
    pub window: Window,
    /// Stop after this many events. `None` follows pagination until
    /// the window is exhausted.
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
struct EventsResponse {
    #[serde(default)]
    events: Vec<serde_json::Value>,
}

/// Fetch every event matching `query`, newest first.
///
/// Both window bounds are always sent (the API's silent
/// `received_to - 1 day` default is the trap this crate exists to
/// bury), pagination cursors are followed to completion unless
/// [`Query::limit`] stops earlier, and the result is sorted
/// newest-first — a guarantee of this function, not an assumption
/// about the server. Events with no parseable `received_at` sort
/// last, in server order.
///
/// # Errors
///
/// Propagates the API failure ([`CliError::MergifyApi`] for HTTP
/// errors). What that means is the caller's call — `queue show`
/// treats it as "could not determine", not as a command failure.
pub async fn fetch(
    client: &Client,
    repository: &str,
    query: &Query,
) -> Result<Vec<Event>, CliError> {
    let path = format!("/v1/repos/{repository}/logs");
    let from = query.window.from().to_rfc3339();
    let to = query.window.to().to_rfc3339();
    let pull_request = query.pull_request.map(|n| n.to_string());
    let per_page = query
        .limit
        .map_or(PER_PAGE_MAX, |limit| limit.clamp(1, PER_PAGE_MAX))
        .to_string();

    let mut base: Vec<(&str, &str)> = Vec::new();
    if let Some(pull_request) = &pull_request {
        base.push(("pull_request", pull_request));
    }
    for event_type in &query.event_types {
        base.push(("event_type", event_type));
    }
    base.push(("received_from", &from));
    base.push(("received_to", &to));
    base.push(("per_page", &per_page));

    let mut raw_events: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut pairs = base.clone();
        if let Some(cursor) = &cursor {
            pairs.push(("cursor", cursor));
        }
        let page = client.get_page::<EventsResponse>(&path, &pairs).await?;
        raw_events.extend(page.body.events);
        if query.limit.is_some_and(|limit| raw_events.len() >= limit) {
            break;
        }
        match page.next_cursor {
            // A server bug echoing the cursor we just used would
            // otherwise loop forever.
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            _ => break,
        }
    }

    let mut events: Vec<Event> = raw_events.into_iter().map(Event::from_raw).collect();
    // Stable sort: ties (and timestamp-less events, which compare
    // smallest and thus sink to the end) keep the server's order.
    events.sort_by_key(|event| std::cmp::Reverse(event.received_at_utc()));
    if let Some(limit) = query.limit {
        events.truncate(limit);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use chrono::TimeDelta;
    use chrono::Utc;
    use mergify_core::http::ApiFlavor;
    use serde_json::json;
    use url::Url;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    use wiremock::matchers::query_param;
    use wiremock::matchers::query_param_is_missing;

    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.uri()).unwrap(), "t", ApiFlavor::Mergify).unwrap()
    }

    fn event(id: u64, received_at: &str) -> serde_json::Value {
        json!({
            "id": id,
            "type": "action.queue.leave",
            "received_at": received_at,
            "metadata": {},
        })
    }

    fn page_body(events: &[serde_json::Value]) -> serde_json::Value {
        json!({"size": events.len(), "per_page": 100, "events": events})
    }

    #[tokio::test]
    async fn fetch_sends_both_window_bounds_and_every_filter() {
        // The explicit window is the whole point: without
        // `received_from` the API silently narrows to 1 day, and
        // `received_to` rides along so the range is what the caller
        // stated, not what the server's clock says.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&[])))
            .expect(1)
            .mount(&server)
            .await;

        let now = at("2026-07-30T00:00:00Z");
        let query = Query {
            pull_request: Some(1700),
            event_types: vec![
                "action.queue.leave".to_string(),
                "command.queue".to_string(),
            ],
            window: Window::last(TimeDelta::days(7), now).unwrap(),
            limit: None,
        };
        fetch(&client(&server), "owner/repo", &query).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let pairs: Vec<(String, String)> = received[0]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(
            pairs.contains(&("pull_request".into(), "1700".into())),
            "got: {pairs:?}",
        );
        // Repeated keys, OR semantics — one `event_type` per value.
        assert!(
            pairs.contains(&("event_type".into(), "action.queue.leave".into())),
            "got: {pairs:?}",
        );
        assert!(
            pairs.contains(&("event_type".into(), "command.queue".into())),
            "got: {pairs:?}",
        );
        let from = pairs
            .iter()
            .find(|(k, _)| k == "received_from")
            .map(|(_, v)| v.clone())
            .expect("received_from must be explicit, never the API's 1-day default");
        assert_eq!(at(&from), at("2026-07-23T00:00:00Z"));
        let to = pairs
            .iter()
            .find(|(k, _)| k == "received_to")
            .map(|(_, v)| v.clone())
            .expect("received_to must be explicit too");
        assert_eq!(at(&to), now);
        assert!(
            pairs.contains(&("per_page".into(), "100".into())),
            "got: {pairs:?}",
        );
    }

    #[tokio::test]
    async fn fetch_omits_the_pull_request_filter_for_a_repo_wide_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param_is_missing("pull_request"))
            .and(query_param_is_missing("event_type"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&[])))
            .expect(1)
            .mount(&server)
            .await;

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: None,
        };
        let events = fetch(&client(&server), "owner/repo", &query).await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn fetch_follows_pagination_to_completion() {
        // Three pages chained by Link-header cursors: the client must
        // walk all of them, not silently return the first.
        let server = MockServer::start().await;
        let link = |cursor: &str| {
            format!("<https://api.example/v1/repos/owner/repo/logs?cursor={cursor}>; rel=\"next\"")
        };
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", link("c2").as_str())
                    .set_body_json(page_body(&[event(3, "2026-07-29T12:00:00Z")])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param("cursor", "c2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", link("c3").as_str())
                    .set_body_json(page_body(&[event(2, "2026-07-29T11:00:00Z")])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param("cursor", "c3"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(page_body(&[event(1, "2026-07-29T10:00:00Z")])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: None,
        };
        let events = fetch(&client(&server), "owner/repo", &query).await.unwrap();
        let ids: Vec<u64> = events
            .iter()
            .map(|e| e.raw["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 2, 1]);
    }

    #[tokio::test]
    async fn fetch_stops_at_the_limit_without_following_further_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param_is_missing("cursor"))
            .and(query_param("per_page", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "link",
                        "<https://api.example/logs?cursor=more>; rel=\"next\"",
                    )
                    .set_body_json(page_body(&[
                        event(2, "2026-07-29T12:00:00Z"),
                        event(1, "2026-07-29T11:00:00Z"),
                    ])),
            )
            .expect(1)
            .mount(&server)
            .await;
        // No mock for cursor=more: requesting it would 404 and fail
        // the fetch, so success proves the limit stopped pagination.

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: Some(2),
        };
        let events = fetch(&client(&server), "owner/repo", &query).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn fetch_guarantees_newest_first_even_when_the_server_does_not() {
        // Ordering is this crate's promise. A server page in the
        // wrong order gets re-sorted; a timestamp-less event sinks to
        // the end instead of poisoning the sort.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&[
                event(1, "2026-07-29T10:00:00Z"),
                json!({"id": 99, "type": "mystery"}),
                event(3, "2026-07-29T12:00:00Z"),
                event(2, "2026-07-29T11:00:00Z"),
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: None,
        };
        let events = fetch(&client(&server), "owner/repo", &query).await.unwrap();
        let ids: Vec<u64> = events
            .iter()
            .map(|e| e.raw["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 2, 1, 99]);
    }

    #[tokio::test]
    async fn fetch_survives_a_server_echoing_the_same_cursor() {
        // A `next` link pointing at the page we just asked for must
        // terminate, not loop forever.
        let server = MockServer::start().await;
        let echo = "<https://api.example/logs?cursor=stuck>; rel=\"next\"";
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", echo)
                    .set_body_json(page_body(&[event(2, "2026-07-29T12:00:00Z")])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .and(query_param("cursor", "stuck"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", echo)
                    .set_body_json(page_body(&[event(1, "2026-07-29T11:00:00Z")])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: None,
        };
        let events = fetch(&client(&server), "owner/repo", &query).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn fetch_keeps_unknown_fields_verbatim() {
        let server = MockServer::start().await;
        let raw = json!({
            "id": 7,
            "type": "action.queue.leave",
            "received_at": "2026-07-29T12:00:00Z",
            "field_from_2027": {"nested": [1, 2, 3]},
        });
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(page_body(std::slice::from_ref(&raw))),
            )
            .expect(1)
            .mount(&server)
            .await;

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: None,
        };
        let events = fetch(&client(&server), "owner/repo", &query).await.unwrap();
        assert_eq!(events[0].raw, raw);
    }

    #[tokio::test]
    async fn fetch_propagates_the_api_failure() {
        // A 403 (token can read the queue but not the whole activity
        // log) surfaces as the typed Mergify API error; the caller
        // decides whether that is fatal.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/repos/owner/repo/logs"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"detail": "nope"})))
            .expect(1)
            .mount(&server)
            .await;

        let query = Query {
            pull_request: None,
            event_types: vec![],
            window: Window::retained(at("2026-07-30T00:00:00Z")),
            limit: None,
        };
        let err = fetch(&client(&server), "owner/repo", &query)
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)), "got: {err:?}");
    }
}
