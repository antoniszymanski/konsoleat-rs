// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use crate::window::Window;
use snafu::{ResultExt, Snafu};
use std::{fmt, num::ParseIntError, rc::Rc};
use zbus::blocking::Connection;

pub fn list_services(conn: &Connection) -> zbus::Result<impl Iterator<Item = Service>> {
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
        .collect::<Vec<_>>()
        .into_iter()
        .map(|service_name| Service {
            conn: conn.clone(),
            name: service_name,
        }))
}

#[derive(Clone)]
pub struct Service {
    pub conn: Connection,
    pub name: Rc<str>,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Snafu)]
#[snafu(context(suffix(Ctx)))]
pub enum ListWindowsError {
    #[snafu(display("Failed to get introspection data"))]
    GetIntrospection { source: zbus::Error },
    #[snafu(display("Failed to parse XML introspection data"))]
    ParseIntrospection { source: zbus_xml::Error },
    #[snafu(display("Failed to parse window ID {input:?} as i32"))]
    ParseWindowId { source: ParseIntError, input: String },
}

impl Service {
    pub fn windows(&self) -> Result<impl Iterator<Item = Window>, ListWindowsError> {
        let body = self
            .conn
            .call_method(
                Some(self.name.as_ref()),
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
            .map(|s| s.parse().context(ParseWindowIdCtx { input: s }))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|window_id| Window {
                service: self.clone(),
                id: window_id,
            }))
    }

    pub fn pid(&self) -> zbus::Result<u32> {
        self.conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/",
                Some("org.freedesktop.DBus"),
                "GetConnectionUnixProcessID",
                &(self.name.as_ref()),
            )?
            .body()
            .deserialize()
    }
}
