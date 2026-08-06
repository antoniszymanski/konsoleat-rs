// SPDX-FileCopyrightText: 2025 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use askama::Template;
use clap::Parser;
use same_file::Handle;
use snafu::{ResultExt, Snafu};
use std::{
    io,
    num::ParseIntError,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::NamedTempFile;
use tokio::{fs, task::spawn_blocking};
use zbus::Connection;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Activate or create a Konsole terminal session in a specified working directory",
    long_about = None
)]
struct Cli {
    #[arg(default_value = ".")]
    workdir: PathBuf,
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum Error {
    #[snafu(display("Failed to canonicalize workdir"))]
    CanonicalizeWorkdir { source: io::Error },
    #[snafu(display("Failed to construct a handle from the workdir"))]
    ConstructHandleFromWorkdir { source: io::Error },
    #[snafu(display("Failed to create a D-Bus connection to the session message bus"))]
    ConnectToSessionBus { source: zbus::Error },
    #[snafu(display("Failed to list D-Bus services"))]
    ListServices { source: zbus::Error },
    #[snafu(display("Failed to list windows of the service"))]
    ListWindows { source: ListWindowsError },
    #[snafu(display("Failed to get current session"))]
    GetCurrentSession { source: zbus::Error },
    #[snafu(display("Failed to list sessions of the window"))]
    ListSessions { source: ListSessionsError },
    #[snafu(display("Failed to get current working directory of the session"))]
    GetSessionCwd { source: GetSessionCwdError },
    #[snafu(display("Failed to construct a handle from a path {path:?}"))]
    ConstructHandleFromPath { source: io::Error, path: PathBuf },
    #[snafu(display("Failed to set current session"))]
    SetCurrentSession { source: zbus::Error },
    #[snafu(display("Failed to get process ID of the service"))]
    GetServicePid { source: zbus::Error },
    #[snafu(display("Failed to activate windows"))]
    ActivateWindows { source: ActivateWindowsError },
    #[snafu(display("Failed to create new session"))]
    CreateSession { source: zbus::Error },
    #[snafu(display("Failed to launch a new Konsole terminal"))]
    LaunchKonsole { source: io::Error },
}

#[snafu::report]
#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut cli = Cli::parse();
    cli.workdir = fs::canonicalize(cli.workdir).await.context(CanonicalizeWorkdirCtx)?;
    let workdir_handle = Handle::from_path(&cli.workdir).context(ConstructHandleFromWorkdirCtx)?;

    let conn = &Connection::session().await.context(ConnectToSessionBusCtx)?;
    let mut first_window = None;

    let services = list_services(conn).await.context(ListServicesCtx)?;
    for service_name in services {
        let windows = list_windows(conn, &service_name).await.context(ListWindowsCtx)?;
        for window_id in windows {
            if first_window.is_none() {
                first_window = Some((service_name.clone(), window_id.clone()))
            }
            let current_session = get_current_session(conn, &service_name, &window_id)
                .await
                .context(GetCurrentSessionCtx)?;
            let cwd = get_session_cwd(conn, &service_name, current_session)
                .await
                .context(GetSessionCwdCtx)?;
            let handle = Handle::from_path(&cwd).context(ConstructHandleFromPathCtx { path: cwd })?;
            if handle == workdir_handle {
                let pid = get_service_pid(conn, &service_name).await.context(GetServicePidCtx)?;
                return activate_windows(conn, pid).await.context(ActivateWindowsCtx);
            }
            let sessions = list_sessions(conn, &service_name, &window_id)
                .await
                .context(ListSessionsCtx)?;
            for session_id in sessions {
                let cwd = get_session_cwd(conn, &service_name, session_id)
                    .await
                    .context(GetSessionCwdCtx)?;
                let handle = Handle::from_path(&cwd).context(ConstructHandleFromPathCtx { path: cwd })?;
                if handle == workdir_handle {
                    set_current_session(conn, &service_name, &window_id, session_id)
                        .await
                        .context(SetCurrentSessionCtx)?;
                    let pid = get_service_pid(conn, &service_name).await.context(GetServicePidCtx)?;
                    return activate_windows(conn, pid).await.context(ActivateWindowsCtx);
                }
            }
        }
    }

