use actix_web::web;

use jmap::types::jmap::JMAPId;
use jmap_client::{client::Client, email, mailbox::Role};
use store::{
    ahash::{AHashMap, AHashSet},
    Store, ThreadId,
};

use crate::{tests::store::utils::StoreCompareWith, JMAPServer};

pub async fn test<T>(server: web::Data<JMAPServer<T>>, client: &mut Client)
where
    T: for<'x> Store<'x> + 'static,
{
    println!("Running Email Merge Threads tests...");

    let mut all_mailboxes = AHashMap::default();

    for (base_test_num, test) in [test_1(), test_2(), test_3()].iter().enumerate() {
        let base_test_num = ((base_test_num * 6) as u32) + 1;
        let mut messages = Vec::new();
        let mut total_messages = 0;
        let mut messages_per_thread =
            build_messages(test, &mut messages, &mut total_messages, None, 0);
        messages_per_thread.sort_unstable();

        let mut mailbox_ids = Vec::with_capacity(6);

        for test_num in 0..=5 {
            mailbox_ids.push(
                client
                    .set_default_account_id(
                        JMAPId::new((base_test_num + test_num) as u64).to_string(),
                    )
                    .mailbox_create("Thread nightmare", None::<String>, Role::None)
                    .await
                    .unwrap()
                    .take_id(),
            );
        }

        for message in &messages {
            client
                .set_default_account_id(JMAPId::new(base_test_num as u64).to_string())
                .email_import(
                    message.to_string().into_bytes(),
                    [mailbox_ids[0].clone()],
                    None::<Vec<String>>,
                    None,
                )
                .await
                .unwrap();
        }

        for message in messages.iter().rev() {
            client
                .set_default_account_id(JMAPId::new((base_test_num + 1) as u64).to_string())
                .email_import(
                    message.to_string().into_bytes(),
                    [mailbox_ids[1].clone()],
                    None::<Vec<String>>,
                    None,
                )
                .await
                .unwrap();
        }

        for chunk in messages.chunks(5) {
            client.set_default_account_id(JMAPId::new((base_test_num + 2) as u64).to_string());

            for message in chunk {
                client
                    .email_import(
                        message.to_string().into_bytes(),
                        [mailbox_ids[2].clone()],
                        None::<Vec<String>>,
                        None,
                    )
                    .await
                    .unwrap();
            }

            client.set_default_account_id(JMAPId::new((base_test_num + 3) as u64).to_string());

            for message in chunk.iter().rev() {
                client
                    .email_import(
                        message.to_string().into_bytes(),
                        [mailbox_ids[3].clone()],
                        None::<Vec<String>>,
                        None,
                    )
                    .await
                    .unwrap();
            }
        }

        for chunk in messages.chunks(5).rev() {
            client.set_default_account_id(JMAPId::new((base_test_num + 4) as u64).to_string());

            for message in chunk {
                client
                    .email_import(
                        message.to_string().into_bytes(),
                        [mailbox_ids[4].clone()],
                        None::<Vec<String>>,
                        None,
                    )
                    .await
                    .unwrap();
            }

            client.set_default_account_id(JMAPId::new((base_test_num + 5) as u64).to_string());

            for message in chunk.iter().rev() {
                client
                    .email_import(
                        message.to_string().into_bytes(),
                        [mailbox_ids[5].clone()],
                        None::<Vec<String>>,
                        None,
                    )
                    .await
                    .unwrap();
            }
        }

        for test_num in 0..=5 {
            let result = client
                .set_default_account_id(JMAPId::new((base_test_num + test_num) as u64).to_string())
                .email_query(
                    email::query::Filter::in_mailbox(mailbox_ids[test_num as usize].clone()).into(),
                    None::<Vec<_>>,
                )
                .await
                .unwrap();

            assert_eq!(
                result.ids().len(),
                total_messages,
                "test# {}/{}",
                base_test_num,
                test_num
            );

            let thread_ids: AHashSet<ThreadId> = result
                .ids()
                .iter()
                .map(|id| JMAPId::parse(id).unwrap().get_prefix_id())
                .collect();

            assert_eq!(
                thread_ids.len(),
                messages_per_thread.len(),
                "{:?}",
                thread_ids
            );

            let mut messages_per_thread_db = Vec::new();

            for thread_id in thread_ids {
                messages_per_thread_db.push(
                    client
                        .thread_get(&JMAPId::new(thread_id as u64).to_string())
                        .await
                        .unwrap()
                        .unwrap()
                        .email_ids()
                        .len(),
                );
            }
            messages_per_thread_db.sort_unstable();

            assert_eq!(messages_per_thread_db, messages_per_thread);
        }

        all_mailboxes.insert(base_test_num as usize, mailbox_ids);
    }

    // Delete all messages and make sure no keys are left in the store.
    for (base_test_num, mailbox_ids) in all_mailboxes {
        for (test_num, mailbox_id) in mailbox_ids.into_iter().enumerate() {
            client
                .set_default_account_id(JMAPId::new((base_test_num + test_num) as u64).to_string())
                .mailbox_destroy(&mailbox_id, true)
                .await
                .unwrap();
        }
    }

    server.store.assert_is_empty();
}

