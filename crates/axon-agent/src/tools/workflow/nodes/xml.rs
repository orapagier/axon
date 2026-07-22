//! XML — Task 2.7 (half of "XML / Markdown"). XML↔JSON conversion using the
//! fast-xml-parser convention: attributes become `@_name` keys, text content
//! becomes a `#text` key (only when mixed with attrs/children — a pure text
//! leaf is just a plain string), and repeated sibling tags become an array.
//! This convention round-trips (an `xmlToJson` result feeds straight back into
//! `jsonToXml`) and is familiar to anyone who has used a modern XML/JSON
//! bridge. Pure Rust via `quick-xml`'s event reader/writer — no TLS/HTTP stack
//! of its own, per dependency policy.
//!
//! Two operations:
//!   - `xmlToJson` — parse a document into `{ <rootTag>: <value> }`. Output
//!     merges onto the incoming item like `htmlExtract` (`includeInputFields`);
//!     the root key wins on a field-name conflict.
//!   - `jsonToXml` — serialize JSON back to an XML string. A single-key
//!     object's key becomes the root tag (the round-trip case); anything else
//!     wraps under `rootName` (default `root`). Output lands under
//!     `outputField`, same convention as `dateTime`/`crypto`.

use crate::tools::workflow::{cfg_str, val_to_string};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use serde_json::{json, Map, Value};
use std::io::Cursor;

/// Wrap a computed result under `outputField` (defaulting to `default_field`),
/// optionally merged onto the incoming item — identical convention to
/// `dateTime`/`crypto`.
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

// ---------------- xmlToJson ----------------

/// Pull an XML string out of a value: a non-blank string is taken as-is; an
/// object is probed at the conventional body-carrying fields (`body` first —
/// Synapse's response shape); an array defers to its first element. Mirrors
/// `htmlExtract`'s `html_from_value`.
fn xml_from_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(m) => ["body", "xml", "data", "text"]
            .iter()
            .find_map(|k| m.get(*k).and_then(xml_from_value)),
        Value::Array(a) => a.first().and_then(xml_from_value),
        _ => None,
    }
}

fn source_xml(config: &Value, input: &Value) -> Result<String, String> {
    config
        .get("xml")
        .and_then(xml_from_value)
        .or_else(|| xml_from_value(input))
        .ok_or_else(|| {
            "XML: no XML found — set the XML field (e.g. {{ $node[\"Synapse\"].body }}) \
             or feed a node whose output is/contains the document"
                .to_string()
        })
}

fn tag_name(name_bytes: &[u8]) -> Result<String, String> {
    std::str::from_utf8(name_bytes)
        .map(str::to_string)
        .map_err(|e| format!("XML: invalid UTF-8 tag name: {e}"))
}

fn read_attrs(e: &BytesStart) -> Result<Map<String, Value>, String> {
    let mut m = Map::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|e| format!("XML: attribute parse error: {e}"))?;
        let key = tag_name(attr.key.local_name().as_ref())?;
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| format!("XML: attribute unescape error: {e}"))?
            .into_owned();
        m.insert(key, Value::String(value));
    }
    Ok(m)
}

/// Build one element's JSON value from its attributes, accumulated text, and
/// (name, value) child pairs in encounter order. A leaf with no attrs/children
/// collapses to a plain string (empty string for `<a/>` / `<a></a>`); anything
/// with attrs or children becomes an object (`@_`-prefixed attrs, `#text` for
/// non-whitespace text, repeated child tags grouped into an array).
fn build_value(attrs: Map<String, Value>, text: String, children: Vec<(String, Value)>) -> Value {
    let trimmed = text.trim();
    if attrs.is_empty() && children.is_empty() {
        return Value::String(trimmed.to_string());
    }
    let mut grouped: Vec<(String, Vec<Value>)> = Vec::new();
    for (name, val) in children {
        match grouped.iter_mut().find(|(n, _)| *n == name) {
            Some((_, vals)) => vals.push(val),
            None => grouped.push((name, vec![val])),
        }
    }
    let mut out = Map::new();
    for (k, v) in attrs {
        out.insert(format!("@_{k}"), v);
    }
    if !trimmed.is_empty() {
        out.insert("#text".to_string(), Value::String(trimmed.to_string()));
    }
    for (name, mut vals) in grouped {
        let v = if vals.len() == 1 {
            vals.pop().expect("checked len == 1")
        } else {
            Value::Array(vals)
        };
        out.insert(name, v);
    }
    Value::Object(out)
}

