//! Markdown — Task 2.7 (half of "XML / Markdown"). Markdown↔HTML conversion
//! so a message body can move between the two without a JavaScript node —
//! render a notice as HTML for an email/Slack card, or turn a scraped/received
//! HTML fragment into clean Markdown for a Telegram/Discord message.
//!
//! `toHtml` uses `pulldown-cmark` (CommonMark + optional GFM tables/
//! strikethrough/tasklists/footnotes) — a correct, well-tested renderer, so
//! that direction is full-fidelity. `toMarkdown` is the harder direction (HTML
//! is unconstrained); rather than pull in another crate, it walks the DOM with
//! `scraper` (already in tree for 2.3) and maps the common tags — headings,
//! paragraphs, emphasis, links, images, lists, code/pre, blockquote, hr, br.
//! Unknown tags pass through as transparent containers (their text still comes
//! out); `script`/`style`/`head` are dropped. This is a pragmatic converter,
//! not a full-fidelity one — good enough for message bodies and scraped
//! content, not a spec-complete HTML-to-Markdown engine.

use crate::tools::workflow::cfg_str;
use scraper::Node;
use serde_json::{Map, Value};

/// Wrap a computed result under `outputField` (defaulting to `default_field`),
/// optionally merged onto the incoming item — identical convention to
/// `dateTime`/`crypto`/`xml`.
fn wrap(config: &Value, input: &Value, default_field: &str, result: Value) -> Value {
    let field = cfg_str(config, "outputField")
        .unwrap_or(default_field)
        .to_string();
    let include = config
        .get("includeInputFields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut out: Map<String, Value> = match (include, input) {
        (true, Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    out.insert(field, result);
    Value::Object(out)
}

/// Pull a string out of a value, probing the given fallback keys on an object
/// (in order) or the first element of an array. Shared shape between the
/// `markdown` and `html` source fields — mirrors `htmlExtract`'s fallback.
fn text_from_value(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(m) => keys
            .iter()
            .find_map(|k| m.get(*k).and_then(|vv| text_from_value(vv, keys))),
        Value::Array(a) => a.first().and_then(|vv| text_from_value(vv, keys)),
        _ => None,
    }
}

const BODY_KEYS: [&str; 4] = ["body", "html", "data", "text"];

fn source_markdown(config: &Value, input: &Value) -> Result<String, String> {
    let keys = ["body", "markdown", "data", "text"];
    config
        .get("markdown")
        .and_then(|v| text_from_value(v, &keys))
        .or_else(|| text_from_value(input, &keys))
        .ok_or_else(|| {
            "Markdown: no Markdown found — set the Markdown field or feed a node whose output \
             is/contains the text"
                .to_string()
        })
}

fn source_html(config: &Value, input: &Value) -> Result<String, String> {
    config
        .get("html")
        .and_then(|v| text_from_value(v, &BODY_KEYS))
        .or_else(|| text_from_value(input, &BODY_KEYS))
        .ok_or_else(|| {
            "Markdown: no HTML found — set the HTML field (e.g. {{ $node[\"Synapse\"].body }}) \
             or feed a node whose output is/contains the page text"
                .to_string()
        })
}

// ---------------- toHtml ----------------

fn markdown_to_html(config: &Value, input: &Value) -> Result<Value, String> {
    use pulldown_cmark::{html, Options, Parser};

    let md = source_markdown(config, input)?;
    let gfm = config.get("gfm").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut opts = Options::empty();
    if gfm {
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TASKLISTS);
        opts.insert(Options::ENABLE_FOOTNOTES);
    }
    let parser = Parser::new_ext(&md, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    Ok(wrap(config, input, "html", Value::String(out)))
}

// ---------------- toMarkdown ----------------

/// Collapse a text node's internal whitespace to single spaces (HTML
/// formatting/indentation is insignificant), preserving at most one leading
/// and/or trailing space so inline siblings ("Hello <b>World</b>") don't run
/// together.
fn collapse_ws(s: &str) -> String {
    let leading = s.starts_with(|c: char| c.is_whitespace());
    let trailing = s.ends_with(|c: char| c.is_whitespace());
    let core = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if core.is_empty() {
        return String::new();
    }
    let mut out = core;
    if leading {
        out.insert(0, ' ');
    }
    if trailing {
        out.push(' ');
    }
    out
}

/// Raw text content of a subtree, whitespace untouched — used for code/pre
/// where indentation is meaningful, unlike prose.
fn raw_text(node: ego_tree::NodeRef<Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => out.push_str(&t.text),
        Node::Element(_) => {
            for c in node.children() {
                raw_text(c, out);
            }
        }
        _ => {}
    }
}

