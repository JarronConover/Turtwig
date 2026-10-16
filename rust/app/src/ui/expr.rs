/*! Expressions in settings are jq (via jaq), run against the UI context object. Strings may hold
`{{ jq }}` templates; a string that is exactly one template yields the raw value, not text. */

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{data, Compiler, Ctx, Vars};
use jaq_json::Val;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

type Filter = jaq_core::Filter<data::JustLut<Val>>;

#[derive(Default)]
pub struct Exprs {
    cache: RefCell<HashMap<String, Result<Rc<Filter>, String>>>,
    pub errors: RefCell<Vec<String>>,
}

impl Exprs {
    fn compile(&self, code: &str) -> Result<Rc<Filter>, String> {
        if let Some(hit) = self.cache.borrow().get(code) {
            return hit.clone();
        }
        let defs = jaq_core::defs().chain(jaq_std::defs()).chain(jaq_json::defs());
        let funs = jaq_core::funs().chain(jaq_std::funs()).chain(jaq_json::funs());
        let loader = Loader::new(defs);
        let arena = Arena::default();
        let result = loader
            .load(&arena, File { code, path: () })
            .map_err(|e| format!("can't parse `{code}`: {e:?}"))
            .and_then(|m| Compiler::default().with_funs(funs).compile(m).map_err(|e| format!("can't compile `{code}`: {e:?}")))
            .map(Rc::new);
        self.cache.borrow_mut().insert(code.to_string(), result.clone());
        result
    }

    /** All outputs of `code` on `input`; errors are recorded (shown in the settings panel) and yield nothing. */
    pub fn run(&self, code: &str, input: &Val) -> Vec<Val> {
        let f = match self.compile(code) {
            Ok(f) => f,
            Err(e) => {
                self.note(e);
                return vec![];
            }
        };
        let ctx = Ctx::<data::JustLut<Val>>::new(&f.lut, Vars::new([]));
        let mut out = vec![];
        for v in f.id.run((ctx, input.clone())).map(jaq_core::unwrap_valr) {
            match v {
                Ok(v) => out.push(v),
                Err(e) => {
                    self.note(format!("`{code}`: {e}"));
                    break;
                }
            }
        }
        out
    }

    pub fn note(&self, e: String) {
        let mut errs = self.errors.borrow_mut();
        if !errs.contains(&e) && errs.len() < 50 {
            errs.push(e);
        }
    }

    pub fn first(&self, code: &str, input: &Val) -> Val {
        self.run(code, input).into_iter().next().unwrap_or(Val::Null)
    }

    pub fn truthy(&self, code: &str, input: &Val) -> bool {
        matches!(self.first(code, input), v if !matches!(v, Val::Null | Val::Bool(false)))
    }

    /** A setting value with its templates filled in, as JSON. `then` values are left as written. */
    pub fn resolve(&self, v: &Value, input: &Val) -> Value {
        match v {
            Value::String(s) if s.contains("{{") => self.template(s, input),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.resolve(x, input)).collect()),
            // A prompt's `then` waits for the text typed (`.input`); every other action resolves here.
            Value::Object(o) => {
                let lazy = o.get("do").and_then(Value::as_str) == Some("prompt");
                Value::Object(o.iter().map(|(k, x)| (k.clone(), if lazy && k == "then" { x.clone() } else { self.resolve(x, input) })).collect())
            }
            other => other.clone(),
        }
    }

    /** Like `resolve`, straight to a context value: a whole-string template keeps its value as computed,
    without a round trip through JSON. */
    pub fn resolve_val(&self, v: &Value, input: &Val) -> Val {
        match v {
            Value::String(s) if s.contains("{{") => {
                let t = s.trim();
                if t.starts_with("{{") && t.ends_with("}}") && t[2..].find("{{").is_none() {
                    self.first(t[2..t.len() - 2].trim(), input)
                } else {
                    to_val(&self.template(s, input))
                }
            }
            Value::Array(a) => Val::Arr(Rc::new(a.iter().map(|x| self.resolve_val(x, input)).collect())),
            Value::Object(o) => {
                let mut map = jaq_json::Map::default();
                let lazy = o.get("do").and_then(Value::as_str) == Some("prompt");
                for (k, x) in o {
                    map.insert(Val::from(k.clone()), if lazy && k == "then" { to_val(x) } else { self.resolve_val(x, input) });
                }
                Val::obj(map)
            }
            other => to_val(other),
        }
    }

    /** Actions resolve like any value; a `then` (anywhere) is left for later, when its input exists. */
    pub fn resolve_action(&self, v: &Value, input: &Val) -> Value {
        self.resolve(v, input)
    }

    fn template(&self, s: &str, input: &Val) -> Value {
        let t = s.trim();
        if t.starts_with("{{") && t.ends_with("}}") && t[2..].find("{{").is_none() {
            return to_json(&self.first(t[2..t.len() - 2].trim(), input));
        }
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            match to_json(&self.first(after[..end].trim(), input)) {
                Value::String(x) => out.push_str(&x),
                Value::Null => {}
                other => out.push_str(&other.to_string()),
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Value::String(out)
    }
}

