mod record;
mod store;
mod summary;
mod text;
mod tui;

use std::{env, path::PathBuf};

use anyhow::Result;
fn main() {
    if let Some(path) = env::var_os(summary::WORKER_ENV) {
        let _ = summary::run_worker(&PathBuf::from(path));
        return;
    }
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
    let mut arguments = env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        anyhow::ensure!(
            matches!(argument.to_str(), Some("sessions" | "--sessions"))
                && arguments.next().is_none(),
            "usage: asciinematic [sessions]"
        );
        return tui::sessions_menu(&data_dir);
    }
    let shell = env::var_os("SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let (_, code) = record::record_new_session(&data_dir, &shell)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
