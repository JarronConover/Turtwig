/*! Sets one value in a JSONC document's text (the settings file), leaving its comments and layout
alone everywhere else. */

use anyhow::{bail, Result};
use serde_json::Value;

struct Entry {
    key: String,
    key_at: usize,
    value: (usize, usize),
}

struct Obj {
    open: usize,
    close: usize,
    entries: Vec<Entry>,
}

struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn skip(&mut self) {
        loop {
            while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
                self.i += 1;
            }
            let rest = &self.s[self.i.min(self.s.len())..];
            if rest.starts_with(b"//") {
                while self.s.get(self.i).is_some_and(|c| *c != b'\n') {
                    self.i += 1;
                }
            } else if rest.starts_with(b"/*") {
                self.i = rest.windows(2).skip(2).position(|w| w == b"*/").map_or(self.s.len(), |j| self.i + j + 4);
            } else {
                return;
            }
        }
    }

    fn string(&mut self) -> Result<String> {
        let start = self.i;
        self.i += 1;
        while let Some(c) = self.s.get(self.i) {
            match c {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    return Ok(serde_json::from_slice(&self.s[start..self.i])?);
                }
                _ => self.i += 1,
            }
        }
        bail!("a string never ends")
    }

    fn value(&mut self) -> Result<()> {
        match self.s.get(self.i) {
            Some(b'{') => {
                self.object()?;
            }
            Some(b'[') => {
                self.i += 1;
                loop {
                    self.skip();
                    match self.s.get(self.i) {
                        Some(b']') => break self.i += 1,
                        Some(b',') => self.i += 1,
                        Some(_) => self.value()?,
                        None => bail!("a list never ends"),
                    }
                }
            }
            Some(b'"') => {
                self.string()?;
            }
            Some(_) => {
                while self.s.get(self.i).is_some_and(|c| !b",]}/ \t\r\n".contains(c)) {
                    self.i += 1;
                }
            }
            None => bail!("a value is missing"),
        }
        Ok(())
    }

    fn object(&mut self) -> Result<Obj> {
        let open = self.i;
        self.i += 1;
        let mut entries = vec![];
        loop {
            self.skip();
            match self.s.get(self.i) {
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Obj { open, close: self.i - 1, entries });
                }
                Some(b',') => self.i += 1,
                Some(b'"') => {
                    let key_at = self.i;
                    let key = self.string()?;
                    self.skip();
                    if self.s.get(self.i) != Some(&b':') {
                        bail!("no `:` after {key:?}");
                    }
                    self.i += 1;
                    self.skip();
                    let start = self.i;
                    self.value()?;
                    entries.push(Entry { key, key_at, value: (start, self.i) });
                }
                _ => bail!("unexpected text at byte {}", self.i),
            }
        }
    }
}

/** `value` as JSON on one line, with a space after `:` and `,` like hand-written settings. */
pub fn inline(value: &Value) -> String {
    match value {
        Value::Object(m) if m.is_empty() => "{}".into(),
        Value::Object(m) => format!("{{ {} }}", m.iter().map(|(k, v)| format!("{}: {}", Value::String(k.clone()), inline(v))).collect::<Vec<_>>().join(", ")),
        Value::Array(a) => format!("[{}]", a.iter().map(inline).collect::<Vec<_>>().join(", ")),
        v => v.to_string(),
    }
}

fn nest(rest: &[String], value: &Value) -> Value {
    rest.iter().rev().fold(value.clone(), |v, k| Value::Object([(k.clone(), v)].into_iter().collect()))
}

fn indent_at(text: &str, at: usize) -> &str {
    let line = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let width = text[line..].find(|c| c != ' ' && c != '\t').unwrap_or(0);
    &text[line..line + width]
}

fn insert(text: &str, obj: &Obj, key: &str, value: &str) -> String {
    let entry = format!("{}: {value}", Value::String(key.to_string()));
    let Some(last) = obj.entries.last() else {
        if text[obj.open..obj.close].contains('\n') || obj.open == 0 {
            let outer = indent_at(text, obj.open);
            return format!("{}\n{outer}  {entry}\n{outer}{}", &text[..=obj.open], &text[obj.close..]);
        }
        return format!("{} {entry} {}", &text[..=obj.open], &text[obj.close..]);
    };
    let end = last.value.1;
    if !text[obj.open..end].contains('\n') {
        return format!("{}, {entry}{}", &text[..end], &text[end..]);
    }
    // After the last entry's comma and any comment on its line, so the comment stays with it.
    let b = text.as_bytes();
    let mut at = end;
    while matches!(b.get(at), Some(b' ' | b'\t')) {
        at += 1;
    }
    let comma = b.get(at) == Some(&b',');
    if comma {
        at += 1;
    }
    let mut eol = at;
    while matches!(b.get(eol), Some(b' ' | b'\t')) {
        eol += 1;
    }
    if text[eol..].starts_with("//") {
        at = text[eol..].find('\n').map_or(text.len(), |i| eol + i);
    }
    let indent = indent_at(text, last.key_at);
    let line = format!("\n{indent}{entry}");
    if comma {
        format!("{}{line}{}", &text[..at], &text[at..])
    } else {
        format!("{},{}{line}{}", &text[..end], &text[end..at], &text[at..])
    }
}

