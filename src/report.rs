//! `maid report`: print the newest run file.

use std::process::ExitCode;

use anyhow::Result;

use crate::app::App;
use crate::record;

pub fn run(app: &App) -> Result<ExitCode> {
    app.print(&record::load(&record::latest(&app.runs_dir())?)?);
    Ok(ExitCode::SUCCESS)
}
