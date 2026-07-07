//! RSS Read (`rss`) — Task 3.3. Fetches a feed URL over the shared HTTP client
//! and parses it with `feed-rs`, which normalizes Atom / RSS 0.x / RSS 1 / RSS 2
//! / JSON Feed onto one model — so "poll a blog/changelog" is one node instead
//! of an HTTP node plus a hand-rolled XML parse. Self-contained like Synapse:
//! the feed URL is a config field, not a primary-input fallback, since it isn't
//! something an upstream node typically produces.
//!
//! Output is a bare array of entries (the list-node convention, so it composes
//! with Loop/Filter/Aggregate/Sort-Limit directly), each row using the field
//! names n8n's RSS Feed Read node uses for developer familiarity: `title`,
//! `link`, `pubDate` (RFC 2822), `isoDate` (RFC 3339), `content`
//! (content:encoded / Atom content, when present), `contentSnippet`
//! (summary/description, plain), `categories`, `creator`, `guid`. Feeds are
//! wildly inconsistent about what they populate — a missing field is `null`/
//! `[]`, never a hard error.

use crate::tools::http::{HttpRequestParams, HttpRequestTool};
use feed_rs::model::Entry;
use serde_json::{json, Value};

fn entry_to_json(e: &Entry) -> Value {
    let title = e.title.as_ref().map(|t| t.content.clone());
    let link = e.links.first().map(|l| l.href.clone());
    let when = e.published.or(e.updated);
    let pub_date = when.map(|d| d.to_rfc2822());
    let iso_date = when.map(|d| d.to_rfc3339());
    let content = e.content.as_ref().and_then(|c| c.body.clone());
    let content_snippet = e
        .summary
        .as_ref()
        .map(|s| s.content.trim().to_string())
        .filter(|s| !s.is_empty());
    let categories: Vec<Value> = e
        .categories
        .iter()
        .map(|c| Value::String(c.term.clone()))
        .collect();
    let creator = e.authors.first().map(|a| a.name.clone());

    json!({
        "guid": e.id,
        "title": title,
        "link": link,
        "pubDate": pub_date,
        "isoDate": iso_date,
        "content": content,
        "contentSnippet": content_snippet,
        "categories": categories,
        "creator": creator,
    })
}

/// Parse already-fetched feed bytes into the node's output shape. Split from
/// `execute` so the parsing/shaping logic is unit-testable without a live HTTP
/// fetch (the table-driven tests below feed real RSS/Atom fixtures straight in).
fn parse_feed(body: &str, max_items: usize) -> Result<Value, String> {
    let feed = feed_rs::parser::parse(body.as_bytes())
        .map_err(|e| format!("RSS Read: could not parse feed: {e}"))?;

    let mut entries: Vec<Value> = feed.entries.iter().map(entry_to_json).collect();
    if max_items > 0 && entries.len() > max_items {
        entries.truncate(max_items);
    }
    Ok(Value::Array(entries))
}

