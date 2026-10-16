/*! Options the app sends for this daemon (settings `daemon`), read anywhere with `num`, `text`, `get`. */

use serde_json::Value;
use std::sync::RwLock;

static OPTIONS: RwLock<Option<Value>> = RwLock::new(None);

pub fn set(options: Value) {
    *OPTIONS.write().unwrap() = Some(options);
}

pub fn get(key: &str) -> Option<Value> {
    OPTIONS.read().unwrap().as_ref().and_then(|o| o.get(key).cloned())
}

pub fn num(key: &str, default: f64) -> f64 {
    get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}
