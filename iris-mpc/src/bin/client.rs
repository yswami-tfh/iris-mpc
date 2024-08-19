#![allow(clippy::needless_range_loop)]
use aws_sdk_sns::{
    config::Region,
    types::{MessageAttributeValue, PublishBatchRequestEntry},
    Client,
};
use aws_sdk_sqs::Client as SqsClient;
use base64::{engine::general_purpose, Engine};
use clap::Parser;
use eyre::{Context, ContextCompat};
use iris_mpc_common::{
    galois_engine::degree4::GaloisRingIrisCodeShare,
    helpers::{
        aws::{construct_message_attributes, NODE_ID_MESSAGE_ATTRIBUTE_NAME},
        sqs::{ResultEvent, SMPCRequest},
    },
    iris_db::{db::IrisDB, iris::IrisCode},
};
use rand::{rngs::StdRng, thread_rng, Rng, SeedableRng};
use serde_json::to_string;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{spawn, sync::Mutex, time::sleep};
use uuid::Uuid;

const N_QUERIES: usize = 64 * 5;
const REGION: &str = "eu-north-1";
const RNG_SEED_SERVER: u64 = 42;
const DB_SIZE: usize = 8 * 1_000;
const ENROLLMENT_REQUEST_TYPE: &str = "enrollment";

#[derive(Debug, Parser)]
struct Opt {
    #[arg(short, long, env)]
    request_topic_arn: String,

    #[arg(short, long, env)]
    response_queue_url: String,

    #[arg(short, long, env)]
    db_index: Option<usize>,

    #[arg(short, long, env)]
    rng_seed: Option<u64>,

    #[arg(short, long, env)]
    n_repeat: Option<usize>,

    #[arg(short, long, env)]
    random: Option<bool>,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();

    let Opt {
        request_topic_arn,
        response_queue_url,
        db_index,
        rng_seed,
        n_repeat,
        random,
    } = Opt::parse();

    let mut rng = if let Some(rng_seed) = rng_seed {
        StdRng::seed_from_u64(rng_seed)
    } else {
        StdRng::from_entropy()
    };

    let n_repeat = n_repeat.unwrap_or(0);

    let region_provider = Region::new(REGION);
    let shared_config = aws_config::from_env().region(region_provider).load().await;
    let client = Client::new(&shared_config);

    let db = IrisDB::new_random_par(DB_SIZE, &mut StdRng::seed_from_u64(RNG_SEED_SERVER));

    let mut choice_rng = thread_rng();

    let expected_results: Arc<Mutex<HashMap<String, Option<u32>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let requests: Arc<Mutex<HashMap<String, IrisCode>>> = Arc::new(Mutex::new(HashMap::new()));
    let responses: Arc<Mutex<HashMap<u32, IrisCode>>> = Arc::new(Mutex::new(HashMap::new()));

    let thread_expected_results = expected_results.clone();
    let thread_requests = requests.clone();
    let thread_responses = responses.clone();

    let recv_thread = spawn(async move {
        let sqs_client = SqsClient::new(&shared_config);
        for _ in 0..N_QUERIES * 3 {
            // Receive responses
            let msg = sqs_client
                .receive_message()
                .max_number_of_messages(1)
                .queue_url(response_queue_url.clone())
                .send()
                .await
                .context("Failed to receive message")?;

            for msg in msg.messages.unwrap_or_default() {
                let result: ResultEvent = serde_json::from_str(&msg.body.context("No body found")?)
                    .context("Failed to parse message body")?;

                println!("Received result: {:?}", result);

                let tmp = thread_expected_results.lock().await;
                let expected_result = tmp.get(&result.signup_id);
                if expected_result.is_none() {
                    eprintln!(
                        "No expected result found for request_id: {}, the SQS message is likely \
                         stale, clear the queue",
                        result.signup_id
                    );
                    continue;
                }
                let expected_result = expected_result.unwrap();

                if expected_result.is_none() {
                    // New insertion
                    assert!(!result.is_match);
                    let request = thread_requests
                        .lock()
                        .await
                        .get(&result.signup_id)
                        .unwrap()
                        .clone();
                    thread_responses
                        .lock()
                        .await
                        .insert(result.serial_id.unwrap(), request);
                } else {
                    // Existing entry
                    println!(
                        "Expected: {:?} Got: {:?}",
                        expected_result, result.serial_id
                    );
                    assert!(result.is_match);
                    assert_eq!(result.serial_id.unwrap(), expected_result.unwrap());
                }

                sqs_client
                    .delete_message()
                    .queue_url(response_queue_url.clone())
                    .receipt_handle(msg.receipt_handle.unwrap())
                    .send()
                    .await
                    .context("Failed to delete message")?;
            }
        }
        eyre::Ok(())
    });

