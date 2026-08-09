// SPDX-FileCopyrightText: 2025 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use crate::{
    service::{ListWindowsError, list_services},
    session::{GetSessionProcInfoError, Session},
    window::{ListSessionsError, Window},
};
use activate_windows::{ActivateWindowsError, activate_windows};
use clap::Parser;
use same_file::Handle;
use snafu::{ResultExt, Snafu};
use std::{
    fs, io,
    ops::Deref,
    path::PathBuf,
    process::{Command, Stdio},
};
use zbus::blocking::Connection;

mod activate_windows;
mod service;
mod session;
mod window;

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

    for service in list_services(conn).context(ListServicesCtx)? {
        for window in service.windows().context(ListWindowsCtx)? {
            let current_session = window.current_session().context(GetCurrentSessionCtx)?;
            for session in window.sessions().context(ListSessionsCtx)? {
                let proc_info = session.proc_info().context(GetSessionProcInfoCtx)?;
                consider_window(&mut oldest_window, &window, proc_info.starttime);
                let handle = match Handle::from_path(&proc_info.cwd) {
                    Ok(v) => Ok(v),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => Err(e),
                }
                .context(ConstructHandleFromPathCtx { path: proc_info.cwd })?;
                if handle != workdir_handle {
                    continue;
                }
                let is_current = session.id == current_session.id;
                consider_session(&mut best_session, &session, proc_info.starttime, is_current)
            }
        }
    }

    if let Some(best_session) = best_session {
        if !best_session.is_current {
            best_session.set_current_session().context(SetCurrentSessionCtx)?
        }
        let pid = best_session.window.service.pid().context(GetServicePidCtx)?;
        return activate_windows(conn, pid).context(ActivateWindowsCtx);
    }

    let pid = match oldest_window {
        Some(oldest_window) => {
            let session = oldest_window.new_session(&cli.workdir).context(CreateSessionCtx)?;
            session.set_current_session().context(SetCurrentSessionCtx)?;
            session.window.service.pid().context(GetServicePidCtx)?
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
    window: Window,
    starttime: u64,
}

impl Deref for AnnotatedWindow {
    type Target = Window;

    fn deref(&self) -> &Self::Target {
        &self.window
    }
}

fn consider_window(best: &mut Option<AnnotatedWindow>, window: &Window, starttime: u64) {
    let should_replace = best.as_ref().map(|best| starttime < best.starttime).unwrap_or(true);
    if should_replace {
        *best = Some(AnnotatedWindow {
            window: window.clone(),
            starttime,
        });
    }
}

#[derive(Debug)]
struct AnnotatedSession {
    session: Session,
    starttime: u64,
    is_current: bool,
}

impl Deref for AnnotatedSession {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

fn consider_session(best: &mut Option<AnnotatedSession>, session: &Session, starttime: u64, is_current: bool) {
    let should_replace = best
        .as_ref()
        .map(|best| (!is_current, starttime) < (!best.is_current, best.starttime))
        .unwrap_or(true);
    if should_replace {
        *best = Some(AnnotatedSession {
            session: session.clone(),
            starttime,
            is_current,
        });
    }
}
