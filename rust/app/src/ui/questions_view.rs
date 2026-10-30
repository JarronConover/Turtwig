/*! A question card that asks several questions at once: a row of options under each, picked one per
question, and the answers go to the agent together once every question has one. */

use super::style;
use crate::workspace::Workspace;
use agent_box_protocol::{Call, Chat};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use serde_json::{json, Map, Value};

/** Which questions a chat's picks are for: a new question card starts over. */
pub(crate) fn stamp(w: &Value) -> String {
    w["tool_input"].to_string()
}

impl Workspace {
    pub(crate) fn questions_card(&mut self, chat: &Chat, qs: &[(String, Vec<String>)], cancel: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.settings.theme.clone();
        let cs = &self.settings.chat.clone();
        let st = |k: &str| match cs.get(k) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let w = chat.waiting_on.clone().unwrap_or_default();
        let picks = self.picks.get(&chat.id).filter(|(s, _)| *s == stamp(&w)).map(|(_, p)| p.clone()).unwrap_or_default();
        let plain = st("waiting_option");
        let mut picked = plain.clone();
        picked.extend(st("waiting_option_picked"));
        let mut card = style::apply(v_flex().gap(px(10.)), &st("waiting"), theme);
        for (q, (text, options)) in qs.iter().enumerate() {
            let mut row = style::apply(h_flex().gap(px(6.)).flex_wrap(), &st("waiting_row"), theme);
            for (i, label) in options.iter().enumerate() {
                let action = json!({"do": "chat.answer", "chat": chat.id, "question": q, "option": i});
                let node = format!("opt:{}:{q}:{i}", chat.id);
                self.clickables.push((node.clone(), label.clone(), action.clone()));
                let look = if picks.get(q).copied().flatten() == Some(i) { &picked } else { &plain };
                row = row.child(
                    style::apply(div().id(SharedString::from(node)).cursor_pointer(), look, theme)
                        .child(label.clone())
                        .on_click(cx.listener(move |ws, _, window, cx| ws.run_action(&action, window, cx))),
                );
            }
            card = card.child(div().child(text.clone())).child(row);
        }
        let action = json!({"do": "chat.answer", "chat": chat.id, "option": "cancel"});
        let node = format!("opt:{}:cancel", chat.id);
        self.clickables.push((node.clone(), cancel.clone(), action.clone()));
        let cancel = style::apply(div().id(SharedString::from(node)).cursor_pointer(), &plain, theme)
            .child(cancel)
            .on_click(cx.listener(move |ws, _, window, cx| ws.run_action(&action, window, cx)));
        card.child(style::apply(h_flex().gap(px(6.)).flex_wrap(), &st("waiting_row"), theme).child(cancel)).into_any_element()
    }

    /** Picks `option` for question `q` (else the first one not picked yet) of a chat's question card, and
    once every question has a pick, answers them all in turn and submits. */
    pub(crate) fn pick_answer(&mut self, id: &str, q: Option<usize>, option: Option<&Value>, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(chat) = self.chat(id).cloned() else { return Err("no such chat".into()) };
        let w = chat.waiting_on.clone().unwrap_or_default();
        let qs = crate::workspace::waiting_questions(&w);
        let entry = self.picks.entry(id.to_string()).or_default();
        if entry.0 != stamp(&w) || entry.1.len() != qs.len() {
            *entry = (stamp(&w), vec![None; qs.len()]);
        }
        let q = q.or_else(|| entry.1.iter().position(Option::is_none)).filter(|q| *q < qs.len()).ok_or("no question to answer")?;
        let labels = &qs[q].1;
        let i = match option {
            Some(Value::Number(n)) => n.as_u64().map(|n| n as usize),
            Some(Value::String(l)) => labels.iter().position(|o| o.eq_ignore_ascii_case(l)),
            _ => None,
        }
        .filter(|i| *i < labels.len())
        .ok_or_else(|| format!("no option {option:?} for question {q}; options: {}", labels.join(", ")))?;
        entry.1[q] = Some(i);
        cx.notify();
        let Some(all) = entry.1.iter().copied().collect::<Option<Vec<usize>>>() else { return Ok(()) };
        self.picks.remove(id);
        let agents = self.settings.get("daemon.agents").cloned().unwrap_or(Value::Null);
        let keys = all.iter().map(|p| self.answer_key(&chat.agent, p + 1)).chain(crate::behavior::submit_keys(&agents, &chat.agent)).collect();
        self.press_in_turn(id, keys, cx);
        Ok(())
    }

    /** Presses `keys` in a chat one at a time, `chat.waiting_key_gap_ms` apart: keys sent together reach
    the agent as one paste, which its question menu ignores. */
    pub(crate) fn press_in_turn(&self, id: &str, keys: Vec<String>, cx: &mut Context<Self>) {
        let gap = self.settings.num("chat.waiting_key_gap_ms", 300.) as u64;
        let remote = self.route(&Call::ChatKeys { chat_id: id.to_string(), keys: vec![] });
        let id = id.to_string();
        cx.spawn(async move |_, cx| {
            for (n, key) in keys.into_iter().enumerate() {
                if n > 0 {
                    cx.background_executor().timer(std::time::Duration::from_millis(gap)).await;
                }
                if remote.call(Call::ChatKeys { chat_id: id.clone(), keys: vec![key] }).await.is_err() {
                    return;
                }
            }
        })
        .detach();
    }
}
