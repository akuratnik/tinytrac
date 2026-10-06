//! Tray contents use the same snapshot and commands as the window.

use tinytrac_core::{DateRange, StartMode, Status};

use crate::{app::Message, data::Data, duration, screen::timers};

#[derive(Debug)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub separator_before: bool,
    pub message: Message,
}

#[derive(Debug)]
pub struct Model {
    pub items: Vec<Item>,
    pub running: bool,
    pub tooltip: String,
}

impl Model {
    pub fn same_menu(&self, other: &Self) -> bool {
        self.items.len() == other.items.len()
            && self.items.iter().zip(&other.items).all(|(a, b)| {
                a.id == b.id
                    && a.label == b.label
                    && a.enabled == b.enabled
                    && a.separator_before == b.separator_before
            })
    }

    pub fn new(data: &Data) -> Self {
        let mut items = Vec::new();
        for tag in &data.tags {
            let timer = data.timer(tag.id);
            if tag.archived && timer.is_none() {
                continue;
            }
            let running = timer.is_some_and(|s| matches!(s.status(), Status::Running(_)));
            let label = timer.map_or_else(
                || data.tag_name(tag.id).to_owned(),
                |session| {
                    let status = if running { "running" } else { "paused" };
                    let minutes = session.logged(data.now.timestamp()).as_mins();
                    format!("{} · {status} {minutes}m", data.tag_name(tag.id))
                },
            );
            let (action, message) = if running {
                ("pause", timers::Message::Pause(tag.id))
            } else {
                (
                    "start",
                    timers::Message::Start(tag.id, StartMode::Exclusive),
                )
            };
            items.push(Item {
                id: format!("{action}/{}", tag.id),
                label,
                enabled: true,
                separator_before: false,
                message: Message::Timers(message),
            });
        }
        let running = data.any_running();
        for (id, label, enabled, message) in [
            (
                "pause-all",
                "Pause all",
                running,
                Message::Timers(timers::Message::PauseAll),
            ),
            ("show", "Show tinytrac", true, Message::ShowWindow),
            ("quit", "Quit", true, Message::Quit),
        ] {
            items.push(Item {
                id: id.into(),
                label: label.into(),
                enabled,
                separator_before: id == "show",
                message,
            });
        }
        let today = duration::short(data.totals(DateRange::day(data.today())).total());
        Self {
            items,
            running,
            tooltip: format!("tinytrac · today {today}"),
        }
    }

