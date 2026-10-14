/*! `patch`: small edits to nodes by `id`, applied after merging, so a change survives updates to the
default layout instead of freezing a copied component. */

use serde_json::Value;

/** Each op names a node by `id`: set (merge keys in), replace, remove, move (into/at, before, after),
or insert a new node (into/at, before, after). */
pub fn apply(root: &mut Value, ops: &[Value]) -> Vec<String> {
    let mut warnings = vec![];
    for (i, op) in ops.iter().enumerate() {
        if let Err(e) = apply_one(root, op) {
            warnings.push(format!("patch[{i}]: {e}"));
        }
    }
    warnings
}

fn apply_one(root: &mut Value, op: &Value) -> Result<(), String> {
    let id = op.get("id").and_then(Value::as_str);
    if let Some(node) = op.get("insert") {
        return place(root, node.clone(), op);
    }
    let id = id.ok_or("needs an `id` (or `insert`)")?;
    if let Some(set) = op.get("set") {
        let n = find_mut(root, id).ok_or_else(|| format!("no node with id `{id}`"))?;
        deep_merge(n, set.clone());
        return Ok(());
    }
    if let Some(node) = op.get("replace") {
        let n = find_mut(root, id).ok_or_else(|| format!("no node with id `{id}`"))?;
        let mut node = node.clone();
        if let Value::Object(o) = &mut node {
            o.entry("id").or_insert(Value::String(id.into()));
        }
        *n = node;
        return Ok(());
    }
    if op.get("remove").and_then(Value::as_bool) == Some(true) {
        take(root, id).ok_or_else(|| format!("no node with id `{id}` in a children list"))?;
        return Ok(());
    }
    if let Some(dest) = op.get("move") {
        let node = take(root, id).ok_or_else(|| format!("no node with id `{id}` in a children list"))?;
        return place(root, node, dest);
    }
    Err("needs one of set, replace, remove, move, insert".into())
}

/** Puts `node` where `dest` says: `{"into": id, "at": n}` (negative counts from the end; default last),
`{"before": id}`, or `{"after": id}`. */
fn place(root: &mut Value, node: Value, dest: &Value) -> Result<(), String> {
    if let Some(target) = dest.get("into").and_then(Value::as_str) {
        let parent = find_mut(root, target).ok_or_else(|| format!("no node with id `{target}`"))?;
        let Value::Object(p) = parent else { return Err(format!("`{target}` is not an object node")) };
        let kids = p.entry("children").or_insert(Value::Array(vec![]));
        if !kids.is_array() {
            *kids = Value::Array(vec![kids.clone()]);
        }
        let list = kids.as_array_mut().ok_or("children is not a list")?;
        let len = list.len() as i64;
        let at = dest.get("at").and_then(Value::as_i64).unwrap_or(len);
        let at = if at < 0 { (len + at + 1).max(0) } else { at.min(len) } as usize;
        list.insert(at, node);
        return Ok(());
    }
    let (sibling, after) = match (dest.get("before").and_then(Value::as_str), dest.get("after").and_then(Value::as_str)) {
        (Some(b), _) => (b, false),
        (_, Some(a)) => (a, true),
        _ => return Err("a destination needs into, before, or after".into()),
    };
    let (list, ix) = find_in_list(root, sibling).ok_or_else(|| format!("no node with id `{sibling}` in a children list"))?;
    list.insert(if after { ix + 1 } else { ix }, node);
    Ok(())
}

fn deep_merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                if v.is_null() {
                    b.remove(&k);
                } else {
                    deep_merge(b.entry(k).or_insert(Value::Null), v);
                }
            }
        }
        (b, o) => *b = o,
    }
}

fn has_id(v: &Value, id: &str) -> bool {
    v.get("id").and_then(Value::as_str) == Some(id)
}

fn find_mut<'a>(v: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if has_id(v, id) {
        return Some(v);
    }
    match v {
        Value::Object(o) => o.values_mut().find_map(|x| find_mut(x, id)),
        Value::Array(a) => a.iter_mut().find_map(|x| find_mut(x, id)),
        _ => None,
    }
}

fn find_in_list<'a>(v: &'a mut Value, id: &str) -> Option<(&'a mut Vec<Value>, usize)> {
    match v {
        Value::Array(a) => {
            if let Some(ix) = a.iter().position(|x| has_id(x, id)) {
                return Some((a, ix));
            }
            a.iter_mut().find_map(|x| find_in_list(x, id))
        }
        Value::Object(o) => o.values_mut().find_map(|x| find_in_list(x, id)),
        _ => None,
    }
}

fn take(root: &mut Value, id: &str) -> Option<Value> {
    let (list, ix) = find_in_list(root, id)?;
    Some(list.remove(ix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn moves_sets_inserts_and_removes_by_id() {
        let mut v = json!({"components": {"Bar": {"type": "row", "id": "bar", "children": [
            {"id": "a"}, {"id": "spacer"}, {"id": "b", "style": {"color": "red"}}
        ]}}});
        let warnings = apply(&mut v, &[
            json!({"id": "a", "move": {"after": "spacer"}}),
            json!({"id": "b", "set": {"style": {"width": 3}}}),
            json!({"insert": {"id": "c"}, "into": "bar", "at": 0}),
            json!({"id": "spacer", "remove": true}),
            json!({"id": "missing", "remove": true}),
        ]);
        let ids: Vec<&str> = v["components"]["Bar"]["children"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
        assert_eq!(v["components"]["Bar"]["children"][2]["style"], json!({"color": "red", "width": 3}));
        assert_eq!(warnings.len(), 1);
    }
}
