pub mod changes;
pub mod get;
pub mod import;
pub mod parse;
pub mod query;
pub mod set;

use std::{borrow::Cow, collections::HashMap};

use changes::JMAPMailLocalStoreChanges;
use get::JMAPMailLocalStoreGet;
use import::{JMAPMailImportItem, JMAPMailLocalStoreImport};
use jmap_store::{
    changes::{JMAPLocalChanges, JMAPState},
    json::JSONValue,
    JMAPChangesResponse, JMAPGet, JMAPGetResponse, JMAPId, JMAPQuery, JMAPQueryChanges,
    JMAPQueryChangesResponse, JMAPQueryResponse, JMAPSet, JMAPSetResponse,
};
use mail_parser::{HeaderName, MessagePartId, RawHeaders};
use query::{JMAPMailComparator, JMAPMailFilterCondition, JMAPMailLocalStoreQuery};
use serde::{Deserialize, Serialize};
use set::JMAPMailLocalStoreSet;
use store::{AccountId, BlobIndex, DocumentId, ThreadId};

pub const MESSAGE_RAW: BlobIndex = 0;
pub const MESSAGE_HEADERS: BlobIndex = 1;
pub const MESSAGE_HEADERS_RAW: BlobIndex = 2;
pub const MESSAGE_HEADERS_PARTS: BlobIndex = 3;
pub const MESSAGE_STRUCTURE: BlobIndex = 4;
pub const MESSAGE_METADATA: BlobIndex = 5;
pub const MESSAGE_PARTS: BlobIndex = 6;

pub type JMAPMailHeaders<'x> = HashMap<HeaderName, JSONValue<'x, JMAPMailProperties<'x>>>;

pub trait JMAPMailIdImpl {
    fn from_email(thread_id: ThreadId, doc_id: DocumentId) -> Self;
    fn get_document_id(&self) -> DocumentId;
    fn get_thread_id(&self) -> ThreadId;
}

impl JMAPMailIdImpl for JMAPId {
    fn from_email(thread_id: ThreadId, doc_id: DocumentId) -> JMAPId {
        (thread_id as JMAPId) << 32 | doc_id as JMAPId
    }

    fn get_document_id(&self) -> DocumentId {
        (self & 0xFFFFFFFF) as DocumentId
    }

    fn get_thread_id(&self) -> ThreadId {
        (self >> 32) as ThreadId
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageMetadata {
    pub html_body: Vec<MessagePartId>,
    pub text_body: Vec<MessagePartId>,
    pub attachments: Vec<MessagePartId>,
    pub size: usize,
    pub received_at: i64,
    pub has_attachments: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageRawHeaders<'x> {
    pub size: usize,
    pub headers: RawHeaders<'x>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageField {
    Internal = 127,
    Body = 128,
    Attachment = 129,
    ReceivedAt = 130,
    Size = 131,
    Keyword = 132,
    Thread = 133,
    ThreadName = 134,
    MessageIdRef = 135,
    ThreadId = 136,
    Mailbox = 137,
}

impl From<MessageField> for u8 {
    fn from(field: MessageField) -> Self {
        field as u8
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub enum JMAPMailHeaderForm {
    Raw,
    Text,
    Addresses,
    GroupedAddresses,
    MessageIds,
    Date,
    URLs,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub struct JMAPMailHeaderProperty<T> {
    pub form: JMAPMailHeaderForm,
    pub header: T,
    pub all: bool,
}

#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Clone)]
pub enum JMAPMailProperties<'x> {
    Id,
    BlobId,
    ThreadId,
    MailboxIds,
    Keywords,
    Size,
    ReceivedAt,
    MessageId,
    InReplyTo,
    References,
    Sender,
    From,
    To,
    Cc,
    Bcc,
    ReplyTo,
    Subject,
    SentAt,
    HasAttachment,
    Preview,
    BodyValues,
    TextBody,
    HtmlBody,
    Attachments,
    BodyStructure,
    RfcHeader(JMAPMailHeaderProperty<HeaderName>),
    OtherHeader(JMAPMailHeaderProperty<Cow<'x, str>>),

    // EmailAddress and EmailAddressGroup object properties
    Name,
    Email,
    Addresses,
}

impl<'x> Default for JMAPMailProperties<'x> {
    fn default() -> Self {
        JMAPMailProperties::Id
    }
}

#[derive(Debug)]
pub enum JMAPMailBodyProperties {
    PartId,
    BlobIndex,
    Size,
    Name,
    Type,
    Charset,
    Headers,
    Disposition,
    Cid,
    Language,
    Location,
    Subparts,
}

pub trait JMAPMailStoreImport<'x> {
    fn mail_import_single(
        &'x self,
        account: AccountId,
        message: JMAPMailImportItem<'x>,
    ) -> jmap_store::Result<JMAPId>;
}

pub trait JMAPMailStoreSet<'x> {
    fn mail_set(
        &self,
        request: JMAPSet<'x, JMAPMailProperties<'x>>,
    ) -> jmap_store::Result<JMAPSetResponse<'x, JMAPMailProperties<'x>>>;
}

pub trait JMAPMailStoreQuery<'x> {
    fn mail_query(
        &'x self,
        query: JMAPQuery<JMAPMailFilterCondition<'x>, JMAPMailComparator<'x>>,
        collapse_threads: bool,
    ) -> jmap_store::Result<JMAPQueryResponse>;
}

pub trait JMAPMailStoreChanges<'x>: JMAPLocalChanges<'x> {
    fn mail_changes(
        &'x self,
        account: AccountId,
        since_state: JMAPState,
        max_changes: usize,
    ) -> jmap_store::Result<JMAPChangesResponse>;

    fn mail_query_changes(
        &'x self,
        query: JMAPQueryChanges<JMAPMailFilterCondition<'x>, JMAPMailComparator<'x>>,
        collapse_threads: bool,
    ) -> jmap_store::Result<JMAPQueryChangesResponse>;
}

#[derive(Debug, Default)]
pub struct JMAPMailStoreGetArguments {
    pub body_properties: Vec<JMAPMailBodyProperties>,
    pub fetch_text_body_values: bool,
    pub fetch_html_body_values: bool,
    pub fetch_all_body_values: bool,
    pub max_body_value_bytes: usize,
}

pub trait JMAPMailStoreGet<'x> {
    fn mail_get(
        &self,
        request: JMAPGet<JMAPMailProperties<'x>>,
        arguments: JMAPMailStoreGetArguments,
    ) -> jmap_store::Result<JMAPGetResponse<'x, JMAPMailProperties<'x>>>;
}

pub trait JMAPMailStore<'x>:
    JMAPMailStoreImport<'x>
    + JMAPMailStoreSet<'x>
    + JMAPMailStoreQuery<'x>
    + JMAPMailStoreGet<'x>
    + JMAPMailStoreChanges<'x>
{
}

pub trait JMAPMailLocalStore<'x>:
    JMAPMailLocalStoreGet<'x>
    + JMAPMailLocalStoreQuery<'x>
    + JMAPMailLocalStoreImport<'x>
    + JMAPMailLocalStoreSet<'x>
    + JMAPMailLocalStoreChanges<'x>
{
}