    pub fn message(&self, id: &str) -> Option<Message> {
        self.items
            .iter()
            .find(|item| item.id == id && item.enabled)
            .map(|item| item.message.clone())
    }
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Zoned};
    use tinytrac_engine::Engine;

    use super::*;

    fn engine() -> Engine {
        let mut engine = Engine::open_in_memory().unwrap();
        at(&mut engine, 0);
        engine
    }

    fn at(engine: &mut Engine, minutes: i64) {
        let now = "2026-10-01T09:00[Europe/Warsaw]".parse::<Zoned>().unwrap()
            + SignedDuration::from_mins(minutes);
        engine.set_clock(move || now.clone());
    }

    fn model(engine: &Engine) -> Model {
        Model::new(&Data::load(engine).unwrap())
    }

    #[test]
    fn empty_menu_keeps_window_and_quit_actions() {
        let menu = model(&engine());
        assert_eq!(
            menu.items
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            ["Pause all", "Show tinytrac", "Quit"]
        );
        assert!(!menu.running);
        assert_eq!(menu.items.iter().filter(|i| i.separator_before).count(), 1);
        assert!(
            menu.items
                .iter()
                .find(|i| i.id == "show")
                .unwrap()
                .separator_before
        );
        assert_eq!(menu.tooltip, "tinytrac · today 0m");
        assert!(!menu.items[0].enabled);
        assert!(menu.message("pause-all").is_none());
        assert!(matches!(menu.message("show"), Some(Message::ShowWindow)));
        assert!(matches!(menu.message("quit"), Some(Message::Quit)));
        assert!(menu.message("unknown").is_none());
    }

    #[test]
    fn labels_and_ids_follow_timer_state() {
        let mut engine = engine();
        let tag = engine.create_tag("Acme & Co").unwrap().id;
        let menu = model(&engine);
        assert_eq!(menu.items[0].label, "Acme & Co");
        assert!(menu.items[0].enabled);
        assert!(matches!(menu.message(&menu.items[0].id),
            Some(Message::Timers(timers::Message::Start(id, StartMode::Exclusive))) if id == tag));
        let start_id = menu.items[0].id.clone();

        engine.start(tag, StartMode::Exclusive).unwrap();
        at(&mut engine, 45);
        let menu = model(&engine);
        assert_eq!(menu.items[0].label, "Acme & Co · running 45m");
        assert_eq!(menu.tooltip, "tinytrac · today 45m");
        assert!(menu.running);
        assert!(matches!(menu.message(&menu.items[0].id),
            Some(Message::Timers(timers::Message::Pause(id))) if id == tag));
        assert!(matches!(
            menu.message("pause-all"),
            Some(Message::Timers(timers::Message::PauseAll))
        ));
        assert!(
            menu.message(&start_id).is_none(),
            "stale actions are ignored"
        );
        let pause_id = menu.items[0].id.clone();

        engine.pause(tag).unwrap();
        at(&mut engine, 60);
        let menu = model(&engine);
        assert_eq!(menu.items[0].label, "Acme & Co · paused 45m");
        assert!(!menu.running);
        assert!(menu.message("pause-all").is_none());
        assert!(menu.message(&pause_id).is_none());
        assert!(matches!(menu.message(&menu.items[0].id),
            Some(Message::Timers(timers::Message::Start(id, StartMode::Exclusive))) if id == tag));
    }

    #[test]
    fn archived_tags_stay_only_while_their_timer_is_open() {
        let mut engine = engine();
        let acme = engine.create_tag("Acme").unwrap().id;
        let globex = engine.create_tag("Globex").unwrap().id;
        engine.start(acme, StartMode::Exclusive).unwrap();
        engine.start(globex, StartMode::Alongside).unwrap();
        engine.set_tag_archived(acme, true).unwrap();
        engine.set_tag_archived(globex, true).unwrap();
        engine.pause(acme).unwrap();
        let menu = model(&engine);
        assert_eq!(menu.items.len(), 5);
        assert_eq!(menu.items[0].label, "Acme · paused 0m");
        assert_eq!(menu.items[1].label, "Globex · running 0m");
        assert!(matches!(menu.message(&menu.items[1].id),
            Some(Message::Timers(timers::Message::Pause(id))) if id == globex));
        engine.stop(acme).unwrap();
        engine.stop(globex).unwrap();
        assert_eq!(model(&engine).items.len(), 3);
    }

    #[test]
    fn a_break_is_running_and_can_be_paused() {
        let mut engine = engine();
        engine
            .update_settings(|settings| settings.break_bank = true)
            .unwrap();
        let tag = engine.create_tag("Acme").unwrap().id;
        engine.start(tag, StartMode::Exclusive).unwrap();
        at(&mut engine, 25);
        engine.start_break(tag).unwrap();
        at(&mut engine, 30);
        let menu = model(&engine);
        assert_eq!(menu.items[0].label, "Acme · running 30m");
        assert!(menu.running);
        assert!(matches!(menu.message(&menu.items[0].id),
            Some(Message::Timers(timers::Message::Pause(id))) if id == tag));
    }

    #[test]
    fn menu_comparison_uses_only_visible_items() {
        let engine = tinytrac_engine::Engine::open_in_memory().unwrap();
        let data = Data::load(&engine).unwrap();
        let baseline = Model::new(&data);
        let mut next = Model::new(&data);
        next.tooltip = "new tooltip".into();
        next.items[0].message = Message::Quit;
        assert!(baseline.same_menu(&next));
        next.items[0].enabled = !next.items[0].enabled;
        assert!(!baseline.same_menu(&next));
        next = Model::new(&data);
        next.items[0].label.push('!');
        assert!(!baseline.same_menu(&next));
        next = Model::new(&data);
        next.items[0].id.push('!');
        assert!(!baseline.same_menu(&next));
        next = Model::new(&data);
        next.items[0].separator_before = !next.items[0].separator_before;
        assert!(!baseline.same_menu(&next));
    }
}
