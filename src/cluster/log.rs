use std::collections::{hash_map, HashMap, HashSet};
use std::task::Poll;

use actix_web::web;
use futures::poll;
use jmap_mail::import::JMAPMailImport;
use jmap_mail::mailbox::{JMAPMailMailbox, JMAPMailboxProperties, Mailbox};
use jmap_mail::query::MailboxId;
use jmap_mail::{MessageField, MessageOutline, MESSAGE_DATA, MESSAGE_RAW};
use store::batch::WriteBatch;
use store::changes::ChangeId;
use store::leb128::Leb128;
use store::raft::{Entry, LogIndex, PendingChanges, RaftId, RawEntry, TermId};
use store::serialize::{LogKey, StoreDeserialize};
use store::tracing::{debug, error};
use store::{
    lz4_flex, AccountId, Collection, ColumnFamily, DocumentId, JMAPId, Store, StoreError, Tag,
};
use store::{JMAPIdPrefix, WriteOperation};
use tokio::sync::{mpsc, oneshot, watch};

use crate::JMAPServer;

use super::rpc::UpdateCollection;
use super::{
    rpc::{self, Request, Response, RpcEvent},
    Cluster,
};
use super::{Peer, PeerId, IPC_CHANNEL_BUFFER};

const BATCH_MAX_ENTRIES: usize = 10;
const BATCH_MAX_SIZE: usize = 10 * 1024 * 1024;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Change {
    InsertMail {
        jmap_id: JMAPId,
        keywords: HashSet<Tag>,
        mailboxes: HashSet<Tag>,
        received_at: i64,
        body: Vec<u8>,
    },
    UpdateMail {
        jmap_id: JMAPId,
        keywords: HashSet<Tag>,
        mailboxes: HashSet<Tag>,
    },
    UpdateMailbox {
        jmap_id: JMAPId,
        mailbox: Mailbox,
    },
    InsertChange {
        change_id: ChangeId,
        entry: Vec<u8>,
    },
    Delete {
        document_id: DocumentId,
    },
    Commit,
}

