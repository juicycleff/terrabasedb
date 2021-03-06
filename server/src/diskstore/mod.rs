/*
 * Created on Wed Aug 05 2020
 *
 * This file is a part of TerrabaseDB
 * Copyright (c) 2020, Sayan Nandan <ohsayan at outlook dot com>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program. If not, see <https://www.gnu.org/licenses/>.
 *
*/

//! This module provides tools for handling persistently stored data

use crate::config::BGSave;
use crate::coredb::{self, Data};
use bincode;
use bytes::Bytes;
use libtdb::TResult;
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::iter::FromIterator;
use std::time::Duration;
use tokio::time;
mod cyansfw;
pub mod snapshot;

type DiskStore = (Vec<String>, Vec<Vec<u8>>);
pub const PERSIST_FILE: &'static str = "./data.bin";

/// Try to get the saved data from disk. This returns `None`, if the `data.bin` wasn't found
/// otherwise the `data.bin` file is deserialized and parsed into a `HashMap`
pub fn get_saved(location: Option<&str>) -> TResult<Option<HashMap<String, Data>>> {
    let file = match fs::read(if let Some(loc) = location {
        loc
    } else {
        PERSIST_FILE
    }) {
        Ok(f) => f,
        Err(e) => match e.kind() {
            ErrorKind::NotFound => return Ok(None),
            _ => return Err("Couldn't read flushed data from disk".into()),
        },
    };
    let parsed: DiskStore = bincode::deserialize(&file)?;
    let parsed: HashMap<String, Data> = HashMap::from_iter(
        parsed
            .0
            .into_iter()
            .zip(parsed.1.into_iter())
            .map(|(key, value)| {
                let data = Data::from_blob(Bytes::from(value));
                (key, data)
            }),
    );
    Ok(Some(parsed))
}

/// Flush the in-memory table onto disk
///
/// This functions takes the entire in-memory table and writes it to the disk,
/// more specifically, the `data.bin` file
pub fn flush_data(filename: &str, data: &HashMap<String, Data>) -> TResult<()> {
    let ds: DiskStore = (
        data.keys().into_iter().map(|val| val.to_string()).collect(),
        data.values().map(|val| val.get_blob().to_vec()).collect(),
    );
    let encoded = bincode::serialize(&ds)?;
    let mut file = fs::File::create(filename)?;
    file.write_all(&encoded)?;
    Ok(())
}

/// The bgsave_scheduler calls the bgsave task in `CoreDB` after `every` seconds
///
/// The time after which the scheduler will wake up the BGSAVE task is determined by
/// `bgsave_cfg` which is to be passed as an argument. If BGSAVE is disabled, this function
/// immediately returns
pub async fn bgsave_scheduler(handle: coredb::CoreDB, bgsave_cfg: BGSave) {
    let duration = match bgsave_cfg {
        BGSave::Disabled => {
            // So, there's no BGSAVE! Looks like our user's pretty confident
            // that there won't be any power failures! Never mind, we'll just
            // shut down the BGSAVE task, and immediately return
            handle.shared.bgsave_task.notified().await;
            return;
        }
        BGSave::Enabled(duration) => {
            // If we're here - the user doesn't trust his power supply or just values
            // his data - which is good! So we'll turn this into a `Duration`
            Duration::from_secs(duration)
        }
    };
    while !handle.shared.is_termsig() {
        if handle.shared.run_bgsave() {
            tokio::select! {
                // Sleep until `duration` from the current time instant
                _ = time::delay_until(time::Instant::now() + duration) => {}
                // Otherwise wait for a notification
                _ = handle.shared.bgsave_task.notified() => {}
            }
        } else {
            handle.shared.bgsave_task.notified().await
        }
    }
}
