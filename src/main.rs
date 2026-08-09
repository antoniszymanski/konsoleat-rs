// SPDX-FileCopyrightText: 2025 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use askama::Template;
use clap::Parser;
use procfs::{ProcError, process::Process};
use same_file::Handle;
use snafu::{ResultExt, Snafu};
use std::{
    fs, io,
    num::ParseIntError,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::NamedTempFile;
use zbus::blocking::Connection;

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
    #[snafu(display("Failed to get process info of the session"))]
    GetSessionProcInfo { source: GetSessionProcInfoError },
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
fn main() -> Result<(), Error> {
    let mut cli = Cli::parse();
    cli.workdir = fs::canonicalize(cli.workdir).context(CanonicalizeWorkdirCtx)?;

    let workdir_handle = Handle::from_path(&cli.workdir).context(ConstructHandleFromWorkdirCtx)?;
    let conn = &Connection::session().context(ConnectToSessionBusCtx)?;

    let mut oldest_window = None;
    let mut best_session = None;

    let services = list_services(conn).context(ListServicesCtx)?;
    for service_name in services {
        let windows = list_windows(conn, &service_name).context(ListWindowsCtx)?;
        for window_id in windows {
            let current_session = get_current_session(conn, &service_name, &window_id).context(GetCurrentSessionCtx)?;
            let sessions = list_sessions(conn, &service_name, &window_id).context(ListSessionsCtx)?;
            for session_id in sessions {
                let proc_info =
                    get_session_proc_info(conn, &service_name, session_id).context(GetSessionProcInfoCtx)?;
                consider_window_candidate(
                    &mut oldest_window,
                    AnnotatedWindow {
                        service_name: service_name.clone(),
                        window_id: window_id.clone(),
                        starttime: proc_info.starttime,
                    },
                );
                let handle = match Handle::from_path(&proc_info.cwd) {
                    Ok(v) => Ok(v),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => Err(e),
                }
                .context(ConstructHandleFromPathCtx { path: proc_info.cwd })?;
                if handle != workdir_handle {
                    continue;
                }
                consider_session_candidate(
                    &mut best_session,
                    AnnotatedSession {
                        service_name: service_name.clone(),
                        window_id: window_id.clone(),
                        session_id,
                        is_current: session_id == current_session,
                        starttime: proc_info.starttime,
                    },
                )
            }
        }
    }

    if let Some(session) = best_session {
        if !session.is_current {
            set_current_session(conn, &session.service_name, &session.window_id, session.session_id)
                .context(SetCurrentSessionCtx)?;
        }
        let pid = get_service_pid(conn, &session.service_name).context(GetServicePidCtx)?;
        return activate_windows(conn, pid).context(ActivateWindowsCtx);
    }

    let pid = match oldest_window {
        Some(window) => {
            let session_id =
                new_session(conn, &window.service_name, &window.window_id, &cli.workdir).context(CreateSessionCtx)?;
            set_current_session(conn, &window.service_name, &window.window_id, session_id)
                .context(SetCurrentSessionCtx)?;
            get_service_pid(conn, &window.service_name).context(GetServicePidCtx)?
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
    activate_windows(conn, pid).context(ActivateWindowsCtx)
}

#[derive(Debug)]
struct AnnotatedWindow {
    service_name: Box<str>,
    window_id: Box<str>,
    starttime: u64,
}

fn consider_window_candidate(best: &mut Option<AnnotatedWindow>, candidate: AnnotatedWindow) {
    let should_replace = best
        .as_ref()
        .map(|best| candidate.starttime < best.starttime)
        .unwrap_or(true);
    if should_replace {
        *best = Some(candidate);
    }
}

#[derive(Debug)]
struct AnnotatedSession {
    service_name: Box<str>,
    window_id: Box<str>,
    session_id: i32,
    is_current: bool,
    starttime: u64,
}

fn consider_session_candidate(best: &mut Option<AnnotatedSession>, candidate: AnnotatedSession) {
    let should_replace = best
        .as_ref()
        .map(|best| (!candidate.is_current, candidate.starttime) < (!best.is_current, best.starttime))
        .unwrap_or(true);
    if should_replace {
        *best = Some(candidate);
    }
}

fn list_services(conn: &Connection) -> zbus::Result<Vec<Box<str>>> {
    Ok(conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )?
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

fn list_windows(conn: &Connection, service_name: &str) -> Result<Vec<Box<str>>, ListWindowsError> {
    let body = conn
        .call_method(
            Some(service_name),
            "/Windows",
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
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

fn list_sessions(conn: &Connection, service_name: &str, window_id: &str) -> Result<Vec<i32>, ListSessionsError> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "sessionList",
        &(),
    )
    .context(CallSessionListCtx)?
    .body()
    .deserialize::<Vec<&str>>()
    .context(CallSessionListCtx)?
    .into_iter()
    .map(|s| s.parse().context(ParseSessionIdCtx { input: s }))
    .collect()
}

fn new_session(conn: &Connection, service_name: &str, window_id: &str, directory: &Path) -> zbus::Result<i32> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "newSession",
        &("" /* default profile */, directory),
    )?
    .body()
    .deserialize()
}

fn get_current_session(conn: &Connection, service_name: &str, window_id: &str) -> zbus::Result<i32> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "currentSession",
        &(),
    )?
    .body()
    .deserialize()
}

fn set_current_session(conn: &Connection, service_name: &str, window_id: &str, session_id: i32) -> zbus::Result<()> {
    conn.call_method(
        Some(service_name),
        format!("/Windows/{window_id}"),
        Some("org.kde.konsole.Window"),
        "setCurrentSession",
        &(session_id),
    )?
    .body()
    .deserialize()
}

#[derive(Debug, Snafu)]
#[snafu(module, context(suffix(Ctx)))]
enum GetSessionProcInfoError {
    #[snafu(display("Failed to get process ID of the session"))]
    Pid { source: zbus::Error },
    #[snafu(display("Failed to construct a process handle"))]
    Handle { source: ProcError, pid: i32 },
    #[snafu(display("Failed to get the current working directory of the process"))]
    Cwd { source: ProcError },
    #[snafu(display("Failed to get stat info of the process"))]
    Stat { source: ProcError },
}

struct SessionProcInfo {
    cwd: PathBuf,
    starttime: u64,
}

fn get_session_proc_info(
    conn: &Connection,
    service_name: &str,
    session_id: i32,
) -> Result<SessionProcInfo, GetSessionProcInfoError> {
    use get_session_proc_info_error::*;
    let pid = get_session_pid(conn, service_name, session_id).context(PidCtx)?;
    let process = Process::new(pid).context(HandleCtx { pid })?;
    Ok(SessionProcInfo {
        cwd: process.cwd().context(CwdCtx)?,
        starttime: process.stat().context(StatCtx)?.starttime,
    })
}

fn get_session_pid(conn: &Connection, service_name: &str, session_id: i32) -> zbus::Result<i32> {
    conn.call_method(
        Some(service_name),
        format!("/Sessions/{session_id}"),
        Some("org.kde.konsole.Session"),
        "processId",
        &(),
    )?
    .body()
    .deserialize()
}

fn get_service_pid(conn: &Connection, service_name: &str) -> zbus::Result<u32> {
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/",
        Some("org.freedesktop.DBus"),
        "GetConnectionUnixProcessID",
        &(service_name),
    )?
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

fn activate_windows(conn: &Connection, pid: u32) -> Result<(), ActivateWindowsError> {
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
