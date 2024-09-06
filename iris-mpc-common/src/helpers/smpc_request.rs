use super::{key_pair::SharesDecodingError, sha256::calculate_sha256};
use crate::helpers::key_pair::SharesEncryptionKeyPairs;
use aws_sdk_sqs::{
    error::SdkError,
    operation::{delete_message::DeleteMessageError, receive_message::ReceiveMessageError},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use eyre::Report;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use thiserror::Error;
use tokio_retry::{
    strategy::{jitter, FixedInterval},
    Retry,
};

#[derive(Serialize, Deserialize, Debug)]
pub struct SQSMessage {
    #[serde(rename = "Type")]
    pub notification_type: String,
    #[serde(rename = "MessageId")]
    pub message_id:        String,
    #[serde(rename = "SequenceNumber")]
    pub sequence_number:   String,
    #[serde(rename = "TopicArn")]
    pub topic_arn:         String,
    #[serde(rename = "Message")]
    pub message:           String,
    #[serde(rename = "Timestamp")]
    pub timestamp:         String,
    #[serde(rename = "UnsubscribeURL")]
    pub unsubscribe_url:   String,
}

pub const SMPC_REQUEST_TYPE_ATTRIBUTE: &str = "message_type";
pub const IDENTITY_DELETION_REQUEST_TYPE: &str = "identity_deletion";
pub const UNIQUENESS_REQUEST_TYPE: &str = "uniqueness";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UniquenessRequest {
    pub batch_size:              Option<usize>,
    pub signup_id:               String,
    pub s3_presigned_url:        String,
    pub iris_shares_file_hashes: [String; 3],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IdentityDeletionRequest {
    pub serial_id: u32,
}

#[derive(Error, Debug)]
pub enum ReceiveRequestError {
    #[error("Failed to read from request SQS: {0}")]
    FailedToReadFromSQS(#[from] SdkError<ReceiveMessageError>),

    #[error("Failed to delete request from SQS: {0}")]
    FailedToDeleteFromSQS(#[from] SdkError<DeleteMessageError>),

    #[error("Failed to mark request as deleted in the database: {0}")]
    FailedToMarkRequestAsDeleted(#[from] Report),

    #[error("Failed to parse {json_name} JSON: {err}")]
    JsonParseError {
        json_name: String,
        err:       serde_json::Error,
    },

    #[error("Request does not contain a message type attribute")]
    NoMessageTypeAttribute,

    #[error("Request does not contain a string message type attribute")]
    NoStringMessageTypeAttribute,

    #[error("Message type attribute is not valid")]
    InvalidMessageType,

    #[error("Failed to join receive handle: {0}")]
    FailedToJoinHandle(#[from] tokio::task::JoinError),
}

impl ReceiveRequestError {
    pub fn json_parse_error(json_name: &str, err: serde_json::error::Error) -> Self {
        ReceiveRequestError::JsonParseError {
            json_name: json_name.to_string(),
            err,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SharesS3Object {
    pub iris_share_0: String,
    pub iris_share_1: String,
    pub iris_share_2: String,
}

#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub struct IrisCodesJSON {
    #[serde(rename = "IRIS_version")]
    pub iris_version:           String,
    pub left_iris_code_shares:  String, // these are base64 encoded strings
    pub right_iris_code_shares: String, // these are base64 encoded strings
    pub left_iris_mask_shares:  String, // these are base64 encoded strings
    pub right_iris_mask_shares: String, // these are base64 encoded strings
}

impl SharesS3Object {
    pub fn get(&self, party_id: usize) -> Option<&String> {
        match party_id {
            0 => Some(&self.iris_share_0),
            1 => Some(&self.iris_share_1),
            2 => Some(&self.iris_share_2),
            _ => None,
        }
    }
}

static S3_HTTP_CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

impl UniquenessRequest {
    pub async fn get_iris_data_by_party_id(
        &self,
        party_id: usize,
    ) -> Result<String, SharesDecodingError> {
        // Send a GET request to the presigned URL
        let retry_strategy = FixedInterval::from_millis(200).map(jitter).take(5);
        let response = Retry::spawn(retry_strategy, || async {
            S3_HTTP_CLIENT
                .get(self.s3_presigned_url.clone())
                .send()
                .await
        })
        .await?;

        // Ensure the request was successful
        if response.status().is_success() {
            // Parse the JSON response into the SharesS3Object struct
            let shares_file: SharesS3Object = match response.json().await {
                Ok(file) => file,
                Err(e) => {
                    tracing::error!("Failed to parse JSON: {}", e);
                    return Err(SharesDecodingError::RequestError(e));
                }
            };

            // Construct the field name dynamically
            let field_name = format!("iris_share_{}", party_id);
            // Access the field dynamically
            if let Some(value) = shares_file.get(party_id) {
                Ok(value.to_string())
            } else {
                tracing::error!("Failed to find field: {}", field_name);
                Err(SharesDecodingError::SecretStringNotFound)
            }
        } else {
            tracing::error!("Failed to download file: {}", response.status());
            Err(SharesDecodingError::ResponseContent {
                status:  response.status(),
                url:     self.s3_presigned_url.clone(),
                message: response.text().await.unwrap_or_default(),
            })
        }
    }

    pub fn decrypt_iris_share(
        &self,
        share: String,
        key_pairs: SharesEncryptionKeyPairs,
    ) -> Result<IrisCodesJSON, SharesDecodingError> {
        let share_bytes = STANDARD
            .decode(share.as_bytes())
            .map_err(|_| SharesDecodingError::Base64DecodeError)?;

        // try decrypting with key_pairs.current_key_pair, if it fails, try decrypting
        // with key_pairs.previous_key_pair (if it exists, otherwise, return an error)
        let decrypted = match key_pairs
            .current_key_pair
            .open_sealed_box(share_bytes.clone())
        {
            Ok(bytes) => Ok(bytes),
            Err(_) => {
                match if let Some(key_pair) = key_pairs.previous_key_pair.clone() {
                    key_pair.open_sealed_box(share_bytes)
                } else {
                    Err(SharesDecodingError::PreviousKeyNotFound)
                } {
                    Ok(bytes) => Ok(bytes),
                    Err(_) => Err(SharesDecodingError::SealedBoxOpenError),
                }
            }
        };

        let iris_share = match decrypted {
            Ok(bytes) => {
                let json_string = String::from_utf8(bytes)
                    .map_err(SharesDecodingError::DecodedShareParsingToUTF8Error)?;

                let iris_share: IrisCodesJSON =
                    serde_json::from_str(&json_string).map_err(SharesDecodingError::SerdeError)?;
                iris_share
            }
            Err(e) => return Err(e),
        };

        Ok(iris_share)
    }

    pub fn validate_iris_share(
        &self,
        party_id: usize,
        share: IrisCodesJSON,
    ) -> Result<bool, SharesDecodingError> {
        let stringified_share = serde_json::to_string(&share)
            .map_err(SharesDecodingError::SerdeError)?
            .into_bytes();

        Ok(self.iris_shares_file_hashes[party_id] == calculate_sha256(stringified_share))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResultEvent {
    pub node_id:            usize,
    pub serial_id:          Option<u32>,
    pub is_match:           bool,
    pub signup_id:          String,
    pub matched_serial_ids: Option<Vec<u32>>,
}

impl ResultEvent {
    pub fn new(
        node_id: usize,
        serial_id: Option<u32>,
        is_match: bool,
        signup_id: String,
        matched_serial_ids: Option<Vec<u32>>,
    ) -> Self {
        Self {
            node_id,
            serial_id,
            is_match,
            signup_id,
            matched_serial_ids,
        }
    }
}
