/*! Quoting the chat: text selected in a transcript gets an "Add to chat" tip (or `chat.add_selection`'s key);
added, it waits as a chip over the composer and goes out as a quote at the top of the next message. */

use super::style;
use crate::workspace::Workspace;
use gpui_kit::base::TextSelection;
use gpui_kit::component::{h_flex, Icon};
use gpui_kit::*;
use serde_json::{Map, Value};

/** Where a drag in a transcript let go, for the tip over what it selected. */
pub(crate) struct SelectTip {
    pub chat: String,
    pub at: Point<Pixels>,
    pub text: String,
}

pub(crate) const ACTIONS: &[(&str, &str)] = &[
    ("chat.add_selection", "{chat?, text?}: quote the text selected in the transcript (or `text`) in a chat's next message, as a chip over its composer"),
    ("chat.unquote", "{chat?, index}: take a quote off a chat's composer"),
];

/** The key a `keys` entry binds to `action`, drawn the macOS way ("cmd-shift-l" → "⇧⌘L"). */
pub(crate) fn key_for(keys: &[Value], action: &str) -> Option<String> {
    let k = keys.iter().rev().find(|k| match k.get("do") {
        Some(Value::String(s)) => s == action,
        Some(Value::Object(o)) => o.get("do").and_then(Value::as_str) == Some(action),
        _ => false,
    })?;
    let parts: Vec<&str> = k.get("key")?.as_str()?.split('-').collect();
    let (key, mods) = parts.split_last()?;
    let mut out: String = [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("cmd", "⌘")].iter().filter(|(m, _)| mods.contains(m)).map(|(_, g)| *g).collect();
    out.push_str(&key.to_uppercase());
    Some(out)
}

/** Quotes over the message that follows them, each line marked with "> ". */
pub(crate) fn with_quotes(quotes: &[String], typed: &str) -> String {
    let mut parts: Vec<String> = quotes.iter().map(|q| q.trim().lines().map(|l| format!("> {l}").trim_end().to_string()).collect::<Vec<_>>().join("\n")).collect();
    parts.push(typed.to_string());
    parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n")
}

