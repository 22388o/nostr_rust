// Implementation of the NIP4 protocol
// https://github.com/nostr-protocol/nips/blob/master/04.md

// Thanks to Yuki Kishimoto for the inspiration with his module
// https://gitlab.com/p2kishimoto/nostr-rs-sdk/-/tree/master/crates/nostr-sdk-base

use crate::events::{Event, EventPrepare};
use crate::nostr_client::Client;
use crate::req::ReqFilter;
use crate::utils::get_timestamp;
use crate::Identity;
use aes::{
    cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit},
    Aes256,
};
use base64::{decode, encode};
use cbc::{Decryptor, Encryptor};
use secp256k1::{ecdh, rand::random, PublicKey, SecretKey, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use std::convert::From;
use std::str::FromStr;
use thiserror::Error;

type Aes256CbcEnc = Encryptor<Aes256>;
type Aes256CbcDec = Decryptor<Aes256>;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PrivateMessage {
    pub author: String,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Error, Debug, Eq, PartialEq)]
pub enum Error {
    #[error(
        r#"Invalid content format. Expected format "<encrypted_text>?iv=<initialization_vec>""#
    )]
    InvalidContentFormat,

    #[error("Error while decoding from base64")]
    Base64DecodeError,

    #[error("Error while encoding to UTF-8")]
    Utf8EncodeError,

    #[error("Wrong encryption block mode.The content must be encrypted using CBC mode!")]
    WrongBlockMode,

    #[error("Secp256k1 Error: {}", _0)]
    Secp256k1Error(secp256k1::Error),
}

impl From<secp256k1::Error> for Error {
    fn from(err: secp256k1::Error) -> Self {
        Self::Secp256k1Error(err)
    }
}

pub fn decrypt(
    sk: &SecretKey,
    pk: &XOnlyPublicKey,
    encrypted_content: &str,
) -> Result<String, Error> {
    let parsed_content: Vec<&str> = encrypted_content.split("?iv=").collect();
    if parsed_content.len() != 2 {
        return Err(Error::InvalidContentFormat);
    }

    let mut encrypted_content: Vec<u8> =
        decode(parsed_content[0]).map_err(|_| Error::Base64DecodeError)?;

    let iv: Vec<u8> = decode(parsed_content[1]).map_err(|_| Error::Base64DecodeError)?;
    let key: Vec<u8> = generate_shared_key(sk, pk)?;

    let cipher = Aes256CbcDec::new(key.as_slice().into(), iv.as_slice().into());
    let result = cipher
        .decrypt_padded_mut::<Pkcs7>(&mut encrypted_content)
        .map_err(|_| Error::WrongBlockMode)?;

    String::from_utf8(result.try_into().unwrap()).map_err(|_| Error::Utf8EncodeError)
}

pub fn encrypt(sk: &SecretKey, pk: &XOnlyPublicKey, text: &str) -> Result<String, Error> {
    let key: Vec<u8> = generate_shared_key(sk, pk)?;
    let iv: [u8; 16] = random();

    let cipher = Aes256CbcEnc::new(key.as_slice().into(), &iv.into());
    let result: Vec<u8> = cipher.encrypt_padded_vec_mut::<Pkcs7>(text.as_bytes());

    Ok(format!("{}?iv={}", encode(result), encode(iv)))
}

fn generate_shared_key(sk: &SecretKey, pk: &XOnlyPublicKey) -> Result<Vec<u8>, Error> {
    let pk_normalized: PublicKey = from_schnorr_pk(pk)?;
    let ssp = ecdh::shared_secret_point(&pk_normalized, sk);

    let mut shared_key = [0u8; 32];
    shared_key.copy_from_slice(&ssp[..32]);
    Ok(shared_key.to_vec())
}

fn from_schnorr_pk(schnorr_pk: &XOnlyPublicKey) -> Result<PublicKey, Error> {
    let mut pk = String::from("02");
    pk.push_str(&schnorr_pk.to_string());

    Ok(PublicKey::from_str(&pk)?)
}

