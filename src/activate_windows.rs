// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use askama::Template;
use snafu::{ResultExt, Snafu};
use std::{io, path::Path};
use tempfile::NamedTempFile;
use zbus::blocking::Connection;

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
pub enum ActivateWindowsError {
    #[snafu(display("Failed to create a tempfile"))]
    CreateTempfile { source: io::Error },
    #[snafu(display("Failed to render the template to the tempfile"))]
    RenderTemplate { source: io::Error },
    #[snafu(display("Failed to load a KWin script"))]
    LoadScript { source: zbus::Error },
    #[snafu(display("Failed to run the script"))]
    RunScript { source: zbus::Error },
    #[snafu(display("Failed to stop the script"))]
    StopScript { source: zbus::Error },
}

pub fn activate_windows(conn: &Connection, pid: u32) -> Result<(), ActivateWindowsError> {
    let temp_path = {
        #[derive(Template)]
        #[template(path = "activate_windows.js.jinja", escape = "none")]
        struct Template {
            pid: u32,
        }
        let mut file = NamedTempFile::with_prefix(".konsoleat-").context(CreateTempfileCtx)?;
        Template { pid }.write_into(&mut file).context(RenderTemplateCtx)?;
        file.into_temp_path()
    };
    let script_id = load_script(conn, &temp_path).context(LoadScriptCtx)?;
    let object_path = format!("/Scripting/Script{script_id}");
    run_script(conn, &object_path).context(RunScriptCtx)?;
    stop_script(conn, &object_path).context(StopScriptCtx)
}

fn load_script(conn: &Connection, path: &Path) -> zbus::Result<i32> {
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "loadScript",
        &(path),
    )?
    .body()
    .deserialize()
}

fn run_script(conn: &Connection, object_path: &str) -> zbus::Result<()> {
    conn.call_method(
        Some("org.kde.KWin"),
        object_path,
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    )?
    .body()
    .deserialize()
}

fn stop_script(conn: &Connection, object_path: &str) -> zbus::Result<()> {
    conn.call_method(
        Some("org.kde.KWin"),
        object_path,
        Some("org.kde.kwin.Script"),
        "stop",
        &(),
    )?
    .body()
    .deserialize()
}
