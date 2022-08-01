use super::{
    conv::IntoForm,
    parse::get_message_part,
    schema::{
        BodyProperty, Email, EmailBodyPart, EmailBodyValue, EmailHeader, HeaderForm,
        HeaderProperty, Property, Value,
    },
    sharing::JMAPShareMail,
    GetRawHeader, HeaderName,
};
use crate::mail::{MessageData, MessageField, MimePart, MimePartType};
use jmap::{
    error::method::MethodError,
    from_timestamp,
    jmap_store::get::{GetHelper, GetObject},
    orm::serialize::JMAPOrm,
    request::{
        get::{GetRequest, GetResponse},
        ACLEnforce, MaybeIdReference,
    },
    types::{blob::JMAPBlob, jmap::JMAPId},
    SUPERUSER_ID,
};
use mail_parser::{
    parsers::preview::{preview_html, preview_text, truncate_html, truncate_text},
    HeaderValue, Message, RfcHeader,
};
use std::{borrow::Cow, sync::Arc};
use store::{
    blob::BlobId,
    core::{
        acl::{ACLToken, ACL},
        vec_map::VecMap,
    },
    AccountId, JMAPStore,
};
use store::{
    core::{collection::Collection, error::StoreError},
    serialize::StoreDeserialize,
};
use store::{DocumentId, Store};

enum FetchRaw {
    Header,
    All,
    None,
}

#[derive(Debug, Clone, Default)]
pub struct GetArguments {
    pub body_properties: Option<Vec<BodyProperty>>,
    pub fetch_text_body_values: Option<bool>,
    pub fetch_html_body_values: Option<bool>,
    pub fetch_all_body_values: Option<bool>,
    pub max_body_value_bytes: Option<usize>,
}

impl GetObject for Email {
    type GetArguments = GetArguments;

    fn default_properties() -> Vec<Self::Property> {
        vec![
            Property::Id,
            Property::BlobId,
            Property::ThreadId,
            Property::MailboxIds,
            Property::Keywords,
            Property::Size,
            Property::ReceivedAt,
            Property::MessageId,
            Property::InReplyTo,
            Property::References,
            Property::Sender,
            Property::From,
            Property::To,
            Property::Cc,
            Property::Bcc,
            Property::ReplyTo,
            Property::Subject,
            Property::SentAt,
            Property::HasAttachment,
            Property::Preview,
            Property::BodyValues,
            Property::TextBody,
            Property::HtmlBody,
            Property::Attachments,
        ]
    }

    fn get_as_id(&self, property: &Self::Property) -> Option<Vec<JMAPId>> {
        match self.properties.get(property)? {
            Value::Id { value } => Some(vec![*value]),
            Value::MailboxIds { value, .. } => {
                Some(value.keys().filter_map(|id| Some(*id.value()?)).collect())
            }
            _ => None,
        }
    }
}

impl Email {
    pub fn default_body_properties() -> Vec<BodyProperty> {
        vec![
            BodyProperty::PartId,
            BodyProperty::BlobId,
            BodyProperty::Size,
            BodyProperty::Name,
            BodyProperty::Type,
            BodyProperty::Charset,
            BodyProperty::Disposition,
            BodyProperty::Cid,
            BodyProperty::Language,
            BodyProperty::Location,
        ]
    }
}

pub enum BlobResult {
    Blob(Vec<u8>),
    Unauthorized,
    NotFound,
}

pub trait JMAPGetMail<T>
where
    T: for<'x> Store<'x> + 'static,
{
    fn mail_get(&self, request: GetRequest<Email>) -> jmap::Result<GetResponse<Email>>;
    fn mail_blob_get(
        &self,
        account_id: AccountId,
        acl: &Arc<ACLToken>,
        blob: &JMAPBlob,
    ) -> store::Result<BlobResult>;
}

