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
    screen::{Outcome, Result, muted},
};

#[derive(Debug, Default)]
pub struct State {
    new_tag: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Start(TagId, StartMode),
    Pause(TagId),
    PauseAll,
    StartBreak(TagId),
    EndBreak(TagId),
    Stop(TagId),
    NewTagChanged(String),
    CreateTag,
}

impl State {
    pub fn update(&mut self, message: Message, engine: &mut Engine) -> Result {
        match message {
            Message::Start(tag, mode) => _ = engine.start(tag, mode)?,
            Message::Pause(tag) => _ = engine.pause(tag)?,
            Message::PauseAll => _ = engine.pause_all()?,
            Message::StartBreak(tag) => _ = engine.start_break(tag)?,
            Message::EndBreak(tag) => _ = engine.end_break(tag)?,
            Message::Stop(tag) => _ = engine.stop(tag)?,
            Message::NewTagChanged(name) => {
                self.new_tag = name;
                return Ok(Outcome::Edited);
            }
            Message::CreateTag => {
                engine.create_tag(&self.new_tag)?;
                self.new_tag.clear();
            }
        }
        Ok(Outcome::Saved)
    }

    /// Rows for every active tag, plus archived ones whose timer is still open.
    /// Holding Ctrl (Cmd on macOS) makes Start leave other timers running.
    pub fn view<'a>(
        &'a self,
        data: &'a Data,
        modifiers: keyboard::Modifiers,
    ) -> Element<'a, Message> {
        let mode = if modifiers.command() {
            StartMode::Alongside
        } else {
            StartMode::Exclusive
        };
        let today = data.totals(DateRange::day(data.today())).by_tag();
        let rows = data
            .tags
            .iter()
            .filter(|tag| !tag.archived || data.timer(tag.id).is_some())
            .map(|tag| {
                let total = today.get(&tag.id).copied().unwrap_or_default();
                tag_row(tag, data.timer(tag.id), total, mode, data)
            });
        let new_tag = text_input("New tag…", &self.new_tag)
            .on_input(Message::NewTagChanged)
            .on_submit(Message::CreateTag);

        column![column(rows).spacing(6), new_tag].spacing(12).into()
    }
}

/// Two lines: the tag and its timer's clock, then today's total, the break
/// bank and the controls. Fits a narrow window.
fn tag_row<'a>(
    tag: &'a Tag,
    timer: Option<&Session>,
    today: jiff::SignedDuration,
    mode: StartMode,
    data: &Data,
) -> Element<'a, Message> {
    let now = data.now.timestamp();
    let id = tag.id;
    let mut balance = None;
    let stop = || {
        button("Stop")
            .style(button::danger)
            .on_press(Message::Stop(id))
    };
    let mut controls = row![].spacing(4);
    let mut clock = None;

    match timer.map(|session| (session, session.status())) {
        Some((session, Status::Running(activity))) => {
            if session.breaks().is_some() {
                balance = Some(session.tally(now).break_balance());
                let (label, message) = match activity {
                    Activity::Work => ("Break", Message::StartBreak(id)),
                    Activity::Break => ("End break", Message::EndBreak(id)),
                };
                controls = controls.push(button(label).style(button::secondary).on_press(message));
            }
            controls = controls
                .push(
                    button("Pause")
                        .style(button::secondary)
                        .on_press(Message::Pause(id)),
                )
                .push(stop());
            clock = Some(text(duration::clock(session.logged(now))).size(20));
        }
        Some((session, _)) => {
            controls = controls
                .push(button("Resume").on_press(Message::Start(id, mode)))
                .push(stop());
            clock = Some(muted(duration::clock(session.logged(now))).size(20));
        }
        None => controls = controls.push(button("Start").on_press(Message::Start(id, mode))),
    }

    let today = duration::short(today);
    let info = match balance {
        Some(balance) => format!("today {today} · bank {}", duration::short(balance)),
        None => format!("today {today}"),
    };
    let running = timer.is_some_and(|session| matches!(session.status(), Status::Running(_)));
    container(
        column![
            row![text(data.tag_name(tag.id).to_owned()).width(Fill)]
                .push(clock)
                .align_y(Center),
            row![muted(info).width(Fill), controls].align_y(Center),
        ]
        .spacing(4),
    )
    .padding(8)
    .style(if running {
        container::rounded_box
    } else {
        container::transparent
    })
    .into()
}