fn render_children(node: ego_tree::NodeRef<Node>, out: &mut String) {
    for child in node.children() {
        render(child, out);
    }
}

fn trim_trailing_spaces(s: &mut String) {
    while s.ends_with(' ') || s.ends_with('\t') {
        s.pop();
    }
}

fn render(node: ego_tree::NodeRef<Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => out.push_str(&collapse_ws(&t.text)),
        Node::Element(el) => {
            let tag = el.name();
            match tag {
                "script" | "style" | "head" => {} // no text from these
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag[1..2].parse::<usize>().unwrap_or(1);
                    out.push_str(&"#".repeat(level));
                    out.push(' ');
                    render_children(node, out);
                    trim_trailing_spaces(out);
                    out.push_str("\n\n");
                }
                "p" => {
                    render_children(node, out);
                    trim_trailing_spaces(out);
                    out.push_str("\n\n");
                }
                "br" => out.push_str("  \n"),
                "hr" => out.push_str("---\n\n"),
                "strong" | "b" => {
                    out.push_str("**");
                    render_children(node, out);
                    out.push_str("**");
                }
                "em" | "i" => {
                    out.push('_');
                    render_children(node, out);
                    out.push('_');
                }
                "code" => {
                    out.push('`');
                    raw_text(node, out);
                    out.push('`');
                }
                "pre" => {
                    let mut inner = String::new();
                    raw_text(node, &mut inner);
                    out.push_str("```\n");
                    out.push_str(inner.trim_matches(|c| c == '\n' || c == '\r'));
                    out.push_str("\n```\n\n");
                }
                "a" => {
                    let href = el.attr("href").unwrap_or("");
                    out.push('[');
                    render_children(node, out);
                    out.push(']');
                    out.push('(');
                    out.push_str(href);
                    out.push(')');
                }
                "img" => {
                    let alt = el.attr("alt").unwrap_or("");
                    let src = el.attr("src").unwrap_or("");
                    out.push_str("![");
                    out.push_str(alt);
                    out.push_str("](");
                    out.push_str(src);
                    out.push(')');
                }
                "ul" => {
                    for child in node.children() {
                        if let Node::Element(e) = child.value() {
                            if e.name() == "li" {
                                out.push_str("- ");
                                render_children(child, out);
                                trim_trailing_spaces(out);
                                out.push('\n');
                            }
                        }
                    }
                    out.push('\n');
                }
                "ol" => {
                    let mut i = 1u32;
                    for child in node.children() {
                        if let Node::Element(e) = child.value() {
                            if e.name() == "li" {
                                out.push_str(&format!("{i}. "));
                                render_children(child, out);
                                trim_trailing_spaces(out);
                                out.push('\n');
                                i += 1;
                            }
                        }
                    }
                    out.push('\n');
                }
                "blockquote" => {
                    let mut inner = String::new();
                    render_children(node, &mut inner);
                    for line in inner.trim().lines() {
                        out.push_str("> ");
                        out.push_str(line);
                        out.push('\n');
                    }
                    out.push('\n');
                }
                _ => render_children(node, out), // transparent container (div, span, body, html, …)
            }
        }
        _ => {} // comments/doctype/PIs ignored
    }
}

/// Collapse 3+ consecutive blank lines left over from nested block separators
/// down to one, and trim the overall edges.
fn normalize_markdown(s: &str) -> String {
    let mut out = s.trim().to_string();
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out
}

