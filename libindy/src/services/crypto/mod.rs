mod ed25519;

extern crate hex;

use self::ed25519::ED25519CryptoType;
use self::hex::FromHex;

use errors::common::CommonError;
use errors::crypto::CryptoError;
use domain::crypto::key::{Key, KeyInfo};
use domain::crypto::did::{Did, MyDidInfo, TheirDidInfo, TheirDid};
use domain::crypto::combo_box::ComboBox;
use domain::route::AuthRecipient;
use domain::route::AnonRecipient;
use utils::crypto::base58;
use utils::crypto::base64;
use utils::crypto::verkey_builder::build_full_verkey;
use utils::crypto::ed25519_sign;
use utils::crypto::ed25519_box;
use utils::crypto::xsalsa20;
use utils::crypto::xsalsa20::{create_key, gen_nonce};

use std::collections::HashMap;
use std::str;
use std::error::Error;
use domain::route::AnonRecipient;
use domain::route::AuthRecipient;

pub const DEFAULT_CRYPTO_TYPE: &'static str = "ed25519";

trait CryptoType {
    fn encrypt(&self, sk: &ed25519_sign::SecretKey, vk: &ed25519_sign::PublicKey, doc: &[u8], nonce: &ed25519_box::Nonce) -> Result<Vec<u8>, CryptoError>;
    fn decrypt(&self, sk: &ed25519_sign::SecretKey, vk: &ed25519_sign::PublicKey, doc: &[u8], nonce: &ed25519_box::Nonce) -> Result<Vec<u8>, CryptoError>;
    fn gen_nonce(&self) -> ed25519_box::Nonce;
    fn create_key(&self, seed: Option<&ed25519_sign::Seed>) -> Result<(ed25519_sign::PublicKey, ed25519_sign::SecretKey), CryptoError>;
    fn validate_key(&self, _vk: &ed25519_sign::PublicKey) -> Result<(), CryptoError>;
    fn sign(&self, sk: &ed25519_sign::SecretKey, doc: &[u8]) -> Result<ed25519_sign::Signature, CryptoError>;
    fn verify(&self, vk: &ed25519_sign::PublicKey, doc: &[u8], signature: &ed25519_sign::Signature) -> Result<bool, CryptoError>;
    fn encrypt_sealed(&self, vk: &ed25519_sign::PublicKey, doc: &[u8]) -> Result<Vec<u8>, CryptoError>;
    fn decrypt_sealed(&self, vk: &ed25519_sign::PublicKey, sk: &ed25519_sign::SecretKey, doc: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

pub struct CryptoService {
    crypto_types: HashMap<&'static str, Box<CryptoType>>
}

impl CryptoService {
    pub fn new() -> CryptoService {
        let mut crypto_types: HashMap<&str, Box<CryptoType>> = HashMap::new();
        crypto_types.insert(DEFAULT_CRYPTO_TYPE, Box::new(ED25519CryptoType::new()));

        CryptoService {
            crypto_types
        }
    }

    pub fn create_key(&self, key_info: &KeyInfo) -> Result<Key, CryptoError> {
        trace!("create_key >>> key_info: {:?}", secret!(key_info));

        let crypto_type_name = key_info.crypto_type
            .as_ref()
            .map(String::as_str)
            .unwrap_or(DEFAULT_CRYPTO_TYPE);

        if !self.crypto_types.contains_key(crypto_type_name) {
            return Err(
                CryptoError::UnknownCryptoError(
                    format!("KeyInfo contains unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let seed = self.convert_seed(key_info.seed.as_ref().map(String::as_ref))?;
        let (vk, sk) = crypto_type.create_key(seed.as_ref())?;
        let mut vk = base58::encode(&vk[..]);
        let sk = base58::encode(&sk[..]);
        if !crypto_type_name.eq(DEFAULT_CRYPTO_TYPE) {
            // Use suffix with crypto type name to store crypto type inside of vk
            vk = format!("{}:{}", vk, crypto_type_name);
        }

        let key = Key::new(vk, sk);

        trace!("create_key <<< key: {:?}", key);

        Ok(key)
    }

    pub fn create_my_did(&self, my_did_info: &MyDidInfo) -> Result<(Did, Key), CryptoError> {
        trace!("create_my_did >>> my_did_info: {:?}", secret!(my_did_info));

        let crypto_type_name = my_did_info.crypto_type
            .as_ref()
            .map(String::as_str)
            .unwrap_or(DEFAULT_CRYPTO_TYPE);

        if !self.crypto_types.contains_key(crypto_type_name) {
            return Err(
                CryptoError::UnknownCryptoError(
                    format!("MyDidInfo info contains unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let seed = self.convert_seed(my_did_info.seed.as_ref().map(String::as_ref))?;
        let (vk, sk) = crypto_type.create_key(seed.as_ref())?;
        let did = match my_did_info.did {
            Some(ref did) => {
                self.validate_did(did)?;
                base58::decode(did)?
            }
            _ if my_did_info.cid == Some(true) => vk[..].to_vec(),
            _ => vk[0..16].to_vec()
        };

        let did = base58::encode(&did);
        let mut vk = base58::encode(&vk[..]);
        let sk = base58::encode(&sk[..]);

        if !crypto_type_name.eq(DEFAULT_CRYPTO_TYPE) {
            // Use suffix with crypto type name to store crypto type inside of vk
            vk = format!("{}:{}", vk, crypto_type_name);
        }

        let did = (Did::new(did, vk.clone()), Key::new(vk, sk));

        trace!("create_my_did <<< did: {:?}", did);

        Ok(did)
    }

    pub fn create_their_did(&self, their_did_info: &TheirDidInfo) -> Result<TheirDid, CryptoError> {
        trace!("create_their_did >>> their_did_info: {:?}", their_did_info);

        // Check did is correct Base58
        base58::decode(&their_did_info.did)?;

        let verkey = build_full_verkey(their_did_info.did.as_str(),
                                       their_did_info.verkey.as_ref().map(String::as_str))?;

        self.validate_key(&verkey)?;

        let did = TheirDid { did: their_did_info.did.clone(), verkey };

        trace!("create_their_did <<< did: {:?}", did);

        Ok(did)
    }

    pub fn sign(&self, my_key: &Key, doc: &[u8]) -> Result<Vec<u8>, CryptoError> {
        trace!("sign >>> my_key: {:?}, doc: {:?}", my_key, doc);

        let crypto_type_name = if my_key.verkey.contains(':') {
            let splits: Vec<&str> = my_key.verkey.split(':').collect();
            splits[1]
        } else {
            DEFAULT_CRYPTO_TYPE
        };

        if !self.crypto_types.contains_key(crypto_type_name) {
            return Err(
                CryptoError::UnknownCryptoError(
                    format!("Trying to sign message with unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let my_sk = ed25519_sign::SecretKey::from_slice(&base58::decode(my_key.signkey.as_str())?)?;
        let signature = crypto_type.sign(&my_sk, doc)?[..].to_vec();

        trace!("sign <<< signature: {:?}", signature);

        Ok(signature)
    }

    pub fn verify(&self, their_vk: &str, msg: &[u8], signature: &[u8]) -> Result<bool, CryptoError> {
        trace!("verify >>> their_vk: {:?}, msg: {:?}, signature: {:?}", their_vk, msg, signature);

        let (their_vk, crypto_type_name) = if their_vk.contains(':') {
            let splits: Vec<&str> = their_vk.split(':').collect();
            (splits[0], splits[1])
        } else {
            (their_vk, DEFAULT_CRYPTO_TYPE)
        };

        if !self.crypto_types.contains_key(crypto_type_name) {
            return Err(CryptoError::UnknownCryptoError(
                format!("Trying to verify message with unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let their_vk = ed25519_sign::PublicKey::from_slice(&base58::decode(&their_vk)?)?;
        let signature = ed25519_sign::Signature::from_slice(&signature)?;

        let valid = crypto_type.verify(&their_vk, msg, &signature)?;

        trace!("verify <<< valid: {:?}", valid);

        Ok(valid)
    }

    pub fn crypto_box(&self, my_key: &Key, their_vk: &str, doc: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        trace!("crypto_box >>> my_key: {:?}, their_vk: {:?}, doc: {:?}", my_key, their_vk, doc);

        let (_my_vk, crypto_type_name) = if my_key.verkey.contains(':') {
            let splits: Vec<&str> = my_key.verkey.split(':').collect();
            (splits[0], splits[1])
        } else {
            (my_key.verkey.as_str(), DEFAULT_CRYPTO_TYPE)
        };

        let (their_vk, their_crypto_type_name) = if their_vk.contains(':') {
            let splits: Vec<&str> = their_vk.split(':').collect();
            (splits[0], splits[1])
        } else {
            (their_vk, DEFAULT_CRYPTO_TYPE)
        };

        if !self.crypto_types.contains_key(&crypto_type_name) {
            return Err(CryptoError::UnknownCryptoError(format!("Trying to crypto_box message with unknown crypto: {}", crypto_type_name)));
        }

        if !crypto_type_name.eq(their_crypto_type_name) {
            // TODO: FIXME: Use dedicated error code
            return Err(CryptoError::UnknownCryptoError(
                format!("My key crypto type is incompatible with their key crypto type: {} {}",
                        crypto_type_name,
                        their_crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(&crypto_type_name).unwrap();

        let my_sk = ed25519_sign::SecretKey::from_slice(&base58::decode(my_key.signkey.as_str())?)?;
        let their_vk = ed25519_sign::PublicKey::from_slice(&base58::decode(their_vk)?)?;
        let nonce = crypto_type.gen_nonce();

        let encrypted_doc = crypto_type.encrypt(&my_sk, &their_vk, doc, &nonce)?;
        let nonce = nonce[..].to_vec();

        trace!("crypto_box <<< encrypted_doc: {:?}, nonce: {:?}", encrypted_doc, nonce);

        Ok((encrypted_doc, nonce))
    }

    pub fn crypto_box_open(&self, my_key: &Key, their_vk: &str, doc: &[u8], nonce: &[u8]) -> Result<Vec<u8>, CryptoError> {
        trace!("crypto_box_open >>> my_key: {:?}, their_vk: {:?}, doc: {:?}, nonce: {:?}", my_key, their_vk, doc, nonce);

        let (_my_vk, crypto_type_name) = if my_key.verkey.contains(':') {
            let splits: Vec<&str> = my_key.verkey.split(':').collect();
            (splits[0], splits[1])
        } else {
            (my_key.verkey.as_str(), DEFAULT_CRYPTO_TYPE)
        };

        let (their_vk, their_crypto_type_name) = if their_vk.contains(':') {
            let splits: Vec<&str> = their_vk.split(':').collect();
            (splits[0], splits[1])
        } else {
            (their_vk, DEFAULT_CRYPTO_TYPE)
        };

        if !self.crypto_types.contains_key(&crypto_type_name) {
            return Err(CryptoError::UnknownCryptoError(
                format!("Trying to crypto_box_open message with unknown crypto: {}", crypto_type_name)));
        }

        if !crypto_type_name.eq(their_crypto_type_name) {
            // TODO: FIXME: Use dedicated error code
            return Err(CryptoError::UnknownCryptoError(
                format!("My key crypto type is incompatible with their key crypto type: {} {}",
                        crypto_type_name,
                        their_crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let my_sk = ed25519_sign::SecretKey::from_slice(&base58::decode(&my_key.signkey)?)?;
        let their_vk = ed25519_sign::PublicKey::from_slice(&base58::decode(their_vk)?)?;
        let nonce = ed25519_box::Nonce::from_slice(&nonce)?;

        let decrypted_doc = crypto_type.decrypt(&my_sk, &their_vk, &doc, &nonce)?;

        trace!("crypto_box_open <<< decrypted_doc: {:?}", decrypted_doc);

        Ok(decrypted_doc)
    }

    pub fn authenticated_encrypt(&self, my_key: &Key, their_vk: &str, doc: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let (msg, nonce) = self.crypto_box(my_key, their_vk, doc)?;

        let combo_box = ComboBox {
            msg: base64::encode(msg.as_slice()),
            sender: my_key.verkey.to_string(),
            nonce: base64::encode(nonce.as_slice())
        };

        let msg_pack = combo_box.to_msg_pack()
            .map_err(|e| CommonError::InvalidState(format!("Can't serialize ComboBox: {:?}", e)))?;

        let res = self.crypto_box_seal(&their_vk, &msg_pack)?;

        Ok(res)
    }

    pub fn authenticated_decrypt(&self, my_key: &Key, doc: &[u8]) -> Result<(String, Vec<u8>), CryptoError> {
        let decrypted_msg = self.crypto_box_seal_open(&my_key, &doc)?;

        let parsed_msg = ComboBox::from_msg_pack(decrypted_msg.as_slice())
            .map_err(|err| CommonError::InvalidStructure(format!("Can't deserialize ComboBox: {:?}", err)))?;

        let doc: Vec<u8> = base64::decode(&parsed_msg.msg)
            .map_err(|err| CommonError::InvalidStructure(format!("Can't decode internal msg filed from base64 {}", err)))?;

        let nonce: Vec<u8> = base64::decode(&parsed_msg.nonce)
            .map_err(|err| CommonError::InvalidStructure(format!("Can't decode nonce from base64 {}", err)))?;

        let decrypted_msg = self.crypto_box_open(&my_key, &parsed_msg.sender, &doc, &nonce)?;

        let res = (parsed_msg.sender, decrypted_msg);

        Ok(res)
    }

    pub fn crypto_box_seal(&self, their_vk: &str, doc: &[u8]) -> Result<Vec<u8>, CryptoError> {
        trace!("crypto_box_seal >>> their_vk: {:?}, doc: {:?}", their_vk, doc);

        let (their_vk, crypto_type_name) = if their_vk.contains(':') {
            let splits: Vec<&str> = their_vk.split(':').collect();
            (splits[0], splits[1])
        } else {
            (their_vk, DEFAULT_CRYPTO_TYPE)
        };

        if !self.crypto_types.contains_key(&crypto_type_name) {
            return Err(CryptoError::UnknownCryptoError(format!("Trying to encrypt sealed message with unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let their_vk = ed25519_sign::PublicKey::from_slice(&base58::decode(their_vk)?)?;

        let encrypted_doc = crypto_type.encrypt_sealed(&their_vk, doc)?;

        trace!("crypto_box_seal <<< encrypted_doc: {:?}", encrypted_doc);

        Ok(encrypted_doc)
    }

    pub fn crypto_box_seal_open(&self, my_key: &Key, doc: &[u8]) -> Result<Vec<u8>, CryptoError> {
        trace!("crypto_box_seal_open >>> my_key: {:?}, doc: {:?}", my_key, doc);

        let (my_vk, crypto_type_name) = if my_key.verkey.contains(':') {
            let splits: Vec<&str> = my_key.verkey.split(':').collect();
            (splits[0], splits[1])
        } else {
            (my_key.verkey.as_str(), DEFAULT_CRYPTO_TYPE)
        };

        if !self.crypto_types.contains_key(&crypto_type_name) {
            return Err(CryptoError::UnknownCryptoError(
                format!("Trying to crypto_box_open sealed message with unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        let my_vk = ed25519_sign::PublicKey::from_slice(&base58::decode(my_vk)?)?;
        let my_sk = ed25519_sign::SecretKey::from_slice(&base58::decode(my_key.signkey.as_str())?)?;

        let decrypted_doc = crypto_type.decrypt_sealed(&my_vk, &my_sk, doc)?;

        trace!("crypto_box_seal_open <<< decrypted_doc: {:?}", decrypted_doc);

        Ok(decrypted_doc)
    }

    pub fn convert_seed(&self, seed: Option<&str>) -> Result<Option<ed25519_sign::Seed>, CryptoError> {
        trace!("convert_seed >>> seed: {:?}", secret!(seed));

        if seed.is_none() {
            trace!("convert_seed <<< res: None");
            return Ok(None);
        }

        let seed = seed.unwrap();

        let bytes = if seed.as_bytes().len() == ed25519_sign::SEEDBYTES {
            // is acceptable seed length
            seed.as_bytes().to_vec()
        } else if seed.ends_with('=') {
            // is base64 string
            base64::decode(&seed)
                .map_err(|err| CommonError::InvalidStructure(format!("Can't deserialize Seed from Base64 string: {:?}", err)))?
        } else if seed.as_bytes().len() == ed25519_sign::SEEDBYTES * 2 {
            // is hex string
            Vec::from_hex(seed)
                .map_err(|err| CommonError::InvalidStructure(err.description().to_string()))?
        } else {
            return Err(CryptoError::CommonError(CommonError::InvalidStructure("Invalid bytes for Seed".to_string())))
        };

        let res = ed25519_sign::Seed::from_slice(bytes.as_slice())?;

        trace!("convert_seed <<< res: {:?}", secret!(&res));

        Ok(Some(res))
    }

    pub fn validate_key(&self, vk: &str) -> Result<(), CryptoError> {
        trace!("validate_key >>> vk: {:?}", vk);

        let (vk, crypto_type_name) = if vk.contains(':') {
            let splits: Vec<&str> = vk.split(':').collect();
            (splits[0], splits[1])
        } else {
            (vk, DEFAULT_CRYPTO_TYPE)
        };

        if !self.crypto_types.contains_key(&crypto_type_name) {
            return Err(CryptoError::UnknownCryptoError(format!("Trying to use key with unknown crypto: {}", crypto_type_name)));
        }

        let crypto_type = self.crypto_types.get(crypto_type_name).unwrap();

        if vk.starts_with('~') {
            base58::decode(&vk[1..])?; // TODO: proper validate abbreviated verkey
        } else {
            let vk = ed25519_sign::PublicKey::from_slice(&base58::decode(vk)?)?;
            crypto_type.validate_key(&vk)?;
        };

        trace!("validate_key <<<");

        Ok(())
    }

    pub fn validate_did(&self, did: &str) -> Result<(), CryptoError> {
        trace!("validate_did >>> did: {:?}", did);

        let did = base58::decode(did)?;

        if did.len() != 16 && did.len() != 32 {
            return Err(CryptoError::CommonError(
                CommonError::InvalidStructure(
                    format!("Trying to use did with unexpected len: {}", did.len()))));
        }

        let res = ();

        trace!("validate_did <<< res: {:?}", res);

        Ok(res)
    }

    /* Authcrypt helper function section */
    fn anon_encrypt_recipient(
        &self,
        recp_vk: &str,
        sym_key: xsalsa20::Key
    ) -> Result<AnonRecipient, CryptoError> {
        //encrypt cek
        let cek = self.encrypt_sealed(recp_vk, &sym_key[..]).map_err(|err| {
            CryptoError::UnknownCryptoError(format!("Failed to encrypt anon recipient {}", err))
        })?;

        //generate struct
        let anon_recipient = AnonRecipient {
            to: recp_vk.to_string(),
            cek: base64::encode(cek.as_slice()),
        };

        Ok(anon_recipient)
    }

    fn anon_decrypt_recipient(
        &self,
        my_key: &Key,
        anon_recipient: AnonRecipient
    ) -> Result<xsalsa20::Key, CryptoError> {
        let cek_as_vec = base64::decode(&anon_recipient.cek).map_err(|err| {
            CryptoError::CommonError(
                CommonError::InvalidStructure(format!("Failed to decode cek for anon_decrypt_recipient {}", err))
            )
        })?;
        let cek_as_bytes = cek_as_vec.as_ref();

        let decrypted_cek = cs
            .decrypt_sealed(my_key, cek_as_bytes)
            .map_err(|err| CryptoError::UnknownCryptoError(format!("Failed to decrypt cek {}", err)))?;

        //convert to secretbox key
        let sym_key = xsalsa20::Key::from_slice(&decrypted_cek[..]).map_err(|err| {
            CryptoError::UnknownCryptoError(format!("Failed to decrypt sym_key {}", err))
        })?;

        //return key
        Ok(sym_key)
    }

        /* ciphertext helper functions*/
    fn decrypt_unpack_ciphertext(
        &self,
        ciphertext: &str,
        iv: &str,
        sym_key: &xsalsa20::Key,
    ) -> Result<String, CryptoError> {

        //convert IV from &str to &Nonce
        let iv_as_vec = base64::decode(iv).map_err(|err|
            CryptoError::CommonError(
                CommonError::InvalidStructure(format!("Failed to decode IV {}", err)))
            )?;
        let iv_as_slice = iv_as_vec.as_slice();
        let nonce = xsalsa20::Nonce::from_slice(iv_as_slice).map_err(|err| {
            CryptoError::CommonError(
                CommonError::InvalidStructure(format!("Failed to convert IV to Nonce type {}", err))
            )
        })?;

        //convert ciphertext to bytes
        let ciphertext_as_vec = base64::decode(ciphertext).map_err(|err| {
            CryptoError::CommonError(
                CommonError::InvalidStructure(format!("Failed to decode ciphertext {}", err))
            )
        })?;
        let ciphertext_as_bytes = ciphertext_as_vec.as_ref();

        //decrypt message
        let plaintext_bytes =
            xsalsa20::decrypt(sym_key, &nonce, ciphertext_as_bytes).map_err(|err| {
                CryptoError::UnknownCryptoError(format!("Failed to decrypt ciphertext {}", err))
            })?;

        //convert message to readable (UTF-8) string
        String::from_utf8(plaintext_bytes).map_err(|err| {
            CryptoError::CommonError(
                CommonError::InvalidStructure(format!("Failed to convert message to UTF-8 {}", err))
            )
        })
    }

    fn encrypt_ciphertext(&self, ciphertext: &str) -> (xsalsa20::Key, xsalsa20::Nonce, Vec<u8>) {
        let sym_key = create_key();
        let iv = gen_nonce();
        let message = xsalsa20::encrypt(&sym_key, &iv, ciphertext.as_bytes());

        (sym_key, iv, message)
    }

    /* Authcrypt helper function section */
    pub fn auth_encrypt_recipient(
        &self,
        my_key: &Key,
        recp_vk: &str,
        sym_key: &xsalsa20::Key
    ) -> Result<AuthRecipient, CryptoError> {
        //encrypt sym_key for recipient
        let (e_cek, cek_nonce) = self.crypto_box(my_key, recp_vk, &sym_key[..])?;

        //serialize enc_header
        let sender_vk_bytes = base58::decode(&my_key.verkey).map_err(|err| {
            CryptoError::CommonError(err(format!("Failed to serialize cek")))
        })?;

        //encrypt enc_from
        let enc_from = cs
            .encrypt_sealed(recp_vk, sender_vk_bytes.as_slice())
            .map_err(|err| {
                CryptoError::CommonError(err(format!("Failed to encrypt sender verkey")))
            })?;;

        //create struct
        let auth_recipient = AuthRecipient {
            enc_from: base64::encode(enc_from.as_slice()),
            e_cek: base64::encode(e_cek.as_slice()),
            cek_nonce: base64::encode(cek_nonce.as_slice()),
            to: recp_vk.to_string(),
        };

        //return AuthRecipient struct
        Ok(auth_recipient)
    }

//    pub fn auth_decrypt_recipient(
//        &self,
//        my_key: &Key,
//        auth_recipient: AuthRecipient
//    ) -> Result<(xsalsa20::Key, String), CryptoError> {
//        //decode enc_from
//        let enc_from_bytes = base64::decode(&auth_recipient.enc_from)
//            .map_err(|err| RouteError::DecodeError(format!("Failed to decode enc_from {}", err)))?;
//
//        //decrypt enc_from
//        let sender_vk_as_vec =
//            cs.decrypt_sealed(my_key, enc_from_bytes.as_ref())
//                .map_err(|err| {
//                    RouteError::EncryptionError(format!("Failed to decrypt sender verkey {}", err))
//                })?;
//
//        //encode sender_vk to base58 to use to decrypt cek
//        let sender_vk = base58::encode(sender_vk_as_vec.as_ref());
//
//        //decode e_cek
//        let e_cek_as_vec = base64::decode(&auth_recipient.e_cek)
//            .map_err(|err| RouteError::DecodeError(format!("Failed to decode e_cek {}", err)))?;
//        let e_cek: &[u8] = e_cek_as_vec.as_ref();
//
//        //type convert cek_nonce
//        let cek_nonce_as_vec = base64::decode(&auth_recipient.cek_nonce).map_err(|err| {
//            RouteError::DecodeError(format!("Failed to decode cek_nonce {}", err))
//        })?;
//        let cek_nonce: &[u8] = cek_nonce_as_vec.as_ref();
//
//        //decrypt cek
//        let decrypted_cek = cs
//            .decrypt(my_key, &sender_vk, e_cek, cek_nonce)
//            .map_err(|err| {
//                RouteError::EncryptionError(format!("Failed to auth decrypt cek {}", err))
//            })?;
//
//        //convert to secretbox key
//        let sym_key = xsalsa20::Key::from_slice(&decrypted_cek[..]).map_err(|err| {
//            RouteError::EncryptionError(format!("Failed to unpack sym_key {}", err))
//        })?;
//
//        //TODO Verify key is in DID Doc
//
//        //return key to decrypt ciphertext and the key used to decrypt with
//        Ok((sym_key, sender_vk))
//    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::crypto::did::MyDidInfo;

    #[test]
    fn create_my_did_with_works_for_empty_info() {
        let service = CryptoService::new();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let my_did = service.create_my_did(&did_info);
        assert!(my_did.is_ok());
    }

    #[test]
    fn create_my_did_works_for_passed_did() {
        let service = CryptoService::new();

        let did = "NcYxiDXkpYi6ov5FcYDi1e";
        let did_info = MyDidInfo { did: Some(did.to_string()), cid: None, seed: None, crypto_type: None };

        let (my_did, _) = service.create_my_did(&did_info).unwrap();
        assert_eq!(did, my_did.did);
    }

    #[test]
    fn create_my_did_not_works_for_invalid_crypto_type() {
        let service = CryptoService::new();

        let did = Some("NcYxiDXkpYi6ov5FcYDi1e".to_string());
        let crypto_type = Some("type".to_string());

        let did_info = MyDidInfo { did: did.clone(), cid: None, seed: None, crypto_type: crypto_type };

        assert!(service.create_my_did(&did_info).is_err());
    }

    #[test]
    fn create_my_did_works_for_seed() {
        let service = CryptoService::new();

        let did = Some("NcYxiDXkpYi6ov5FcYDi1e".to_string());
        let seed = Some("00000000000000000000000000000My1".to_string());

        let did_info_with_seed = MyDidInfo { did: did.clone(), cid: None, seed, crypto_type: None };
        let did_info_without_seed = MyDidInfo { did: did.clone(), cid: None, seed: None, crypto_type: None };

        let (did_with_seed, _) = service.create_my_did(&did_info_with_seed).unwrap();
        let (did_without_seed, _) = service.create_my_did(&did_info_without_seed).unwrap();

        assert_ne!(did_with_seed.verkey, did_without_seed.verkey)
    }

    #[test]
    fn create_their_did_works_without_verkey() {
        let service = CryptoService::new();
        let did = "CnEDk9HrMnmiHXEV1WFgbVCRteYnPqsJwrTdcZaNhFVW";

        let their_did_info = TheirDidInfo::new(did.to_string(), None);
        let their_did = service.create_their_did(&their_did_info).unwrap();

        assert_eq!(did.to_string(), their_did.did);
        assert_eq!(did.to_string(), their_did.verkey);
    }

    #[test]
    fn create_their_did_works_for_full_verkey() {
        let service = CryptoService::new();
        let did = "8wZcEriaNLNKtteJvx7f8i";
        let verkey = "5L2HBnzbu6Auh2pkDRbFt5f4prvgE2LzknkuYLsKkacp";

        let their_did_info = TheirDidInfo::new(did.to_string(), Some(verkey.to_string()));
        let their_did = service.create_their_did(&their_did_info).unwrap();

        assert_eq!(did.to_string(), their_did.did);
        assert_eq!(verkey, their_did.verkey);
    }

    #[test]
    fn create_their_did_works_for_abbreviated_verkey() {
        let service = CryptoService::new();
        let did = "8wZcEriaNLNKtteJvx7f8i";
        let their_did_info = TheirDidInfo::new(did.to_string(), Some("~NcYxiDXkpYi6ov5FcYDi1e".to_string()));
        let their_did = service.create_their_did(&their_did_info).unwrap();

        assert_eq!(did.to_string(), their_did.did);
        assert_eq!("5L2HBnzbu6Auh2pkDRbFt5f4prvgE2LzknkuYLsKkacp", their_did.verkey);
    }

    #[test]
    fn sign_works() {
        let service = CryptoService::new();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };

        let message = r#"message"#;
        let (_, my_key) = service.create_my_did(&did_info).unwrap();
        let sig = service.sign(&my_key, message.as_bytes());
        assert!(sig.is_ok());
    }

    #[test]
    fn sign_works_for_invalid_signkey() {
        let service = CryptoService::new();
        let message = r#"message"#;
        let my_key = Key::new("8wZcEriaNLNKtteJvx7f8i".to_string(), "5L2HBnzbu6Auh2pkDRbFt5f4prvgE2LzknkuYLsKkacp".to_string());
        assert!(service.sign(&my_key, message.as_bytes()).is_err());
    }

    #[test]
    fn sign_verify_works() {
        let service = CryptoService::new();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let message = r#"message"#;
        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();
        let signature = service.sign(&my_key, message.as_bytes()).unwrap();
        let valid = service.verify(&my_did.verkey, message.as_bytes(), &signature).unwrap();
        assert!(valid);
    }

    #[test]
    fn sign_verify_works_for_verkey_contained_crypto_type() {
        let service = CryptoService::new();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let message = r#"message"#;
        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();
        let signature = service.sign(&my_key, message.as_bytes()).unwrap();
        let verkey = my_did.verkey + ":ed25519";
        let valid = service.verify(&verkey, message.as_bytes(), &signature).unwrap();
        assert!(valid);
    }


    #[test]
    fn sign_verify_works_for_verkey_contained_invalid_crypto_type() {
        let service = CryptoService::new();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let message = r#"message"#;
        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();
        let signature = service.sign(&my_key, message.as_bytes()).unwrap();
        let verkey = format!("crypto_type:{}", my_did.verkey);
        assert!(service.verify(&verkey, message.as_bytes(), &signature).is_err());
    }

    #[test]
    fn verify_not_works_for_invalid_verkey() {
        let service = CryptoService::new();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let message = r#"message"#;
        let (_, my_key) = service.create_my_did(&did_info).unwrap();
        let signature = service.sign(&my_key, message.as_bytes()).unwrap();
        let verkey = "AnnxV4t3LUHKZaxVQDWoVaG44NrGmeDYMA4Gz6C2tCZd";
        let valid = service.verify(verkey, message.as_bytes(), &signature).unwrap();
        assert_eq!(false, valid);
    }

    #[test]
    fn crypto_box_works() {
        let service = CryptoService::new();
        let msg = "some message";
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let (_, my_key) = service.create_my_did(&did_info).unwrap();
        let (their_did, _) = service.create_my_did(&did_info.clone()).unwrap();
        let their_did = Did::new(their_did.did, their_did.verkey);
        let encrypted_message = service.crypto_box(&my_key, &their_did.verkey, msg.as_bytes());
        assert!(encrypted_message.is_ok());
    }

    #[test]
    fn crypto_box_and_crypto_box_open_works() {
        let service = CryptoService::new();

        let msg = "some message";

        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };

        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();

        let my_key_for_encrypt = my_key.clone();

        let their_did_for_decrypt = Did::new(my_did.did, my_did.verkey);

        let (their_did, their_key) = service.create_my_did(&did_info.clone()).unwrap();

        let my_key_for_decrypt = their_key.clone();

        let their_did_for_encrypt = Did::new(their_did.did, their_did.verkey);

        let (encrypted_message, noce) = service.crypto_box(&my_key_for_encrypt, &their_did_for_encrypt.verkey, msg.as_bytes()).unwrap();

        let decrypted_message = service.crypto_box_open(&my_key_for_decrypt, &their_did_for_decrypt.verkey, &encrypted_message, &noce).unwrap();

        assert_eq!(msg.as_bytes().to_vec(), decrypted_message);
    }


    #[test]
    fn crypto_box_and_crypto_box_open_works_for_verkey_contained_crypto_type() {
        let service = CryptoService::new();

        let msg = "some message";

        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };

        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();

        let my_key_for_encrypt = my_key.clone();

        let their_did_for_decrypt = Did::new(my_did.did, my_did.verkey);

        let (their_did, their_key) = service.create_my_did(&did_info.clone()).unwrap();
        let my_key_for_decrypt = their_key.clone();

        let their_did_for_encrypt = Did::new(their_did.did, their_did.verkey);

        let (encrypted_message, noce) = service.crypto_box(&my_key_for_encrypt, &their_did_for_encrypt.verkey, msg.as_bytes()).unwrap();

        let verkey = their_did_for_decrypt.verkey + ":ed25519";

        let decrypted_message = service.crypto_box_open(&my_key_for_decrypt, &verkey, &encrypted_message, &noce).unwrap();

        assert_eq!(msg.as_bytes().to_vec(), decrypted_message);
    }

    #[test]
    fn crypto_box_seal_works() {
        let service = CryptoService::new();
        let msg = "some message";
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let (did, _) = service.create_my_did(&did_info.clone()).unwrap();
        let did = Did::new(did.did, did.verkey);
        let encrypted_message = service.crypto_box_seal(&did.verkey, msg.as_bytes());
        assert!(encrypted_message.is_ok());
    }

    #[test]
    fn crypto_box_seal_and_crypto_box_seal_open_works() {
        let service = CryptoService::new();
        let msg = "some message".as_bytes();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let (did, key) = service.create_my_did(&did_info.clone()).unwrap();
        let encrypt_did = Did::new(did.did.clone(), did.verkey.clone());
        let encrypted_message = service.crypto_box_seal(&encrypt_did.verkey, msg).unwrap();
        let decrypted_message = service.crypto_box_seal_open(&key, &encrypted_message).unwrap();
        assert_eq!(msg, decrypted_message.as_slice());
    }

    #[test]
    fn authenticated_encrypt_works() {
        let service = CryptoService::new();
        let msg = "some message".as_bytes();
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let ( _ , my_key) = service.create_my_did(&did_info).unwrap();
        let (their_did, _their_key) = service.create_my_did(&did_info.clone()).unwrap();
        let their_did_for_encrypt = Did::new(their_did.did, their_did.verkey);
        let encrypted_message = service.authenticated_encrypt(&my_key, &their_did_for_encrypt.verkey, msg);
        assert!(encrypted_message.is_ok());
    }

    #[test]
    fn authenticated_encrypt_and_authenticated_decrypt_works() {
        let service = CryptoService::new();
        let msg = "some message";
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();
        let my_key_for_encrypt = my_key.clone();
        let their_did_for_decrypt = Did::new(my_did.did, my_did.verkey);
        let (their_did, their_key) = service.create_my_did(&did_info.clone()).unwrap();
        let my_key_for_decrypt = their_key.clone();
        let their_did_for_encrypt = Did::new(their_did.did, their_did.verkey);
        let encrypted_message = service.authenticated_encrypt(&my_key_for_encrypt, &their_did_for_encrypt.verkey, msg.as_bytes()).unwrap();
        let (their_vk, decrypted_message) = service.authenticated_decrypt(&my_key_for_decrypt,&encrypted_message).unwrap();
        assert_eq!(msg.as_bytes().to_vec(), decrypted_message);
        assert_eq!(&their_vk, &their_did_for_decrypt.verkey);

    }

    pub fn test_auth_encrypt_recipient_works() {
        //setup route_service
        let service: CryptoService = CryptoService::new();

        //setup DIDs and Keys
        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();
        let (their_did, their_key) = service.create_my_did(&did_info.clone()).unwrap();

        let my_key_for_encrypt = my_key.clone();
        let their_did_for_encrypt = Did::new(their_did.did, their_did.verkey);

        let my_key_for_decrypt = their_key.clone();
        let their_did_for_decrypt = Did::new(my_did.did, my_did.verkey);

        //sym_key
        let sym_key = xsalsa20::create_key();

        //pack then unpack message
        let auth_recipient = service
            .auth_encrypt_recipient(&my_key_for_encrypt, &their_did_for_encrypt.verkey, &sym_key);

        //verify no errors are thrown
        assert_eq!(auth_recipient.is_ok());
    }

//    pub fn test_auth_encrypt_and_auth_decrypt_recipient_works() {
//        //setup route_service
//        let service: CryptoService = CryptoService::new();
//
//        //setup DIDs and Keys
//        let did_info = MyDidInfo { did: None, cid: None, seed: None, crypto_type: None };
//        let (my_did, my_key) = service.create_my_did(&did_info).unwrap();
//        let (their_did, their_key) = service.create_my_did(&did_info.clone()).unwrap();
//
//        let my_key_for_encrypt = my_key.clone();
//        let their_did_for_encrypt = Did::new(their_did.did, their_did.verkey);
//
//        let my_key_for_decrypt = their_key.clone();
//        let their_did_for_decrypt = Did::new(my_did.did, my_did.verkey);
//
//        //sym_key
//        let sym_key = xsalsa20::create_key();
//
//        //pack then unpack message
//        let auth_recipient = service
//            .auth_encrypt_recipient(&my_key_for_encrypt, &their_did_for_encrypt.verkey, &sym_key);
//
//        let (expected_sym_key, sender_vk) = service
//            .auth_decrypt_recipient(&recv_key, auth_recipient, cs.clone())
//            .unwrap();
//
//        //verify same plaintext goes in and comes out
//        assert_eq!(expected_sym_key, sym_key);
//        assert_eq!(expected_send_key.verkey, sender_vk);
//    }
}