fn build_message(message: usize, in_reply_to: Option<usize>, thread_num: usize) -> String {
    if let Some(in_reply_to) = in_reply_to {
        format!(
            "Message-ID: <{}>\nReferences: <{}>\nSubject: re: T{}\n\nreply\n",
            message, in_reply_to, thread_num
        )
    } else {
        format!(
            "Message-ID: <{}>\nSubject: T{}\n\nmsg\n",
            message, thread_num
        )
    }
}

fn build_messages(
    three: &ThreadTest,
    messages: &mut Vec<String>,
    total_messages: &mut usize,
    in_reply_to: Option<usize>,
    thread_num: usize,
) -> Vec<usize> {
    let mut messages_per_thread = Vec::new();
    match three {
        ThreadTest::Message => {
            *total_messages += 1;
            messages.push(build_message(*total_messages, in_reply_to, thread_num));
        }
        ThreadTest::MessageWithReplies(replies) => {
            *total_messages += 1;
            messages.push(build_message(*total_messages, in_reply_to, thread_num));
            let in_reply_to = Some(*total_messages);
            for reply in replies {
                build_messages(reply, messages, total_messages, in_reply_to, thread_num);
            }
        }
        ThreadTest::Root(items) => {
            for (thread_num, item) in items.iter().enumerate() {
                let count_start = *total_messages;
                build_messages(item, messages, total_messages, None, thread_num);
                messages_per_thread.push(*total_messages - count_start);
            }
        }
    }
    messages_per_thread
}

pub fn build_thread_test_messages() -> Vec<String> {
    let mut messages = Vec::new();
    let mut total_messages = 0;
    build_messages(&test_3(), &mut messages, &mut total_messages, None, 0);
    messages
}

pub enum ThreadTest {
    Message,
    MessageWithReplies(Vec<ThreadTest>),
    Root(Vec<ThreadTest>),
}

fn test_1() -> ThreadTest {
    ThreadTest::Root(vec![
        ThreadTest::Message,
        ThreadTest::MessageWithReplies(vec![
            ThreadTest::Message,
            ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::Message,
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::Message,
                    ThreadTest::Message,
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::Message,
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::Message,
                            ThreadTest::Message,
                        ]),
                    ]),
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::Message,
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::Message,
                            ThreadTest::Message,
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::Message,
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                ]),
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                ]),
                            ]),
                        ]),
                    ]),
                ]),
            ]),
        ]),
    ])
}

fn test_2() -> ThreadTest {
    ThreadTest::Root(vec![
        ThreadTest::MessageWithReplies(vec![
            ThreadTest::Message,
            ThreadTest::Message,
            ThreadTest::Message,
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::Message,
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::Message,
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                    ]),
                                    ThreadTest::Message,
                                ]),
                                ThreadTest::Message,
                            ]),
                            ThreadTest::Message,
                        ]),
                        ThreadTest::Message,
                    ]),
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(
                                vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::Message,
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                        ]),
                                    ]),
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::Message,
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                    ]),
                                ],
                            )]),
                            ThreadTest::Message,
                        ]),
                        ThreadTest::Message,
                        ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                    ]),
                                ]),
                                ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::Message,
                                        ThreadTest::Message,
                                        ThreadTest::Message,
                                    ]),
                                    ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                    ]),
                                ]),
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                ]),
                            ]),
                            ThreadTest::Message,
                            ThreadTest::Message,
                        ])]),
                    ]),
                ]),
                ThreadTest::Message,
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::Message,
                        ]),
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                ]),
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                    ]),
                                    ThreadTest::Message,
                                ]),
                            ]),
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::Message,
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                        ThreadTest::Message,
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                    ]),
                                ]),
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                    ]),
                                ]),
                                ThreadTest::Message,
                                ThreadTest::Message,
                            ]),
                        ]),
                        ThreadTest::Message,
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                ]),
                                ThreadTest::Message,
                                ThreadTest::Message,
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                            ]),
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::Message,
                                        ThreadTest::Message,
                                    ]),
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                ]),
                            ]),
                            ThreadTest::Message,
                            ThreadTest::Message,
                        ]),
                    ]),
                    ThreadTest::Message,
                    ThreadTest::Message,
                ]),
            ]),
        ]),
        ThreadTest::Message,
        ThreadTest::MessageWithReplies(vec![ThreadTest::Message, ThreadTest::Message]),
    ])
}

