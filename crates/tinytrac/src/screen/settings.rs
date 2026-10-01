//! The break bank, week start, and tag management.

use std::{collections::BTreeMap, num::NonZeroU32};

use iced::{
    Alignment::Center,
    Element, Fill,
    widget::{button, column, pick_list, row, text, text_input, toggler},
};
use tinytrac_core::{BreakPolicy, TagId};
use tinytrac_engine::{Engine, WeekStart};

use crate::{
    data::Data,
    screen::{Error, Outcome, Result, heading, muted},
};

/// Edit buffers. The ratio is filled from the saved settings when the screen
/// opens; a tag name is only buffered once the user edits it, so the list
/// otherwise shows the saved names.
#[derive(Debug, Default)]
pub struct State {
    break_minutes: String,
    work_minutes: String,
    tag_names: BTreeMap<TagId, String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    BreakBank(bool),
    BreakMinutes(String),
    WorkMinutes(String),
    SaveRatio,
    WeekStart(WeekStart),
    TagName(TagId, String),
    RenameTag(TagId),
    Archive(TagId, bool),
}

impl State {
    pub fn new(data: &Data) -> Self {
        let policy = data.settings.break_policy;
        Self {
            break_minutes: policy.break_minutes.to_string(),
            work_minutes: policy.work_minutes.to_string(),
            tag_names: BTreeMap::new(),
        }
    }

    pub fn update(&mut self, message: Message, engine: &mut Engine) -> Result {
        match message {
            Message::BreakBank(on) => {
                // Turning the bank on uses the ratio in the fields, saved or not.
                let policy = if on { Some(self.policy()?) } else { None };
                engine.update_settings(|settings| {
                    settings.break_bank = on;
                    if let Some(policy) = policy {
                        settings.break_policy = policy;
                    }
                })?;
            }
            Message::SaveRatio => {
                let policy = self.policy()?;
                engine.update_settings(|settings| settings.break_policy = policy)?;
            }
            Message::WeekStart(start) => {
                engine.update_settings(|settings| settings.week_start = start)?;
            }
            Message::RenameTag(id) => {
                let Some(name) = self.tag_names.get(&id) else {
                    return Ok(Outcome::Edited);
                };
                engine.rename_tag(id, name)?;
                self.tag_names.remove(&id);
            }
            Message::Archive(id, archived) => _ = engine.set_tag_archived(id, archived)?,
            Message::BreakMinutes(minutes) => {
                self.break_minutes = minutes;
                return Ok(Outcome::Edited);
            }
            Message::WorkMinutes(minutes) => {
                self.work_minutes = minutes;
                return Ok(Outcome::Edited);
            }
            Message::TagName(id, name) => {
                self.tag_names.insert(id, name);
                return Ok(Outcome::Edited);
            }
        }
        Ok(Outcome::Saved)
    }

    fn policy(&self) -> Result<BreakPolicy> {
        let parse = || {
            Some(BreakPolicy {
                break_minutes: self.break_minutes.trim().parse().ok()?,
                work_minutes: self.work_minutes.trim().parse::<NonZeroU32>().ok()?,
            })
        };
        parse().ok_or(Error::Input(
            "Break and work minutes must be whole numbers, with work above zero",
        ))
    }

    pub fn view<'a>(&'a self, data: &'a Data) -> Element<'a, Message> {
        let settings = data.settings;
        let break_bank = column![
            toggler(settings.break_bank)
                .label("Break bank")
                .on_toggle(Message::BreakBank),
            muted("Work earns break time. Breaks are logged while you have credit."),
            row![
                text_input("5", &self.break_minutes)
                    .on_input(Message::BreakMinutes)
                    .on_submit(Message::SaveRatio)
                    .width(60),
                text("min break per"),
                text_input("25", &self.work_minutes)
                    .on_input(Message::WorkMinutes)
                    .on_submit(Message::SaveRatio)
                    .width(60),
                text("min work"),
                button("Save")
                    .style(button::secondary)
                    .on_press(Message::SaveRatio),
            ]
            .spacing(6)
            .align_y(Center),
        ]
        .spacing(6);

        let week_start = row![
            text("Week starts on").width(Fill),
            pick_list(
                [WeekStart::Monday, WeekStart::Sunday],
                Some(settings.week_start),
                Message::WeekStart
            ),
        ]
        .align_y(Center);

        let tags = data.tags.iter().map(|tag| {
            let name = self
                .tag_names
                .get(&tag.id)
                .map_or(tag.name(), String::as_str);
            row![
                text_input("Name", name)
                    .on_input(move |name| Message::TagName(tag.id, name))
                    .on_submit(Message::RenameTag(tag.id)),
                toggler(tag.archived)
                    .label("Archived")
                    .on_toggle(move |archived| Message::Archive(tag.id, archived)),
            ]
            .spacing(8)
            .align_y(Center)
            .into()
        });

        column![
            heading("Settings"),
            break_bank,
            week_start,
            heading("Tags"),
            muted("Press Enter to rename. Archived tags keep their history."),
            column(tags).spacing(6),
        ]
        .spacing(14)
        .into()
    }
}