impl Workspace {
    /** The tip over a transcript's selected text (`chat.selection_tip`), once the drag lets go. */
    pub(crate) fn selection_tip(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self.select_tip.as_ref().filter(|t| t.chat == chat)?.at;
        let cfg = self.settings.chat.get("selection_tip").cloned().unwrap_or(Value::Bool(true));
        if cfg == Value::Bool(false) || !TextSelection::has_selection(window, cx) {
            return None;
        }
        let text = TextSelection::selected_text(window, cx);
        if text.trim().is_empty() {
            return None;
        }
        if let Some(t) = self.select_tip.as_mut() {
            t.text = text.clone();
        }
        let theme = self.settings.theme.clone();
        let st = cfg.get("style").and_then(Value::as_object).cloned().unwrap_or_default();
        let key_st = cfg.get("key").and_then(Value::as_object).cloned().unwrap_or_default();
        let offset = cfg.get("offset").and_then(Value::as_f64).unwrap_or(10.) as f32;
        let label = self.tr("add_to_chat", "Add to chat", &[]);
        let key = key_for(&self.settings.keys, "chat.add_selection");
        let action = serde_json::json!({"do": "chat.add_selection", "chat": chat, "text": text});
        self.clickables.push(("selection-tip".into(), label.clone(), action.clone()));
        let pill = style::apply(h_flex().id("selection-tip").items_center().cursor_pointer(), &st, &theme)
            .child(label)
            .children(key.map(|k| style::apply(div(), &key_st, &theme).child(k)))
            // On press, not click: pressing anywhere clears the selection, and the tip with it, before a release.
            .on_mouse_down(MouseButton::Left, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.run_action(&action, window, cx);
            }));
        Some(deferred(anchored().position(point(at.x, at.y - px(offset))).anchor(Anchor::BottomCenter).snap_to_window_with_margin(px(8.)).child(pill)).with_priority(2).into_any_element())
    }

    /** The quotes waiting to go out with a chat's next message, as chips (`chat.quote_chip`). */
    pub(crate) fn quote_chips(&mut self, chat: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let quotes = self.quotes.get(chat).cloned().unwrap_or_default();
        let theme = self.settings.theme.clone();
        let cfg = self.settings.chat.get("quote_chip").cloned().unwrap_or(Value::Null);
        let st: Map<String, Value> = cfg.get("style").and_then(Value::as_object).cloned().unwrap_or_default();
        let icon = cfg.get("icon").and_then(Value::as_str).unwrap_or("message-square-quote").to_string();
        let size = cfg.get("icon_size").and_then(Value::as_f64).unwrap_or(13.) as f32;
        let chars = cfg.get("chars").and_then(Value::as_u64).unwrap_or(24) as usize;
        quotes
            .iter()
            .enumerate()
            .map(|(index, q)| {
                let flat = q.split_whitespace().collect::<Vec<_>>().join(" ");
                let shown = if flat.chars().count() > chars { format!("“{}…", flat.chars().take(chars).collect::<String>().trim_end()) } else { format!("“{flat}”") };
                let remove = serde_json::json!({"do": "chat.unquote", "chat": chat, "index": index});
                let tip = self.tr("remove_quote", "Remove this quote", &[]);
                self.clickables.push((format!("quote-x:{index}"), tip, remove.clone()));
                style::apply(h_flex().id(SharedString::from(format!("quote:{index}"))).items_center(), &st, &theme)
                    .child(Icon::empty().path(format!("icons/{icon}.svg")).size(px(size)))
                    .child(shown)
                    .child(
                        div()
                            .id(SharedString::from(format!("quote-x:{index}")))
                            .cursor_pointer()
                            .opacity(0.6)
                            .hover(|s| s.opacity(1.))
                            .child(Icon::empty().path("icons/x.svg").size(px(size - 2.)))
                            .on_click(cx.listener(move |ws, _, window, cx| ws.run_action(&remove, window, cx))),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/** Runs `name` if it's one of this module's actions; false otherwise. */
pub(crate) fn dispatch(ws: &mut Workspace, name: &str, a: &Map<String, Value>, window: &mut Window, cx: &mut Context<Workspace>) -> bool {
    let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.trim().is_empty());
    match name {
        "chat.add_selection" => {
            let chat = s("chat").or_else(|| ws.select_tip.as_ref().map(|t| t.chat.clone())).or_else(|| ws.current_chat().map(|c| c.id.clone()));
            let live = Some(TextSelection::selected_text(window, cx)).filter(|t| !t.trim().is_empty());
            let text = s("text").or(live).or_else(|| ws.select_tip.as_ref().map(|t| t.text.clone()).filter(|t| !t.trim().is_empty()));
            if let (Some(chat), Some(text)) = (chat, text) {
                ws.quotes.entry(chat.clone()).or_default().push(text);
                ws.select_tip = None;
                TextSelection::clear(window, cx);
                ws.focus_area_action("composer", Some(chat), window, cx);
            }
            true
        }
        "chat.unquote" => {
            let chat = s("chat").or_else(|| ws.current_chat().map(|c| c.id.clone()));
            let index = a.get("index").and_then(Value::as_u64).map(|i| i as usize);
            if let (Some(list), Some(i)) = (chat.and_then(|c| ws.quotes.get_mut(&c)), index) {
                if i < list.len() {
                    list.remove(i);
                }
            }
            cx.notify();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_lead_the_message_and_the_key_reads_like_a_menu() {
        assert_eq!(super::with_quotes(&["a\nb ".into(), "c".into()], "my reply"), "> a\n> b\n\n> c\n\nmy reply");
        assert_eq!(super::with_quotes(&["a".into()], ""), "> a");
        let keys = vec![serde_json::json!({"key": "cmd-shift-l", "do": "chat.add_selection"})];
        assert_eq!(super::key_for(&keys, "chat.add_selection").as_deref(), Some("⇧⌘L"));
    }
}