/// Consume events until this element's matching End (quick-xml enforces
/// well-formed nesting, so a child Start's own recursive call consumes exactly
/// up to its own End before returning here).
fn parse_element(reader: &mut Reader<&[u8]>, attrs: Map<String, Value>) -> Result<Value, String> {
    let mut text = String::new();
    let mut children: Vec<(String, Value)> = Vec::new();
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("XML: parse error: {e}"))?
        {
            Event::Start(e) => {
                let name = tag_name(e.name().local_name().as_ref())?;
                let child_attrs = read_attrs(&e)?;
                let value = parse_element(reader, child_attrs)?;
                children.push((name, value));
            }
            Event::Empty(e) => {
                let name = tag_name(e.name().local_name().as_ref())?;
                let child_attrs = read_attrs(&e)?;
                children.push((name, build_value(child_attrs, String::new(), Vec::new())));
            }
            Event::Text(t) => {
                // Entity/char refs (`&amp;`, `&#38;`) now arrive as separate
                // `GeneralRef` events (quick-xml 0.41+), so Text content here
                // never contains an escape sequence — no unescape needed.
                let decoded = t.decode().map_err(|e| format!("XML: parse error: {e}"))?;
                text.push_str(&decoded);
            }
            Event::GeneralRef(r) => {
                if let Some(ch) = r
                    .resolve_char_ref()
                    .map_err(|e| format!("XML: parse error: {e}"))?
                {
                    text.push(ch);
                } else {
                    let name = r.decode().map_err(|e| format!("XML: parse error: {e}"))?;
                    let resolved =
                        quick_xml::escape::resolve_predefined_entity(&name).ok_or_else(|| {
                            format!("XML: parse error: unrecognized entity reference '&{name};'")
                        })?;
                    text.push_str(resolved);
                }
            }
            Event::CData(c) => {
                text.push_str(&String::from_utf8_lossy(&c.into_inner()));
            }
            Event::End(_) => break,
            Event::Eof => return Err("XML: unexpected end of document (unclosed tag)".to_string()),
            _ => {} // Comment, PI — ignored inside content
        }
    }
    Ok(build_value(attrs, text, children))
}

/// Parse an XML document into `{ <rootTag>: value }` — the reusable core of
/// `xmlToJson`. Also called directly by Extract from File's `xml` operation,
/// which reads the document off disk/base64 rather than a config expression.
pub(crate) fn parse_document(xml_str: &str) -> Result<Value, String> {
    let mut reader = Reader::from_str(xml_str);
    // trim_text(true) trims each Text *event* independently, which — since
    // entity refs now split one logical text run into multiple Text events
    // (quick-xml 0.41+) — eats the space on either side of an entity
    // ("Tom &amp; Jerry" -> "Tom" + "&" + "Jerry", losing both spaces).
    // Leading/trailing whitespace of the whole element is still stripped by
    // `build_value`'s final `text.trim()`, so nothing is lost by leaving this off.

    loop {
        match reader
            .read_event()
            .map_err(|e| format!("XML: parse error: {e}"))?
        {
            Event::Start(e) => {
                let name = tag_name(e.name().local_name().as_ref())?;
                let attrs = read_attrs(&e)?;
                let value = parse_element(&mut reader, attrs)?;
                return Ok(json!({ name: value }));
            }
            Event::Empty(e) => {
                let name = tag_name(e.name().local_name().as_ref())?;
                let attrs = read_attrs(&e)?;
                return Ok(json!({ name: build_value(attrs, String::new(), Vec::new()) }));
            }
            Event::Eof => return Err("XML: no root element found (empty document)".to_string()),
            _ => continue, // Decl, Comment, PI, DocType before the root
        }
    }
}

