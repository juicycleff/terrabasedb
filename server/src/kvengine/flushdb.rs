/*
 * Created on Thu Sep 24 2020
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

use crate::coredb::CoreDB;
use crate::protocol::{responses, ActionGroup, Connection};
use libtdb::TResult;

/// Delete all the keys in the database
pub async fn flushdb(handle: &CoreDB, con: &mut Connection, act: ActionGroup) -> TResult<()> {
    if act.howmany() != 0 {
        return con
            .write_response(responses::fresp::R_ACTION_ERR.to_owned())
            .await;
    }
    {
        handle.acquire_write().get_mut_ref().clear()
    }
    con.write_response(responses::fresp::R_OKAY.to_owned())
        .await?;
    Ok(())
}
