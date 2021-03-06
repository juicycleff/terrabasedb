/*
 * Created on Mon Jul 13 2020
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

//! # The core database engine

use crate::config::BGSave;
use crate::config::SnapshotConfig;
use crate::diskstore;
use crate::protocol::Connection;
use crate::protocol::Query;
use crate::queryengine;
use bytes::Bytes;
use diskstore::PERSIST_FILE;
use libtdb::TResult;
use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;
use parking_lot::RwLockWriteGuard;
use std::collections::HashMap;
use std::sync::Arc;
use tokio;
use tokio::sync::Notify;

/// This is a thread-safe database handle, which on cloning simply
/// gives another atomic reference to the `shared` which is a `Shared` object
#[derive(Debug, Clone)]
pub struct CoreDB {
    /// The shared object, which contains a `Shared` object wrapped in a thread-safe
    /// RC
    pub shared: Arc<Shared>,
    /// The number of background tasks that should be expected
    ///
    /// This is used by the `Drop` implementation to avoid killing the database in the event
    /// that a background service is still working. The calculation is pretty straightforward:
    /// ```text
    /// 1 (for the current process) + if bgsave is running + if snapshotting is enabled
    /// ```
    /// This should **not be changed** during runtime, and should only be initialized when `CoreDB`
    /// is first initialized
    background_tasks: usize,
}

/// A shared _state_
#[derive(Debug)]
pub struct Shared {
    /// This is used by the `BGSAVE` task. `Notify` is used to signal a task
    /// to wake up
    pub bgsave_task: Notify,
    /// The snapshot service notifier
    pub snapshot_service: Notify,
    /// A `Coretable` wrapped in a R/W lock
    pub table: RwLock<Coretable>,
}

impl Shared {
    /// This task performs a `sync`hronous background save operation
    ///
    /// It runs BGSAVE and then returns control to the caller. The caller is responsible
    /// for periodically calling BGSAVE. This returns `false`, **if** the database
    /// is shutting down. Otherwise `true` is returned
    pub fn run_bgsave(&self) -> bool {
        let rlock = self.table.read();
        if rlock.terminate {
            return false;
        }
        // Kick in BGSAVE
        match diskstore::flush_data(PERSIST_FILE, rlock.get_ref()) {
            Ok(_) => log::info!("BGSAVE completed successfully"),
            Err(e) => log::error!("BGSAVE failed with error: '{}'", e),
        }
        true
    }
    /// Check if the server has received a termination signal
    pub fn is_termsig(&self) -> bool {
        self.table.read().terminate
    }
}

/// The `Coretable` holds all the key-value pairs in a `HashMap`
/// and the `terminate` field, which when set to true will cause all other
/// background tasks to terminate
#[derive(Debug)]
pub struct Coretable {
    /// The core table contain key-value pairs
    coremap: HashMap<String, Data>,
    /// The termination signal flag
    pub terminate: bool,
}

impl Coretable {
    /// Get a reference to the inner `HashMap`
    pub const fn get_ref<'a>(&'a self) -> &'a HashMap<String, Data> {
        &self.coremap
    }
    /// Get a **mutable** reference to the inner `HashMap`
    pub fn get_mut_ref<'a>(&'a mut self) -> &'a mut HashMap<String, Data> {
        &mut self.coremap
    }
}

/// A wrapper for `Bytes`
#[derive(Debug, PartialEq, Clone)]
pub struct Data {
    /// The blob of data
    blob: Bytes,
}

impl Data {
    /// Create a new blob from a string
    pub fn from_string(val: String) -> Self {
        Data {
            blob: Bytes::from(val.into_bytes()),
        }
    }
    /// Create a new blob from an existing `Bytes` instance
    pub const fn from_blob(blob: Bytes) -> Self {
        Data { blob }
    }
    /// Get the inner blob (raw `Bytes`)
    pub const fn get_blob(&self) -> &Bytes {
        &self.blob
    }
}

impl CoreDB {
    #[cfg(debug_assertions)]
    /// Flush the coretable entries when in debug mode
    pub fn print_debug_table(&self) {
        if self.acquire_read().coremap.len() == 0 {
            println!("In-memory table is empty");
        } else {
            println!("{:#?}", self.acquire_read());
        }
    }

    /// Returns the expected `Arc::strong_count` for the `CoreDB` object
    pub fn expected_strong_count(&self) -> usize {
        self.background_tasks + 1
    }

    /// Execute a query that has already been validated by `Connection::read_query`
    pub async fn execute_query(&self, query: Query, con: &mut Connection) -> TResult<()> {
        match query {
            Query::Simple(q) => queryengine::execute_simple(&self, con, q).await?,
            // TODO(@ohsayan): Pipeline commands haven't been implemented yet
            Query::Pipelined(_) => unimplemented!(),
        }
        // Once we're done executing, flush the stream
        con.flush_stream().await
    }

    /// Create a new `CoreDB` instance
    ///
    /// This also checks if a local backup of previously saved data is available.
    /// If it is - it restores the data. Otherwise it creates a new in-memory table
    pub fn new(bgsave: BGSave, snapshot_cfg: SnapshotConfig) -> TResult<Self> {
        let coretable = diskstore::get_saved(Some(PERSIST_FILE))?;
        let background_tasks: usize =
            snapshot_cfg.is_enabled() as usize + !bgsave.is_disabled() as usize;
        let db = if let Some(coretable) = coretable {
            CoreDB {
                shared: Arc::new(Shared {
                    bgsave_task: Notify::new(),
                    table: RwLock::new(Coretable {
                        coremap: coretable,
                        terminate: false,
                    }),
                    snapshot_service: Notify::new(),
                }),
                background_tasks,
            }
        } else {
            CoreDB::new_empty(background_tasks)
        };
        // Spawn the background save task in a separate task
        tokio::spawn(diskstore::bgsave_scheduler(db.clone(), bgsave));
        // Spawn the snapshot service in a separate task
        tokio::spawn(diskstore::snapshot::snapshot_service(
            db.clone(),
            snapshot_cfg,
        ));
        Ok(db)
    }
    /// Create an empty in-memory table
    pub fn new_empty(background_tasks: usize) -> Self {
        CoreDB {
            shared: Arc::new(Shared {
                bgsave_task: Notify::new(),
                table: RwLock::new(Coretable {
                    coremap: HashMap::<String, Data>::new(),
                    terminate: false,
                }),
                snapshot_service: Notify::new(),
            }),
            background_tasks,
        }
    }
    /// Acquire a write lock
    pub fn acquire_write(&self) -> RwLockWriteGuard<'_, Coretable> {
        self.shared.table.write()
    }
    /// Acquire a read lock
    pub fn acquire_read(&self) -> RwLockReadGuard<'_, Coretable> {
        self.shared.table.read()
    }
    /// Flush the contents of the in-memory table onto disk
    pub fn flush_db(&self) -> TResult<()> {
        let data = &self.acquire_write();
        diskstore::flush_data(PERSIST_FILE, &data.coremap)?;
        Ok(())
    }

    /// Get a deep copy of the `HashMap`
    ///
    /// **⚠ Do note**: This is super inefficient since it performs an actual
    /// clone of the `HashMap` and doesn't do any `Arc`-business! This function
    /// can be used by test functions and the server, but **use with caution!**
    pub fn get_hashmap_deep_clone(&self) -> HashMap<String, Data> {
        (*self.acquire_read().get_ref()).clone()
    }

    #[cfg(test)]
    /// **⚠⚠⚠ This deletes everything stored in the in-memory table**
    pub fn finish_db(&self) {
        self.acquire_write().coremap.clear()
    }
}

impl Drop for CoreDB {
    // This prevents us from killing the database, in the event someone tries
    // to access it
    // If this is indeed the last DB instance, we should tell BGSAVE and the snapshot
    // service to quit
    fn drop(&mut self) {
        // If the strong count is equal to the `expected_strong_count()`
        // then the background services are still running, so don't terminate
        // the database
        if Arc::strong_count(&self.shared) == self.expected_strong_count() {
            // Acquire a lock to prevent anyone from writing something
            let mut coretable = self.shared.table.write();
            coretable.terminate = true;
            // Drop the write lock first to avoid BGSAVE ending up in failing
            // to get a read lock
            drop(coretable);
            // Notify the background tasks to quit
            self.shared.bgsave_task.notify();
            self.shared.snapshot_service.notify();
        }
    }
}
