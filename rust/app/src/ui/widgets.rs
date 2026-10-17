/*! Interactive nodes beyond boxes and text: form controls, progress, tabs, drawings and clocks. Each
binds to state like `input` does, so agents read and set them through `.state`. */

use crate::workspace::Workspace;
use gpui_kit::*;
use jaq_json::Val;
use serde_json::{Map, Value};

pub const NODE_TYPES: &[(&str, &str)] = &[];

/** Events these nodes fire besides the ones every node has. */
pub const EVENTS: &[&str] = &[];

impl Workspace {
    /** The content of a widget node (`kind` is one of NODE_TYPES). */
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_widget(&mut self, _kind: &str, _id: &str, _props: &Map<String, Value>, _obj: &Map<String, Value>, _ctx: &Val, _st: &Map<String, Value>, _window: &mut Window, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![]
    }
}
