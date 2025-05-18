mod app;
mod profile;
mod proto;

use crate::app::{App, AppLogger};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // TODO: use clap
    let args: Vec<String> = std::env::args().skip(1).collect();

    // parse args
    let mut profile = None;
    match args.as_slice() {
        [] => {}
        [profile_arg] => {
            profile = Some(profile_arg.clone());
        }
        _ => {
            println!("Couldn't parse command line arguments: {args:?}");
            println!("Usage: cargo run -- [profile]");
            anyhow::bail!("failed to parse args");
        }
    }

    // initialize app
    let app = App::new(profile).await?;

    // set up global logger
    let logger = AppLogger::new();
    log::set_boxed_logger(Box::new(logger))?;
    log::set_max_level(log::LevelFilter::Info);

    // run tui
    let terminal = ratatui::init();
    let app_result = app.run(terminal).await;
    ratatui::restore();
    app_result
}