#[derive(Debug)]
enum State {
    BecomeLeader,
    Synchronize,
    AppendEntries,
    PushChanges {
        collections: Vec<UpdateCollection>,
        changes: PendingChanges,
    },
    Wait,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum AppendEntriesRequest {
    Synchronize {
        last_log: RaftId,
    },
    UpdateLog {
        last_log: RaftId,
        entries: Vec<store::raft::RawEntry>,
    },
    UpdateStore {
        account_id: AccountId,
        collection: Collection,
        changes: Vec<Change>,
    },
}

pub struct Event {
    pub response_tx: oneshot::Sender<rpc::Response>,
    pub request: AppendEntriesRequest,
}

impl<T> Cluster<T>
where
    T: for<'x> Store<'x> + 'static,
{
    pub fn spawn_raft_leader(&self, peer: &Peer, mut log_index_rx: watch::Receiver<LogIndex>) {
        let peer_tx = peer.tx.clone();
        let mut online_rx = peer.online_rx.clone();
        let peer_name = peer.to_string();
        let local_name = self.addr.to_string();

        let term = self.term;
        let mut last_log = self.last_log;
        let main_tx = self.tx.clone();
        let core = self.core.clone();

        let mut state = State::BecomeLeader;
        let mut last_commited_id = RaftId::none();
        let mut last_sent_id = RaftId::none();

        tokio::spawn(async move {
            debug!(
                "[{}] Starting raft leader process for peer {}.",
                local_name, peer_name
            );

            'main: loop {
                // Poll the receiver to make sure this node is still the leader.
                match poll!(Box::pin(log_index_rx.changed())) {
                    Poll::Ready(result) => match result {
                        Ok(_) => {
                            last_log.index = *log_index_rx.borrow();
                            last_log.term = term;
                            if matches!(&state, State::Wait) {
                                state = State::AppendEntries;
                            }
                        }
                        Err(_) => {
                            debug!(
                                "[{}] Raft leader process for {} exiting.",
                                local_name, peer_name
                            );
                            break;
                        }
                    },
                    Poll::Pending => (),
                }

                let request = match &mut state {
                    State::BecomeLeader => {
                        state = State::Synchronize;
                        Request::BecomeFollower { term, last_log }
                    }
                    State::Synchronize => Request::AppendEntries {
                        term,
                        request: AppendEntriesRequest::Synchronize { last_log },
                    },
                    State::PushChanges {
                        changes,
                        collections,
                    } => {
                        match prepare_changes(&core, term, changes, !collections.is_empty()).await {
                            Ok(request) => request,
                            Err(err) => {
                                error!("Failed to prepare changes: {:?}", err);
                                continue;
                            }
                        }
                    }
                    State::Wait => {
                        // Wait for the next change
                        if log_index_rx.changed().await.is_ok() {
                            last_log.index = *log_index_rx.borrow();
                            last_log.term = term;
                            debug!("[{}] Received new log index: {:?}", local_name, last_log);
                        } else {
                            debug!(
                                "[{}] Raft leader process for {} exiting.",
                                local_name, peer_name
                            );
                            break;
                        }
                        state = State::AppendEntries;
                        continue;
                    }
                    State::AppendEntries => {
                        let _core = core.clone();
                        match core
                            .spawn_worker(move || {
                                _core
                                    .store
                                    .get_raft_entries(last_sent_id, BATCH_MAX_ENTRIES)
                            })
                            .await
                        {
                            Ok(entries) => {
                                if !entries.is_empty() {
                                    last_sent_id = entries.last().unwrap().id;

                                    println!("Sending updatelog {:?} {:?}", last_sent_id, last_log);

                                    Request::AppendEntries {
                                        term,
                                        request: AppendEntriesRequest::UpdateLog {
                                            last_log,
                                            entries,
                                        },
                                    }
                                } else {
                                    debug!(
                                        "[{}] Peer {} is up to date with {:?}.",
                                        local_name, peer_name, last_sent_id
                                    );
                                    state = State::Wait;
                                    continue;
                                }
                            }
                            Err(err) => {
                                error!("Error getting raft entries: {:?}", err);
                                last_sent_id = last_commited_id;
                                state = State::Wait;
                                continue;
                            }
                        }
                    }
                };

                let response = if let Some(response) = send_request(&peer_tx, request).await {
                    response
                } else {
                    debug!(
                        "[{}] Raft leader process for {} exiting (peer_tx channel closed).",
                        local_name, peer_name
                    );
                    break;
                };

                match response {
                    Response::BecomeFollower {
                        term: peer_term,
                        success,
                    } => {
                        if !success || peer_term > term {
                            if let Err(err) = main_tx
                                .send(super::Event::StepDown { term: peer_term })
                                .await
                            {
                                error!("Error sending step down message: {}", err);
                            }
                            break;
                        }
                    }

                    Response::SynchronizeLog { matched } => {
                        if !matched.is_none() {
                            let local_match = match core.get_next_raft_id(matched).await {
                                Ok(Some(local_match)) => local_match,
                                Ok(None) => {
                                    error!("Log sync failed: local match is null");
                                    break;
                                }
                                Err(err) => {
                                    error!("Error getting next raft id: {:?}", err);
                                    break;
                                }
                            };
                            //println!("leader log {:?}, peer log {:?}", local_match, matched);

                            if local_match != matched {
                                // TODO delete out of sync entries
                                error!(
                                "Failed to match raft logs with {}, local match: {:?}, peer match: {:?}", peer_name,
                                local_match, matched
                            );
                                break;
                            }
                        }

                        last_commited_id = matched;
                        last_sent_id = matched;
                        state = State::AppendEntries;
                    }
                    Response::None => {
                        // Wait until the peer is back online
                        'online: loop {
                            tokio::select! {
                                changed = log_index_rx.changed() => {
                                    match changed {
                                        Ok(()) => {
                                            last_log.index = *log_index_rx.borrow();
                                            last_log.term = term;

                                            debug!(
                                                "[{}] Received new log index while waiting: {:?}",
                                                local_name, last_log
                                            );
                                        }
                                        Err(_) => {
                                            debug!(
                                                "[{}] Raft leader process for {} exiting.",
                                                local_name, peer_name
                                            );
                                            break 'main;
                                        }
                                    }
                                },
                                online = online_rx.changed() => {
                                    match online {
                                        Ok(()) => {
                                            if *online_rx.borrow() {
                                                debug!("Peer {} is back online (rpc).", peer_name);
                                                break 'online;
                                            } else {
                                                debug!("Peer {} is still offline (rpc).", peer_name);
                                                continue 'online;
                                            }
                                        },
                                        Err(_) => {
                                            debug!(
                                                "[{}] Raft leader process for {} exiting.",
                                                local_name, peer_name
                                            );
                                            break 'main;
                                        },
                                    }
                                }
                            };
                        }
                        state = State::BecomeLeader;
                    }
                    Response::NeedUpdates { collections } => {
                        state = get_next_changes(&core, collections).await;
                    }
                    Response::Continue => match &mut state {
                        State::PushChanges {
                            changes,
                            collections,
                        } if changes.is_empty() => {
                            if collections.is_empty() {
                                last_commited_id = last_sent_id;
                                state = State::AppendEntries;
                            } else {
                                state = get_next_changes(&core, std::mem::take(collections)).await;
                            }
                        }
                        _ => (),
                    },

                    response @ (Response::UpdatePeers { .. }
                    | Response::Vote { .. }
                    | Response::Pong) => {
                        error!(
                            "Unexpected response from peer {}: {:?}",
                            peer_name, response
                        );
                    }
                }
            }
        });
    }

    pub fn spawn_raft_follower(&self) -> mpsc::Sender<Event> {
        let (tx, mut rx) = mpsc::channel::<Event>(IPC_CHANNEL_BUFFER);
        let core = self.core.clone();

        tokio::spawn(async move {
            let mut commit_id = RaftId::none();
            let mut pending_entries = Vec::new();

            while let Some(event) = rx.recv().await {
                event
                    .response_tx
                    .send(match event.request {
                        AppendEntriesRequest::Synchronize { last_log } => {
                            println!("Set commit id from sync {:?}", last_log);
                            commit_id = last_log;
                            Response::SynchronizeLog {
                                matched: match core.get_prev_raft_id(last_log).await {
                                    Ok(Some(matched)) => {
                                        if commit_id != last_log {
                                            println!(
                                                "Commit index mismatch on sync: {:?} != {:?}",
                                                commit_id, last_log
                                            );
                                        } else {
                                            println!(
                                                "OK commit (sync): {:?} == {:?}",
                                                commit_id, last_log
                                            );
                                        }

                                        core.set_up_to_date(matched == last_log);
                                        matched
                                    }
                                    Ok(None) => {
                                        if last_log.is_none() {
                                            core.set_up_to_date(true);
                                        }
                                        RaftId::none()
                                    }
                                    Err(err) => {
                                        debug!("Failed to get prev raft id: {:?}", err);
                                        RaftId::none()
                                    }
                                },
                            }
                        }
                        AppendEntriesRequest::UpdateLog { last_log, entries } => {
                            println!("Set commit id from update log {:?}", last_log);
                            commit_id = last_log;
                            handle_update_log(&core, last_log, entries, &mut pending_entries).await
                        }
                        AppendEntriesRequest::UpdateStore {
                            account_id,
                            collection,
                            changes,
                        } => {
                            handle_update_store(
                                &core,
                                &mut pending_entries,
                                commit_id,
                                account_id,
                                collection,
                                changes,
                            )
                            .await
                        }
                    })
                    .unwrap_or_else(|_| error!("Oneshot response channel closed."));
            }
        });
        tx
    }

    pub async fn handle_become_follower(
        &mut self,
        peer_id: PeerId,
        response_tx: oneshot::Sender<rpc::Response>,
        term: TermId,
        last_log: RaftId,
    ) {
        if self.term < term {
            self.term = term;
        }

        if self.term == term && self.log_is_behind_or_eq(last_log.term, last_log.index) {
            self.follow_leader(peer_id)
                .send(Event {
                    response_tx,
                    request: AppendEntriesRequest::Synchronize { last_log },
                })
                .await
                .unwrap_or_else(|err| error!("Failed to send event: {}", err));
        } else {
            response_tx
                .send(Response::BecomeFollower {
                    term: self.term,
                    success: false,
                })
                .unwrap_or_else(|_| error!("Oneshot response channel closed."));
        }
    }

    pub async fn handle_append_entries(
        &mut self,
        peer_id: PeerId,
        response_tx: oneshot::Sender<rpc::Response>,
        term: TermId,
        request: AppendEntriesRequest,
    ) {
        if term > self.term {
            self.term = term;
        }

        match self.is_following_peer(peer_id) {
            Some(tx) => {
                tx.send(Event {
                    response_tx,
                    request,
                })
                .await
                .unwrap_or_else(|err| error!("Failed to send event: {}", err));
            }
            _ => response_tx
                .send(rpc::Response::BecomeFollower {
                    term: self.term,
                    success: false,
                })
                .unwrap_or_else(|_| error!("Oneshot response channel closed.")),
        }
    }
}

