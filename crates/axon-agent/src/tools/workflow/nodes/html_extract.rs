//! Retina (HTML Extract) — Task 2.3. CSS-selector extraction over an HTML
//! document, turning "Synapse fetched a page" into real web scraping without a
//! JavaScript node. `scraper` is compile-heavy but runtime-light (no TLS, no
//! HTTP of its own) — fine for the e2-micro.
//!
//! Each extraction rule names a `key` (output field), a `cssSelector`, and what
//! to return per match: the element's `text` (default), its inner `html`, or an
//! `attribute` value. `returnArray` decides first-match-only vs every match.
//! The HTML source is the `html` config field (an expression like
//! `{{ $node["Synapse"].body }}`); left blank, the node falls back to the
//! primary input — the input itself when it's a string, or its `body` / `html` /
//! `data` / `text` field (in that order), which matches Synapse's
//! `{ status, body, ... }` response shape out of the box.
//!
//! Output is ONE object with all the extraction keys (a page reduces to one
//! item, like Aggregate); `includeInputFields` merges it onto the incoming item,
//! same convention as Soma/`dateTime`/`crypto`.

use serde_json::{Map, Value};

/// Pull an HTML string out of a value: a non-blank string is taken as-is; an
/// object is probed at the conventional body-carrying fields (`body` first —
/// Synapse's response shape); an array defers to its first element.
fn html_from_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(m) => ["body", "html", "data", "text"]
            .iter()
            .find_map(|k| m.get(*k).and_then(html_from_value)),
        Value::Array(a) => a.first().and_then(html_from_value),
        _ => None,
    }
}

/// The HTML to parse: the `html` config value when it yields something, else
/// the primary input. Both go through the same probing so an expression that
/// resolves to a whole Synapse response object still finds the body.
fn source_html(config: &Value, input: &Value) -> Result<String, String> {
    config
        .get("html")
        .and_then(html_from_value)
        .or_else(|| html_from_value(input))
        .ok_or_else(|| {
            "HTML Extract: no HTML found — set the HTML field (e.g. {{ $node[\"Synapse\"].body }}) \
             or feed a node whose output is/contains the page text"
                .to_string()
        })
}

