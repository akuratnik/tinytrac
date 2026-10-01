//! One row per tag, with its timer controls, and a field to add tags.

use iced::{
    Alignment::Center,
    Element, Fill, keyboard,
    widget::{button, column, container, row, text, text_input},
};
use tinytrac_core::{Activity, DateRange, Session, StartMode, Status, Tag, TagId};
use tinytrac_engine::Engine;

use crate::{
    data::Data,
    duration,
    screen::{Result, muted},
};

#[derive(Debug, Default)]
pub struct State {
    new_tag: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Start(TagId),
    Pause(TagId),
    StartBreak(TagId),
    EndBreak(TagId),
    Stop(TagId),
    NewTagChanged(String),
    CreateTag,
}

impl State {
    pub fn update(
        &mut self,
        message: Message,
        engine: &mut Engine,
        modifiers: keyboard::Modifiers,
    ) -> Result {
        match message {
            Message::Start(tag) => {
                let mode = if modifiers.command() {
                    StartMode::Alongside
                } else {
                    StartMode::Exclusive
                };
                engine.start(tag, mode)?;
            }
            Message::Pause(tag) => _ = engine.pause(tag)?,
            Message::StartBreak(tag) => _ = engine.start_break(tag)?,
            Message::EndBreak(tag) => _ = engine.end_break(tag)?,
            Message::Stop(tag) => _ = engine.stop(tag)?,
            Message::NewTagChanged(name) => self.new_tag = name,
            Message::CreateTag => {
                engine.create_tag(&self.new_tag)?;
                self.new_tag.clear();
            }
        }
        Ok(())
    }

    pub fn view<'a>(&'a self, data: &'a Data) -> Element<'a, Message> {
        let today = data.totals(DateRange::day(data.today())).by_tag();
        let rows = data.active_tags().map(|tag| {
            let total = today.get(&tag.id).copied().unwrap_or_default();
            tag_row(tag, data.timer(tag.id), total, data)
        });
        let new_tag = text_input("New tag…", &self.new_tag)
            .on_input(Message::NewTagChanged)
            .on_submit(Message::CreateTag);

        column![column(rows).spacing(6), new_tag].spacing(12).into()
    }
}

fn tag_row<'a>(
    tag: &'a Tag,
    timer: Option<&Session>,
    today: jiff::SignedDuration,
    data: &Data,
) -> Element<'a, Message> {
    let now = data.now.timestamp();
    let id = tag.id;
    let label = column![
        text(tag.name()),
        muted(format!("today {}", duration::short(today)))
    ];

    let controls: Element<'a, Message> = match timer.map(|session| (session, session.status())) {
        Some((session, Status::Running(activity))) => {
            let mut buttons = row![].spacing(4).align_y(Center);
            if let Some(balance) = session.breaks().map(|_| session.tally(now).break_balance()) {
                let (label, message) = match activity {
                    Activity::Work => ("Break", Message::StartBreak(id)),
                    Activity::Break => ("End break", Message::EndBreak(id)),
                };
                buttons = buttons
                    .push(muted(format!("bank {}", duration::short(balance))))
                    .push(button(label).style(button::secondary).on_press(message));
            }
            buttons
                .push(
                    button("Pause")
                        .style(button::secondary)
                        .on_press(Message::Pause(id)),
                )
                .push(
                    button("Stop")
                        .style(button::danger)
                        .on_press(Message::Stop(id)),
                )
                .push(text(duration::clock(session.logged(now))).size(20))
                .into()
        }
        Some((session, _)) => row![
            muted(duration::clock(session.logged(now))),
            button("Resume").on_press(Message::Start(id)),
            button("Stop")
                .style(button::danger)
                .on_press(Message::Stop(id)),
        ]
        .spacing(4)
        .align_y(Center)
        .into(),
        None => button("Start").on_press(Message::Start(id)).into(),
    };

    let running = timer.is_some_and(|session| matches!(session.status(), Status::Running(_)));
    container(row![label.width(Fill), controls].spacing(8).align_y(Center))
        .padding(8)
        .style(if running {
            container::rounded_box
        } else {
            container::transparent
        })
        .into()
}
