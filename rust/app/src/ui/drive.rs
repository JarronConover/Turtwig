/*! Driving settings-built UI without a mouse: typing into and submitting fields, focus, hover, scrolling,
and drops, plus what inspect.json shows about them. */

use crate::workspace::Workspace;
use gpui_kit::*;
use serde_json::{Map, Value};

pub(crate) const ACTIONS: &[(&str, &str)] = &[];

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(_ws: &mut Workspace, _name: &str, _a: &Map<String, Value>, _window: &mut Window, _cx: &mut Context<Workspace>) -> bool {
    false
}

/** What inspect.json shows under `ui`. */
pub(crate) fn inspect(_ws: &Workspace, _window: &Window, _cx: &App) -> Value {
    Value::Object(Map::new())
}