/// Whitespace cleanup for extracted text. Raw HTML text nodes carry the
/// document's indentation/newlines, so `trimValues` collapses runs of
/// whitespace to single spaces; inner HTML and attribute values only trim at
/// the edges (collapsing inside markup would corrupt it).
fn clean(raw: String, return_value: &str, trim: bool) -> String {
    if !trim {
        return raw;
    }
    if return_value == "text" {
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        raw.trim().to_string()
    }
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let html = source_html(config, input)?;

    let rules = config
        .get("extractionValues")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rules.is_empty() {
        return Err("HTML Extract: add at least one extraction (a Key + CSS Selector)".to_string());
    }
    let trim = config
        .get("trimValues")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let include = config
        .get("includeInputFields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let doc = scraper::Html::parse_document(&html);

    let mut out: Map<String, Value> = match (include, input) {
        (true, Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };

    for (i, rule) in rules.iter().enumerate() {
        let key = rule
            .get("key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("HTML Extract: extraction #{} needs a Key", i + 1))?;
        let selector_str = rule
            .get("cssSelector")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("HTML Extract '{key}': needs a CSS Selector"))?;
        let selector = scraper::Selector::parse(selector_str).map_err(|e| {
            format!("HTML Extract '{key}': invalid CSS selector '{selector_str}': {e}")
        })?;
        let return_value = rule
            .get("returnValue")
            .and_then(|v| v.as_str())
            .unwrap_or("text");
        let return_array = rule
            .get("returnArray")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let attribute = rule
            .get("attribute")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if return_value == "attribute" && attribute.is_empty() {
            return Err(format!(
                "HTML Extract '{key}': Return is set to Attribute — name the attribute (e.g. href)"
            ));
        }

        let mut values: Vec<Value> = Vec::new();
        for el in doc.select(&selector) {
            // An element without the requested attribute contributes nothing
            // (rather than a null hole), so `a[href]`-style scrapes stay clean.
            let raw = match return_value {
                "attribute" => match el.attr(attribute) {
                    Some(a) => a.to_string(),
                    None => continue,
                },
                "html" => el.inner_html(),
                _ => el.text().collect::<Vec<_>>().join(""),
            };
            values.push(Value::String(clean(raw, return_value, trim)));
            if !return_array {
                break;
            }
        }

        let result = if return_array {
            Value::Array(values)
        } else {
            values.into_iter().next().unwrap_or(Value::Null)
        };
        out.insert(key.to_string(), result);
    }

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(key: &str, selector: &str, extra: Value) -> Value {
        let mut r = json!({ "key": key, "cssSelector": selector });
        if let (Some(obj), Some(ex)) = (r.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        r
    }

    fn cfg(rules: Vec<Value>, extra: Value) -> Value {
        let mut c = json!({ "extractionValues": { "parameters": rules } });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    const PAGE: &str = r#"<html><body>
        <h1>  Hello
            World </h1>
        <ul>
            <li class="item">One</li>
            <li class="item">Two</li>
            <li class="item">Three</li>
        </ul>
        <a class="link" href="/first">First</a>
        <a class="link">no href</a>
        <a class="link" href="/second">Second</a>
        <div id="box"><b>bold</b> text</div>
    </body></html>"#;

    // Single text extraction takes the first match, whitespace collapsed.
    #[test]
    fn text_single_first_match_collapsed() {
        let c = cfg(
            vec![rule("title", "h1", json!({}))],
            json!({ "html": PAGE }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "title": "Hello World" }));
    }

    // returnArray gathers every match.
    #[test]
    fn return_array_collects_all_matches() {
        let c = cfg(
            vec![rule("items", "li.item", json!({ "returnArray": true }))],
            json!({ "html": PAGE }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "items": ["One", "Two", "Three"] }));
    }

    // Attribute mode returns the attribute; elements missing it are skipped.
    #[test]
    fn attribute_mode_skips_elements_without_it() {
        let c = cfg(
            vec![rule(
                "links",
                "a.link",
                json!({ "returnValue": "attribute", "attribute": "href", "returnArray": true }),
            )],
            json!({ "html": PAGE }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "links": ["/first", "/second"] }));
    }

    // html mode returns inner HTML with markup intact.
    #[test]
    fn html_mode_returns_inner_html() {
        let c = cfg(
            vec![rule("box", "#box", json!({ "returnValue": "html" }))],
            json!({ "html": PAGE }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "box": "<b>bold</b> text" }));
    }

    // No match → null (single) / [] (array), never an error.
    #[test]
    fn no_match_yields_null_or_empty_array() {
        let c = cfg(
            vec![
                rule("missing", ".nope", json!({})),
                rule("missing_list", ".nope", json!({ "returnArray": true })),
            ],
            json!({ "html": PAGE }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "missing": null, "missing_list": [] }));
    }

    // Several rules compose into one object.
    #[test]
    fn multiple_rules_build_one_object() {
        let c = cfg(
            vec![
                rule("title", "h1", json!({})),
                rule("first_item", "li.item", json!({})),
            ],
            json!({ "html": PAGE }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "title": "Hello World", "first_item": "One" }));
    }

    // An invalid CSS selector errors with the key and selector named.
    #[test]
    fn invalid_selector_errors() {
        let c = cfg(vec![rule("x", "li[", json!({}))], json!({ "html": PAGE }));
        let err = execute(&c, &Value::Null).unwrap_err();
        assert!(err.contains("invalid CSS selector"), "got: {err}");
    }

    // No rules at all is a config error, not a silent empty object.
    #[test]
    fn no_rules_errors() {
        let c = json!({ "html": PAGE });
        assert!(execute(&c, &Value::Null).is_err());
    }

    // Attribute mode without an attribute name is a config error.
    #[test]
    fn attribute_mode_without_name_errors() {
        let c = cfg(
            vec![rule("x", "a", json!({ "returnValue": "attribute" }))],
            json!({ "html": PAGE }),
        );
        assert!(execute(&c, &Value::Null).is_err());
    }

    // trimValues=false keeps the raw text nodes untouched.
    #[test]
    fn trim_off_keeps_raw_text() {
        let c = cfg(
            vec![rule("title", "h1", json!({}))],
            json!({ "html": "<h1>  spaced  out  </h1>", "trimValues": false }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "title": "  spaced  out  " }));
    }

    // Blank html config falls back to a string input…
    #[test]
    fn falls_back_to_string_input() {
        let c = cfg(vec![rule("title", "h1", json!({}))], json!({}));
        let out = execute(&c, &json!("<h1>From Input</h1>")).unwrap();
        assert_eq!(out, json!({ "title": "From Input" }));
    }

    // …and to the input object's `body` field (Synapse's response shape).
    #[test]
    fn falls_back_to_input_body_field() {
        let c = cfg(vec![rule("title", "h1", json!({}))], json!({}));
        let input = json!({ "status": 200, "body": "<h1>Synapse Page</h1>" });
        let out = execute(&c, &input).unwrap();
        assert_eq!(out, json!({ "title": "Synapse Page" }));
    }

    // An expression resolving to the whole Synapse response object also works.
    #[test]
    fn html_config_accepts_a_response_object() {
        let c = cfg(
            vec![rule("title", "h1", json!({}))],
            json!({ "html": { "status": 200, "body": "<h1>Object Body</h1>" } }),
        );
        let out = execute(&c, &Value::Null).unwrap();
        assert_eq!(out, json!({ "title": "Object Body" }));
    }

    // No HTML anywhere is a teaching error.
    #[test]
    fn no_html_errors() {
        let c = cfg(vec![rule("t", "h1", json!({}))], json!({}));
        let err = execute(&c, &Value::Null).unwrap_err();
        assert!(err.contains("no HTML found"), "got: {err}");
    }

    // includeInputFields merges the extractions onto the incoming item.
    #[test]
    fn include_input_fields_merges() {
        let c = cfg(
            vec![rule("title", "h1", json!({}))],
            json!({ "html": "<h1>T</h1>", "includeInputFields": true }),
        );
        let input = json!({ "url": "https://x.test" });
        let out = execute(&c, &input).unwrap();
        assert_eq!(out, json!({ "url": "https://x.test", "title": "T" }));
    }
}