pub async fn handle_update_log<T>(
    core_: &web::Data<JMAPServer<T>>,
    commit_id: RaftId,
    entries: Vec<RawEntry>,
    pending_entries: &mut Vec<RawEntry>,
) -> Response
where
    T: for<'x> Store<'x> + 'static,
{
    debug_assert!(!entries.is_empty());

    let core = core_.clone();
    match core_
        .spawn_worker(move || {
            let mut update_collections = HashMap::new();
            for raw_entry in &entries {
                let mut entry = Entry::deserialize(&raw_entry.data).ok_or_else(|| {
                    StoreError::InternalError(format!(
                        "Failed to deserialize entry: {:?}",
                        raw_entry
                    ))
                })?;

                while let Some((account_id, collections)) = entry.next_account() {
                    for collection in collections {
                        if let hash_map::Entry::Vacant(e) =
                            update_collections.entry((account_id, collection))
                        {
                            e.insert(UpdateCollection {
                                account_id,
                                collection,
                                from_change_id: if let Some(last_change_id) =
                                    core.store.get_last_change_id(account_id, collection)?
                                {
                                    if raw_entry.id.index <= last_change_id {
                                        continue;
                                    } else {
                                        Some(last_change_id)
                                    }
                                } else {
                                    None
                                },
                            });
                        }
                    }
                }
            }

            Ok((update_collections, entries))
        })
        .await
    {
        Ok((collections, entries)) => {
            if !collections.is_empty() {
                core_.set_up_to_date(false);
                *pending_entries = entries;
                Response::NeedUpdates {
                    collections: collections.into_values().collect(),
                }
            } else if commit_log(core_, entries, commit_id).await {
                Response::Continue
            } else {
                Response::None
            }
        }
        Err(err) => {
            debug!("Worker failed: {:?}", err);
            Response::None
        }
    }
}