    let pid = match first_window {
        Some((service_name, window_id)) => {
            let session_id = new_session(conn, &service_name, &window_id, &cli.workdir)
                .await
                .context(CreateSessionCtx)?;
            set_current_session(conn, &service_name, &window_id, session_id)
                .await
                .context(SetCurrentSessionCtx)?;
            get_service_pid(conn, &service_name).await.context(GetServicePidCtx)?
        }
        None => Command::new("konsole")
            .arg("--workdir")
            .arg(cli.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context(LaunchKonsoleCtx)?
            .id(),
    };
    activate_windows(conn, pid).await.context(ActivateWindowsCtx)
}

async fn list_services(conn: &Connection) -> zbus::Result<Vec<Box<str>>> {
    Ok(conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .await?
        .body()
        .deserialize::<Vec<&str>>()?
        .into_iter()
        .filter(|s| s.starts_with("org.kde.konsole"))
        .map(|s| s.into())
        .collect())
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum ListWindowsError {
    #[snafu(display("Failed to get introspection data"))]
    GetIntrospection { source: zbus::Error },
    #[snafu(display("Failed to parse XML introspection data"))]
    ParseIntrospection { source: zbus_xml::Error },
}

async fn list_windows(conn: &Connection, service_name: &str) -> Result<Vec<Box<str>>, ListWindowsError> {
    let body = conn
        .call_method(
            Some(service_name),
            "/Windows",
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .await
        .context(GetIntrospectionCtx)?
        .body();
    let bytes = body.deserialize::<&str>().context(GetIntrospectionCtx)?.as_bytes();
    Ok(zbus_xml::Node::from_reader(bytes)
        .context(ParseIntrospectionCtx)?
        .nodes()
        .iter()
        .filter_map(|node| node.name())
        .map(|s| s.into())
        .collect())
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum ListSessionsError {
    #[snafu(display("Failed to call D-Bus method \"sessionList\""))]
    CallSessionList { source: zbus::Error },
    #[snafu(display("Failed to parse session ID {input:?} as i32"))]
    ParseSessionId { source: ParseIntError, input: String },
}

async fn list_sessions(conn: &Connection, service_name: &str, window_id: &str) -> Result<Vec<i32>, ListSessionsError> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "sessionList",
        &(),
    )
    .await
    .context(CallSessionListCtx)?
    .body()
    .deserialize::<Vec<&str>>()
    .context(CallSessionListCtx)?
    .into_iter()
    .map(|s| s.parse().context(ParseSessionIdCtx { input: s }))
    .collect()
}

async fn new_session(conn: &Connection, service_name: &str, window_id: &str, directory: &Path) -> zbus::Result<i32> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "newSession",
        &("" /* default profile */, directory),
    )
    .await?
    .body()
    .deserialize()
}

async fn get_current_session(conn: &Connection, service_name: &str, window_id: &str) -> zbus::Result<i32> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "currentSession",
        &(),
    )
    .await?
    .body()
    .deserialize()
}

async fn set_current_session(
    conn: &Connection,
    service_name: &str,
    window_id: &str,
    session_id: i32,
) -> zbus::Result<()> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "setCurrentSession",
        &(session_id),
    )
    .await?
    .body()
    .deserialize()
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum GetSessionCwdError {
    #[snafu(display("Failed to get process ID of the session"))]
    GetSessionPid { source: zbus::Error },
    #[snafu(display("Failed to get current working directory for process ID {pid}"))]
    GetProcessCwd { source: io::Error, pid: i32 },
}

async fn get_session_cwd(
    conn: &Connection,
    service_name: &str,
    session_id: i32,
) -> Result<PathBuf, GetSessionCwdError> {
    let pid: i32 = conn
        .call_method(
            Some(service_name),
            format!("/Sessions/{session_id}"),
            Some("org.kde.konsole.Session"),
            "processId",
            &(),
        )
        .await
        .context(GetSessionPidCtx)?
        .body()
        .deserialize()
        .context(GetSessionPidCtx)?;
    fs::read_link(format!("/proc/{pid}/cwd"))
        .await
        .context(GetProcessCwdCtx { pid })
}

async fn get_service_pid(conn: &Connection, service_name: &str) -> zbus::Result<u32> {
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/",
        Some("org.freedesktop.DBus"),
        "GetConnectionUnixProcessID",
        &(service_name),
    )
    .await?
    .body()
    .deserialize()
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum ActivateWindowsError {
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

async fn activate_windows(conn: &Connection, pid: u32) -> Result<(), ActivateWindowsError> {
    let temp_path = spawn_blocking(move || {
        #[derive(Template)]
        #[template(path = "activate_windows.js.jinja", escape = "none")]
        struct Template {
            pid: u32,
        }
        let mut file = NamedTempFile::with_prefix(".konsoleat-").context(CreateTempfileCtx)?;
        Template { pid }.write_into(&mut file).context(RenderTemplateCtx)?;
        Ok(file.into_temp_path())
    })
    .await
    .expect("failed to execute blocking task")?;
    let script_id = load_script(conn, &temp_path).await.context(LoadScriptCtx)?;
    let object_path = format!("/Scripting/Script{script_id}");
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