fn html_to_markdown(config: &Value, input: &Value) -> Result<Value, String> {
    let html = source_html(config, input)?;
    let doc = scraper::Html::parse_fragment(&html);
    let mut out = String::new();
    render_children(doc.tree.root(), &mut out);
    Ok(wrap(
        config,
        input,
        "markdown",
        Value::String(normalize_markdown(&out)),
    ))
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("toHtml");
    match operation {
        "toHtml" => markdown_to_html(config, input),
        "toMarkdown" => html_to_markdown(config, input),
        other => Err(format!("Unknown Markdown operation: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(op: &str, extra: Value) -> Value {
        let mut c = json!({ "operation": op });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // ---- toHtml ----

    #[test]
    fn basic_markdown_renders() {
        let out = execute(
            &cfg(
                "toHtml",
                json!({ "markdown": "# Hi\n\nSome **bold** text." }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out["html"],
            json!("<h1>Hi</h1>\n<p>Some <strong>bold</strong> text.</p>\n")
        );
    }

    #[test]
    fn gfm_table_and_strikethrough() {
        let md = "~~gone~~\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let out = execute(&cfg("toHtml", json!({ "markdown": md })), &Value::Null).unwrap();
        let html = out["html"].as_str().unwrap();
        assert!(html.contains("<del>gone</del>"), "got: {html}");
        assert!(html.contains("<table>"), "got: {html}");
    }

    #[test]
    fn gfm_off_disables_tables() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let out = execute(
            &cfg("toHtml", json!({ "markdown": md, "gfm": false })),
            &Value::Null,
        )
        .unwrap();
        assert!(!out["html"].as_str().unwrap().contains("<table>"));
    }

    #[test]
    fn falls_back_to_string_input() {
        let out = execute(&cfg("toHtml", json!({})), &json!("hi")).unwrap();
        assert_eq!(out["html"], json!("<p>hi</p>\n"));
    }

    #[test]
    fn no_markdown_errors() {
        let err = execute(&cfg("toHtml", json!({})), &Value::Null).unwrap_err();
        assert!(err.contains("no Markdown found"), "got: {err}");
    }

    // ---- toMarkdown ----

    #[test]
    fn headings_and_paragraphs() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<h1>Title</h1><p>Body text.</p>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["markdown"], json!("# Title\n\nBody text."));
    }

    #[test]
    fn bold_italic_and_links() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": r#"<p><b>bold</b> and <i>italic</i> and <a href="/x">link</a></p>"# }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out["markdown"],
            json!("**bold** and _italic_ and [link](/x)")
        );
    }

    #[test]
    fn unordered_and_ordered_lists() {
        let ul = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<ul><li>One</li><li>Two</li></ul>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(ul["markdown"], json!("- One\n- Two"));
        let ol = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<ol><li>One</li><li>Two</li></ol>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(ol["markdown"], json!("1. One\n2. Two"));
    }

    #[test]
    fn inline_and_fenced_code_preserve_whitespace() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<p>Run <code>a  b</code> now.</p><pre><code>line1\n  line2</code></pre>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out["markdown"],
            json!("Run `a  b` now.\n\n```\nline1\n  line2\n```")
        );
    }

    #[test]
    fn blockquote_and_hr() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<blockquote>Wise words.</blockquote><hr>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["markdown"], json!("> Wise words.\n\n---"));
    }

    #[test]
    fn image_tag() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": r#"<img src="/x.png" alt="pic">"# }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["markdown"], json!("![pic](/x.png)"));
    }

    #[test]
    fn unknown_tags_pass_through_as_containers() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<section><p>Inside</p></section>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["markdown"], json!("Inside"));
    }

    #[test]
    fn script_and_style_are_dropped() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<p>Text</p><script>evil()</script><style>.a{}</style>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["markdown"], json!("Text"));
    }

    #[test]
    fn formatting_whitespace_between_blocks_collapses() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<div>\n  <p>A</p>\n  <p>B</p>\n</div>" }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["markdown"], json!("A\n\nB"));
    }

    #[test]
    fn no_html_errors() {
        let err = execute(&cfg("toMarkdown", json!({})), &Value::Null).unwrap_err();
        assert!(err.contains("no HTML found"), "got: {err}");
    }

    #[test]
    fn output_field_and_include_input_fields() {
        let out = execute(
            &cfg(
                "toMarkdown",
                json!({ "html": "<p>Hi</p>", "outputField": "md", "includeInputFields": true }),
            ),
            &json!({ "id": 3 }),
        )
        .unwrap();
        assert_eq!(out["id"], json!(3));
        assert_eq!(out["md"], json!("Hi"));
    }
}
