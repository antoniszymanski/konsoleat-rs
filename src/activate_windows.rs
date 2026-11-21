// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use askama::Template;
use heapless::format;
use snafu::{ResultExt, Snafu};
use std::{io, os::fd::AsRawFd, path::Path, process};
use tempfile::tempfile;
use zbus::Connection;

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

#[derive(Template)]
#[template(path = "activate_windows.js.jinja", escape = "none")]
struct ActivateWindowsTemplate {
    pid: u32,
}

pub async fn activate_windows(conn: &Connection, pid: u32) -> Result<(), ActivateWindowsError> {
    let mut file = tempfile().context(CreateTempfileCtx)?;
    ActivateWindowsTemplate { pid }
        .write_into(&mut file)
        .context(RenderTemplateCtx)?;
    let temp_path = unsafe { format!(30; "/proc/{}/fd/{}", process::id(), file.as_raw_fd()).unwrap_unchecked() };
    let script_id = load_script(conn, Path::new(temp_path.as_str()))
        .await
        .context(LoadScriptCtx)?;
    let object_path = unsafe { format!(28; "/Scripting/Script{script_id}").unwrap_unchecked() };
    run_script(conn, &object_path).await.context(RunScriptCtx)?;
    stop_script(conn, &object_path).await.context(StopScriptCtx)
}

async fn load_script(conn: &Connection, path: &Path) -> zbus::Result<i32> {
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "loadScript",
        &(path),
    )
    .await?
    .body()
    .deserialize()
}

async fn run_script(conn: &Connection, object_path: &str) -> zbus::Result<()> {
    conn.call_method(
        Some("org.kde.KWin"),
        object_path,
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    )
    .await?
    .body()
    .deserialize()
}

async fn stop_script(conn: &Connection, object_path: &str) -> zbus::Result<()> {
    conn.call_method(
        Some("org.kde.KWin"),
        object_path,
        Some("org.kde.kwin.Script"),
        "stop",
        &(),
    )
    .await?
    .body()
    .deserialize()
}
