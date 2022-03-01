use std::{collections::HashMap, sync::Arc};

use nlp::Language;
use rocksdb::BoundColumnFamily;
use store::{
    batch::{LogAction, WriteAction, WriteBatch},
    field::{FieldOptions, Text, TokenIterator, UpdateField},
    serialize::{
        serialize_acd_key_leb128, serialize_blob_key, serialize_bm_internal, serialize_bm_tag_key,
        serialize_bm_term_key, serialize_bm_text_key, serialize_changelog_key, serialize_index_key,
        serialize_stored_key, BLOB_KEY, BM_FREED_IDS, BM_TOMBSTONED_IDS, BM_USED_IDS,
    },
    term_index::TermIndexBuilder,
    AccountId, ChangeLogId, CollectionId, DocumentId, StoreBlob, StoreError, StoreUpdate,
};

use crate::{
    bitmaps::set_clear_bits,
    blob::{blob_to_key, serialize_blob_keys_from_value},
    changelog::ChangeLogWriter,
    document_id::AssignedDocumentId,
    RocksDBStore,
};

impl StoreUpdate for RocksDBStore {
    type UncommittedId = AssignedDocumentId;

    fn update_documents(
        &self,
        account: AccountId,
        batches: Vec<WriteBatch<AssignedDocumentId>>,
    ) -> store::Result<()> {
        let cf_values = self.get_handle("values")?;
        let cf_indexes = self.get_handle("indexes")?;
        let cf_bitmaps = self.get_handle("bitmaps")?;
        let mut write_batch = rocksdb::WriteBatch::default();

        let mut change_log_list = HashMap::new();
        let mut bitmap_list = HashMap::new();

        for batch in batches {
            match batch.action {
                WriteAction::Insert(document_id) => {
                    let document_id = match document_id {
                        AssignedDocumentId::Freed(document_id) => {
                            // Remove document id from freed ids
                            bitmap_list
                                .entry(serialize_bm_internal(
                                    account,
                                    batch.collection,
                                    BM_FREED_IDS,
                                ))
                                .or_insert_with(HashMap::new)
                                .insert(document_id, false);
                            document_id
                        }
                        AssignedDocumentId::New(document_id) => document_id,
                    };

                    // Add document id to collection
                    bitmap_list
                        .entry(serialize_bm_internal(
                            account,
                            batch.collection,
                            BM_USED_IDS,
                        ))
                        .or_insert_with(HashMap::new)
                        .insert(document_id, true);

                    self.update_document(
                        &mut write_batch,
                        &cf_values,
                        &cf_indexes,
                        &mut bitmap_list,
                        account,
                        batch.collection,
                        document_id,
                        batch.default_language,
                        batch.fields,
                    )?;
                }
                WriteAction::Update(document_id) => {
                    self.update_document(
                        &mut write_batch,
                        &cf_values,
                        &cf_indexes,
                        &mut bitmap_list,
                        account,
                        batch.collection,
                        document_id,
                        batch.default_language,
                        batch.fields,
                    )?;
                }
                WriteAction::Delete(document_id) => {
                    // Remove any external blobs
                    if let Some(blob) = self
                        .db
                        .get_cf(
                            &cf_values,
                            &serialize_blob_key(account, batch.collection, document_id),
                        )
                        .map_err(|e| StoreError::InternalError(e.into_string()))?
                    {
                        // Decrement blob count
                        serialize_blob_keys_from_value(&blob)
                            .ok_or(StoreError::DataCorruption)?
                            .into_iter()
                            .for_each(|key| {
                                write_batch.merge_cf(&cf_values, &key, (-1i64).to_le_bytes());
                            });
                    }

                    // Add document id to tombstoned ids
                    bitmap_list
                        .entry(serialize_bm_internal(
                            account,
                            batch.collection,
                            BM_TOMBSTONED_IDS,
                        ))
                        .or_insert_with(HashMap::new)
                        .insert(document_id, true);
                }
            }

            let change_id = if let Some(change_id) = batch.log_id {
                change_id
            } else {
                self.assign_change_id(account, batch.collection)?
            };
            let change_log_entry = change_log_list
                .entry((account, batch.collection, change_id))
                .or_insert_with(ChangeLogWriter::new);

            match batch.log_action {
                LogAction::Insert(id) => {
                    change_log_entry.inserts.push(id);
                }
                LogAction::Update(id) => {
                    change_log_entry.updates.push(id);
                }
                LogAction::Delete(id) => {
                    change_log_entry.deletes.push(id);
                }
                LogAction::Move(old_id, id) => {
                    change_log_entry.inserts.push(id);
                    change_log_entry.deletes.push(old_id);
                }
            }
        }

        if !change_log_list.is_empty() {
            let cf_log = self.get_handle("log")?;
            for ((account, collection, change_id), change_log) in change_log_list {
                write_batch.put_cf(
                    &cf_log,
                    serialize_changelog_key(account, collection, change_id),
                    change_log.serialize(),
                );
            }
        }

        for (key, doc_id_list) in bitmap_list {
            write_batch.merge_cf(&cf_bitmaps, key, set_clear_bits(doc_id_list.into_iter()))
        }

        self.db
            .write(write_batch)
            .map_err(|e| StoreError::InternalError(e.to_string()))?;

        Ok(())
    }

