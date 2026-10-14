/*! Attachments: files dropped or pasted onto a chat wait as preview chips above its composer while they
upload (to the box, for box chats), then go out with the next message as paths the agent can read. */

use super::style;
use crate::workspace::{stage_one, Attachment, Workspace};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Clone, PartialEq)]
pub enum Status {
    Uploading,
    Ready(String),
    Failed(String),
}

#[derive(Clone)]
pub struct Pending {
    pub id: u64,
    pub name: String,
    pub preview: PathBuf,
    pub is_image: bool,
    pub status: Status,
}

pub fn is_image(p: &std::path::Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp"))
}

impl Workspace {
    fn chat_label(&self, chat: &str) -> String {
        if chat == crate::designer::CHAT_ID {
            return "the designer".into();
        }
        self.chat(chat).map(|c| format!("“{}”", c.name)).unwrap_or_else(|| "this chat".into())
    }

    pub(crate) fn attach(&mut self, chat: String, files: Vec<Attachment>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        if files.is_empty() {
            return;
        }
        let local = chat == crate::designer::CHAT_ID || self.chat(&chat).map(|c| c.host == "mac").unwrap_or(false);
        let host = (!local).then(|| self.settings.connection.ssh_host.clone());
        window.push_notification(format!("Attaching {} to {}", if files.len() == 1 { "a file".to_string() } else { format!("{} files", files.len()) }, self.chat_label(&chat)), cx);
        for file in files {
            let file = match file {
                Attachment::Bytes { bytes, ext } => match crate::workspace::write_upload(&bytes, &ext) {
                    Ok(p) => Attachment::Path(p),
                    Err(e) => {
                        window.push_notification(format!("Couldn't attach: {e}"), cx);
                        continue;
                    }
                },
                other => other,
            };
            let Attachment::Path(path) = file else { continue };
            self.next_attachment += 1;
            let id = self.next_attachment;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            self.attachments.entry(chat.clone()).or_default().push(Pending {
                id,
                name,
                is_image: is_image(&path),
                preview: path.clone(),
                status: Status::Uploading,
            });
            let (chat, host) = (chat.clone(), host.clone());
            cx.spawn_in(window, async move |this, cx| {
                let result = cx.background_spawn(async move { stage_one(&path, host.as_deref()) }).await;
                this.update_in(cx, |ws, window, cx| {
                    if let Some(p) = ws.attachments.get_mut(&chat).and_then(|v| v.iter_mut().find(|p| p.id == id)) {
                        p.status = match result {
                            Ok(agent_path) => Status::Ready(agent_path),
                            Err(e) => Status::Failed(e),
                        };
                    }
                    let waiting = ws.attachments.get(&chat).is_some_and(|v| v.iter().any(|p| p.status == Status::Uploading));
                    if !waiting && ws.send_when_ready.remove(&chat) {
                        ws.send_composer(&chat, window, cx);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /** Paths of the chat's uploaded attachments (clearing them); None while any is still uploading. */
    pub(crate) fn take_attachments(&mut self, chat: &str) -> Option<Vec<String>> {
        let list = self.attachments.get(chat)?;
        if list.iter().any(|p| p.status == Status::Uploading) {
            return None;
        }
        let paths = list.iter().filter_map(|p| if let Status::Ready(a) = &p.status { Some(a.clone()) } else { None }).collect();
        self.attachments.remove(chat);
        Some(paths)
    }

    pub(crate) fn attachments_json(&self, chat: &str) -> Value {
        Value::Array(
            self.attachments
                .get(chat)
                .map(|v| {
                    v.iter()
                        .map(|p| {
                            let status = match &p.status {
                                Status::Uploading => "uploading",
                                Status::Ready(_) => "ready",
                                Status::Failed(_) => "failed",
                            };
                            serde_json::json!({"name": p.name, "image": p.is_image, "status": status})
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    pub(crate) fn attachments_view(&mut self, chat: Option<String>, props: &Map<String, Value>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = chat?;
        let list = self.attachments.get(&chat).filter(|v| !v.is_empty())?.clone();
        let theme = self.settings.theme.clone();
        let cs = self.settings.chat.clone();
        let st = |k: &str| cs.get(k).and_then(Value::as_object).cloned().unwrap_or_default();
        let size = props.get("size").and_then(Value::as_f64).unwrap_or(64.) as f32;
        let danger = style::color(&Value::from("@danger"), &theme).unwrap_or(red());
        let mut row = style::apply(div().flex().flex_row().flex_wrap(), &st("attachments"), &theme);
        for p in list {
            let (chat2, id) = (chat.clone(), p.id);
            let remove = div()
                .id(SharedString::from(format!("att-x:{id}")))
                .absolute()
                .top(px(3.))
                .right(px(3.))
                .size(px(18.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.65))
                .cursor_pointer()
                .child(Icon::empty().path("icons/x.svg").size(px(11.)).text_color(white()))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    if let Some(v) = ws.attachments.get_mut(&chat2) {
                        v.retain(|x| x.id != id);
                    }
                    cx.notify();
                }));
            let failed = if let Status::Failed(e) = &p.status { Some(e.clone()) } else { None };
            let body: AnyElement = if p.is_image {
                img(p.preview.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element()
            } else {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .p(px(4.))
                    .child(Icon::empty().path("icons/file.svg").size(px(18.)))
                    .child(div().w_full().text_size(px(10.)).text_center().truncate().child(p.name.clone()))
                    .into_any_element()
            };
            let width = if p.is_image { crate::workspace::fit(&p.preview, size * 2.5, size).0.max(size) } else { size };
            let mut chip = style::apply(div().id(SharedString::from(format!("att:{id}"))).relative().w(px(width)).h(px(size)).overflow_hidden(), &st("attachment"), &theme)
                .child(body)
                .when(p.status == Status::Uploading, |d| {
                    d.child(div().absolute().inset_0().flex().items_center().justify_center().bg(hsla(0., 0., 0., 0.45)).child(Spinner::new().small()))
                })
                .child(remove);
            if let Some(e) = failed {
                chip = chip.border_2().border_color(danger).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(format!("Upload failed: {e}")).build(w, cx));
            } else {
                let name = p.name.clone();
                chip = chip.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx));
            }
            row = row.child(chip);
        }
        Some(row.into_any_element())
    }
}
