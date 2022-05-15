use std::collections::HashMap;

use actix_web::web;
use jmap::id::{state::JMAPState, JMAPIdSerialize};
use jmap_client::{
    client::Client,
    core::{
        query::Filter,
        set::{Create, SetError, SetErrorType, SetRequest},
    },
    mailbox::{self, Mailbox, Role},
    Error, Set,
};
use serde::{Deserialize, Serialize};
use store::Store;

use crate::{tests::store::utils::StoreCompareWith, JMAPServer};

pub async fn test<T>(server: web::Data<JMAPServer<T>>, client: &mut Client)
where
    T: for<'x> Store<'x> + 'static,
{
    // Create test mailboxes
    let id_map = create_test_mailboxes(client).await;

    println!("{:?}", id_map);

    // Sort by name
    assert_eq!(
        client
            .mailbox_query(
                None::<mailbox::query::Filter>,
                [mailbox::query::Comparator::name()].into()
            )
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        [
            "drafts",
            "spam2",
            "inbox",
            "1",
            "2",
            "3",
            "sent",
            "spam",
            "1.1",
            "1.2",
            "trash",
            "spam1",
            "1.1.1.1",
            "1.1.1.1.1",
            "1.1.1",
            "1.2.1"
        ]
    );

    // Sort by name as tree
    let mut request = client.build();
    request
        .query_mailbox()
        .sort([mailbox::query::Comparator::name()])
        .arguments()
        .sort_as_tree(true);
    assert_eq!(
        request
            .send_query_mailbox()
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        [
            "drafts",
            "inbox",
            "1",
            "1.1",
            "1.1.1",
            "1.1.1.1",
            "1.1.1.1.1",
            "1.2",
            "1.2.1",
            "2",
            "3",
            "sent",
            "spam",
            "spam1",
            "spam2",
            "trash"
        ]
    );

    // Sort as tree with filters
    let mut request = client.build();
    request
        .query_mailbox()
        .filter(mailbox::query::Filter::name("level"))
        .sort([mailbox::query::Comparator::name()])
        .arguments()
        .sort_as_tree(true);
    assert_eq!(
        request
            .send_query_mailbox()
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        [
            "1",
            "1.1",
            "1.1.1",
            "1.1.1.1",
            "1.1.1.1.1",
            "1.2",
            "1.2.1",
            "2",
            "3"
        ]
    );

    // Filter as tree
    let mut request = client.build();
    request
        .query_mailbox()
        .filter(mailbox::query::Filter::name("spam"))
        .sort([mailbox::query::Comparator::name()])
        .arguments()
        .filter_as_tree(true)
        .sort_as_tree(true);
    assert_eq!(
        request
            .send_query_mailbox()
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        ["spam", "spam1", "spam2"]
    );

    let mut request = client.build();
    request
        .query_mailbox()
        .filter(mailbox::query::Filter::name("level"))
        .sort([mailbox::query::Comparator::name()])
        .arguments()
        .filter_as_tree(true)
        .sort_as_tree(true);
    assert_eq!(
        request.send_query_mailbox().await.unwrap().ids(),
        Vec::<&str>::new()
    );

    // Filter by role
    assert_eq!(
        client
            .mailbox_query(
                mailbox::query::Filter::role(Role::Inbox).into(),
                [mailbox::query::Comparator::name()].into()
            )
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        ["inbox"]
    );

    assert_eq!(
        client
            .mailbox_query(
                mailbox::query::Filter::has_any_role(true).into(),
                [mailbox::query::Comparator::name()].into()
            )
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        ["drafts", "inbox", "sent", "spam", "trash"]
    );

    // Duplicate role
    let mut request = client.build();
    request
        .set_mailbox()
        .update(&id_map["sent"])
        .role(Role::Inbox);
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .updated(&id_map["sent"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::InvalidProperties,
            ..
        }))
    ));

    // Duplicate name
    let mut request = client.build();
    request.set_mailbox().update(&id_map["2"]).name("Level 3");
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .updated(&id_map["2"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::InvalidProperties,
            ..
        }))
    ));

    // Circular relationship
    let mut request = client.build();
    request
        .set_mailbox()
        .update(&id_map["1"])
        .parent_id((&id_map["1.1.1.1.1"]).into());
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .updated(&id_map["1"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::InvalidProperties,
            ..
        }))
    ));

    let mut request = client.build();
    request
        .set_mailbox()
        .update(&id_map["1"])
        .parent_id((&id_map["1"]).into());
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .updated(&id_map["1"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::InvalidProperties,
            ..
        }))
    ));

    // Invalid parentId
    let mut request = client.build();
    request
        .set_mailbox()
        .update(&id_map["1"])
        .parent_id(u64::MAX.to_jmap_string().into());
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .updated(&id_map["1"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::InvalidProperties,
            ..
        }))
    ));

    // Obtain state
    let state = client
        .mailbox_changes(JMAPState::Initial.to_jmap_string(), 0)
        .await
        .unwrap()
        .new_state()
        .to_string();

    // Rename and move mailbox
    let mut request = client.build();
    request
        .set_mailbox()
        .update(&id_map["1.1.1.1.1"])
        .name("Renamed and moved")
        .parent_id((&id_map["2"]).into());
    assert!(request
        .send_set_mailbox()
        .await
        .unwrap()
        .updated(&id_map["1.1.1.1.1"])
        .is_ok());

    // Verify changes
    let state = client.mailbox_changes(state, 0).await.unwrap();
    assert_eq!(state.created().len(), 0);
    assert_eq!(state.updated().len(), 1);
    assert_eq!(state.destroyed().len(), 0);
    assert_eq!(state.arguments().updated_properties(), None);
    let state = state.new_state().to_string();

    // Insert email into Inbox
    let mail_id = client
        .email_import(
            b"From: test@test.com\nSubject: hey\n\ntest".to_vec(),
            [&id_map["inbox"]],
            None::<Vec<&str>>,
            None,
        )
        .await
        .unwrap()
        .unwrap_id();

    // Only email properties must have changed
    let state = client.mailbox_changes(state, 0).await.unwrap();
    assert_eq!(state.created().len(), 0);
    assert_eq!(
        state
            .updated()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        &[&id_map["inbox"]]
    );
    assert_eq!(state.destroyed().len(), 0);
    assert_eq!(
        state.arguments().updated_properties(),
        Some(
            &[
                mailbox::Property::TotalEmails,
                mailbox::Property::UnreadEmails,
                mailbox::Property::TotalThreads,
                mailbox::Property::UnreadThreads,
            ][..]
        )
    );
    let state = state.new_state().to_string();

    // Move email from Inbox to Trash
    client
        .email_set_mailboxes(&mail_id, [&id_map["trash"]])
        .await
        .unwrap();

    // E-mail properties of both Inbox and Trash must have changed
    let state = client.mailbox_changes(state, 0).await.unwrap();
    assert_eq!(state.created().len(), 0);
    assert_eq!(state.updated().len(), 2);
    assert_eq!(state.destroyed().len(), 0);
    let mut folder_ids = vec![&id_map["trash"], &id_map["inbox"]];
    let mut updated_ids = state
        .updated()
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>();
    updated_ids.sort_unstable();
    folder_ids.sort_unstable();
    assert_eq!(updated_ids, folder_ids);
    assert_eq!(
        state.arguments().updated_properties(),
        Some(
            &[
                mailbox::Property::TotalEmails,
                mailbox::Property::UnreadEmails,
                mailbox::Property::TotalThreads,
                mailbox::Property::UnreadThreads,
            ][..]
        )
    );

    // Deleting folders with children is not allowed
    let mut request = client.build();
    request.set_mailbox().destroy([&id_map["1"]]);
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .destroyed(&id_map["1"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::MailboxHasChild,
            ..
        }))
    ));

    // Deleting folders with contents is not allowed (unless remove_emails is true)
    let mut request = client.build();
    request.set_mailbox().destroy([&id_map["trash"]]);
    assert!(matches!(
        request
            .send_set_mailbox()
            .await
            .unwrap()
            .destroyed(&id_map["trash"]),
        Err(Error::Set(SetError {
            type_: SetErrorType::MailboxHasEmail,
            ..
        }))
    ));

    // Delete Trash folder and its contents
    let mut request = client.build();
    request
        .set_mailbox()
        .destroy([&id_map["trash"]])
        .arguments()
        .on_destroy_remove_emails(true);
    assert!(request
        .send_set_mailbox()
        .await
        .unwrap()
        .destroyed(&id_map["trash"])
        .is_ok());

    // Verify that Trash folder and its contents are gone
    assert!(client
        .mailbox_get(&id_map["trash"], None)
        .await
        .unwrap()
        .is_none());
    assert!(client
        .email_get(&mail_id, None::<Vec<_>>)
        .await
        .unwrap()
        .is_none());

    // Check search results after changing folder properties
    let mut request = client.build();
    request
        .set_mailbox()
        .update(&id_map["drafts"])
        .name("Borradores")
        .sort_order(100)
        .parent_id((&id_map["2"]).into())
        .role(Role::None);
    assert!(request
        .send_set_mailbox()
        .await
        .unwrap()
        .updated(&id_map["drafts"])
        .is_ok());
    assert_eq!(
        client
            .mailbox_query(
                Filter::and([
                    mailbox::query::Filter::name("Borradores").into(),
                    mailbox::query::Filter::parent_id((&id_map["2"]).into()).into(),
                    Filter::not([mailbox::query::Filter::has_any_role(true)])
                ])
                .into(),
                [mailbox::query::Comparator::name()].into()
            )
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        ["drafts"]
    );
    assert!(client
        .mailbox_query(
            mailbox::query::Filter::name("Drafts").into(),
            [mailbox::query::Comparator::name()].into()
        )
        .await
        .unwrap()
        .ids()
        .is_empty());
    assert!(client
        .mailbox_query(
            mailbox::query::Filter::role(Role::Drafts).into(),
            [mailbox::query::Comparator::name()].into()
        )
        .await
        .unwrap()
        .ids()
        .is_empty());
    assert_eq!(
        client
            .mailbox_query(
                mailbox::query::Filter::parent_id(None::<&str>).into(),
                [mailbox::query::Comparator::name()].into()
            )
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        ["inbox", "sent", "spam"]
    );
    assert_eq!(
        client
            .mailbox_query(
                mailbox::query::Filter::has_any_role(true).into(),
                [mailbox::query::Comparator::name()].into()
            )
            .await
            .unwrap()
            .ids()
            .iter()
            .map(|id| id_map.get(id).unwrap())
            .collect::<Vec<_>>(),
        ["inbox", "sent", "spam"]
    );

    let mut request = client.build();
    request.query_mailbox().arguments().sort_as_tree(true);
    let mut ids = request.send_query_mailbox().await.unwrap().unwrap_ids();
    ids.reverse();
    for id in ids {
        client.mailbox_destroy(&id, true).await.unwrap();
    }
    server.store.assert_is_empty();
}