pub fn to_val(v: &Value) -> Val {
    serde_json::from_value(v.clone()).unwrap_or(Val::Null)
}

pub fn to_json(v: &Val) -> Value {
    use jaq_json::Num;
    match v {
        Val::Null => Value::Null,
        Val::Bool(b) => Value::Bool(*b),
        Val::Num(Num::Int(i)) => Value::from(*i as i64),
        Val::Num(Num::Float(f)) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
        Val::Num(n) => serde_json::from_str(&n.to_string()).unwrap_or(Value::Null),
        Val::TStr(b) | Val::BStr(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
        Val::Arr(a) => Value::Array(a.iter().map(to_json).collect()),
        Val::Obj(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (match to_json(k) { Value::String(s) => s, other => other.to_string() }, to_json(v)))
                .collect(),
        ),
    }
}

/** The context with extra named values (loop items, component props) added at the top level.
Only the top-level map is copied; everything under it is shared. */
pub fn with(base: &Val, extra: Vec<(String, Val)>) -> Val {
    let mut map = match base {
        Val::Obj(m) => (**m).clone(),
        _ => Default::default(),
    };
    for (k, v) in extra {
        map.insert(Val::from(k), v);
    }
    Val::obj(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_prompts_then_waits_for_input() {
        let e = Exprs::default();
        let ctx = to_val(&json!({"b": {"id": "x1"}}));
        let branch = e.resolve(&json!({"do": "if", "cond": true, "then": {"do": "open", "id": "{{ .b.id }}"}}), &ctx);
        assert_eq!(branch["then"]["id"], "x1");
        let ask = e.resolve(&json!({"do": "prompt", "then": {"do": "chat.new", "branch": "{{ .input }}"}}), &ctx);
        assert_eq!(ask["then"]["branch"], "{{ .input }}");
    }

    #[test]
    fn orders_orchestrators_first_and_supports_elif() {
        let e = Exprs::default();
        let ctx = to_val(&json!({"s": {"items": [
            {"path": "a", "chats": [{"kind": "feature"}]},
            {"path": "o", "chats": [{"kind": "orchestrator"}]}
        ]}}));
        let order = "[.s.items[] | select(.chats | any(.kind == \"orchestrator\"))] + [.s.items[] | select(.chats | any(.kind == \"orchestrator\") | not)] | map(.path)";
        assert_eq!(to_json(&e.first(order, &ctx)), json!(["o", "a"]));
        assert_eq!(to_json(&e.first("if false then 1 elif true then 2 else 3 end", &ctx)), json!(2));
        assert!(e.errors.borrow().is_empty());
    }

    #[test]
    fn drop_validity_expressions_evaluate() {
        let e = Exprs::default();
        let ctx = to_val(&json!({
            "projects": [{"repo": "redo", "items": [{"path": "a"}, {"path": "b"}, {"path": "c"}]}, {"repo": "data", "items": [{"path": "d"}]}],
            "sections": [],
            "worktrees": [{"path": "a", "repo": "redo", "section": null}, {"path": "b", "repo": "redo", "section": null}, {"path": "c", "repo": "redo", "section": null}, {"path": "d", "repo": "data", "section": null}],
        }));
        let valid = ".dragged as $d | .props.w as $t | .zone as $z | (if $t.section then [.sections[] | select(.id == $t.section.id) | .items[].path] else [.projects[] | select(.repo == $t.repo) | .items[].path] end) as $list | ($list | index($t.path)) as $ti | ($list | index($d)) as $di | $d != $t.path and ($t.section != null or ([.worktrees[] | select(.path == $d)][0].repo == $t.repo)) and ($di == null or ($z == \"before\" and $di != $ti - 1) or ($z == \"after\" and $di != $ti + 1))";
        let check = |dragged: &str, target: &str, repo: &str, zone: &str| {
            let c = with(&ctx, vec![
                ("dragged".into(), to_val(&json!(dragged))),
                ("zone".into(), to_val(&json!(zone))),
                ("props".into(), to_val(&json!({"w": {"path": target, "repo": repo, "section": null}}))),
            ]);
            e.truthy(valid, &c)
        };
        assert!(check("a", "c", "redo", "before"));
        assert!(!check("a", "b", "redo", "before"), "a is already right above b");
        assert!(!check("a", "a", "redo", "after"), "onto itself");
        assert!(!check("a", "d", "data", "before"), "another project's row");
        assert!(check("c", "a", "redo", "before"));
        assert!(e.errors.borrow().is_empty(), "{:?}", e.errors.borrow());
    }
}
