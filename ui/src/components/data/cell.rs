use serde_json::Value;

pub fn cell_string(v: &Value) -> String {
    match v {
        Value::Null => "—".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

pub fn cell_class(_header: &str, v: &Value) -> &'static str {
    match v {
        Value::Null => "null-val",
        Value::Bool(true) => "bool-true",
        Value::Bool(false) => "bool-false",
        Value::Number(_) => "num-val",
        Value::String(_) | Value::Array(_) | Value::Object(_) => "str-val",
    }
}

/// Pretty-print a JSON value with span class hints for syntax highlighting.
pub fn pretty_json_html(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, 0);
    out
}

fn write_value(out: &mut String, v: &Value, depth: usize) {
    let pad = "  ".repeat(depth);
    match v {
        Value::Null => out.push_str(r#"<span class="j-null">null</span>"#),
        Value::Bool(b) => {
            out.push_str(&format!(r#"<span class="j-bool">{}</span>"#, b));
        }
        Value::Number(n) => {
            out.push_str(&format!(r#"<span class="j-num">{}</span>"#, n));
        }
        Value::String(s) => {
            let escaped = html_escape(s);
            out.push_str(&format!(r#"<span class="j-str">"{}"</span>"#, escaped));
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            out.push('\n');
            let inner_pad = "  ".repeat(depth + 1);
            for (i, item) in arr.iter().enumerate() {
                out.push_str(&inner_pad);
                write_value(out, item, depth + 1);
                if i + 1 < arr.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            out.push('\n');
            let inner_pad = "  ".repeat(depth + 1);
            let last = map.len() - 1;
            for (i, (k, val)) in map.iter().enumerate() {
                out.push_str(&inner_pad);
                out.push_str(&format!(
                    r#"<span class="j-key">"{}"</span>: "#,
                    html_escape(k)
                ));
                write_value(out, val, depth + 1);
                if i < last {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