async fn create_test_mailboxes(client: &mut Client) -> HashMap<String, String> {
    let mut mailbox_map = HashMap::new();
    let mut request = client.build();
    build_create_query(
        request.set_mailbox(),
        &mut mailbox_map,
        serde_json::from_slice(TEST_MAILBOXES).unwrap(),
        None,
    );
    let mut result = request.send_set_mailbox().await.unwrap();
    let mut id_map = HashMap::with_capacity(mailbox_map.len());
    for (create_id, local_id) in mailbox_map {
        let server_id = result.created(&create_id).unwrap().unwrap_id();
        id_map.insert(local_id.clone(), server_id.clone());
        id_map.insert(server_id, local_id);
    }
    id_map
}

fn build_create_query(
    request: &mut SetRequest<Mailbox<Set>, mailbox::SetArguments>,
    mailbox_map: &mut HashMap<String, String>,
    mailboxes: Vec<TestMailbox>,
    parent_id: Option<String>,
) {
    for mailbox in mailboxes {
        let create_mailbox = request
            .create()
            .name(mailbox.name)
            .sort_order(mailbox.order);
        if let Some(role) = mailbox.role {
            create_mailbox.role(role);
        }
        if let Some(parent_id) = &parent_id {
            create_mailbox.parent_id_ref(parent_id);
        }
        let create_mailbox_id = create_mailbox.create_id().unwrap();
        mailbox_map.insert(create_mailbox_id.clone(), mailbox.id);

        if let Some(children) = mailbox.children {
            build_create_query(request, mailbox_map, children, create_mailbox_id.into());
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TestMailbox {
    id: String,
    name: String,
    role: Option<Role>,
    order: u32,
    children: Option<Vec<TestMailbox>>,
}

const TEST_MAILBOXES: &[u8] = br#"
[
    {
        "id": "inbox",
        "name": "Inbox",
        "role": "INBOX",
        "order": 5,
        "children": [
            {
                "name": "Level 1",
                "id": "1",
                "order": 4,
                "children": [
                    {
                        "name": "Sub-Level 1.1",
                        "id": "1.1",

                        "order": 3,
                        "children": [
                            {
                                "name": "Z-Sub-Level 1.1.1",
                                "id": "1.1.1",
                                "order": 2,
                                "children": [
                                    {
                                        "name": "X-Sub-Level 1.1.1.1",
                                        "id": "1.1.1.1",
                                        "order": 1,
                                        "children": [
                                            {
                                                "name": "Y-Sub-Level 1.1.1.1.1",
                                                "id": "1.1.1.1.1",
                                                "order": 0
                                            }
                                        ]
                                    }
                                ]
                            }
                        ]
                    },
                    {
                        "name": "Sub-Level 1.2",
                        "id": "1.2",
                        "order": 7,
                        "children": [
                            {
                                "name": "Z-Sub-Level 1.2.1",
                                "id": "1.2.1",
                                "order": 6
                            }
                        ]
                    }
                ]
            },
            {
                "name": "Level 2",
                "id": "2",
                "order": 8
            },
            {
                "name": "Level 3",
                "id": "3",
                "order": 9
            }
        ]
    },
    {
        "id": "sent",
        "name": "Sent",
        "role": "SENT",
        "order": 15
    },
    {
        "id": "drafts",
        "name": "Drafts",
        "role": "DRAFTS",
        "order": 14
    },
    {
        "id": "trash",
        "name": "Trash",
        "role": "TRASH",
        "order": 13
    },
    {
        "id": "spam",
        "name": "Spam",
        "role": "JUNK",
        "order": 12,
        "children": [{
            "id": "spam1",
            "name": "Work Spam",
            "order": 11,
            "children": [{
                "id": "spam2",
                "name": "Friendly Spam",
                "order": 10
            }]
        }]
    }
]
"#;
