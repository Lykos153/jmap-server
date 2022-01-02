pub mod batch;
pub mod field;
pub mod leb128;
pub mod mutex_map;
pub mod search_snippet;
pub mod serialize;
pub mod term_index;

use std::{borrow::Cow, iter::FromIterator};

use batch::DocumentWriter;
use nlp::Language;

#[derive(Debug)]
pub enum StoreError {
    InternalError(String),
    SerializeError(String),
    DeserializeError(String),
    InvalidArguments(String),
    ParseError,
    DataCorruption,
    NotFound,
    InvalidArgument,
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub type AccountId = u32;
pub type CollectionId = u8;
pub type DocumentId = u32;
pub type ThreadId = u32;
pub type FieldId = u8;
pub type FieldNumber = u16;
pub type TagId = u8;
pub type Integer = u32;
pub type LongInteger = u64;
pub type Float = f64;
pub type TermId = u64;
pub type ChangeLogId = u64;

pub trait DocumentSet: DocumentSetBitOps + Eq + Clone + Sized {
    type Item;

    fn new() -> Self;
    fn contains(&self, document: Self::Item) -> bool;

    fn is_empty(&self) -> bool;
    fn len(&self) -> usize;
}

pub trait DocumentSetBitOps<Rhs: Sized = Self> {
    fn intersection(&mut self, other: &Rhs);
    fn union(&mut self, other: &Rhs);
    fn difference(&mut self, other: &Rhs);
}

pub trait UncommittedDocumentId: Clone + Send + Sync {
    fn get_document_id(&self) -> DocumentId;
}

#[derive(Debug)]
pub enum FieldValue<'x> {
    Keyword(Cow<'x, str>),
    Text(Cow<'x, str>),
    FullText(TextQuery<'x>),
    Integer(Integer),
    LongInteger(LongInteger),
    Float(Float),
    Tag(Tag<'x>),
}

#[derive(Debug, Clone)]
pub enum Tag<'x> {
    Static(TagId),
    Id(Integer),
    Text(Cow<'x, str>),
}

#[derive(Debug)]
pub struct TextQuery<'x> {
    pub text: Cow<'x, str>,
    pub language: Language,
    pub match_phrase: bool,
}

impl<'x> TextQuery<'x> {
    pub fn query(text: Cow<'x, str>, language: Language) -> Self {
        TextQuery {
            language,
            match_phrase: (text.starts_with('"') && text.ends_with('"'))
                || (text.starts_with('\'') && text.ends_with('\'')),
            text,
        }
    }

    pub fn query_english(text: Cow<'x, str>) -> Self {
        TextQuery::query(text, Language::English)
    }
}

#[derive(Debug)]
pub enum ComparisonOperator {
    LowerThan,
    LowerEqualThan,
    GreaterThan,
    GreaterEqualThan,
    Equal,
}

#[derive(Debug)]
pub struct FilterCondition<'x> {
    pub field: FieldId,
    pub op: ComparisonOperator,
    pub value: FieldValue<'x>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum LogicalOperator {
    And,
    Or,
    Not,
}

#[derive(Debug)]
pub enum Filter<'x, T: DocumentSet> {
    Condition(FilterCondition<'x>),
    Operator(FilterOperator<'x, T>),
    DocumentSet(T),
    None,
}

impl<'x, T> Filter<'x, T>
where
    T: DocumentSet,
{
    pub fn new_condition(field: FieldId, op: ComparisonOperator, value: FieldValue<'x>) -> Self {
        Filter::Condition(FilterCondition { field, op, value })
    }

    pub fn eq(field: FieldId, value: FieldValue<'x>) -> Self {
        Filter::Condition(FilterCondition {
            field,
            op: ComparisonOperator::Equal,
            value,
        })
    }

    pub fn lt(field: FieldId, value: FieldValue<'x>) -> Self {
        Filter::Condition(FilterCondition {
            field,
            op: ComparisonOperator::LowerThan,
            value,
        })
    }

    pub fn le(field: FieldId, value: FieldValue<'x>) -> Self {
        Filter::Condition(FilterCondition {
            field,
            op: ComparisonOperator::LowerEqualThan,
            value,
        })
    }

    pub fn gt(field: FieldId, value: FieldValue<'x>) -> Self {
        Filter::Condition(FilterCondition {
            field,
            op: ComparisonOperator::GreaterThan,
            value,
        })
    }

    pub fn ge(field: FieldId, value: FieldValue<'x>) -> Self {
        Filter::Condition(FilterCondition {
            field,
            op: ComparisonOperator::GreaterEqualThan,
            value,
        })
    }

    pub fn and(conditions: Vec<Filter<'x, T>>) -> Self {
        Filter::Operator(FilterOperator {
            operator: LogicalOperator::And,
            conditions,
        })
    }

    pub fn or(conditions: Vec<Filter<'x, T>>) -> Self {
        Filter::Operator(FilterOperator {
            operator: LogicalOperator::Or,
            conditions,
        })
    }

    pub fn not(conditions: Vec<Filter<'x, T>>) -> Self {
        Filter::Operator(FilterOperator {
            operator: LogicalOperator::Not,
            conditions,
        })
    }
}

#[derive(Debug)]
pub struct FilterOperator<'x, T: DocumentSet> {
    pub operator: LogicalOperator,
    pub conditions: Vec<Filter<'x, T>>,
}

pub struct FieldComparator {
    pub field: FieldId,
    pub ascending: bool,
}

pub struct DocumentSetComparator<T: DocumentSet> {
    pub set: T,
    pub ascending: bool,
}

pub enum Comparator<T: DocumentSet> {
    List(Vec<Comparator<T>>),
    Field(FieldComparator),
    DocumentSet(DocumentSetComparator<T>),
    None,
}

impl<T> Default for Comparator<T>
where
    T: DocumentSet,
{
    fn default() -> Self {
        Comparator::None
    }
}

impl<T> Comparator<T>
where
    T: DocumentSet,
{
    pub fn ascending(field: FieldId) -> Self {
        Comparator::Field(FieldComparator {
            field,
            ascending: true,
        })
    }

    pub fn descending(field: FieldId) -> Self {
        Comparator::Field(FieldComparator {
            field,
            ascending: false,
        })
    }
}

pub trait StoreUpdate {
    type UncommittedId: UncommittedDocumentId;

    fn assign_document_id(
        &self,
        account: AccountId,
        collection: CollectionId,
        last_assigned_id: Option<Self::UncommittedId>,
    ) -> crate::Result<Self::UncommittedId>;

    fn update_document(&self, document: DocumentWriter<Self::UncommittedId>) -> crate::Result<()> {
        self.update_documents(vec![document])
    }

    fn update_documents(&self, documents: Vec<DocumentWriter<Self::UncommittedId>>) -> Result<()>;
}

pub trait StoreQuery<'x>: StoreDocumentSet {
    type Iter: DocumentSet + Iterator<Item = DocumentId>;

    fn query(
        &'x self,
        account: AccountId,
        collection: CollectionId,
        filter: Filter<Self::Set>,
        sort: Comparator<Self::Set>,
    ) -> Result<Self::Iter>;
}

pub trait StoreGet {
    fn get_value<T>(
        &self,
        account: Option<AccountId>,
        collection: Option<CollectionId>,
        field: Option<FieldId>,
    ) -> Result<Option<T>>
    where
        Vec<u8>: serialize::StoreDeserialize<T>;

