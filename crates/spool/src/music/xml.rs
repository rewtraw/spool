//! Subsonic's XML, from the same values its JSON is made of: a field holding a plain value is
//! an attribute, an object is an element, and a list is one element per item, named for the
//! field.

use serde_json::Value;

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn element(name: &str, v: &Value, out: &mut String) {
    out.push('<');
    out.push_str(name);
    let Value::Object(map) = v else {
        // A list of plain values, such as a song's genres by name, or text of its own.
        out.push('>');
        if let Some(s) = scalar(v) {
            escape(&s, out);
        }
        out.push_str("</");
        out.push_str(name);
        out.push('>');
        return;
    };
    let mut children = vec![];
    let mut text = None;
    for (k, v) in map {
        if k == "value" {
            text = scalar(v);
            continue;
        }
        match scalar(v) {
            Some(s) => {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                escape(&s, out);
                out.push('"');
            }
            None if v.is_null() || v.as_array().is_some_and(|a| a.is_empty()) => {}
            None => children.push((k, v)),
        }
    }
    if children.is_empty() && text.is_none() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    if let Some(t) = text {
        escape(&t, out);
    }
    for (k, v) in children {
        match v {
            Value::Array(items) => items.iter().for_each(|i| element(k, i, out)),
            Value::Null => {}
            v => element(k, v, out),
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// The whole answer, `subsonic-response` and all.
pub fn document(body: &Value) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let mut root = body.clone();
    if let Value::Object(m) = &mut root {
        m.insert("xmlns".into(), Value::String("http://subsonic.org/restapi".into()));
    }
    element("subsonic-response", &root, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fields_become_attributes_and_lists_repeat() {
        let x = document(&json!({"status": "ok", "albumList2": {"album": [{"id": "al-1", "name": "Rock & Roll"}, {"id": "al-2", "name": "B"}]}}));
        assert!(x.contains(r#"status="ok""#));
        assert!(x.contains(r#"<albumList2><album id="al-1" name="Rock &amp; Roll"/><album id="al-2" name="B"/></albumList2>"#), "{x}");
    }

    #[test]
    fn empty_lists_leave_an_empty_element() {
        let x = document(&json!({"status": "ok", "playlists": {"playlist": []}}));
        assert!(x.contains("<playlists/>"), "{x}");
    }
}