fn xml_to_json(config: &Value, input: &Value) -> Result<Value, String> {
    let xml_str = source_xml(config, input)?;
    let parsed = parse_document(&xml_str)?;

    let include = config
        .get("includeInputFields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut out: Map<String, Value> = match (include, input) {
        (true, Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    if let Value::Object(m) = parsed {
        out.extend(m);
    }
    Ok(Value::Object(out))
}

// ---------------- jsonToXml ----------------

fn source_value<'a>(config: &'a Value, input: &'a Value) -> &'a Value {
    match config.get("json") {
        None | Some(Value::Null) => input,
        Some(Value::String(s)) if s.trim().is_empty() => input,
        Some(v) => v,
    }
}

/// The root (tag, value) pair: a single-key object's key/value is used as-is
/// (the round-trip case, symmetric with `xml_to_json`'s output shape);
/// anything else — multiple keys, a bare array/scalar — wraps under
/// `default_root`.
fn root_pair(data: &Value, default_root: &str) -> (String, Value) {
    if let Value::Object(m) = data {
        if m.len() == 1 {
            let (k, v) = m.iter().next().expect("checked len == 1");
            return (k.clone(), v.clone());
        }
    }
    (default_root.to_string(), data.clone())
}

fn xml_write_err<E: std::fmt::Display>(e: E) -> String {
    format!("XML: write error: {e}")
}

fn write_text(writer: &mut Writer<Cursor<Vec<u8>>>, s: &str) -> Result<(), String> {
    let escaped = quick_xml::escape::escape(s);
    writer
        .write_event(Event::Text(BytesText::from_escaped(escaped)))
        .map_err(xml_write_err)
}

/// Serialize one (tag, value) pair. An array value repeats the tag once per
/// element (how a grouped child list from `xmlToJson` round-trips); an object
/// splits into `@_`-attributes, an optional `#text`, and child elements
/// (recursing, so nested arrays/objects compose); a scalar becomes the
/// element's text content; `Null`/empty string self-closes.
fn serialize_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    tag: &str,
    value: &Value,
) -> Result<(), String> {
    if let Value::Array(items) = value {
        for item in items {
            serialize_element(writer, tag, item)?;
        }
        return Ok(());
    }
    match value {
        Value::Null => {
            writer
                .write_event(Event::Empty(BytesStart::new(tag)))
                .map_err(xml_write_err)?;
        }
        Value::Object(m) => {
            let mut start = BytesStart::new(tag);
            let mut text: Option<&Value> = None;
            let mut children: Vec<(&String, &Value)> = Vec::new();
            for (k, v) in m {
                if let Some(attr_name) = k.strip_prefix("@_") {
                    start.push_attribute((attr_name, val_to_string(v).as_str()));
                } else if k == "#text" {
                    text = Some(v);
                } else {
                    children.push((k, v));
                }
            }
            if children.is_empty() && text.is_none() {
                writer
                    .write_event(Event::Empty(start))
                    .map_err(xml_write_err)?;
            } else {
                writer
                    .write_event(Event::Start(start))
                    .map_err(xml_write_err)?;
                if let Some(t) = text {
                    write_text(writer, &val_to_string(t))?;
                }
                for (k, v) in children {
                    serialize_element(writer, k, v)?;
                }
                writer
                    .write_event(Event::End(BytesEnd::new(tag)))
                    .map_err(xml_write_err)?;
            }
        }
        scalar => {
            let s = val_to_string(scalar);
            if s.is_empty() {
                writer
                    .write_event(Event::Empty(BytesStart::new(tag)))
                    .map_err(xml_write_err)?;
            } else {
                writer
                    .write_event(Event::Start(BytesStart::new(tag)))
                    .map_err(xml_write_err)?;
                write_text(writer, &s)?;
                writer
                    .write_event(Event::End(BytesEnd::new(tag)))
                    .map_err(xml_write_err)?;
            }
        }
    }
    Ok(())
}

fn json_to_xml(config: &Value, input: &Value) -> Result<Value, String> {
    let data = source_value(config, input);
    if data.is_null() {
        return Err(
            "XML: nothing to convert — fill the JSON field (e.g. {{ $node[\"XML\"] }}) \
             or feed a node output"
                .to_string(),
        );
    }
    let default_root = cfg_str(config, "rootName").unwrap_or("root");
    let (root_tag, root_value) = root_pair(data, default_root);
    // A bare array/scalar at the root can't be a single XML root element on
    // its own — wrap it as the root's `item` child(ren) so the output is
    // always well-formed.
    let root_value = match root_value {
        Value::Array(_) => json!({ "item": root_value }),
        other => other,
    };

    let pretty = config
        .get("pretty")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mut writer = if pretty {
        Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2)
    } else {
        Writer::new(Cursor::new(Vec::new()))
    };
    let declaration = config
        .get("declaration")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if declaration {
        writer
            .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .map_err(xml_write_err)?;
    }
    serialize_element(&mut writer, &root_tag, &root_value)?;

    let bytes = writer.into_inner().into_inner();
    let xml_string =
        String::from_utf8(bytes).map_err(|e| format!("XML: internal UTF-8 error: {e}"))?;
    Ok(wrap(config, input, "xml", Value::String(xml_string)))
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("xmlToJson");
    match operation {
        "xmlToJson" => xml_to_json(config, input),
        "jsonToXml" => json_to_xml(config, input),
        other => Err(format!("Unknown XML operation: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(op: &str, extra: Value) -> Value {
        let mut c = json!({ "operation": op });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // ---- xmlToJson ----

    #[test]
    fn parses_nested_elements() {
        let xml = "<note><to>Tove</to><from>Jani</from></note>";
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "note": { "from": "Jani", "to": "Tove" } }));
    }

    #[test]
    fn attributes_become_at_underscore_keys() {
        let xml = r#"<user id="42" role="admin">Ada</user>"#;
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(
            out,
            json!({ "user": { "@_id": "42", "@_role": "admin", "#text": "Ada" } })
        );
    }

    #[test]
    fn repeated_siblings_become_an_array() {
        let xml = "<items><item>A</item><item>B</item><item>C</item></items>";
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "items": { "item": ["A", "B", "C"] } }));
    }

    #[test]
    fn text_only_leaf_is_a_plain_string() {
        let xml = "<root><name>Ada</name></root>";
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "root": { "name": "Ada" } }));
    }

    #[test]
    fn self_closed_empty_leaf_is_empty_string() {
        let xml = "<root><flag/><other></other></root>";
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "root": { "flag": "", "other": "" } }));
    }

    #[test]
    fn attrs_only_self_closed_becomes_object() {
        let xml = r#"<root><img src="x.png"/></root>"#;
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "root": { "img": { "@_src": "x.png" } } }));
    }

    #[test]
    fn mixed_content_keeps_text_and_children() {
        let xml = "<p>Hello <b>World</b></p>";
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "p": { "#text": "Hello", "b": "World" } }));
    }

    #[test]
    fn entities_and_cdata_decode() {
        let xml = "<root><a>Tom &amp; Jerry</a><b><![CDATA[<raw>]]></b></root>";
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "root": { "a": "Tom & Jerry", "b": "<raw>" } }));
    }

    #[test]
    fn ignores_declaration_and_comments() {
        let xml = r#"<?xml version="1.0"?><!-- hi --><root>ok</root>"#;
        let out = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        assert_eq!(out, json!({ "root": "ok" }));
    }

    #[test]
    fn falls_back_to_string_input() {
        let out = execute(&cfg("xmlToJson", json!({})), &json!("<a>hi</a>")).unwrap();
        assert_eq!(out, json!({ "a": "hi" }));
    }

    #[test]
    fn include_input_fields_merges_root_key() {
        let c = cfg(
            "xmlToJson",
            json!({ "xml": "<name>Ada</name>", "includeInputFields": true }),
        );
        let input = json!({ "url": "https://x.test" });
        let out = execute(&c, &input).unwrap();
        assert_eq!(out, json!({ "url": "https://x.test", "name": "Ada" }));
    }

    #[test]
    fn no_xml_errors() {
        let err = execute(&cfg("xmlToJson", json!({})), &Value::Null).unwrap_err();
        assert!(err.contains("no XML found"), "got: {err}");
    }

    #[test]
    fn malformed_xml_errors_cleanly() {
        let err = execute(
            &cfg("xmlToJson", json!({ "xml": "<a><b></a>" })),
            &Value::Null,
        )
        .unwrap_err();
        assert!(err.contains("XML"), "got: {err}");
    }

    // ---- jsonToXml ----

    #[test]
    fn round_trips_xml_to_json_output() {
        let xml = r#"<user id="1">Ada</user>"#;
        let parsed = execute(&cfg("xmlToJson", json!({ "xml": xml })), &Value::Null).unwrap();
        let back = execute(
            &cfg(
                "jsonToXml",
                json!({ "json": parsed, "declaration": false, "pretty": false }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(back["xml"], json!(r#"<user id="1">Ada</user>"#));
    }

    #[test]
    fn multiple_top_level_keys_wrap_under_root_name() {
        let out = execute(
            &cfg(
                "jsonToXml",
                json!({ "json": { "a": 1, "b": 2 }, "declaration": false, "pretty": false }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["xml"], json!("<root><a>1</a><b>2</b></root>"));
    }

    #[test]
    fn custom_root_name_used_for_non_object_data() {
        let out = execute(
            &cfg(
                "jsonToXml",
                json!({ "json": "hi", "rootName": "message", "declaration": false, "pretty": false }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out["xml"], json!("<message>hi</message>"));
    }

    #[test]
    fn arrays_serialize_as_repeated_elements() {
        let out = execute(
            &cfg(
                "jsonToXml",
                json!({
                    "json": { "items": { "item": ["A", "B"] } },
                    "declaration": false,
                    "pretty": false,
                }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out["xml"],
            json!("<items><item>A</item><item>B</item></items>")
        );
    }

    #[test]
    fn declaration_toggle() {
        let with_decl = execute(
            &cfg(
                "jsonToXml",
                json!({ "json": { "a": "b" }, "pretty": false }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert!(with_decl["xml"]
            .as_str()
            .unwrap()
            .starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        let without = execute(
            &cfg(
                "jsonToXml",
                json!({ "json": { "a": "b" }, "declaration": false, "pretty": false }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert!(!without["xml"].as_str().unwrap().starts_with("<?xml"));
    }

    #[test]
    fn output_field_and_include_input_fields() {
        let c = cfg(
            "jsonToXml",
            json!({
                "json": { "a": "b" },
                "outputField": "doc",
                "includeInputFields": true,
                "declaration": false,
                "pretty": false,
            }),
        );
        let out = execute(&c, &json!({ "id": 7 })).unwrap();
        assert_eq!(out["id"], json!(7));
        assert_eq!(out["doc"], json!("<a>b</a>"));
    }

    #[test]
    fn attribute_values_are_escaped() {
        let out = execute(
            &cfg(
                "jsonToXml",
                json!({
                    "json": { "a": { "@_title": "Tom & \"Jerry\"" } },
                    "declaration": false,
                    "pretty": false,
                }),
            ),
            &Value::Null,
        )
        .unwrap();
        // Round-trip through xmlToJson proves the escaping is well-formed and
        // decodes back to the original value.
        let back = execute(
            &cfg("xmlToJson", json!({ "xml": out["xml"].as_str().unwrap() })),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(back["a"]["@_title"], json!("Tom & \"Jerry\""));
    }

    #[test]
    fn missing_data_errors() {
        let err = execute(&cfg("jsonToXml", json!({})), &Value::Null).unwrap_err();
        assert!(err.contains("nothing to convert"), "got: {err}");
    }
}
