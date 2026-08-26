// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use crate::{service::Service, session::Session};
use snafu::{ResultExt, Snafu};
use std::{num::ParseIntError, path::Path};

#[derive(Debug, Clone)]
pub struct Window {
    pub service: Service,
    /// Konsole registers windows under `/Windows/<managerId>`; `managerId` is a C++ `int` and maps to Rust `i32`.
    /// See: <https://github.com/KDE/konsole/blob/v26.07.90/src/ViewManager.cpp#L95>.
    pub id: i32,
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
pub enum ListSessionsError {
    #[snafu(display("Failed to call D-Bus method \"sessionList\""))]
    CallSessionList { source: zbus::Error },
    #[snafu(display("Failed to parse session ID {input:?} as i32"))]
    ParseSessionId { source: ParseIntError, input: String },
}

impl Window {
    pub fn sessions(&self) -> Result<impl Iterator<Item = Session>, ListSessionsError> {
        Ok(self
            .service
            .conn
            .call_method(
                Some(self.service.name.as_ref()),
                format!("/Windows/{}", self.id),
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
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|session_id| Session {
                window: self.clone(),
                id: session_id,
            }))
    }

    pub fn current_session(&self) -> zbus::Result<Session> {
        let session_id = self
            .service
            .conn
            .call_method(
                Some(self.service.name.as_ref()),
                format!("/Windows/{}", self.id),
                Some("org.kde.konsole.Window"),
                "currentSession",
                &(),
            )?
            .body()
            .deserialize()?;
        Ok(Session {
            window: self.clone(),
            id: session_id,
        })
    }

    pub fn new_session(&self, directory: &Path) -> zbus::Result<Session> {
        let session_id = self
            .service
            .conn
            .call_method(
                Some(self.service.name.as_ref()),
                format!("/Windows/{}", self.id),
                Some("org.kde.konsole.Window"),
                "newSession",
                // Konsole uses the default profile when given an empty string.
                // See: <https://github.com/KDE/konsole/blob/v26.07.90/src/ViewManager.cpp#L1523>.
                &("", directory),
            )?
            .body()
            .deserialize()?;
        Ok(Session {
            window: self.clone(),
            id: session_id,
        })
    }
}
