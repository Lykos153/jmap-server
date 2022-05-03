use store::core::document::Document;
use store::core::error::StoreError;
use store::core::tag::Tag;
use store::nlp::Language;
use store::serialize::{StoreDeserialize, StoreSerialize};
use store::write::options::{IndexOptions, Options};
use store::{AccountId, DocumentId, JMAPStore, Store};

use crate::error::set::SetError;
use crate::{protocol::json::JSONValue, Property};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub trait PropertySchema:
    Property
    + Eq
    + Hash
    + Copy
    + Into<u8>
    + serde::Serialize
    + for<'de> serde::Deserialize<'de>
    + Sync
    + Send
{
    fn required() -> &'static [Self];
    fn indexed() -> &'static [(Self, u64)];
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
pub struct TinyORM<T>
where
    T: PropertySchema,
{
    #[serde(bound(
        serialize = "HashMap<T, JSONValue>: serde::Serialize",
        deserialize = "HashMap<T, JSONValue>: serde::Deserialize<'de>"
    ))]
    properties: HashMap<T, JSONValue>,
    #[serde(bound(
        serialize = "HashMap<T, HashSet<Tag>>: serde::Serialize",
        deserialize = "HashMap<T, HashSet<Tag>>: serde::Deserialize<'de>"
    ))]
    tags: HashMap<T, HashSet<Tag>>,
}

impl<T> Default for TinyORM<T>
where
    T: PropertySchema,
{
    fn default() -> Self {
        Self {
            properties: HashMap::new(),
            tags: HashMap::new(),
        }
    }
}

