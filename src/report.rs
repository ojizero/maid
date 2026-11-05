//! `maid report`: print a saved run and cache its HTML view.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::app::{App, progress, usage};
use crate::output::{self, Format};
use crate::{record, report_html, sys};

#[derive(clap::Args)]
pub struct Args {
    /// Run file name, with or without .json [default: newest run]
    #[arg(long, value_name = "ID")]
    run: Option<String>,
    /// Replace the saved HTML report (requires --format html)
    #[arg(long)]
    force: bool,
}

pub fn run(app: &App, args: &Args) -> Result<ExitCode> {
    if args.force && !matches!(app.format, Format::Html) {
        return usage(Err(anyhow::anyhow!("--force requires --format html")));
    }
    let output = {
        // Select and cache under the same lock as progress saves and pruning.
        let _lock = if matches!(app.format, Format::Html) {
            Some(record::lock(&app.runs_dir())?)
        } else {
            None
        };
        let path = match &args.run {
            Some(id) => record::find(&app.runs_dir(), id)?,
            None => record::latest(&app.runs_dir())?,
        };
        if matches!(app.format, Format::Html) {
            html(&path, args.force)?
        } else {
            output::render(&record::load(&path)?, app.format, &app.home)
        }
    };
    // A slow stdout reader must not block progress saves.
    print!("{output}");
    Ok(ExitCode::SUCCESS)
}

fn html(run: &Path, force: bool) -> Result<String> {
    let path = run.with_extension("html");
    if !force {
        match std::fs::read_to_string(&path) {
            Ok(html) => {
                progress(&format!("using {}", path.display()));
                return Ok(html);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }
    let html = report_html::render(&record::load(run)?);
    sys::write_atomic(&path, html.as_bytes())?;
    progress(&format!("saved {}", path.display()));
    Ok(html)
}