impl<T> JMAPGetMail<T> for JMAPStore<T>
where
    T: for<'x> Store<'x> + 'static,
{
    fn mail_get(&self, request: GetRequest<Email>) -> jmap::Result<GetResponse<Email>> {
        // Initialize helpers
        let account_id = request.account_id.get_document_id();
        let mut helper = GetHelper::new(
            self,
            request,
            Some(|ids: Vec<DocumentId>| {
                Ok(self
                    .get_multi_document_value(
                        account_id,
                        Collection::Mail,
                        ids.iter().copied(),
                        MessageField::ThreadId.into(),
                    )?
                    .into_iter()
                    .zip(ids)
                    .filter_map(
                        |(thread_id, document_id): (Option<DocumentId>, DocumentId)| {
                            JMAPId::from_parts(thread_id?, document_id).into()
                        },
                    )
                    .collect::<Vec<JMAPId>>())
            }),
            (|account_id: AccountId, member_of: &[AccountId]| {
                self.mail_shared_messages(account_id, member_of, ACL::ReadItems)
            })
            .into(),
        )?;

        // Process arguments
        let body_properties = helper
            .request
            .arguments
            .body_properties
            .take()
            .unwrap_or_else(Email::default_body_properties);
        let fetch_text_body_values = helper
            .request
            .arguments
            .fetch_text_body_values
            .unwrap_or(false);
        let fetch_html_body_values = helper
            .request
            .arguments
            .fetch_html_body_values
            .unwrap_or(false);
        let fetch_all_body_values = helper
            .request
            .arguments
            .fetch_all_body_values
            .unwrap_or(false);
        let max_body_value_bytes = helper.request.arguments.max_body_value_bytes.unwrap_or(0);
        let fetch_raw = if body_properties
            .iter()
            .any(|prop| matches!(prop, BodyProperty::Headers | BodyProperty::Header(_)))
        {
            FetchRaw::All
        } else if helper.properties.iter().any(|prop| {
            matches!(
                prop,
                Property::Header(HeaderProperty {
                    form: HeaderForm::Raw,
                    ..
                }) | Property::Header(HeaderProperty {
                    header: HeaderName::Other(_),
                    ..
                }) | Property::BodyStructure
            )
        }) {
            FetchRaw::Header
        } else {
            FetchRaw::None
        };

        // Add Id Property
        if !helper.properties.contains(&Property::Id) {
            helper.properties.push(Property::Id);
        }

        // Get items
        helper.get(|id, properties| {
            let document_id = id.get_document_id();

            // Fetch message metadata
            let message_data_bytes = self
                .blob_get(
                    &self
                        .get_document_value::<BlobId>(
                            account_id,
                            Collection::Mail,
                            document_id,
                            MessageField::Metadata.into(),
                        )?
                        .ok_or_else(|| {
                            StoreError::DataCorruption(format!(
                                "Email metadata blobId for {}/{} does not exist.",
                                account_id, document_id
                            ))
                        })?,
                )?
                .ok_or_else(|| {
                    StoreError::DataCorruption(format!(
                        "Email metadata blob linked to {}/{} does not exist.",
                        account_id, document_id
                    ))
                })?;

            // Deserialize message data
            let mut message_data =
                MessageData::deserialize(&message_data_bytes).ok_or_else(|| {
                    StoreError::DataCorruption(format!(
                        "Failed to deserialize email metadata for {}/{}",
                        account_id, document_id
                    ))
                })?;

            // Fetch raw message only if needed
            let raw_message = match &fetch_raw {
                FetchRaw::All => {
                    Some(self.blob_get(&message_data.raw_message)?.ok_or_else(|| {
                        StoreError::DataCorruption(format!(
                            "Raw email message not found for {}/{}.",
                            account_id, document_id
                        ))
                    })?)
                }
                FetchRaw::Header => Some(
                    self.blob_get_range(
                        &message_data.raw_message,
                        0..message_data.body_offset as u32,
                    )?
                    .ok_or_else(|| {
                        StoreError::DataCorruption(format!(
                            "Raw email message not found for {}/{}.",
                            account_id, document_id
                        ))
                    })?,
                ),
                FetchRaw::None => None,
            };

            // Fetch ORM
            let fields = self
                .get_orm::<Email>(account_id, document_id)?
                .ok_or_else(|| StoreError::InternalError("ORM not found for Email.".to_string()))?;

            // Add requested properties to result
            let mut email = VecMap::with_capacity(properties.len());
            for property in properties {
                let value = match property {
                    Property::Id => Value::Id { value: id }.into(),
                    Property::BlobId => Value::Blob {
                        value: JMAPBlob::from(&message_data.raw_message),
                    }
                    .into(),
                    Property::ThreadId => Value::Id {
                        value: id.get_prefix_id().into(),
                    }
                    .into(),
                    Property::MailboxIds => {
                        fields
                            .get_tags(&Property::MailboxIds)
                            .map(|tags| Value::MailboxIds {
                                value: tags
                                    .iter()
                                    .map(|tag| (MaybeIdReference::Value(tag.as_id().into()), true))
                                    .collect(),
                                set: true,
                            })
                    }
                    Property::Keywords => fields
                        .get_tags(&Property::Keywords)
                        .map(|tags| Value::Keywords {
                            value: tags.iter().map(|tag| (tag.into(), true)).collect(),
                            set: true,
                        })
                        .unwrap_or(Value::Keywords {
                            value: VecMap::new(),
                            set: true,
                        })
                        .into(),
                    Property::Size => Value::Size {
                        value: message_data.size,
                    }
                    .into(),
                    Property::ReceivedAt => Value::Date {
                        value: from_timestamp(message_data.received_at),
                    }
                    .into(),
                    Property::MessageId | Property::InReplyTo | Property::References => {
                        message_data.header(
                            &property.as_rfc_header(),
                            &HeaderForm::MessageIds,
                            false,
                        )
                    }
                    Property::Sender
                    | Property::From
                    | Property::To
                    | Property::Cc
                    | Property::Bcc
                    | Property::ReplyTo => message_data.header(
                        &property.as_rfc_header(),
                        &HeaderForm::Addresses,
                        false,
                    ),
                    Property::Subject => {
                        message_data.header(&RfcHeader::Subject, &HeaderForm::Text, false)
                    }
                    Property::SentAt => {
                        message_data.header(&RfcHeader::Date, &HeaderForm::Date, false)
                    }
                    Property::HasAttachment => Value::Bool {
                        value: message_data.has_attachments,
                    }
                    .into(),
                    Property::Header(header) => {
                        match (&header.header, &header.form, &raw_message) {
                            (HeaderName::Other(_), _, Some(raw_message))
                            | (HeaderName::Rfc(_), HeaderForm::Raw, Some(raw_message)) => {
                                if let Some(offsets) = message_data
                                    .mime_parts
                                    .get(0)
                                    .and_then(|h| h.raw_headers.get_raw_header(&header.header))
                                {
                                    header
                                        .form
                                        .parse_offsets(&offsets, raw_message, header.all)
                                        .into_form(&header.form, header.all)
                                } else if header.all {
                                    Value::TextList { value: Vec::new() }.into()
                                } else {
                                    None
                                }
                            }
                            (HeaderName::Rfc(header_name), _, _) => {
                                message_data.header(header_name, &header.form, header.all)
                            }
                            _ => None,
                        }
                    }
                    /*Property::Header(header) => {
                        if let Some(offsets) = message_data
                            .mime_parts
                            .get(0)
                            .and_then(|h| h.raw_headers.get_raw_header(&header.header))
                        {
                            header
                                .form
                                .parse_offsets(&offsets, raw_message.as_ref().unwrap(), header.all)
                                .into_form(&header.form, header.all)
                        } else if header.all {
                            Value::TextList { value: Vec::new() }.into()
                        } else {
                            None
                        }
                    }*/
                    Property::Headers => Value::Headers {
                        value: if let Some(root_part) = message_data.mime_parts.get(0) {
                            root_part.as_email_headers(raw_message.as_ref().unwrap())
                        } else {
                            Vec::new()
                        },
                    }
                    .into(),
                    Property::Preview => {
                        if !message_data.text_body.is_empty() || !message_data.html_body.is_empty()
                        {
                            let parts = if !message_data.text_body.is_empty() {
                                &message_data.text_body
                            } else {
                                &message_data.html_body
                            };

                            #[allow(clippy::type_complexity)]
                            let (preview_fnc, blob_id): (
                                fn(Cow<str>, usize) -> Cow<str>,
                                _,
                            ) = match &parts
                                .get(0)
                                .and_then(|p| message_data.mime_parts.get(*p))
                                .ok_or_else(|| {
                                    StoreError::DataCorruption(format!(
                                        "Missing message part for {}/{}",
                                        account_id, document_id
                                    ))
                                })?
                                .mime_type
                            {
                                MimePartType::Text { blob_id } => (preview_text, blob_id),
                                MimePartType::Html { blob_id } => (preview_html, blob_id),
                                _ => {
                                    return Err(StoreError::DataCorruption(format!(
                                        "Message part blobId not found for {}/{}.",
                                        account_id, document_id
                                    ))
                                    .into());
                                }
                            };

                            Value::Text {
                                value: preview_fnc(
                                    String::from_utf8(self.blob_get(blob_id)?.ok_or_else(
                                        || {
                                            StoreError::DataCorruption(format!(
                                                "Message part blob not found for {}/{}.",
                                                account_id, document_id
                                            ))
                                        },
                                    )?)
                                    .map_or_else(
                                        |err| String::from_utf8_lossy(err.as_bytes()).into_owned(),
                                        |s| s,
                                    )
                                    .into(),
                                    256,
                                )
                                .into_owned(),
                            }
                            .into()
                        } else {
                            None
                        }
                    }
                    Property::BodyValues => {
                        let mut body_values = VecMap::new();
                        for (part_id, mime_part) in message_data.mime_parts.iter().enumerate() {
                            if (message_data.html_body.contains(&part_id)
                                && (fetch_all_body_values || fetch_html_body_values))
                                || (message_data.text_body.contains(&part_id)
                                    && (fetch_all_body_values || fetch_text_body_values))
                            {
                                let blob = self
                                    .blob_get(mime_part.mime_type.blob_id().ok_or_else(|| {
                                        StoreError::DataCorruption(format!(
                                            "BodyValue blobId not found for {}/{}.",
                                            account_id, document_id
                                        ))
                                    })?)?
                                    .ok_or_else(|| {
                                        StoreError::DataCorruption(format!(
                                            "BodyValue blob not found for {}/{}.",
                                            account_id, document_id
                                        ))
                                    })?;

                                body_values.append(
                                    part_id.to_string(),
                                    mime_part.as_body_value(
                                        String::from_utf8(
                                            blob.into_iter()
                                                .filter(|&ch| ch != b'\r')
                                                .collect::<Vec<_>>(),
                                        )
                                        .map_or_else(
                                            |err| {
                                                String::from_utf8_lossy(err.as_bytes()).into_owned()
                                            },
                                            |s| s,
                                        ),
                                        max_body_value_bytes,
                                    ),
                                );
                            }
                        }
                        Value::BodyValues { value: body_values }.into()
                    }
                    Property::TextBody => Some(
                        message_data
                            .mime_parts
                            .as_body_parts(
                                &message_data.text_body,
                                &body_properties,
                                raw_message.as_deref(),
                                None,
                            )
                            .into(),
                    ),
                    Property::HtmlBody => Some(
                        message_data
                            .mime_parts
                            .as_body_parts(
                                &message_data.html_body,
                                &body_properties,
                                raw_message.as_deref(),
                                None,
                            )
                            .into(),
                    ),
                    Property::Attachments => Some(
                        message_data
                            .mime_parts
                            .as_body_parts(
                                &message_data.attachments,
                                &body_properties,
                                raw_message.as_deref(),
                                None,
                            )
                            .into(),
                    ),
                    Property::BodyStructure => message_data
                        .mime_parts
                        .as_body_structure(&body_properties, raw_message.as_deref(), None)
                        .map(|b| b.into()),
                    Property::Invalid(property) => {
                        return Err(MethodError::InvalidArguments(format!(
                            "Unknown property {:?}",
                            property
                        )));
                    }
                };

                email.append(property.clone(), value.unwrap_or_default());
            }

            Ok(Some(Email { properties: email }))
        })
    }

    fn mail_blob_get(
        &self,
        account_id: AccountId,
        acl: &Arc<ACLToken>,
        blob: &JMAPBlob,
    ) -> store::Result<BlobResult> {
        if !self.blob_account_has_access(&blob.id, &acl.member_of)? && !acl.is_member(SUPERUSER_ID)
        {
            if let Some(shared_ids) = self
                .mail_shared_messages(account_id, &acl.member_of, ACL::ReadItems)?
                .as_ref()
            {
                if !self.blob_document_has_access(
                    &blob.id,
                    account_id,
                    Collection::Mail,
                    shared_ids,
                )? {
                    return Ok(BlobResult::Unauthorized);
                }
            } else {
                return Ok(BlobResult::Unauthorized);
            }
        }

        let bytes = self.blob_get(&blob.id)?;
        Ok(if let (Some(message), Some(inner_id)) = (
            bytes.as_ref().and_then(|b| Message::parse(b)),
            blob.inner_id,
        ) {
            get_message_part(message, inner_id, false).map(|bytes| bytes.into_owned())
        } else {
            bytes
        }
        .map(BlobResult::Blob)
        .unwrap_or(BlobResult::NotFound))
    }
}