pub async fn handle_update_store<T>(
    core_: &web::Data<JMAPServer<T>>,
    pending_entries: &mut Vec<RawEntry>,
    commit_id: RaftId,
    account_id: AccountId,
    collection: Collection,
    changes: Vec<Change>,
) -> Response
where
    T: for<'x> Store<'x> + 'static,
{
    //println!("{:#?}", changes);

    let core = core_.clone();
    match core_
        .spawn_worker(move || {
            let mut do_commit = false;
            let mut document_batch = WriteBatch::new(account_id);
            let mut log_batch = Vec::with_capacity(changes.len());

            debug!(
                "Inserting {} changes in {}/{:?}...",
                changes.len(),
                account_id,
                collection
            );

            for change in changes {
                match change {
                    Change::InsertMail {
                        jmap_id,
                        keywords,
                        mailboxes,
                        received_at,
                        body,
                    } => {
                        core.store.raft_update_mail(
                            &mut document_batch,
                            account_id,
                            jmap_id,
                            mailboxes,
                            keywords,
                            Some((
                                lz4_flex::decompress_size_prepended(&body).map_err(|err| {
                                    StoreError::InternalError(format!(
                                        "Failed to decompress raft update: {}",
                                        err
                                    ))
                                })?,
                                received_at,
                            )),
                        )?;
                    }
                    Change::UpdateMail {
                        jmap_id,
                        keywords,
                        mailboxes,
                    } => {
                        core.store.raft_update_mail(
                            &mut document_batch,
                            account_id,
                            jmap_id,
                            mailboxes,
                            keywords,
                            None,
                        )?;
                    }
                    Change::UpdateMailbox { jmap_id, mailbox } => {
                        core.store.raft_update_mailbox(
                            &mut document_batch,
                            account_id,
                            jmap_id,
                            mailbox,
                        )?;
                    }
                    Change::InsertChange { change_id, entry } => {
                        log_batch.push(WriteOperation::set(
                            ColumnFamily::Logs,
                            LogKey::serialize_change(account_id, collection, change_id),
                            entry,
                        ));
                    }
                    Change::Delete { document_id } => {
                        document_batch.delete_document(collection, document_id)
                    }
                    Change::Commit => {
                        do_commit = true;
                    }
                }
            }
            if !document_batch.is_empty() {
                core.store.write(document_batch)?;
            }
            if !log_batch.is_empty() {
                core.store.db.write(log_batch)?;
            }

            Ok(do_commit)
        })
        .await
    {
        Ok(do_commit) => {
            if do_commit && !commit_log(core_, std::mem::take(pending_entries), commit_id).await {
                Response::None
            } else {
                Response::Continue
            }
        }
        Err(err) => {
            debug!("Failed to update store: {:?}", err);
            Response::None
        }
    }
}

