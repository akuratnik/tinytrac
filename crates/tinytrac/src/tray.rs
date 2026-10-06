//! Native tray resources stay on iced's event-loop thread.

mod model;

use std::fmt;

use iced::Subscription;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use crate::{app::Message, data::Data};
use model::Model;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Tray {
    icon: Option<TrayIcon>,
    model: Model,
}

impl fmt::Debug for Tray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tray")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Tray {
    pub fn new(data: &Data) -> Result<Self> {
        let model = Model::new(data);
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu(&model)?))
            .with_icon(icon(model.running)?)
            .with_menu_on_left_click(false)
            .with_tooltip(&model.tooltip)
            .build()?;
        Ok(Self {
            icon: Some(icon),
            model,
        })
    }

    #[cfg(test)]
    pub fn headless(data: &Data) -> Self {
        Self {
            icon: None,
            model: Model::new(data),
        }
    }

    pub fn refresh(&mut self, data: &Data) -> Result<()> {
        let model = Model::new(data);
        let previous = std::mem::replace(&mut self.model, model);
        let model = &self.model;
        if let Some(native) = &self.icon {
            if !model.same_menu(&previous) {
                native.set_menu(Some(Box::new(menu(model)?)));
            }
            if model.tooltip != previous.tooltip {
                native.set_tooltip(Some(&model.tooltip))?;
            }
            if model.running != previous.running {
                native.set_icon(Some(icon(model.running)?))?;
            }
        }
        Ok(())
    }

    pub fn message(&self, id: &str) -> Option<Message> {
        self.model.message(id)
    }
}

fn menu(model: &Model) -> Result<Menu> {
    let menu = Menu::new();
    for item in &model.items {
        if item.separator_before {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        menu.append(&MenuItem::with_id(
            &item.id,
            item.label.replace('&', "&&"),
            item.enabled,
            None,
        ))?;
    }
    Ok(menu)
}

/// Opaque clock faces keep the white hands visible on light and dark panels.
fn icon(running: bool) -> Result<Icon> {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0_i32..32 {
        for x in 0_i32..32 {
            let radius = (x - 16).pow(2) + (y - 16).pow(2);
            let hand = (15..=17).contains(&x) && (7..=17).contains(&y)
                || (16..=23).contains(&x) && (15..=17).contains(&y);
            let pixel = if hand {
                [255, 255, 255, 255]
            } else if radius <= 196 {
                if running {
                    [30, 170, 90, 255]
                } else {
                    [120, 130, 145, 255]
                }
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    Ok(Icon::from_rgba(rgba, 32, 32)?)
}

pub fn subscription() -> Subscription<Message> {
    Subscription::batch([Subscription::run(events), Subscription::run(host_changes)])
}

fn events() -> impl iced::futures::Stream<Item = Message> {
    let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
    let menu_sender = sender.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_sender.unbounded_send(Message::TrayMenu(event.id.0));
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            let _ = sender.unbounded_send(Message::TrayMenu("show".into()));
        }
    }));
    receiver
}

#[cfg(not(target_os = "linux"))]
fn host_changes() -> impl iced::futures::Stream<Item = Message> {
    iced::futures::stream::once(async { Message::TrayHost(true) })
}

/// Watch the name: hosts may hardcode their registration property to true.
#[cfg(target_os = "linux")]
fn host_changes() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::SinkExt;

    iced::stream::channel(1, |mut sender| async move {
        if let Err(error) = watch_host(&mut sender).await {
            eprintln!("tinytrac: cannot watch tray host: {error}");
        }
        let _ = sender.send(Message::TrayHost(false)).await;
    })
}

#[cfg(target_os = "linux")]
async fn watch_host(
    sender: &mut iced::futures::channel::mpsc::Sender<Message>,
) -> zbus::Result<()> {
    use iced::futures::{SinkExt, StreamExt};

    let connection = zbus::Connection::session().await?;
    let proxy = zbus::fdo::DBusProxy::new(&connection).await?;
    let name = "org.kde.StatusNotifierWatcher";
    // Subscribe first so a change during the initial query cannot be missed.
    let mut changes = proxy
        .receive_name_owner_changed_with_args(&[(0, name)])
        .await?;
    let mut available = proxy.name_has_owner(name.try_into()?).await?;
    let _ = sender.send(Message::TrayHost(available)).await;
    while let Some(change) = changes.next().await {
        let next = change.args()?.new_owner().is_some();
        if next != available {
            available = next;
            if sender.send(Message::TrayHost(available)).await.is_err() {
                break;
            }
        }
    }
    Ok(())
}
