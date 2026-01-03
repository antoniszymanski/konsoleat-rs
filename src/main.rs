// SPDX-FileCopyrightText: 2025 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use askama::Template;
use clap::Parser;
use procfs::process::Process;
use snafu::{ResultExt, Snafu};
use std::{
    io,
    num::ParseIntError,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::NamedTempFile;
use tokio::fs;
use zbus::{Connection, proxy};

/// Activate or create a Konsole terminal session in a specified working directory
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[arg(default_value = ".")]
    workdir: PathBuf,
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum Error {
    #[snafu(display("Failed to canonicalize workdir"))]
    CanonicalizeWorkdir { source: io::Error },
    #[snafu(display("Failed to create a DBus connection to the session message bus"))]
    ConnectToSessionBus { source: zbus::Error },
    #[snafu(display("Failed to list DBus services"))]
    ListServices { source: zbus::Error },
    #[snafu(display("Failed to list windows of the service"))]
    ListWindows { source: ListWindowsError },
    #[snafu(display("Failed to get current session"))]
    GetCurrentSession { source: zbus::Error },
    #[snafu(display("Failed to list sessions of the window"))]
    ListSessions { source: ListSessionsError },
    #[snafu(display("Failed to get current working directory of the session"))]
    GetSessionCwd { source: GetSessionCwdError },
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

    let conn = Connection::session().await.context(ConnectToSessionBusCtx)?;
    let mut first_window = None;

    let services = list_services(&conn).await.context(ListServicesCtx)?;
    for service_name in services {
        let windows = list_windows(&conn, &service_name).await.context(ListWindowsCtx)?;
        for window_id in windows {
            if first_window.is_none() {
                first_window = Some((service_name.clone(), window_id.clone()))
            }
            let current_session = get_current_session(&conn, &service_name, &window_id)
                .await
                .context(GetCurrentSessionCtx)?;
            let cwd = get_session_cwd(&conn, &service_name, current_session)
                .await
                .context(GetSessionCwdCtx)?;
            if cli.workdir == cwd {
                let pid = get_service_pid(&conn, &service_name).await.context(GetServicePidCtx)?;
                return activate_windows(&conn, pid).await.context(ActivateWindowsCtx);
            }
            let sessions = list_sessions(&conn, &service_name, &window_id)
                .await
                .context(ListSessionsCtx)?;
            for session_id in sessions {
                let cwd = get_session_cwd(&conn, &service_name, session_id)
                    .await
                    .context(GetSessionCwdCtx)?;
                if cli.workdir == cwd {
                    set_current_session(&conn, &service_name, &window_id, session_id)
                        .await
                        .context(SetCurrentSessionCtx)?;
                    let pid = get_service_pid(&conn, &service_name).await.context(GetServicePidCtx)?;
                    return activate_windows(&conn, pid).await.context(ActivateWindowsCtx);
                }
            }
        }
    }

    let pid = match first_window {
        Some((service_name, window_id)) => {
            let session_id = new_session(&conn, &service_name, &window_id, &cli.workdir)
                .await
                .context(CreateSessionCtx)?;
            set_current_session(&conn, &service_name, &window_id, session_id)
                .await
                .context(SetCurrentSessionCtx)?;
            get_service_pid(&conn, &service_name).await.context(GetServicePidCtx)?
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
    activate_windows(&conn, pid).await.context(ActivateWindowsCtx)
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
    #[snafu(display("Failed to call DBus method \"sessionList\""))]
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

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
enum GetSessionCwdError {
    #[snafu(display("Failed to get process ID of the session"))]
    GetSessionPid { source: zbus::Error },
    #[snafu(display("Failed to read current working directory for process ID {pid}"))]
    ReadProcessCwd { source: procfs::ProcError, pid: i32 },
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
    Process::new(pid)
        .context(ReadProcessCwdCtx { pid })?
        .cwd()
        .context(ReadProcessCwdCtx { pid })
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
    .await
    .map(|_| ())
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
    #[snafu(display("Failed to create a ScriptingProxy"))]
    CreateScriptingProxy { source: zbus::Error },
    #[snafu(display("Failed to load a KWin script"))]
    LoadScript { source: zbus::Error },
    #[snafu(display("Failed to create a ScriptProxy"))]
    CreateScriptProxy { source: zbus::Error },
    #[snafu(display("Failed to run the script"))]
    RunScript { source: zbus::Error },
    #[snafu(display("Failed to stop the script"))]
    StopScript { source: zbus::Error },
    #[snafu(display("Failed to unload a KWin script"))]
    UnloadScript { source: zbus::Error },
}

async fn activate_windows(conn: &Connection, pid: u32) -> Result<(), ActivateWindowsError> {
    #[derive(Template)]
    #[template(path = "activate_windows.js.jinja", escape = "none")]
    struct Template {
        pid: u32,
    }
    let mut file = NamedTempFile::with_prefix("konsole-at-").context(CreateTempfileCtx)?;
    Template { pid }.write_into(&mut file).context(RenderTemplateCtx)?;
    let scripting_proxy = ScriptingProxy::new(conn).await.context(CreateScriptingProxyCtx)?;
    let plugin_name = file.path();
    let script_id = scripting_proxy
        .load_script(file.path(), plugin_name)
        .await
        .context(LoadScriptCtx)?;
    let script_proxy = ScriptProxy::new(conn, format!("/Scripting/Script{script_id}"))
        .await
        .context(CreateScriptProxyCtx)?;
    script_proxy.run().await.context(RunScriptCtx)?;
    script_proxy.stop().await.context(StopScriptCtx)?;
    scripting_proxy
        .unload_script(plugin_name)
        .await
        .context(UnloadScriptCtx)?;
    Ok(())
}

#[proxy(
    interface = "org.kde.kwin.Scripting",
    default_service = "org.kde.KWin",
    default_path = "/Scripting"
)]
trait Scripting {
    #[zbus(name = "loadScript")]
    fn load_script(&self, file_path: &Path, plugin_name: &Path) -> zbus::Result<i32>;

    #[zbus(name = "unloadScript")]
    fn unload_script(&self, plugin_name: &Path) -> zbus::Result<bool>;

    #[zbus(name = "isScriptLoaded")]
    fn is_script_loaded(&self, plugin_name: &Path) -> zbus::Result<bool>;
}

#[proxy(interface = "org.kde.kwin.Script", default_service = "org.kde.KWin")]
trait Script {
    #[zbus(name = "run")]
    fn run(&self) -> zbus::Result<()>;

    #[zbus(name = "stop")]
    fn stop(&self) -> zbus::Result<()>;
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