impl MimePart {
    pub fn as_body_part(
        &self,
        part_id: usize,
        properties: &[BodyProperty],
        message_raw: Option<&[u8]>,
        base_blob_id: Option<&BlobId>,
    ) -> EmailBodyPart {
        let mut body_part = VecMap::with_capacity(properties.len());
        let blob_id = self.mime_type.blob_id();

        for property in properties {
            match property {
                BodyProperty::PartId => {
                    body_part.append(
                        BodyProperty::PartId,
                        if blob_id.is_some() {
                            Value::Text {
                                value: part_id.to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::BlobId => {
                    body_part.append(
                        BodyProperty::BlobId,
                        if blob_id.is_some() {
                            Value::Blob {
                                value: if let Some(base_blob_id) = base_blob_id {
                                    JMAPBlob::new_inner(base_blob_id.clone(), part_id as u32)
                                } else {
                                    JMAPBlob::from(*blob_id.as_ref().unwrap())
                                },
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Size => {
                    body_part.append(
                        BodyProperty::Size,
                        Value::Size {
                            value: if blob_id.is_some() { self.size } else { 0 },
                        },
                    );
                }
                BodyProperty::Name => {
                    body_part.append(
                        BodyProperty::Name,
                        if let Some(value) = &self.name {
                            Value::Text {
                                value: value.to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Type => {
                    body_part.append(
                        BodyProperty::Type,
                        if let Some(mime_type) = self.type_.as_deref().or(match &self.mime_type {
                            MimePartType::Text { .. } => Some("text/plain"),
                            MimePartType::Html { .. } => Some("text/html"),
                            _ => None,
                        }) {
                            Value::Text {
                                value: mime_type.to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Charset => {
                    body_part.append(
                        BodyProperty::Charset,
                        if let Some(value) = &self.charset {
                            Value::Text {
                                value: value.to_string(),
                            }
                        } else if let MimePartType::Text { .. } | MimePartType::Html { .. } =
                            &self.mime_type
                        {
                            Value::Text {
                                value: "us-ascii".to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Disposition => {
                    body_part.append(
                        BodyProperty::Disposition,
                        if let Some(value) = &self.disposition {
                            Value::Text {
                                value: value.to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Cid => {
                    body_part.append(
                        BodyProperty::Cid,
                        if let Some(value) = &self.cid {
                            Value::Text {
                                value: value.to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Language => {
                    body_part.append(
                        BodyProperty::Language,
                        if let Some(value) = &self.language {
                            Value::TextList {
                                value: value.to_vec(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Location => {
                    body_part.append(
                        BodyProperty::Location,
                        if let Some(value) = &self.location {
                            Value::Text {
                                value: value.to_string(),
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                BodyProperty::Header(header) => {
                    if let Some(message_raw) = message_raw {
                        let mut value = if !header.all {
                            Value::Null
                        } else {
                            Value::TextList { value: Vec::new() }
                        };

                        if let Some(offsets) = self.raw_headers.get_raw_header(&header.header) {
                            if let Some(value_) = header
                                .form
                                .parse_offsets(&offsets, message_raw, header.all)
                                .into_form(&header.form, header.all)
                            {
                                value = value_;
                            }
                        }

                        body_part.append(BodyProperty::Header(header.clone()), value);
                    }
                }
                BodyProperty::Headers => match message_raw {
                    Some(message_raw) if !self.raw_headers.is_empty() => {
                        body_part.append(
                            BodyProperty::Headers,
                            Value::Headers {
                                value: self.as_email_headers(message_raw),
                            },
                        );
                    }
                    _ => (),
                },
                BodyProperty::Subparts => {
                    body_part.append(
                        BodyProperty::Subparts,
                        Value::BodyPartList { value: Vec::new() },
                    );
                }
            }
        }

        EmailBodyPart {
            properties: body_part,
        }
    }

    pub fn as_body_value(&self, body_value: String, max_body_value: usize) -> EmailBodyValue {
        EmailBodyValue {
            is_encoding_problem: self.is_encoding_problem.into(),
            is_truncated: (max_body_value > 0 && body_value.len() > max_body_value).into(),
            value: if max_body_value == 0 || body_value.len() <= max_body_value {
                body_value
            } else if matches!(&self.mime_type, MimePartType::Html { .. }) {
                truncate_html(body_value.into(), max_body_value).to_string()
            } else {
                truncate_text(body_value.into(), max_body_value).to_string()
            },
        }
    }
}

pub trait AsBodyParts {
    fn as_body_parts(
        &self,
        parts: &[usize],
        properties: &[BodyProperty],
        message_raw: Option<&[u8]>,
        base_blob_id: Option<&BlobId>,
    ) -> Vec<EmailBodyPart>;
}

impl AsBodyParts for Vec<MimePart> {
    fn as_body_parts(
        &self,
        parts: &[usize],
        properties: &[BodyProperty],
        message_raw: Option<&[u8]>,
        base_blob_id: Option<&BlobId>,
    ) -> Vec<EmailBodyPart> {
        parts
            .iter()
            .filter_map(|part_id| {
                Some(self.get(*part_id)?.as_body_part(
                    *part_id,
                    properties,
                    message_raw,
                    base_blob_id,
                ))
            })
            .collect::<Vec<_>>()
    }
}

pub trait AsBodyStructure {
    fn as_body_structure(
        &self,
        properties: &[BodyProperty],
        message_raw: Option<&[u8]>,
        base_blob_id: Option<&BlobId>,
    ) -> Option<EmailBodyPart>;
}

impl AsBodyStructure for Vec<MimePart> {
    fn as_body_structure(
        &self,
        properties: &[BodyProperty],
        message_raw: Option<&[u8]>,
        base_blob_id: Option<&BlobId>,
    ) -> Option<EmailBodyPart> {
        let mut stack = Vec::new();
        let root_part = self.get(0)?;
        let mut body_structure = root_part.as_body_part(0, properties, message_raw, base_blob_id);

        if let MimePartType::MultiPart {
            subparts: part_list,
        } = &root_part.mime_type
        {
            let mut subparts = Vec::with_capacity(part_list.len());
            let mut part_list_iter = part_list.iter();

            loop {
                while let Some(part_id) = part_list_iter.next() {
                    let subpart = self.get(*part_id)?;

                    subparts.push(self.get(*part_id)?.as_body_part(
                        *part_id,
                        properties,
                        message_raw,
                        base_blob_id,
                    ));

                    if let MimePartType::MultiPart {
                        subparts: part_list,
                    } = &subpart.mime_type
                    {
                        stack.push((part_list_iter, subparts));
                        part_list_iter = part_list.iter();
                        subparts = Vec::with_capacity(part_list.len());
                    }
                }

                if let Some((prev_part_list_iter, mut prev_subparts)) = stack.pop() {
                    let prev_part = prev_subparts.last_mut().unwrap();
                    prev_part.properties.append(
                        BodyProperty::Subparts,
                        Value::BodyPartList { value: subparts },
                    );
                    part_list_iter = prev_part_list_iter;
                    subparts = prev_subparts;
                } else {
                    break;
                }
            }

            body_structure.properties.append(
                BodyProperty::Subparts,
                Value::BodyPartList { value: subparts },
            );
        }

        body_structure.into()
    }
}

pub trait AsEmailHeaders {
    fn as_email_headers(&self, message_raw: &[u8]) -> Vec<EmailHeader>;
}

impl AsEmailHeaders for MimePart {
    fn as_email_headers(&self, message_raw: &[u8]) -> Vec<EmailHeader> {
        let mut headers = Vec::with_capacity(self.raw_headers.len());
        for (header, from_offset, to_offset) in &self.raw_headers {
            for value in
                HeaderForm::Raw.parse_offsets(&[(*from_offset, *to_offset)], message_raw, true)
            {
                if let HeaderValue::Text(value) = value {
                    headers.push(EmailHeader {
                        name: header.as_str().to_string(),
                        value: value.into_owned(),
                    });
                }
            }
        }

        headers
    }
}
