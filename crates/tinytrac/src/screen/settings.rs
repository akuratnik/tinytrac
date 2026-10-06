//! The break bank, week start, and tag management.

use std::{collections::BTreeMap, fmt::Write, num::NonZeroU32};

use iced::{
    Alignment::Center,
    Element, Fill,
    widget::{button, column, pick_list, row, text, text_input, toggler},
};
use tinytrac_core::{BreakPolicy, TagId};
use tinytrac_engine::{Engine, SyncTarget, WeekStart};

use crate::{
    data::Data,
    screen::{Error, Outcome, Result, heading, muted},
};

/// Edit buffers. The ratio is filled from the saved settings when the screen
/// opens; a tag name is only buffered once the user edits it, so the list
/// otherwise shows the saved names.
#[derive(Debug, Default)]
pub struct State {
    sync_folder: String,
    pub sync_mode: SyncMode,
    break_minutes: String,
    work_minutes: String,
    tag_names: BTreeMap<TagId, String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    #[default]
    Off,
    Google,
    Folder,
}
impl std::fmt::Display for SyncMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "Off",
            Self::Google => "Google Drive",
            Self::Folder => "Folder",
        })
    }
}

#[derive(Debug, Clone)]
pub enum Account {
    SyncMode(SyncMode),
    SignIn,
    CopyLink,
    CancelSignIn,
    SignOut,
}

#[derive(Debug, Clone, Copy)]
pub enum AccountState<'a> {
    Unavailable,
    SignedOut,
    Waiting,
    Saving,
    SignedIn(&'a str),
    Expired { label: &'a str, reason: &'a str },
    LinkFallback,
}

#[derive(Debug, Clone)]
pub enum Message {
    Account(Account),
    SyncFolder(String),
    SaveSyncFolder,
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
            sync_mode: match &data.sync_target {
                None => SyncMode::Off,
                Some(SyncTarget::Folder(_)) => SyncMode::Folder,
                Some(SyncTarget::Remote { .. }) => SyncMode::Google,
            },
            sync_folder: match &data.sync_target {
                Some(SyncTarget::Folder(path)) => path.display().to_string(),
                _ => String::new(),
            },
            break_minutes: policy.break_minutes.to_string(),
            work_minutes: policy.work_minutes.to_string(),
            tag_names: BTreeMap::new(),
        }
    }

    pub fn folder_buffer(&self) -> &str {
        &self.sync_folder
    }
    pub fn set_folder_buffer(&mut self, folder: String) {
        self.sync_folder = folder;
    }

    pub fn folder_target(&self) -> Option<SyncTarget> {
        let folder = self.sync_folder.trim();
        (!folder.is_empty()).then(|| SyncTarget::Folder(folder.into()))
    }

    pub fn update(&mut self, message: Message, engine: &mut Engine) -> Result {
        match message {
            // The app routes account actions before invoking this screen.
            Message::Account(_) | Message::SaveSyncFolder => return Ok(Outcome::Edited),
            Message::SyncFolder(folder) => {
                self.sync_folder = folder;
                return Ok(Outcome::Edited);
            }
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

    pub fn view<'a>(
        &'a self,
        data: &'a Data,
        sync_status: &'a str,
        account: AccountState<'a>,
    ) -> Element<'a, Message> {
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
            let row = row![
                text_input("Name", name)
                    .on_input(move |name| Message::TagName(tag.id, name))
                    .on_submit(Message::RenameTag(tag.id)),
                toggler(tag.archived)
                    .label("Archived")
                    .on_toggle(move |archived| Message::Archive(tag.id, archived)),
            ]
            .spacing(8)
            .align_y(Center);
            if data.tag_name(tag.id) == tag.name() {
                row.into()
            } else {
                column![muted(data.tag_name(tag.id)), row].spacing(4).into()
            }
        });

        column![
            heading("Settings"),
            break_bank,
            week_start,
            self.sync_view(data, account),
            muted(if data.sync_target.is_none() {
                "Sync disabled"
            } else {
                sync_status
            }),
        ]
        .extend(diagnostics(&data.sync_diagnostics).map(|message| muted(message).into()))
        .push(heading("Tags"))
        .push(muted(
            "Press Enter to rename. Archived tags keep their history.",
        ))
        .push(column(tags).spacing(6))
        .spacing(14)
        .into()
    }
    fn sync_view<'a>(&'a self, data: &'a Data, account: AccountState<'a>) -> Element<'a, Message> {
        let busy = matches!(account, AccountState::Saving);
        let modes = sync_modes(!matches!(account, AccountState::Unavailable));
        let selection: Element<'_, Message> = if busy {
            text(self.sync_mode.to_string()).into()
        } else {
            pick_list(modes, Some(self.sync_mode), |mode| {
                Message::Account(Account::SyncMode(mode))
            })
            .into()
        };
        let mut sync = column![text("Sync"), selection].spacing(6);
        if busy && self.sync_mode != SyncMode::Google {
            sync = sync.push(text("Updating keychain…"));
        }
        match self.sync_mode {
            SyncMode::Off => {}
            SyncMode::Folder => {
                sync = sync.push(text("Sync folder")).push(
                    row![
                        text_input("Folder path (empty disables sync)", &self.sync_folder)
                            .on_input(Message::SyncFolder)
                            .on_submit(Message::SaveSyncFolder),
                        button("Save folder")
                            .style(button::secondary)
                            .on_press_maybe((!busy).then_some(Message::SaveSyncFolder)),
                    ]
                    .spacing(6),
                );
            }
            SyncMode::Google => {
                let sign_in =
                    || button("Sign in with Google").on_press(Message::Account(Account::SignIn));
                let sign_out = || {
                    button("Sign out")
                        .style(button::secondary)
                        .on_press(Message::Account(Account::SignOut))
                };
                match account {
                    AccountState::Unavailable => {
                        sync = sync.push(text("Google sign-in is unavailable in this build"));
                        if matches!(data.sync_target, Some(SyncTarget::Remote { .. })) {
                            sync = sync.push(sign_out());
                        }
                    }
                    AccountState::SignedOut => sync = sync.push(sign_in()),
                    AccountState::Waiting | AccountState::LinkFallback => {
                        sync = sync.push(text("Waiting for browser…"));
                        if matches!(account, AccountState::LinkFallback) {
                            sync = sync.push(text("Could not open the browser. Copy the link and open it in your browser."))
                                .push(button("Copy sign-in link").on_press(Message::Account(Account::CopyLink)));
                        }
                        sync = sync.push(
                            button("Cancel").on_press(Message::Account(Account::CancelSignIn)),
                        );
                    }
                    AccountState::Saving => sync = sync.push(text("Updating keychain…")),
                    AccountState::SignedIn(label) => {
                        sync = sync
                            .push(text(format!("Signed in as {label}")))
                            .push(sign_in())
                            .push(sign_out());
                    }
                    AccountState::Expired { label, reason } => {
                        sync = sync
                            .push(text(format!("{label}: {reason}")))
                            .push(sign_in())
                            .push(sign_out());
                    }
                }
                sync = sync.push(muted("The first sync uploads this Mac's history. Sign in to the same Google account on both Macs."));
            }
        }
        let active = match &data.sync_target {
            Some(SyncTarget::Folder(path)) if self.sync_mode != SyncMode::Folder => {
                Some(path.display().to_string())
            }
            Some(SyncTarget::Remote { label, .. }) if self.sync_mode != SyncMode::Google => {
                Some(label.clone())
            }
            _ => None,
        };
        if let Some(active) = active {
            let action = if self.sync_mode == SyncMode::Folder {
                "save a folder"
            } else {
                "finish signing in"
            };
            sync = sync.push(text(format!(
                "Still syncing to {active} until you {action}."
            )));
        }
        sync.into()
    }
}