impl Client {
    /// Send private message to a public key
    ///
    /// # Example
    /// ```rust
    /// use nostr_rust::{nostr_client::Client, Identity};
    /// use std::str::FromStr;
    /// let mut client = Client::new(vec![env!("RELAY_URL")]).unwrap();
    /// let identity = Identity::from_str(env!("SECRET_KEY")).unwrap();
    /// let pubkey = "884704bd421721e292edbff42eb77547fe115c6ff9825b08fc366be4cd69e9f6";
    ///
    /// client.send_private_message(&identity, pubkey, "Hello from Rust Nostr Client!", 0).unwrap();
    /// ```
    pub fn send_private_message(
        &mut self,
        identity: &Identity,
        hex_pubkey: &str,
        message: &str,
        difficulty_target: u16,
    ) -> Result<Event, Error> {
        let x_pub_key = secp256k1::XOnlyPublicKey::from_str(hex_pubkey)?;
        let encrypted_message = encrypt(&identity.secret_key, &x_pub_key, message)?;

        let event = EventPrepare {
            pub_key: identity.public_key_str.clone(),
            created_at: get_timestamp(),
            kind: 4,
            tags: vec![vec!["p".to_string(), hex_pubkey.to_string()]],
            content: encrypted_message,
        }
        .to_event(identity, difficulty_target);

        self.publish_event(&event).unwrap();
        Ok(event)
    }

    /// Get private events (messages) with a public key
    ///
    /// # Example
    /// ```rust
    /// use nostr_rust::{nostr_client::Client, Identity};
    /// use std::str::FromStr;
    /// let mut client = Client::new(vec![env!("RELAY_URL")]).unwrap();
    /// let identity = Identity::from_str(env!("SECRET_KEY")).unwrap();
    /// let pubkey = "884704bd421721e292edbff42eb77547fe115c6ff9825b08fc366be4cd69e9f6";
    /// let messages = client.get_private_events_with(&identity, pubkey, 10).unwrap();
    /// ```
    pub fn get_private_events_with(
        &mut self,
        identity: &Identity,
        hex_pubkey: &str,
        limit: u64,
    ) -> Result<Vec<Event>, Error> {
        let events = self
            .get_events_of(vec![
                ReqFilter {
                    ids: None,
                    authors: Some(vec![identity.public_key_str.clone()]),
                    kinds: Some(vec![4]),
                    e: None,
                    p: Some(vec![hex_pubkey.to_string()]),
                    since: None,
                    until: None,
                    limit: Some(limit),
                },
                ReqFilter {
                    ids: None,
                    authors: Some(vec![hex_pubkey.to_string()]),
                    kinds: Some(vec![4]),
                    e: None,
                    p: Some(vec![identity.public_key_str.clone()]),
                    since: None,
                    until: None,
                    limit: Some(limit),
                },
            ])
            .unwrap();

        Ok(events)
    }

    /// Get private messages with a public key
    ///
    /// # Example
    /// ```rust
    /// use nostr_rust::{nostr_client::Client, Identity};
    /// use std::str::FromStr;
    /// let mut client = Client::new(vec![env!("RELAY_URL")]).unwrap();
    /// let identity = Identity::from_str(env!("SECRET_KEY")).unwrap();
    /// let pubkey = "884704bd421721e292edbff42eb77547fe115c6ff9825b08fc366be4cd69e9f6";
    /// let messages = client.get_private_messages_with(&identity, pubkey, 10).unwrap();
    /// ```
    pub fn get_private_messages_with(
        &mut self,
        identity: &Identity,
        hex_pubkey: &str,
        limit: u64,
    ) -> Result<Vec<PrivateMessage>, Error> {
        let x_pub_key = secp256k1::XOnlyPublicKey::from_str(hex_pubkey)?;
        let events =
            self.get_private_events_with(identity, x_pub_key.to_string().as_str(), limit)?;
        let mut messages: Vec<PrivateMessage> = vec![];

        for event in events {
            let decrypted_message = match decrypt(&identity.secret_key, &x_pub_key, &event.content)
            {
                Ok(message) => message,
                Err(_) => continue,
            };

            let private_message = PrivateMessage {
                author: event.pub_key,
                content: decrypted_message,
                timestamp: event.created_at,
            };

            messages.push(private_message);
        }

        // Sort messages by timestamp
        messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        // Reverse order
        messages.reverse();

        Ok(messages)
    }

    // TODO: get a list of private messages with a list of public keys
}
