//! Recent days with their entries: edit elapsed time, move to another tag,
//! delete, or add time by hand.

use std::fmt;

use iced::{
    Alignment::Center,
    Element, Fill,
    widget::{button, column, pick_list, row, text, text_input},
};
use jiff::{SignedDuration, ToSpan, civil::Date};
use tinytrac_core::{Session, SessionId, Status, TagId};
use tinytrac_engine::Engine;

use crate::{
    data::{Data, HISTORY_DAYS},
    duration,
    screen::{Error, Result, heading, muted},
};

#[derive(Debug, Default)]
pub struct State {
    form: Option<Form>,
}

/// The entry being edited, or added when `session` is `None`.
#[derive(Debug, Clone)]
struct Form {
    session: Option<SessionId>,
    tag: Option<TagId>,
    day: String,
    elapsed: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Add,
    Edit(SessionId),
    Tag(TagChoice),
    Day(String),
    Elapsed(String),
    Save,
    Delete,
    Cancel,
}

/// A tag as offered in a pick list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChoice {
    id: TagId,
    name: String,
}

impl fmt::Display for TagChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl State {
    pub fn update(&mut self, message: Message, engine: &mut Engine, data: &Data) -> Result {
        match message {
            Message::Add => {
                self.form = Some(Form {
                    session: None,
                    tag: data.active_tags().next().map(|tag| tag.id),
                    day: data.today().to_string(),
                    elapsed: String::new(),
                });
            }
            Message::Edit(id) => {
                self.form = data.recent.iter().find(|s| s.id == id).map(|session| Form {
                    session: Some(id),
                    tag: Some(session.tag),
                    day: session.day.to_string(),
                    elapsed: duration::short(session.logged(data.now.timestamp())),
                });
            }
            Message::Tag(choice) => self.edit(|form| form.tag = Some(choice.id)),
            Message::Day(day) => self.edit(|form| form.day = day),
            Message::Elapsed(elapsed) => self.edit(|form| form.elapsed = elapsed),
            Message::Save => {
                if let Some(form) = &self.form {
                    form.save(engine, data)?;
                }
                self.form = None;
            }
            Message::Delete => {
                if let Some(id) = self.form.take().and_then(|form| form.session) {
                    engine.delete_session(id)?;
                }
            }
            Message::Cancel => self.form = None,
        }
        Ok(())
    }

    pub fn view<'a>(&'a self, data: &'a Data) -> Element<'a, Message> {
        let totals = data.totals(data.recent_range());
        let mut days = column![
            row![
                heading("History"),
                button("Add time")
                    .style(button::secondary)
                    .on_press(Message::Add)
            ]
            .spacing(8)
            .align_y(Center)
        ]
        .spacing(12);

        if let Some(form) = self.form.as_ref().filter(|form| form.session.is_none()) {
            days = days.push(form.view(data));
        }
        for offset in 0..HISTORY_DAYS {
            let day = data.today() - offset.days();
            let entries: Vec<&Session> = data.recent.iter().filter(|s| s.day == day).collect();
            let total = totals
                .days()
                .find(|(date, _)| *date == day)
                .map(|(_, tags)| tags.values().copied().sum::<SignedDuration>());
            if entries.is_empty() && total.is_none() {
                continue;
            }
            days = days.push(self.day_view(day, total.unwrap_or_default(), &entries, data));
        }
        days.into()
    }

    fn day_view<'a>(
        &'a self,
        day: Date,
        total: SignedDuration,
        entries: &[&Session],
        data: &'a Data,
    ) -> Element<'a, Message> {
        let header = row![
            text(day.strftime("%a %-d %b").to_string()).width(Fill),
            text(duration::short(total)),
        ];
        let rows = entries.iter().map(|session| match &self.form {
            Some(form) if form.session == Some(session.id) => form.view(data),
            _ => entry_row(session, data),
        });
        column![header, column(rows).spacing(2).padding([0, 12])]
            .spacing(4)
            .into()
    }

    fn edit(&mut self, change: impl FnOnce(&mut Form)) {
        if let Some(form) = &mut self.form {
            change(form);
        }
    }
}

fn entry_row<'a>(session: &Session, data: &'a Data) -> Element<'a, Message> {
    let logged = session.logged(data.now.timestamp());
    let state = match session.status() {
        Status::Running(_) => " · running",
        Status::Paused => " · paused",
        Status::Stopped if !session.adjustment().is_zero() => " · edited",
        Status::Stopped => "",
    };
    row![
        text(data.tag_name(session.tag)).width(Fill),
        muted(state),
        text(duration::short(logged)),
        button("Edit")
            .style(button::text)
            .on_press(Message::Edit(session.id)),
    ]
    .spacing(8)
    .align_y(Center)
    .into()
}

impl Form {
    fn view<'a>(&'a self, data: &'a Data) -> Element<'a, Message> {
        let choices: Vec<TagChoice> = data
            .active_tags()
            .map(|tag| TagChoice {
                id: tag.id,
                name: tag.name().to_owned(),
            })
            .collect();
        let selected = choices.iter().find(|c| Some(c.id) == self.tag).cloned();

        let mut fields = row![pick_list(choices, selected, Message::Tag).width(Fill)].spacing(6);
        if self.session.is_none() {
            fields = fields.push(
                text_input("YYYY-MM-DD", &self.day)
                    .on_input(Message::Day)
                    .width(120),
            );
        }
        fields = fields.push(
            text_input("1:30", &self.elapsed)
                .on_input(Message::Elapsed)
                .on_submit(Message::Save)
                .width(80),
        );

        let mut actions = row![
            button("Save").on_press(Message::Save),
            button("Cancel")
                .style(button::secondary)
                .on_press(Message::Cancel),
        ]
        .spacing(6);
        if self.session.is_some() {
            actions = actions.push(
                button("Delete")
                    .style(button::danger)
                    .on_press(Message::Delete),
            );
        }
        column![fields, actions].spacing(6).into()
    }

    fn save(&self, engine: &mut Engine, data: &Data) -> Result {
        let elapsed =
            duration::parse(&self.elapsed).ok_or(Error::Input("Enter a duration like 1:30"))?;
        let tag = self.tag.ok_or(Error::Input("Pick a tag"))?;
        let Some(id) = self.session else {
            let day: Date = (self.day.trim().parse())
                .map_err(|_| Error::Input("Enter a day like 2026-10-01"))?;
            engine.add_entry(tag, day, elapsed)?;
            return Ok(());
        };
        let session = data
            .recent
            .iter()
            .find(|s| s.id == id)
            .ok_or(Error::Input("That entry no longer exists"))?;
        if session.tag != tag {
            engine.retag(id, tag)?;
        }
        let shown = duration::short(session.logged(data.now.timestamp()));
        if self.elapsed.trim() != shown {
            engine.set_logged(id, elapsed)?;
        }
        Ok(())
    }
}