fn test_3() -> ThreadTest {
    ThreadTest::Root(vec![
        ThreadTest::MessageWithReplies(vec![ThreadTest::Message, ThreadTest::Message]),
        ThreadTest::Message,
        ThreadTest::MessageWithReplies(vec![
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::Message,
                    ThreadTest::Message,
                    ThreadTest::Message,
                ]),
                ThreadTest::Message,
                ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                ThreadTest::Message,
            ]),
            ThreadTest::Message,
            ThreadTest::Message,
        ]),
        ThreadTest::Message,
        ThreadTest::MessageWithReplies(vec![
            ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::Message,
                ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(vec![
                    ThreadTest::Message,
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(vec![
                            ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(
                                vec![ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                ])],
                            )]),
                            ThreadTest::Message,
                            ThreadTest::Message,
                        ])]),
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::Message,
                                ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::Message,
                                    ]),
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                        ThreadTest::Message,
                                    ]),
                                ]),
                            ]),
                        ]),
                    ]),
                    ThreadTest::Message,
                    ThreadTest::Message,
                ])]),
                ThreadTest::Message,
            ]),
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::Message,
                ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(vec![
                    ThreadTest::Message,
                ])]),
                ThreadTest::Message,
            ]),
        ]),
        ThreadTest::MessageWithReplies(vec![
            ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(vec![
                ThreadTest::Message,
                ThreadTest::MessageWithReplies(vec![ThreadTest::Message, ThreadTest::Message]),
                ThreadTest::Message,
                ThreadTest::Message,
            ])]),
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::Message,
                        ThreadTest::Message,
                        ThreadTest::Message,
                        ThreadTest::Message,
                    ]),
                    ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(vec![
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(
                                vec![
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::Message,
                                        ThreadTest::Message,
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                        ]),
                                    ]),
                                    ThreadTest::Message,
                                ],
                            )]),
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(
                                vec![
                                    ThreadTest::Message,
                                    ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                                ],
                            )]),
                        ]),
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::Message,
                        ]),
                    ])]),
                    ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                    ThreadTest::Message,
                ]),
                ThreadTest::MessageWithReplies(vec![ThreadTest::Message]),
                ThreadTest::Message,
            ]),
        ]),
        ThreadTest::MessageWithReplies(vec![
            ThreadTest::Message,
            ThreadTest::MessageWithReplies(vec![
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::MessageWithReplies(vec![
                                ThreadTest::MessageWithReplies(vec![
                                    ThreadTest::MessageWithReplies(vec![
                                        ThreadTest::MessageWithReplies(vec![
                                            ThreadTest::Message,
                                            ThreadTest::Message,
                                            ThreadTest::MessageWithReplies(vec![
                                                ThreadTest::Message,
                                                ThreadTest::Message,
                                            ]),
                                        ]),
                                        ThreadTest::Message,
                                    ]),
                                    ThreadTest::Message,
                                    ThreadTest::Message,
                                ]),
                                ThreadTest::Message,
                                ThreadTest::Message,
                                ThreadTest::Message,
                            ]),
                            ThreadTest::Message,
                        ]),
                        ThreadTest::Message,
                    ]),
                    ThreadTest::Message,
                ]),
                ThreadTest::MessageWithReplies(vec![
                    ThreadTest::Message,
                    ThreadTest::MessageWithReplies(vec![
                        ThreadTest::Message,
                        ThreadTest::Message,
                        ThreadTest::MessageWithReplies(vec![
                            ThreadTest::Message,
                            ThreadTest::MessageWithReplies(vec![ThreadTest::MessageWithReplies(
                                vec![ThreadTest::Message, ThreadTest::Message],
                            )]),
                            ThreadTest::Message,
                        ]),
                        ThreadTest::Message,
                    ]),
                ]),
            ]),
        ]),
    ])
}
