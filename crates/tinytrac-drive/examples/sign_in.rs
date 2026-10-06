//! Manual Google smoke test; credentials remain in memory for this process only.

use std::{env, fs, io, path::PathBuf, process::Command, sync::atomic::AtomicBool};

use serde::Deserialize;
use tinytrac_drive::{Client, Drive, Endpoints, authorize};
use tinytrac_engine::{Engine, SyncTarget, Transport};

#[derive(Deserialize)]
struct ClientFile {
    installed: Desktop,
}

#[derive(Deserialize)]
struct Desktop {
    client_id: String,
    client_secret: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::var_os("TINYTRAC_GOOGLE_CLIENT").map_or_else(
        || {
            PathBuf::from(env::var_os("HOME").unwrap_or_default())
                .join(".config/tinytrac/google-client.json")
        },
        PathBuf::from,
    );
    let desktop = serde_json::from_slice::<ClientFile>(&fs::read(path)?)?.installed;
    let client = Client {
        id: desktop.client_id,
        secret: desktop.client_secret,
    };
    let endpoints = Endpoints::default();
    let authorization = authorize(&client, &endpoints, &AtomicBool::new(false), open_browser)?;
    println!("Connected as {}", authorization.account.email);
    let mut engine = Engine::open_in_memory()?;
    engine.set_sync_target(Some(SyncTarget::Remote {
        id: authorization.account.id,
        label: authorization.account.email,
    }))?;
    let context = engine.sync_context()?.ok_or("missing sync target")?;
    let mut drive = Drive::new(client, authorization.refresh_token, context, endpoints);
    for id in drive.list()? {
        println!("Batch: {id}");
    }
    Ok(())
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command.arg(url).spawn()?;
    Ok(())
}
