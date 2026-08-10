// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use crate::window::Window;
use procfs::{ProcError, process::Process};
use snafu::{ResultExt, Snafu};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Session {
    pub window: Window,
    /// Konsole registers sessions under `/Sessions/<sessionId>`; `sessionId` is a C++ `int` and maps to Rust `i32`.
    /// See: <https://github.com/KDE/konsole/blob/v26.07.90/src/session/Session.cpp#L123>.
    pub id: i32,
}

impl Session {
    pub fn set_current_session(&self) -> zbus::Result<()> {
        self.window
            .service
            .conn
            .call_method(
                Some(self.window.service.name.as_ref()),
                format!("/Windows/{}", self.window.id),
                Some("org.kde.konsole.Window"),
                "setCurrentSession",
                &(self.id),
            )?
            .body()
            .deserialize()
    }

    pub fn pid(&self) -> zbus::Result<i32> {
        self.window
            .service
            .conn
            .call_method(
                Some(self.window.service.name.as_ref()),
                format!("/Sessions/{}", self.id),
                Some("org.kde.konsole.Session"),
                "processId",
                &(),
            )?
            .body()
            .deserialize()
    }

    pub fn proc_info(&self) -> Result<SessionProcInfo, GetSessionProcInfoError> {
        use get_session_proc_info_error::*;
        let pid = self.pid().context(PidCtx)?;
        let process = Process::new(pid).context(HandleCtx { pid })?;
        Ok(SessionProcInfo {
            cwd: process.cwd().context(CwdCtx)?,
            starttime: process.stat().context(StatCtx)?.starttime,
        })
    }
}

#[derive(Debug, Snafu)]
#[snafu(module, context(suffix(Ctx)))]
pub enum GetSessionProcInfoError {
    #[snafu(display("Failed to get process ID of the session"))]
    Pid { source: zbus::Error },
    #[snafu(display("Failed to construct a process handle"))]
    Handle { source: ProcError, pid: i32 },
    #[snafu(display("Failed to get the current working directory of the process"))]
    Cwd { source: ProcError },
    #[snafu(display("Failed to get stat info of the process"))]
    Stat { source: ProcError },
}

pub struct SessionProcInfo {
    pub cwd: PathBuf,
    pub starttime: u64,
}