    // Prepare query
    for query_idx in 0..N_QUERIES {
        let request_id = Uuid::new_v4();
        println!("Building request: {}", request_id);

        let template = if random.is_some() {
            // Automatic random tests
            let options = if responses.lock().await.len() == 0 {
                2
            } else {
                3
            };
            match choice_rng.gen_range(0..options) {
                0 => {
                    println!("Sending new iris code");
                    expected_results
                        .lock()
                        .await
                        .insert(request_id.to_string(), None);
                    IrisCode::random_rng(&mut rng)
                }
                1 => {
                    println!("Sending iris code from db");
                    let db_index = rng.gen_range(0..db.db.len());
                    expected_results
                        .lock()
                        .await
                        .insert(request_id.to_string(), Some(db_index as u32));
                    db.db[db_index].clone()
                }
                2 => {
                    println!("Sending freshly inserted iris code");
                    let tmp = responses.lock().await;
                    let keys = tmp.keys().collect::<Vec<_>>();
                    let idx = rng.gen_range(0..keys.len());
                    let iris_code = tmp.get(keys[idx]).unwrap().clone();
                    expected_results
                        .lock()
                        .await
                        .insert(request_id.to_string(), Some(*keys[idx]));
                    iris_code
                }
                _ => unreachable!(),
            }
        } else {
            // Manually passed cli arguments
            if let Some(db_index) = db_index {
                if query_idx < n_repeat {
                    db.db[db_index].clone()
                } else {
                    IrisCode::random_rng(&mut rng)
                }
            } else {
                let mut rng = StdRng::seed_from_u64(1337); // TODO
                IrisCode::random_rng(&mut rng)
            }
        };

        requests
            .lock()
            .await
            .insert(request_id.to_string(), template.clone());

        let shared_code = GaloisRingIrisCodeShare::encode_iris_code(
            &template.code,
            &template.mask,
            &mut StdRng::seed_from_u64(RNG_SEED_SERVER),
        );
        let shared_mask = GaloisRingIrisCodeShare::encode_mask_code(
            &template.mask,
            &mut StdRng::seed_from_u64(RNG_SEED_SERVER),
        );

        let mut messages = vec![];
        for i in 0..3 {
            let sns_id = Uuid::new_v4();
            let iris_code =
                general_purpose::STANDARD.encode(bytemuck::cast_slice(&shared_code[i].coefs));
            let mask_code =
                general_purpose::STANDARD.encode(bytemuck::cast_slice(&shared_mask[i].coefs));

            let request_message = SMPCRequest {
                batch_size: None,
                request_id: request_id.to_string(),
                iris_code,
                mask_code,
            };

            let mut message_attributes = construct_message_attributes()?;
            message_attributes.insert(
                NODE_ID_MESSAGE_ATTRIBUTE_NAME.to_string(),
                MessageAttributeValue::builder()
                    .data_type("String")
                    .string_value(i.to_string())
                    .build()?,
            );

            messages.push(
                PublishBatchRequestEntry::builder()
                    .message(to_string(&request_message)?)
                    .id(sns_id.to_string())
                    .message_group_id(ENROLLMENT_REQUEST_TYPE)
                    .set_message_attributes(Some(message_attributes))
                    .build()
                    .unwrap(),
            );
        }

        // Send all messages in batch
        client
            .publish_batch()
            .topic_arn(request_topic_arn.clone())
            .set_publish_batch_request_entries(Some(messages))
            .send()
            .await?;

        if (query_idx + 1) % 32 == 0 {
            sleep(Duration::from_secs(1)).await;
        }
    }

    sleep(Duration::from_secs(10)).await;

    // Receive all messages
    recv_thread.await??;

    Ok(())
}
