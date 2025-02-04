/*
 * Copyright (c) 2020-2022, Stalwart Labs Ltd.
 *
 * This file is part of the Stalwart JMAP Server.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of
 * the License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 * in the LICENSE file at the top-level directory of this distribution.
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * You can be released from the requirements of the AGPLv3 license by
 * purchasing a commercial license. Please contact licensing@stalw.art
 * for more details.
*/

use crate::core::bitmap::Bitmap;
use crate::core::collection::Collection;
use crate::serialize::leb128::Leb128Iterator;
use crate::serialize::StoreDeserialize;
use crate::write::batch;
use crate::AccountId;
use std::convert::TryInto;

#[derive(Debug)]
pub enum Entry {
    Item {
        account_id: AccountId,
        changed_collections: Bitmap<Collection>,
    },
    Snapshot {
        changed_accounts: Vec<(Bitmap<Collection>, Vec<AccountId>)>,
    },
}

impl Entry {
    pub fn next_account(&mut self) -> Option<(AccountId, Bitmap<Collection>)> {
        match self {
            Entry::Item {
                account_id,
                changed_collections,
            } => {
                if !changed_collections.is_empty() {
                    Some((*account_id, changed_collections.clear()))
                } else {
                    None
                }
            }
            Entry::Snapshot { changed_accounts } => loop {
                let (collections, account_ids) = changed_accounts.last_mut()?;
                if let Some(account_id) = account_ids.pop() {
                    return Some((account_id, collections.clone()));
                } else {
                    changed_accounts.pop();
                }
            },
        }
    }
}

impl StoreDeserialize for Entry {
    fn deserialize(bytes: &[u8]) -> Option<Self> {
        match *bytes.first()? {
            batch::Change::ENTRY => Entry::Item {
                account_id: AccountId::from_le_bytes(
                    bytes
                        .get(1..1 + std::mem::size_of::<AccountId>())?
                        .try_into()
                        .ok()?,
                ),
                changed_collections: u64::from_le_bytes(
                    bytes
                        .get(1 + std::mem::size_of::<AccountId>()..)?
                        .try_into()
                        .ok()?,
                )
                .into(),
            },
            batch::Change::SNAPSHOT => {
                let mut bytes_it = bytes.get(1..)?.iter();
                let total_collections = bytes_it.next_leb128()?;
                let mut changed_accounts = Vec::with_capacity(total_collections);

                for _ in 0..total_collections {
                    let collections = bytes_it.next_leb128::<u64>()?.into();
                    let total_accounts = bytes_it.next_leb128()?;
                    let mut accounts = Vec::with_capacity(total_accounts);

                    for _ in 0..total_accounts {
                        accounts.push(bytes_it.next_leb128()?);
                    }

                    changed_accounts.push((collections, accounts));
                }

                Entry::Snapshot { changed_accounts }
            }
            _ => {
                return None;
            }
        }
        .into()
    }
}