    fn assign_change_id(
        &self,
        account: AccountId,
        collection: CollectionId,
    ) -> store::Result<ChangeLogId> {
        let id_assigner_lock = self.get_id_assigner(account, collection)?;
        let mut id_assigner = id_assigner_lock
            .lock()
            .map_err(|_| StoreError::InternalError("Failed to obtain MutexLock".to_string()))?;

        Ok(id_assigner.assign_change_id())
    }

    fn assign_document_id(
        &self,
        account: AccountId,
        collection: CollectionId,
    ) -> store::Result<AssignedDocumentId> {
        let id_assigner_lock = self.get_id_assigner(account, collection)?;
        let mut id_assigner = id_assigner_lock
            .lock()
            .map_err(|_| StoreError::InternalError("Failed to obtain MutexLock".to_string()))?;

        Ok(id_assigner.assign_id())
    }
}

impl RocksDBStore {
    #[allow(clippy::too_many_arguments)]
    fn update_document(
        &self,
        batch: &mut rocksdb::WriteBatch,
        cf_values: &Arc<BoundColumnFamily>,
        cf_indexes: &Arc<BoundColumnFamily>,
        bitmap_list: &mut HashMap<Vec<u8>, HashMap<DocumentId, bool>>,
        account: AccountId,
        collection: CollectionId,
        document_id: DocumentId,
        default_language: Language,
        fields: Vec<UpdateField>,
    ) -> crate::Result<()> {
        // Full text term positions
        let mut term_index = TermIndexBuilder::new();
        let mut blob_fields = Vec::new();

        for field in fields.iter() {
            match field {
                UpdateField::Text(t) => {
                    let (is_stored, is_sorted, is_clear, blob_index) = match t.get_options() {
                        FieldOptions::None => (false, false, false, None),
                        FieldOptions::Store => (true, false, false, None),
                        FieldOptions::Sort => (false, true, false, None),
                        FieldOptions::StoreAndSort => (true, true, false, None),
                        FieldOptions::StoreAsBlob(blob_index) => {
                            (false, false, false, Some(blob_index))
                        }
                        FieldOptions::Clear => (false, false, true, None),
                    };

                    let text = match &t.value {
                        Text::Default(text) => text,
                        Text::Keyword(text) => {
                            bitmap_list
                                .entry(serialize_bm_text_key(
                                    account,
                                    collection,
                                    t.get_field(),
                                    text,
                                ))
                                .or_insert_with(HashMap::new)
                                .insert(document_id, !is_clear);
                            text
                        }
                        Text::Tokenized(text) => {
                            for token in TokenIterator::new(text, Language::English, false) {
                                bitmap_list
                                    .entry(serialize_bm_text_key(
                                        account,
                                        collection,
                                        t.get_field(),
                                        &token.word,
                                    ))
                                    .or_insert_with(HashMap::new)
                                    .insert(document_id, !is_clear);
                            }
                            text
                        }
                        Text::Full(ft) => {
                            let terms = self.get_terms(TokenIterator::new(
                                ft.text.as_ref(),
                                if ft.language == Language::Unknown {
                                    default_language
                                } else {
                                    ft.language
                                },
                                true,
                            ))?;
                            if !terms.is_empty() {
                                for term in &terms {
                                    bitmap_list
                                        .entry(serialize_bm_term_key(
                                            account,
                                            collection,
                                            t.get_field(),
                                            term.id,
                                            true,
                                        ))
                                        .or_insert_with(HashMap::new)
                                        .insert(document_id, !is_clear);

                                    if term.id_stemmed != term.id {
                                        bitmap_list
                                            .entry(serialize_bm_term_key(
                                                account,
                                                collection,
                                                t.get_field(),
                                                term.id_stemmed,
                                                false,
                                            ))
                                            .or_insert_with(HashMap::new)
                                            .insert(document_id, !is_clear);
                                    }
                                }

                                term_index.add_item(t.get_field(), blob_index.unwrap_or(0), terms);
                            }
                            &ft.text
                        }
                    };

                    if let Some(blob_index) = blob_index {
                        blob_fields.push((blob_index, text.as_bytes()));
                    } else if !is_clear {
                        if is_stored {
                            batch.put_cf(
                                cf_values,
                                serialize_stored_key(
                                    account,
                                    collection,
                                    document_id,
                                    t.get_field(),
                                ),
                                text.as_bytes(),
                            );
                        }

                        if is_sorted {
                            batch.put_cf(
                                cf_indexes,
                                &serialize_index_key(
                                    account,
                                    collection,
                                    document_id,
                                    t.get_field(),
                                    text.as_bytes(),
                                ),
                                &[],
                            );
                        }
                    } else {
                        batch.delete_cf(
                            cf_values,
                            serialize_stored_key(account, collection, document_id, t.get_field()),
                        );

                        batch.delete_cf(
                            cf_indexes,
                            &serialize_index_key(
                                account,
                                collection,
                                document_id,
                                t.get_field(),
                                text.as_bytes(),
                            ),
                        );
                    }
                }
                UpdateField::Tag(t) => {
                    bitmap_list
                        .entry(serialize_bm_tag_key(
                            account,
                            collection,
                            t.get_field(),
                            &t.value,
                        ))
                        .or_insert_with(HashMap::new)
                        .insert(document_id, !t.is_clear());
                }
                UpdateField::Binary(b) => {
                    if let FieldOptions::StoreAsBlob(blob_index) = b.get_options() {
                        blob_fields.push((blob_index, b.value.as_ref()));
                    } else {
                        batch.put_cf(
                            cf_values,
                            serialize_stored_key(account, collection, document_id, b.get_field()),
                            &b.value,
                        );
                    }
                }
                UpdateField::Integer(i) => {
                    if !i.is_clear() {
                        if i.is_stored() {
                            batch.put_cf(
                                cf_values,
                                serialize_stored_key(
                                    account,
                                    collection,
                                    document_id,
                                    i.get_field(),
                                ),
                                &i.value.to_le_bytes(),
                            );
                        }

                        if i.is_sorted() {
                            batch.put_cf(
                                cf_indexes,
                                &serialize_index_key(
                                    account,
                                    collection,
                                    document_id,
                                    i.get_field(),
                                    &i.value.to_be_bytes(),
                                ),
                                &[],
                            );
                        }
                    } else {
                        batch.delete_cf(
                            cf_values,
                            serialize_stored_key(account, collection, document_id, i.get_field()),
                        );

                        batch.delete_cf(
                            cf_indexes,
                            &serialize_index_key(
                                account,
                                collection,
                                document_id,
                                i.get_field(),
                                &i.value.to_be_bytes(),
                            ),
                        );
                    }
                }
                UpdateField::LongInteger(i) => {
                    if !i.is_clear() {
                        if i.is_stored() {
                            batch.put_cf(
                                cf_values,
                                serialize_stored_key(
                                    account,
                                    collection,
                                    document_id,
                                    i.get_field(),
                                ),
                                &i.value.to_le_bytes(),
                            );
                        }

                        if i.is_sorted() {
                            batch.put_cf(
                                cf_indexes,
                                &serialize_index_key(
                                    account,
                                    collection,
                                    document_id,
                                    i.get_field(),
                                    &i.value.to_be_bytes(),
                                ),
                                &[],
                            );
                        }
                    } else {
                        batch.delete_cf(
                            cf_values,
                            serialize_stored_key(account, collection, document_id, i.get_field()),
                        );

                        batch.delete_cf(
                            cf_indexes,
                            &serialize_index_key(
                                account,
                                collection,
                                document_id,
                                i.get_field(),
                                &i.value.to_be_bytes(),
                            ),
                        );
                    }
                }
                UpdateField::Float(f) => {
                    if !f.is_clear() {
                        if f.is_stored() {
                            batch.put_cf(
                                cf_values,
                                serialize_stored_key(
                                    account,
                                    collection,
                                    document_id,
                                    f.get_field(),
                                ),
                                &f.value.to_le_bytes(),
                            );
                        }

                        if f.is_sorted() {
                            batch.put_cf(
                                cf_indexes,
                                &serialize_index_key(
                                    account,
                                    collection,
                                    document_id,
                                    f.get_field(),
                                    &f.value.to_be_bytes(),
                                ),
                                &[],
                            );
                        }
                    } else {
                        batch.delete_cf(
                            cf_values,
                            serialize_stored_key(account, collection, document_id, f.get_field()),
                        );

                        batch.delete_cf(
                            cf_indexes,
                            &serialize_index_key(
                                account,
                                collection,
                                document_id,
                                f.get_field(),
                                &f.value.to_be_bytes(),
                            ),
                        );
                    }
                }
            };
        }

        // Compress and store TermIndex
        if !term_index.is_empty() {
            batch.put_cf(
                cf_values,
                &serialize_acd_key_leb128(account, collection, document_id),
                &term_index.compress(),
            );
        }

        // Store external blobs
        if !blob_fields.is_empty() {
            let mut blob_index_last = None;
            let mut blob_entries = Vec::with_capacity(
                std::mem::size_of::<usize>()
                    + (blob_fields.len() * (32 + std::mem::size_of::<u32>())),
            );

            blob_fields.sort_unstable_by_key(|(blob_index, _)| *blob_index);

            for (blob_index, blob) in &blob_fields {
                if let Some(blob_index_last) = blob_index_last {
                    if blob_index_last + 1 != *blob_index {
                        return Err(StoreError::InternalError(
                            "Blob indexes are not sequential".into(),
                        ));
                    }
                } else if *blob_index != 0 {
                    return Err(StoreError::InternalError(
                        "First blob index is not 0".into(),
                    ));
                }
                blob_index_last = Some(blob_index);
                let blob_key = blob_to_key(blob);
                self.store_blob(&blob_key, blob)?;

                // Increment blob count
                batch.merge_cf(cf_values, &blob_key, (1i64).to_le_bytes());

                blob_entries.extend_from_slice(&blob_key[BLOB_KEY.len()..]);
            }

            batch.put_cf(
                cf_values,
                &serialize_blob_key(account, collection, document_id),
                &blob_entries,
            );
        }
        Ok(())
    }
}
