/*
 * Created on Fri Aug 14 2020
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

//! # `SET` queries
//! This module provides functions to work with `SET` queries

use crate::coredb::{self, CoreDB};
use crate::protocol::{responses, ActionGroup, Connection};
use coredb::Data;
use libtdb::TResult;
use std::collections::hash_map::Entry;
use std::hint::unreachable_unchecked;

/// Run a `SET` query
pub async fn set(handle: &CoreDB, con: &mut Connection, act: ActionGroup) -> TResult<()> {
    let howmany = act.howmany();
    if howmany != 2 {
        // There should be exactly 2 arguments
        return con
            .write_response(responses::fresp::R_ACTION_ERR.to_owned())
            .await;
    }
    let mut it = act.into_iter();
    let did_we = {
        let mut whandle = handle.acquire_write();
        let writer = whandle.get_mut_ref();
        if let Entry::Vacant(e) = writer.entry(
            it.next()
                .unwrap_or_else(|| unsafe { unreachable_unchecked() }),
        ) {
            e.insert(Data::from_string(
                it.next()
                    .unwrap_or_else(|| unsafe { unreachable_unchecked() }),
            ));
            true
        } else {
            false
        }
    };
    if did_we {
        con.write_response(responses::fresp::R_OKAY.to_owned())
            .await?;
    } else {
        con.write_response(responses::fresp::R_OVERWRITE_ERR.to_owned())
            .await?;
    }
    #[cfg(debug_assertions)]
    {
        handle.print_debug_table();
    }
    Ok(())
}
