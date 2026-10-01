//! Totals per tag over a chosen range of days.

use std::cmp::Reverse;

use iced::{
    Alignment::Center,
    Element, Fill,
    widget::{button, column, row, rule, text, text_input},
};
use jiff::{SignedDuration, civil::Date};
use tinytrac_core::{DateRange, Session, Totals};
use tinytrac_engine::Engine;

use crate::{
    data::Data,
    duration,
    screen::{Error, Result, heading, muted},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Today,
    Week,
    Month,
    LastMonth,
    Custom,
}

impl Preset {
    const ALL: [Preset; 5] = [
        Preset::Today,
        Preset::Week,
        Preset::Month,
        Preset::LastMonth,
        Preset::Custom,
    ];

    fn label(self) -> &'static str {
        match self {
            Preset::Today => "Today",
            Preset::Week => "This week",
            Preset::Month => "This month",
            Preset::LastMonth => "Last month",
            Preset::Custom => "Custom",
        }
    }
}

#[derive(Debug)]
pub struct State {
    preset: Preset,
    range: DateRange,
    from: String,
    to: String,
    sessions: Vec<Session>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Preset(Preset),
    From(String),
    To(String),
    ApplyCustom,
}

impl State {
    pub fn new(data: &Data) -> Self {
        let range = DateRange::month_of(data.today());
        Self {
            preset: Preset::Month,
            range,
            from: range.first().to_string(),
            to: range.last().to_string(),
            sessions: Vec::new(),
        }
    }

    pub fn update(&mut self, message: Message, engine: &Engine, data: &Data) -> Result {
        match message {
            Message::Preset(preset) => self.preset = preset,
            Message::From(from) => self.from = from,
            Message::To(to) => self.to = to,
            Message::ApplyCustom => {
                let parse = |input: &str| input.trim().parse::<Date>();
                let (Ok(from), Ok(to)) = (parse(&self.from), parse(&self.to)) else {
                    return Err(Error::Input("Enter days like 2026-10-01"));
                };
                self.range = DateRange::new(from, to);
            }
        }
        self.reload(engine, data)
    }

    /// Reloads the sessions for the current range.
    pub fn reload(&mut self, engine: &Engine, data: &Data) -> Result {
        let today = data.today();
        self.range = match self.preset {
            Preset::Today => DateRange::day(today),
            Preset::Week => DateRange::week_of(today, data.settings.week_start.into()),
            Preset::Month => DateRange::month_of(today),
            Preset::LastMonth => DateRange::month_before(today),
            Preset::Custom => self.range,
        };
        self.sessions = engine.sessions(self.range)?;
        Ok(())
    }

    pub fn view<'a>(&'a self, data: &'a Data) -> Element<'a, Message> {
        let presets = row(Preset::ALL.map(|preset| {
            let style = if preset == self.preset {
                button::primary
            } else {
                button::secondary
            };
            button(preset.label())
                .style(style)
                .on_press(Message::Preset(preset))
                .into()
        }))
        .spacing(4)
        .wrap();

        let mut content = column![heading("Reports"), presets].spacing(12);
        if self.preset == Preset::Custom {
            content = content.push(
                row![
                    text_input("From", &self.from)
                        .on_input(Message::From)
                        .on_submit(Message::ApplyCustom),
                    text_input("To", &self.to)
                        .on_input(Message::To)
                        .on_submit(Message::ApplyCustom),
                    button("Show").on_press(Message::ApplyCustom),
                ]
                .spacing(6),
            );
        }

        let totals = Totals::compute(
            &self.sessions,
            self.range,
            data.now.time_zone(),
            data.now.timestamp(),
        );
        let mut by_tag: Vec<(String, SignedDuration)> = totals
            .by_tag()
            .into_iter()
            .map(|(tag, total)| (data.tag_name(tag).to_owned(), total))
            .collect();
        by_tag.sort_by_key(|(_, total)| Reverse(*total));

        let range = format!("{} – {}", self.range.first(), self.range.last());
        content
            .push(muted(range))
            .push(
                column(by_tag.into_iter().map(|(name, total)| {
                    row![text(name).width(Fill), text(duration::short(total))].into()
                }))
                .spacing(4),
            )
            .push(rule::horizontal(1))
            .push(
                row![
                    text("Total").width(Fill),
                    text(duration::short(totals.total()))
                ]
                .align_y(Center),
            )
            .into()
    }
}
