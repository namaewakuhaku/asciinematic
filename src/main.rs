mod record;
mod store;
mod text;
mod tui;

use std::{env, path::PathBuf};

use anyhow::Result;
use uuid::Uuid;

fn main() {
    if let Err(error) = run() {
        eprintln!("asciinematic: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let data_dir = env::var_os("ASCIINEMATIC_HOME")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(store::default_data_dir)?;
    let shell = env::var_os("SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let id = Uuid::new_v4().simple().to_string();
    let name = env::var("ASCIINEMATIC_NAME").unwrap_or_else(|_| format!("session-{}", &id[..8]));
    let path = store::create_session(&data_dir, &id, &name, &shell.to_string_lossy())?;

    eprintln!("Recording {name:?}. Press Ctrl-T twice for controls; exit the shell to finish.\r");
    match record::run(&path, &shell) {
        Ok(code) => {
            eprintln!("\r\nSaved {}.", path.display());
            if code != 0 {
                std::process::exit(code);
            }
        }
        Err(error) => {
            let _ = store::finish(&path, 0);
            return Err(error);
        }
    }
    Ok(())
}
