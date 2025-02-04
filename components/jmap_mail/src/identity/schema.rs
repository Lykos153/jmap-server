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

use std::fmt::Display;

use jmap::{orm, types::jmap::JMAPId};
use serde::{Deserialize, Serialize};
use store::{core::vec_map::VecMap, FieldId};

use crate::mail::schema::EmailAddress;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub properties: VecMap<Property, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Value {
    Id { value: JMAPId },
    Text { value: String },
    Bool { value: bool },
    Addresses { value: Vec<EmailAddress> },
    Null,
}

impl Default for Value {
    fn default() -> Self {
        Value::Null
    }
}

impl orm::Value for Value {
    fn index_as(&self) -> orm::Index {
        orm::Index::Null
    }

    fn is_empty(&self) -> bool {
        match self {
            Value::Text { value } => value.is_empty(),
            Value::Null => true,
            _ => false,
        }
    }

    fn len(&self) -> usize {
        match self {
            Value::Id { .. } => std::mem::size_of::<JMAPId>(),
            Value::Text { value } => value.len(),
            Value::Bool { .. } => std::mem::size_of::<bool>(),
            Value::Addresses { value } => value.iter().fold(0, |acc, x| {
                acc + x.email.len() + x.name.as_ref().map(|n| n.len()).unwrap_or(0)
            }),
            Value::Null => 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Copy, Clone)]
#[repr(u8)]
pub enum Property {
    Id = 0,
    Name = 1,
    Email = 2,
    ReplyTo = 3,
    Bcc = 4,
    TextSignature = 5,
    HtmlSignature = 6,
    MayDelete = 7,
    Invalid = 8,
}

impl Property {
    pub fn parse(value: &str) -> Self {
        match value {
            "id" => Property::Id,
            "name" => Property::Name,
            "email" => Property::Email,
            "replyTo" => Property::ReplyTo,
            "bcc" => Property::Bcc,
            "textSignature" => Property::TextSignature,
            "htmlSignature" => Property::HtmlSignature,
            "mayDelete" => Property::MayDelete,
            _ => Property::Invalid,
        }
    }
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::Name => write!(f, "name"),
            Property::Email => write!(f, "email"),
            Property::ReplyTo => write!(f, "replyTo"),
            Property::Bcc => write!(f, "bcc"),
            Property::TextSignature => write!(f, "textSignature"),
            Property::HtmlSignature => write!(f, "htmlSignature"),
            Property::MayDelete => write!(f, "mayDelete"),
            Property::Invalid => Ok(()),
        }
    }
}

impl From<Property> for FieldId {
    fn from(property: Property) -> Self {
        property as FieldId
    }
}

impl From<FieldId> for Property {
    fn from(field: FieldId) -> Self {
        match field {
            0 => Property::Id,
            1 => Property::Name,
            2 => Property::Email,
            3 => Property::ReplyTo,
            4 => Property::Bcc,
            5 => Property::TextSignature,
            6 => Property::HtmlSignature,
            7 => Property::MayDelete,
            _ => Property::Invalid,
        }
    }
}

impl TryFrom<&str> for Property {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match Property::parse(value) {
            Property::Invalid => Err(()),
            property => Ok(property),
        }
    }
}