    fn get_document_value<T>(
        &self,
        account: AccountId,
        collection: CollectionId,
        document: DocumentId,
        field: FieldId,
        field_num: FieldNumber,
    ) -> Result<Option<T>>
    where
        Vec<u8>: serialize::StoreDeserialize<T>;

    fn get_multi_document_value<T>(
        &self,
        account: AccountId,
        collection: CollectionId,
        documents: impl Iterator<Item = DocumentId>,
        field: FieldId,
    ) -> Result<Vec<Option<T>>>
    where
        Vec<u8>: serialize::StoreDeserialize<T>;
}

pub trait StoreDocumentSet {
    type Set: DocumentSet<Item = DocumentId>
        + IntoIterator<Item = DocumentId>
        + FromIterator<DocumentId>
        + std::fmt::Debug;
}

pub trait StoreTag: StoreDocumentSet {
    fn get_tag(
        &self,
        account: AccountId,
        collection: CollectionId,
        field: FieldId,
        tag: Tag,
    ) -> Result<Option<Self::Set>> {
        Ok(self
            .get_tags(account, collection, field, &[tag])?
            .pop()
            .unwrap())
    }

    fn get_tags(
        &self,
        account: AccountId,
        collection: CollectionId,
        field: FieldId,
        tags: &[Tag],
    ) -> Result<Vec<Option<Self::Set>>>;

    fn set_tag(
        &self,
        account: AccountId,
        collection: CollectionId,
        document: DocumentId,
        field: FieldId,
        tag: Tag,
    ) -> Result<()>;

    fn clear_tag(
        &self,
        account: AccountId,
        collection: CollectionId,
        document: DocumentId,
        field: FieldId,
        tag: Tag,
    ) -> Result<()>;

    fn has_tag(
        &self,
        account: AccountId,
        collection: CollectionId,
        document: DocumentId,
        field: FieldId,
        tag: Tag,
    ) -> Result<bool>;
}

pub trait StoreDelete {
    fn delete_document(
        &self,
        account: AccountId,
        collection: CollectionId,
        document: DocumentId,
    ) -> Result<()> {
        self.delete_document_bulk(account, collection, &[document])
    }
    fn delete_document_bulk(
        &self,
        account: AccountId,
        collection: CollectionId,
        documents: &[DocumentId],
    ) -> Result<()>;
    fn delete_account(&self, account: AccountId) -> Result<()>;
    fn delete_collection(&self, account: AccountId, collection: CollectionId) -> Result<()>;
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ChangeLogEntry {
    Insert(ChangeLogId),
    Update(ChangeLogId),
    Delete(ChangeLogId),
}

pub struct ChangeLog {
    pub changes: Vec<ChangeLogEntry>,
    pub from_change_id: ChangeLogId,
    pub to_change_id: ChangeLogId,
}

impl Default for ChangeLog {
    fn default() -> Self {
        Self {
            changes: Vec::with_capacity(10),
            from_change_id: 0,
            to_change_id: 0,
        }
    }
}

pub enum ChangeLogQuery {
    All,
    Since(ChangeLogId),
    SinceInclusive(ChangeLogId),
    RangeInclusive(ChangeLogId, ChangeLogId),
}

pub trait StoreChangeLog {
    fn get_last_change_id(
        &self,
        account: AccountId,
        collection: CollectionId,
    ) -> Result<Option<ChangeLogId>>;
    fn get_changes(
        &self,
        account: AccountId,
        collection: CollectionId,
        query: ChangeLogQuery,
    ) -> Result<Option<ChangeLog>>;
}

pub trait StoreTombstone: StoreDocumentSet {
    fn purge_tombstoned(&self, account: AccountId, collection: CollectionId) -> Result<()>;

    fn get_tombstoned_ids(
        &self,
        account: AccountId,
        collection: CollectionId,
    ) -> crate::Result<Option<Self::Set>>;
}

pub trait Store<'x>:
    StoreUpdate
    + StoreQuery<'x>
    + StoreGet
    + StoreDelete
    + StoreTag
    + StoreChangeLog
    + Send
    + Sync
    + Sized
{
}