pub(crate) async fn execute(config: &Value) -> Result<Value, String> {
    let url = config
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if url.is_empty() {
        return Err("RSS Read: set the Feed URL".to_string());
    }

    let ignore_ssl = config
        .get("ignoreSSL")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // 0 = no limit, matching Extract from File's maxRows convention.
    let max_items = config
        .get("maxItems")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let params = HttpRequestParams {
        method: "GET".to_string(),
        url: url.to_string(),
        response_format: Some("text".to_string()),
        allow_unauthorized_certs: Some(ignore_ssl),
        timeout_seconds: Some(30),
        ..Default::default()
    };

    let resp = HttpRequestTool::new()
        .request(params)
        .await
        .map_err(|e| format!("RSS Read: fetch failed: {e}"))?;

    let body = resp
        .body
        .as_str()
        .ok_or_else(|| "RSS Read: fetched body was not text".to_string())?;

    parse_feed(body, max_items)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS2: &str = r#"<?xml version="1.0"?>
    <rss version="2.0">
      <channel>
        <title>Example Blog</title>
        <link>https://example.test/</link>
        <description>An example feed</description>
        <item>
          <title>First Post</title>
          <link>https://example.test/first</link>
          <guid>https://example.test/first</guid>
          <pubDate>Mon, 06 Jan 2025 12:00:00 GMT</pubDate>
          <description>&lt;p&gt;Hello world&lt;/p&gt;</description>
          <category>rust</category>
          <category>backend</category>
        </item>
        <item>
          <title>Second Post</title>
          <link>https://example.test/second</link>
          <guid>https://example.test/second</guid>
          <pubDate>Tue, 07 Jan 2025 12:00:00 GMT</pubDate>
          <description>Second body</description>
        </item>
      </channel>
    </rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
      <title>Example Atom Feed</title>
      <link href="https://example.test/"/>
      <updated>2025-01-06T12:00:00Z</updated>
      <id>urn:uuid:feed</id>
      <entry>
        <title>Atom Entry</title>
        <link href="https://example.test/atom-entry"/>
        <id>urn:uuid:entry-1</id>
        <updated>2025-01-06T12:00:00Z</updated>
        <author><name>Jane Doe</name></author>
        <summary>An atom summary</summary>
        <content type="html">&lt;p&gt;Full content&lt;/p&gt;</content>
      </entry>
    </feed>"#;

    // RSS 2.0 items map to title/link/guid/pubDate/contentSnippet/categories.
    #[test]
    fn parses_rss2_items() {
        let out = parse_feed(RSS2, 0).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["title"], json!("First Post"));
        assert_eq!(arr[0]["link"], json!("https://example.test/first"));
        assert_eq!(arr[0]["guid"], json!("https://example.test/first"));
        assert_eq!(arr[0]["categories"], json!(["rust", "backend"]));
        assert!(arr[0]["pubDate"].as_str().unwrap().contains("2025"));
        assert!(arr[0]["isoDate"].as_str().unwrap().starts_with("2025-01-06"));
    }

    // Atom entries map authors→creator and content→content (HTML body kept as-is).
    #[test]
    fn parses_atom_entries() {
        let out = parse_feed(ATOM, 0).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["title"], json!("Atom Entry"));
        assert_eq!(arr[0]["creator"], json!("Jane Doe"));
        assert_eq!(arr[0]["contentSnippet"], json!("An atom summary"));
        assert_eq!(arr[0]["content"], json!("<p>Full content</p>"));
    }

    // maxItems caps the output without erroring.
    #[test]
    fn max_items_truncates() {
        let out = parse_feed(RSS2, 1).unwrap();
        assert_eq!(out.as_array().unwrap().len(), 1);
    }

    // maxItems = 0 means no limit.
    #[test]
    fn zero_max_items_means_no_limit() {
        let out = parse_feed(RSS2, 0).unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2);
    }

    // A feed with no items yields an empty array, not an error.
    #[test]
    fn empty_channel_yields_empty_array() {
        let empty = r#"<rss version="2.0"><channel><title>T</title><link>https://x.test</link><description>d</description></channel></rss>"#;
        let out = parse_feed(empty, 0).unwrap();
        assert_eq!(out, json!([]));
    }

    // An item missing optional fields still parses — nulls, not errors.
    #[test]
    fn missing_optional_fields_are_null() {
        let minimal = r#"<rss version="2.0"><channel><title>T</title><link>https://x.test</link><description>d</description>
            <item><title>Only Title</title></item>
        </channel></rss>"#;
        let out = parse_feed(minimal, 0).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr[0]["title"], json!("Only Title"));
        assert_eq!(arr[0]["link"], Value::Null);
        assert_eq!(arr[0]["creator"], Value::Null);
        assert_eq!(arr[0]["categories"], json!([]));
    }

    // Malformed XML surfaces feed-rs's own error instead of panicking.
    #[test]
    fn malformed_feed_errors() {
        let err = parse_feed("<rss><channel><title>unterminated", 0).unwrap_err();
        assert!(err.contains("could not parse feed"), "got: {err}");
    }

    // Blank URL is a teaching error caught before any fetch is attempted.
    #[tokio::test]
    async fn blank_url_errors() {
        let err = execute(&json!({ "url": "" })).await.unwrap_err();
        assert!(err.contains("Feed URL"), "got: {err}");
    }

    // TEMP smoke test against a real public feed — full fetch+parse runtime
    // path. Run manually with `--ignored`; not part of the permanent suite.
    #[tokio::test]
    #[ignore]
    async fn live_fetch_smoke_test() {
        let out = execute(&json!({ "url": "https://hnrss.org/frontpage", "maxItems": 3 }))
            .await
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert!(arr[0]["title"].as_str().unwrap().len() > 0);
        assert!(arr[0]["link"].as_str().unwrap().starts_with("http"));
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    }
}