pub async fn commit_log<T>(
    core_: &web::Data<JMAPServer<T>>,
    entries: Vec<RawEntry>,
    commit_id: RaftId,
) -> bool
where
    T: for<'x> Store<'x> + 'static,
{
    if !entries.is_empty() {
        let core = core_.clone();
        let last_log = entries.last().map(|e| e.id);

        match core_
            .spawn_worker(move || core.store.insert_raft_entries(entries))
            .await
        {
            Ok(_) => {
                // If this node matches the leader's commit index,
                // read-only requests can be accepted on this node.
                if let Some(last_log) = last_log {
                    core_.update_raft_index(last_log.index);
                    core_.store_changed(last_log).await;

                    if commit_id != last_log {
                        println!("Commit index mismatch: {:?} != {:?}", commit_id, last_log);
                    } else {
                        println!("OK commit: {:?} == {:?}", commit_id, last_log);
                    }
                    if commit_id == last_log {
                        core_.set_up_to_date(true);
                    }
                }
            }
            Err(err) => {
                error!("Failed to commit pending changes: {:?}", err);
                return false;
            }
        }
    }

    true
}

async fn send_request(peer_tx: &mpsc::Sender<rpc::RpcEvent>, request: Request) -> Option<Response> {
    let (response_tx, rx) = oneshot::channel();
    peer_tx
        .send(RpcEvent::NeedResponse {
            request,
            response_tx,
        })
        .await
        .ok()?;
    rx.await.unwrap_or(Response::None).into()
}

async fn get_next_changes<T>(
    core: &web::Data<JMAPServer<T>>,
    mut collections: Vec<UpdateCollection>,
) -> State
where
    T: for<'x> Store<'x> + 'static,
{
    loop {
        let collection = if let Some(collection) = collections.pop() {
            collection
        } else {
            return State::AppendEntries;
        };

        let _core = core.clone();
        match core
            .spawn_worker(move || {
                _core.store.get_pending_changes(
                    collection.account_id,
                    collection.collection,
                    collection.from_change_id,
                    matches!(collection.collection, Collection::Thread),
                )
            })
            .await
        {
            Ok(changes) => {
                //debug_assert!(!changes.is_empty(), "{:#?}", changes);
                //println!("Changes: {:#?}", changes);
                if !changes.is_empty() {
                    return State::PushChanges {
                        collections,
                        changes,
                    };
                }
            }
            Err(err) => {
                error!("Error getting raft changes: {:?}", err);
                return State::Synchronize;
            }
        }
    }
}

async fn prepare_changes<T>(
    core: &web::Data<JMAPServer<T>>,
    term: TermId,
    changes: &mut PendingChanges,
    has_more_changes: bool,
) -> store::Result<Request>
where
    T: for<'x> Store<'x> + 'static,
{
    let mut batch_size = 0;
    let mut push_changes = Vec::new();

    loop {
        let item = if let Some(document_id) = changes.inserts.min() {
            changes.inserts.remove(document_id);
            fetch_item(
                core,
                changes.account_id,
                changes.collection,
                document_id,
                true,
            )
            .await?
        } else if let Some(document_id) = changes.updates.min() {
            changes.updates.remove(document_id);
            fetch_item(
                core,
                changes.account_id,
                changes.collection,
                document_id,
                false,
            )
            .await?
        } else if let Some(document_id) = changes.deletes.min() {
            changes.deletes.remove(document_id);
            Some((
                Change::Delete { document_id },
                std::mem::size_of::<Change>(),
            ))
        } else if let Some(change_id) = changes.changes.min() {
            changes.changes.remove(change_id);
            fetch_raw_change(core, changes.account_id, changes.collection, change_id).await?
        } else {
            break;
        };

        if let Some((item, item_size)) = item {
            push_changes.push(item);
            batch_size += item_size;
        } else {
            debug_assert!(
                false,
                "Failed to fetch item in collection {:?}",
                changes.collection,
            );
        }

        if batch_size >= BATCH_MAX_SIZE {
            break;
        }
    }

    if changes.is_empty() && !has_more_changes {
        push_changes.push(Change::Commit);
    }

    Ok(Request::AppendEntries {
        term,
        request: AppendEntriesRequest::UpdateStore {
            account_id: changes.account_id,
            collection: changes.collection,
            changes: push_changes,
        },
    })
}

