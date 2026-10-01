//! tinytrac: a tiny native time tracker.

mod app;
mod data;
mod duration;
mod screen;

use std::path::PathBuf;

use app::App;

fn main() -> iced::Result {
    let path = database_path();
    iced::application(move || App::boot(&path), App::update, App::view)
        .title("tinytrac")
        .subscription(App::subscription)
        .window_size((460.0, 720.0))
        .run()
}

/// `$TINYTRAC_DB` if set, otherwise `tinytrac.db` in the platform's data
/// directory (e.g. `~/.local/share/tinytrac` on Linux).
fn database_path() -> PathBuf {
    if let Some(path) = std::env::var_os("TINYTRAC_DB") {
        return path.into();
    }
    let Some(dirs) = directories::ProjectDirs::from("", "", "tinytrac") else {
        eprintln!("tinytrac: no home directory to keep data in; set TINYTRAC_DB");
        std::process::exit(1);
    };
    if let Err(error) = std::fs::create_dir_all(dirs.data_dir()) {
        eprintln!(
            "tinytrac: cannot create {}: {error}",
            dirs.data_dir().display()
        );
        std::process::exit(1);
    }
    dirs.data_dir().join("tinytrac.db")
}