impl<T> TinyORM<T>
where
    T: PropertySchema + 'static,
{
    pub const FIELD_ID: u8 = u8::MAX;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn track_changes(source: &TinyORM<T>) -> TinyORM<T> {
        TinyORM {
            properties: HashMap::new(),
            tags: source.tags.clone(),
        }
    }

    pub fn set(&mut self, property: T, value: JSONValue) {
        self.properties.insert(property, value);
    }

    pub fn remove(&mut self, property: &T) -> Option<JSONValue> {
        self.properties.remove(property)
    }

    pub fn has_property(&self, property: &T) -> bool {
        self.properties.contains_key(property)
    }

    pub fn tag(&mut self, property: T, tag: Tag) {
        self.tags
            .entry(property)
            .or_insert_with(HashSet::new)
            .insert(tag);
    }

    pub fn untag(&mut self, property: &T, tag: &Tag) {
        self.tags.get_mut(property).map(|set| set.remove(tag));
    }

    pub fn untag_all(&mut self, property: &T) {
        if let Some(set) = self.tags.get_mut(property) {
            set.clear()
        }
    }

    pub fn get_tags(&self, property: &T) -> Option<&HashSet<Tag>> {
        self.tags.get(property)
    }

    pub fn has_tags(&self, property: &T) -> bool {
        self.tags
            .get(property)
            .map(|set| !set.is_empty())
            .unwrap_or(false)
    }

    pub fn get_unsigned_int(&self, property: &T) -> Option<u64> {
        self.properties
            .get(property)
            .and_then(|value| value.to_unsigned_int())
    }

    pub fn get_string(&self, property: &T) -> Option<&str> {
        self.properties
            .get(property)
            .and_then(|value| value.to_string())
    }

    pub fn insert_validate(self, document: &mut Document) -> crate::error::set::Result<()> {
        for property in T::required() {
            if self
                .properties
                .get(property)
                .map(|v| v.is_empty())
                .unwrap_or(true)
            {
                return Err(SetError::invalid_property(
                    property.to_string(),
                    "Property cannot be empty.".to_string(),
                ));
            }
        }
        self.insert(document).map_err(|err| err.into())
    }

    pub fn insert(self, document: &mut Document) -> store::Result<()> {
        self.insert_orm(document)?;
        self.update_document(document, false);
        Ok(())
    }

    pub fn merge_validate(
        self,
        document: &mut Document,
        changes: TinyORM<T>,
    ) -> crate::error::set::Result<bool> {
        for property in T::required() {
            if changes
                .properties
                .get(property)
                .map(|v| v.is_empty())
                .unwrap_or_else(|| self.properties.get(property).is_none())
            {
                return Err(SetError::invalid_property(
                    property.to_string(),
                    "Property cannot be empty.".to_string(),
                ));
            }
        }
        self.merge(document, changes).map_err(|err| err.into())
    }

    pub fn get_changed_tags(&self, changes: &Self, property: &T) -> HashSet<Tag> {
        match (self.tags.get(property), changes.tags.get(property)) {
            (Some(this), Some(changes)) if this != changes => {
                let mut tag_diff = HashSet::new();
                for tag in this {
                    if !changes.contains(tag) {
                        tag_diff.insert(tag.clone());
                    }
                }
                for tag in changes {
                    if !this.contains(tag) {
                        tag_diff.insert(tag.clone());
                    }
                }
                tag_diff
            }
            (Some(this), None) => this.clone(),
            (None, Some(changes)) => changes.clone(),
            _ => HashSet::with_capacity(0),
        }
    }

    pub fn merge(mut self, document: &mut Document, changes: Self) -> store::Result<bool> {
        let indexed = T::indexed();

        for (property, value) in changes.properties {
            let (is_indexed, index_options) = indexed
                .iter()
                .filter_map(|(p, index_type)| {
                    if p == &property {
                        Some((true, index_type))
                    } else {
                        None
                    }
                })
                .next()
                .unwrap_or((false, &0));

            if let Some(current_value) = self.properties.get(&property) {
                if current_value == &value {
                    continue;
                } else if is_indexed {
                    match &current_value {
                        JSONValue::String(text) => {
                            document.text(
                                property,
                                text.clone(),
                                Language::Unknown,
                                (*index_options).clear(),
                            );
                        }
                        JSONValue::Number(number) => {
                            document.number(property, number, (*index_options).clear());
                        }
                        value => {
                            debug_assert!(false, "ORM unsupported type: {:?}", value);
                        }
                    }
                }
            }

            match &value {
                JSONValue::String(text) => {
                    if is_indexed {
                        document.text(property, text.clone(), Language::Unknown, *index_options);
                    }

                    self.properties.insert(property, value);
                }
                JSONValue::Number(number) => {
                    if is_indexed {
                        document.number(property, number, *index_options);
                    }
                    self.properties.insert(property, value);
                }
                JSONValue::Null => {
                    self.properties.remove(&property);
                }
                _ => {
                    self.properties.insert(property, value);
                }
            }
        }

        if self.tags != changes.tags {
            for (property, tags) in &self.tags {
                if let Some(changed_tags) = changes.tags.get(property) {
                    if tags != changed_tags {
                        for tag in tags {
                            if !changed_tags.contains(tag) {
                                document.tag(*property, tag.clone(), IndexOptions::new().clear());
                            }
                        }
                    }
                }
            }

            for (property, changed_tags) in &changes.tags {
                if let Some(tags) = self.tags.get(property) {
                    if changed_tags != tags {
                        for changed_tag in changed_tags {
                            if !tags.contains(changed_tag) {
                                document.tag(*property, changed_tag.clone(), IndexOptions::new());
                            }
                        }
                    }
                } else {
                    for changed_tag in changed_tags {
                        document.tag(*property, changed_tag.clone(), IndexOptions::new());
                    }
                }
            }

            self.tags = changes.tags;
        }

        if !document.is_empty() {
            self.insert_orm(document)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn delete(self, document: &mut Document) {
        TinyORM::<T>::delete_orm(document);
        self.update_document(document, true);
    }

    fn update_document(self, document: &mut Document, is_delete: bool) {
        let indexed = T::indexed();
        if indexed.is_empty() && self.tags.is_empty() {
            return;
        }

        for (property, value) in self.properties {
            let (is_indexed, index_options) = indexed
                .iter()
                .filter_map(|(p, index_options)| {
                    if p == &property {
                        Some((
                            true,
                            if !is_delete {
                                *index_options
                            } else {
                                (*index_options).clear()
                            },
                        ))
                    } else {
                        None
                    }
                })
                .next()
                .unwrap_or((false, 0));
            if is_indexed {
                match value {
                    JSONValue::String(text) => {
                        document.text(property, text, Language::Unknown, index_options);
                    }
                    JSONValue::Number(number) => {
                        document.number(property, &number, index_options);
                    }
                    value => {
                        debug_assert!(false, "ORM unsupported type: {:?}", value);
                    }
                }
            }
        }

        let index_options = if !is_delete {
            IndexOptions::new()
        } else {
            IndexOptions::new().clear()
        };
        for (property, tags) in self.tags {
            for tag in tags {
                document.tag(property, tag, index_options);
            }
        }
    }

    pub fn insert_orm(&self, document: &mut Document) -> store::Result<()> {
        document.binary(
            Self::FIELD_ID,
            self.serialize().ok_or_else(|| {
                StoreError::SerializeError("Failed to serialize ORM object.".to_string())
            })?,
            IndexOptions::new().store(),
        );
        Ok(())
    }

    pub fn delete_orm(document: &mut Document) {
        document.binary(
            Self::FIELD_ID,
            Vec::with_capacity(0),
            IndexOptions::new().clear(),
        );
    }
}

impl<T> From<HashMap<T, JSONValue>> for TinyORM<T>
where
    T: PropertySchema,
{
    fn from(properties: HashMap<T, JSONValue>) -> Self {
        TinyORM {
            properties,
            ..Default::default()
        }
    }
}

impl<T> StoreSerialize for TinyORM<T>
where
    T: PropertySchema,
{
    fn serialize(&self) -> Option<Vec<u8>> {
        rmp_serde::encode::to_vec(&self).ok()
    }
}

impl<T> StoreDeserialize for TinyORM<T>
where
    T: PropertySchema,
{
    fn deserialize(bytes: &[u8]) -> Option<Self> {
        rmp_serde::decode::from_slice(bytes).ok()
    }
}

pub trait JMAPOrm {
    fn get_orm<U>(
        &self,
        account: AccountId,
        document: DocumentId,
    ) -> store::Result<Option<TinyORM<U>>>
    where
        U: PropertySchema + 'static;
}

impl<T> JMAPOrm for JMAPStore<T>
where
    T: for<'x> Store<'x> + 'static,
{
    fn get_orm<U>(
        &self,
        account: AccountId,
        document: DocumentId,
    ) -> store::Result<Option<TinyORM<U>>>
    where
        U: PropertySchema + 'static,
    {
        self.get_document_value::<TinyORM<U>>(
            account,
            U::collection(),
            document,
            TinyORM::<U>::FIELD_ID,
        )
    }
}