/** `text` with the value at `path` (keys from the top) set to `value`: replaced where it is, or added to
the deepest object on the path that exists. */
pub fn set(text: &str, path: &[String], value: &Value) -> Result<String> {
    if path.is_empty() {
        bail!("no key to set");
    }
    let s = text.as_bytes();
    let mut scan = Scan { s, i: 0 };
    scan.skip();
    if s.get(scan.i) != Some(&b'{') {
        bail!("the file isn't a JSON object");
    }
    let mut obj = scan.object()?;
    for (n, key) in path.iter().enumerate() {
        let rest = &path[n + 1..];
        let Some(e) = obj.entries.iter().rev().find(|e| &e.key == key) else {
            return Ok(insert(text, &obj, key, &inline(&nest(rest, value))));
        };
        if rest.is_empty() || s[e.value.0] != b'{' {
            let (a, b) = e.value;
            return Ok(format!("{}{}{}", &text[..a], inline(&nest(rest, value)), &text[b..]));
        }
        obj = Scan { s, i: e.value.0 }.object()?;
    }
    unreachable!("the loop returns at the last key")
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    fn set(text: &str, path: &str, value: Value) -> String {
        let path: Vec<String> = path.split('.').map(str::to_string).collect();
        let out = super::set(text, &path, &value).unwrap();
        let parsed: Value = serde_json_lenient::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        assert_eq!(parsed.pointer(&format!("/{}", path.join("/"))), Some(&value), "{out}");
        out
    }

    const FILE: &str = "// mine\n{\n  \"$schema\": \"./settings.schema.json\",\n  // calm\n  \"motion\": { \"move\": { \"duration\": 380 } },\n\n  \"daemon\": {\n    \"move_leftovers\": \"delete\" // tidy\n  },\n  \"patch\": [\n    { \"id\": \"x\", \"remove\": true }\n  ]\n}\n";

    #[test]
    fn replaces_a_value_in_place_and_keeps_every_comment() {
        let out = set(FILE, "daemon.move_leftovers", json!("hide"));
        assert_eq!(out, FILE.replace("\"delete\"", "\"hide\""));
        let out = set(FILE, "motion.move.duration", json!(200));
        assert!(out.contains("{ \"move\": { \"duration\": 200 } }"));
    }

    #[test]
    fn adds_keys_where_they_belong() {
        let out = set(FILE, "daemon.state_repo_url", json!("git@x:y.git"));
        assert!(out.contains("\"move_leftovers\": \"delete\", // tidy\n    \"state_repo_url\": \"git@x:y.git\"\n  },"), "{out}");
        let out = set(FILE, "motion.enter", json!({"duration": 1}));
        assert!(out.contains("{ \"move\": { \"duration\": 380 }, \"enter\": { \"duration\": 1 } }"), "{out}");
        let out = set(FILE, "connection.ssh_host", json!("coder.me"));
        assert!(out.contains("  ],\n  \"connection\": { \"ssh_host\": \"coder.me\" }\n}"), "{out}");
        assert!(out.starts_with("// mine\n{\n  \"$schema\""));
    }

    #[test]
    fn replaces_a_non_object_on_the_way_and_fills_an_empty_file() {
        let out = set("{ \"app\": 3 }", "app.default_host", json!("box"));
        assert_eq!(out, "{ \"app\": { \"default_host\": \"box\" } }");
        let out = set("{\n}\n", "a.b", json!([1, "x"]));
        assert_eq!(out, "{\n  \"a\": { \"b\": [1, \"x\"] }\n}\n");
        set("{}", "a", json!(null));
    }

    #[test]
    fn refuses_text_it_cannot_read() {
        assert!(super::set("[1]", &["a".into()], &json!(1)).is_err());
        assert!(super::set("{ \"a\": \"open", &["a".into()], &json!(1)).is_err());
    }
}
