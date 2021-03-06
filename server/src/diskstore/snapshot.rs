/*
 * Created on Thu Oct 01 2020
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

//! Tools for creating snapshots

use crate::config::SnapshotConfig;
use crate::coredb::CoreDB;
use crate::diskstore;
use chrono::prelude::*;
use libtdb::TResult;
use std::fs;
use std::io::ErrorKind;

const DIR_SNAPSHOT: &'static str = "snapshots";
/// The default snapshot count is 12, assuming that the user would take a snapshot
/// every 2 hours (or 7200 seconds)
const DEF_SNAPSHOT_COUNT: usize = 12;

/// # Snapshot Engine
///
/// This object provides methods to create and delete snapshots. There should be a
/// `snapshot_scheduler` which should hold an instance of this object, on startup.
/// Whenever the duration expires, the caller should call `mksnap()`
pub struct SnapshotEngine<'a> {
    /// File names of the snapshots (relative paths)
    snaps: queue::Queue,
    /// An atomic reference to the coretable
    dbref: &'a CoreDB,
}

impl<'a> SnapshotEngine<'a> {
    /// Create a new `Snapshot` instance
    ///
    /// This also attempts to check if the snapshots directory exists;
    /// If the directory doesn't exist, then it is created
    pub fn new<'b: 'a>(maxtop: usize, dbref: &'b CoreDB) -> TResult<Self> {
        match fs::create_dir(DIR_SNAPSHOT) {
            Ok(_) => (),
            Err(e) => match e.kind() {
                ErrorKind::AlreadyExists => (),
                _ => return Err(e.into()),
            },
        }
        Ok(SnapshotEngine {
            snaps: queue::Queue::new(if maxtop == 0 {
                (DEF_SNAPSHOT_COUNT, true)
            } else {
                (maxtop, false)
            }),
            dbref,
        })
    }
    /// Generate the snapshot name
    fn get_snapname(&self) -> String {
        Utc::now()
            .format("./snapshots/%Y%m%d-%H%M%S.snapshot")
            .to_string()
    }
    /// Create a snapshot
    pub fn mksnap(&mut self) -> bool {
        let rlock = self.dbref.acquire_read();
        if rlock.terminate {
            // The database is shutting down, don't create a snapshot
            return false;
        }
        let snapname = self.get_snapname();
        if let Err(e) = diskstore::flush_data(&snapname, &rlock.get_ref()) {
            log::error!("Snapshotting failed with error: '{}'", e);
            return true;
        } else {
            log::info!("Successfully created snapshot");
        }
        // Release the read lock for the poor clients who are waiting for a write lock
        drop(rlock);
        log::info!("Snapshot created");
        if let Some(old_snapshot) = self.snaps.add(snapname.clone()) {
            if let Err(e) = fs::remove_file(old_snapshot) {
                log::error!(
                    "Failed to delete snapshot '{}' with error '{}'",
                    snapname,
                    e
                );
            } else {
                log::info!("Successfully removed old snapshot");
            }
        }
        true
    }
    /// Delete all snapshots
    pub fn clearall(&mut self) -> TResult<()> {
        for snap in self.snaps.iter() {
            fs::remove_file(snap)?;
        }
        Ok(())
    }
    /// Get the name of snapshots
    pub fn get_snapshots(&self) -> std::slice::Iter<String> {
        self.snaps.iter()
    }
}

#[test]
fn test_snapshot() {
    let db = CoreDB::new_empty(3);
    let mut write = db.acquire_write();
    let _ = write.get_mut_ref().insert(
        String::from("ohhey"),
        crate::coredb::Data::from_string(String::from("heya!")),
    );
    drop(write);
    let mut snapengine = SnapshotEngine::new(4, &db).unwrap();
    let _ = snapengine.mksnap();
    let current = snapengine.get_snapshots().next().unwrap();
    let read_hmap = diskstore::get_saved(Some(current)).unwrap().unwrap();
    let dbhmap = db.get_hashmap_deep_clone();
    assert_eq!(read_hmap, dbhmap);
    snapengine.clearall().unwrap();
}

use std::time::Duration;
use tokio::time;
/// The snapshot service
///
/// This service calls `SnapEngine::mksnap()` periodically to create snapshots. Whenever
/// the interval for snapshotting expires or elapses, we create a snapshot. The snapshot service
/// keeps creating snapshots, as long as the database keeps running, i.e `CoreDB` does return true for
/// `is_termsig()`
pub async fn snapshot_service(handle: CoreDB, ss_config: SnapshotConfig) {
    match ss_config {
        SnapshotConfig::Disabled => {
            // since snapshotting is disabled, we'll imediately return
            handle.shared.bgsave_task.notified().await;
            return;
        }
        SnapshotConfig::Enabled(configuration) => {
            let (duration, atmost) = configuration.decompose();
            let duration = Duration::from_secs(duration);
            let mut sengine = match SnapshotEngine::new(atmost, &handle) {
                Ok(ss) => ss,
                Err(e) => {
                    log::error!("Failed to initialize snapshot service with error: '{}'", e);
                    return;
                }
            };
            while !handle.shared.is_termsig() {
                if sengine.mksnap() {
                    tokio::select! {
                        _ = time::delay_until(time::Instant::now() + duration) => {},
                        _ = handle.shared.bgsave_task.notified() => {}
                    }
                } else {
                    handle.shared.bgsave_task.notified().await;
                }
            }
        }
    }
}

mod queue {
    //! An extremely simple queue implementation which adds more items to the queue
    //! freely and once the threshold limit is reached, it pops off the oldest element and returns it
    //!
    //! This implementation is specifically built for use with the snapshotting utility
    use std::slice::Iter;
    #[derive(Debug, PartialEq)]
    pub struct Queue {
        queue: Vec<String>,
        maxlen: usize,
        dontpop: bool,
    }
    impl Queue {
        pub fn new((maxlen, dontpop): (usize, bool)) -> Self {
            Queue {
                queue: Vec::with_capacity(maxlen),
                maxlen,
                dontpop,
            }
        }
        /// This returns a `String` only if the queue is full. Otherwise, a `None` is returned most of the time
        pub fn add(&mut self, item: String) -> Option<String> {
            if self.dontpop {
                // We don't need to pop anything since the user
                // wants to keep all the items in the queue
                self.queue.push(item);
                return None;
            } else {
                // The user wants to keep a maximum of `maxtop` items
                // so we will check if the current queue is full
                // if it is full, then the `maxtop` limit has been reached
                // so we will remove the oldest item and then push the
                // new item onto the stack
                let x = if self.is_overflow() { self.pop() } else { None };
                self.queue.push(item);
                x
            }
        }
        /// Returns an iterator over the slice of strings
        pub fn iter(&self) -> Iter<String> {
            self.queue.iter()
        }
        /// Check if we have reached the maximum queue size limit
        fn is_overflow(&self) -> bool {
            self.queue.len() == self.maxlen
        }
        /// Remove the last item inserted
        fn pop(&mut self) -> Option<String> {
            if self.queue.len() != 0 {
                Some(self.queue.remove(0))
            } else {
                None
            }
        }
    }

    #[test]
    fn test_queue() {
        let mut q = Queue::new((4, false));
        assert!(q.add(String::from("snap1")).is_none());
        assert!(q.add(String::from("snap2")).is_none());
        assert!(q.add(String::from("snap3")).is_none());
        assert!(q.add(String::from("snap4")).is_none());
        assert_eq!(q.add(String::from("snap5")), Some(String::from("snap1")));
        assert_eq!(q.add(String::from("snap6")), Some(String::from("snap2")));
    }

    #[test]
    fn test_queue_dontpop() {
        // This means that items can only be added or all of them can be deleted
        let mut q = Queue::new((4, true));
        assert!(q.add(String::from("snap1")).is_none());
        assert!(q.add(String::from("snap2")).is_none());
        assert!(q.add(String::from("snap3")).is_none());
        assert!(q.add(String::from("snap4")).is_none());
        assert!(q.add(String::from("snap5")).is_none());
        assert!(q.add(String::from("snap6")).is_none());
    }
}