fn sync_modes(google_available: bool) -> Vec<SyncMode> {
    if google_available {
        vec![SyncMode::Off, SyncMode::Google, SyncMode::Folder]
    } else {
        vec![SyncMode::Off, SyncMode::Folder]
    }
}

fn diagnostics(data: &tinytrac_engine::SyncDiagnostics) -> impl Iterator<Item = String> {
    let conflicts = (data.conflicts > 0).then(|| {
        format!(
            "{} {} made on two devices at once; the newer one was kept.",
            data.conflicts,
            if data.conflicts == 1 {
                "edit was"
            } else {
                "edits were"
            }
        )
    });
    let rejected = (data.rejected > 0).then(|| {
        let mut message = format!(
            "{} {} from other devices {} rejected",
            data.rejected,
            if data.rejected == 1 {
                "change"
            } else {
                "changes"
            },
            if data.rejected == 1 { "was" } else { "were" }
        );
        if let Some(reason) = &data.reason {
            let _ = write!(message, " (latest: {reason})");
        }
        message
    });
    conflicts.into_iter().chain(rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinytrac_engine::SyncDiagnostics;

    #[test]
    fn account_controls_match_state() {
        let engine = Engine::open_in_memory().unwrap();
        let data = Data::load(&engine).unwrap();
        let state = State {
            sync_mode: SyncMode::Google,
            ..State::new(&data)
        };
        let controls = [
            "Sign in with Google",
            "Sign out",
            "Cancel",
            "Copy sign-in link",
            "Updating keychain…",
        ];
        for (account, expected) in [
            (AccountState::Unavailable, vec![]),
            (AccountState::SignedOut, vec!["Sign in with Google"]),
            (AccountState::Waiting, vec!["Cancel"]),
            (AccountState::Saving, vec!["Updating keychain…"]),
            (
                AccountState::SignedIn("me@example.com"),
                vec!["Sign in with Google", "Sign out"],
            ),
            (
                AccountState::Expired {
                    label: "me@example.com",
                    reason: "Permission revoked",
                },
                vec!["Sign in with Google", "Sign out"],
            ),
            (
                AccountState::LinkFallback,
                vec!["Cancel", "Copy sign-in link"],
            ),
        ] {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(600.0, 1000.0),
                state.view(&data, "Not synced yet", account),
            );
            for control in controls {
                assert_eq!(
                    ui.find(control).is_ok(),
                    expected.contains(&control),
                    "{account:?}: {control}"
                );
            }
        }
    }

    #[test]
    fn google_option_requires_build_credentials() {
        assert_eq!(sync_modes(false), [SyncMode::Off, SyncMode::Folder]);
        assert!(sync_modes(true).contains(&SyncMode::Google));
    }

    #[test]
    fn diagnostics_hide_zero_counts_and_use_singular_plural_and_latest() {
        assert_eq!(diagnostics(&SyncDiagnostics::default()).count(), 0);
        let mut data = SyncDiagnostics {
            conflicts: 1,
            rejected: 1,
            reason: Some("bad body".into()),
        };
        assert_eq!(
            diagnostics(&data).collect::<Vec<_>>(),
            [
                "1 edit was made on two devices at once; the newer one was kept.",
                "1 change from other devices was rejected (latest: bad body)",
            ]
        );
        data.conflicts = 2;
        data.rejected = 2;
        assert_eq!(
            diagnostics(&data).collect::<Vec<_>>(),
            [
                "2 edits were made on two devices at once; the newer one was kept.",
                "2 changes from other devices were rejected (latest: bad body)",
            ]
        );
    }
}
