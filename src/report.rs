//! `maid report`: print the last run file.

use std::process::ExitCode;

use anyhow::Result;

use crate::app::App;
use crate::record;

pub fn run(app: &App) -> Result<ExitCode> {
    app.print(&record::load(&app.run_file())?);
    Ok(ExitCode::SUCCESS)
}