async fn fetch_item<T>(
    core: &web::Data<JMAPServer<T>>,
    account_id: AccountId,
    collection: Collection,
    document_id: DocumentId,
    is_insert: bool,
) -> store::Result<Option<(Change, usize)>>
where
    T: for<'x> Store<'x> + 'static,
{
    match collection {
        Collection::Mail => fetch_email(core, account_id, document_id, is_insert).await,
        Collection::Mailbox => fetch_mailbox(core, account_id, document_id).await,
        _ => Err(StoreError::InternalError(
            "Unsupported collection for changes".into(),
        )),
    }
}

async fn fetch_email<T>(
    core: &web::Data<JMAPServer<T>>,
    account_id: AccountId,
    document_id: DocumentId,
    is_insert: bool,
) -> store::Result<Option<(Change, usize)>>
where
    T: for<'x> Store<'x> + 'static,
{
    let _core = core.clone();
    core.spawn_worker(move || {
        let mut item_size = std::mem::size_of::<Change>();

        let mailboxes = if let Some(mailboxes) = _core.store.get_document_tags(
            account_id,
            Collection::Mail,
            document_id,
            MessageField::Mailbox.into(),
        )? {
            item_size += mailboxes.items.len() * std::mem::size_of::<MailboxId>();
            mailboxes.items
        } else {
            return Ok(None);
        };
        let keywords = if let Some(keywords) = _core.store.get_document_tags(
            account_id,
            Collection::Mail,
            document_id,
            MessageField::Keyword.into(),
        )? {
            item_size += keywords.items.iter().map(|tag| tag.len()).sum::<usize>();
            keywords.items
        } else {
            HashSet::new()
        };

        let jmap_id = if let Some(thread_id) = _core.store.get_document_tag_id(
            account_id,
            Collection::Mail,
            document_id,
            MessageField::ThreadId.into(),
        )? {
            JMAPId::from_parts(thread_id, document_id)
        } else {
            return Ok(None);
        };

        Ok(if is_insert {
            if let (Some(body), Some(message_data_bytes)) = (
                _core
                    .store
                    .get_blob(account_id, Collection::Mail, document_id, MESSAGE_RAW)?,
                _core
                    .store
                    .get_blob(account_id, Collection::Mail, document_id, MESSAGE_DATA)?,
            ) {
                // Deserialize the message data
                let (message_data_len, read_bytes) =
                    usize::from_leb128_bytes(&message_data_bytes[..])
                        .ok_or(StoreError::DataCorruption)?;
                let message_outline = MessageOutline::deserialize(
                    &message_data_bytes[read_bytes + message_data_len..],
                )
                .ok_or(StoreError::DataCorruption)?;

                // Compress body
                let body = lz4_flex::compress_prepend_size(&body);
                item_size += body.len();
                Some((
                    Change::InsertMail {
                        jmap_id,
                        keywords,
                        mailboxes,
                        body,
                        received_at: message_outline.received_at,
                    },
                    item_size,
                ))
            } else {
                None
            }
        } else {
            Some((
                Change::UpdateMail {
                    jmap_id,
                    keywords,
                    mailboxes,
                },
                item_size,
            ))
        })
    })
    .await
}

async fn fetch_mailbox<T>(
    core: &web::Data<JMAPServer<T>>,
    account_id: AccountId,
    document_id: DocumentId,
) -> store::Result<Option<(Change, usize)>>
where
    T: for<'x> Store<'x> + 'static,
{
    let _core = core.clone();
    core.spawn_worker(move || {
        Ok(_core
            .store
            .get_document_value::<Mailbox>(
                account_id,
                Collection::Mailbox,
                document_id,
                JMAPMailboxProperties::Id.into(),
            )?
            .map(|mailbox| {
                (
                    Change::UpdateMailbox {
                        jmap_id: document_id as JMAPId,
                        mailbox,
                    },
                    std::mem::size_of::<Mailbox>(),
                )
            }))
    })
    .await
}

async fn fetch_raw_change<T>(
    core: &web::Data<JMAPServer<T>>,
    account_id: AccountId,
    collection: Collection,
    change_id: ChangeId,
) -> store::Result<Option<(Change, usize)>>
where
    T: for<'x> Store<'x> + 'static,
{
    let _core = core.clone();
    core.spawn_worker(move || {
        Ok(_core
            .store
            .db
            .get::<Vec<u8>>(
                ColumnFamily::Logs,
                &LogKey::serialize_change(account_id, collection, change_id),
            )?
            .map(|entry| {
                (
                    Change::InsertChange { change_id, entry },
                    std::mem::size_of::<Change>(),
                )
            }))
    })
    .await
}
