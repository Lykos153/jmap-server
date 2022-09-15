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

use actix_web::web;

use jmap::types::jmap::JMAPId;
use jmap_client::{client::Client, mailbox::Role};
use store::Store;

use crate::{tests::store::utils::StoreCompareWith, JMAPServer};

pub async fn test<T>(server: web::Data<JMAPServer<T>>, client: &mut Client)
where
    T: for<'x> Store<'x> + 'static,
{
    println!("Running Email Thread tests...");

    let mailbox_id = client
        .set_default_account_id(JMAPId::new(1).to_string())
        .mailbox_create("JMAP Get", None::<String>, Role::None)
        .await
        .unwrap()
        .take_id();

    let mut expected_result = vec!["".to_string(); 5];
    let mut thread_id = "".to_string();

    for num in [5, 3, 1, 2, 4] {
        let mut email = client
            .email_import(
                format!("Subject: test\nReferences: <1234>\n\n{}", num).into_bytes(),
                [&mailbox_id],
                None::<Vec<String>>,
                Some(10000i64 + num as i64),
            )
            .await
            .unwrap();
        thread_id = email.thread_id().unwrap().to_string();
        expected_result[num - 1] = email.take_id();
    }

    assert_eq!(
        client
            .thread_get(&thread_id)
            .await
            .unwrap()
            .unwrap()
            .email_ids(),
        expected_result
    );

    client.mailbox_destroy(&mailbox_id, true).await.unwrap();

    server.store.assert_is_empty();
}
