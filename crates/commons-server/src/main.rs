//! `commons-server` — the entry point.

use commons_server::config::{self, Cli};
use commons_server::{run, StartError};

fn main() {
    let cli = match Cli::from_env() {
        Ok(c) => c,
        // `--help` and `--version` arrive here as Err, and printing them on
        // stdout with a zero status is what a user expects from either.
        Err(msg) => {
            if msg.starts_with("commons ") && !msg.contains('\n') {
                println!("{msg}");
                std::process::exit(0);
            }
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    // Explicit --config wins; otherwise look in the XDG config dir.
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| config::xdg_config_dir().join("commons.toml"));

    let file = match config::load_file(&config_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let resolved = match config::resolve(&cli, file.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = runtime.block_on(run(resolved)) {
        report(e);
        std::process::exit(1);
    }
}

fn report(e: StartError) {
    eprintln!("error: {e}");
    // A configuration problem is the user's to fix and should not read like a
    // crash; a bind failure usually means something else already owns the port.
    if let StartError::Bind { addr, .. } = &e {
        eprintln!("hint: is something already listening on {addr}?");
    }
}
