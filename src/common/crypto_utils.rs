use aes::Aes256;
use base64::{engine::general_purpose, Engine as _};
use block_padding::Pkcs7;
use cbc::{
    cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit},
    Decryptor, Encryptor,
};
use sha2::{Sha256, Digest};
use rand::Rng;
use thiserror::Error;

type Aes256CbcEnc = Encryptor<Aes256>;
type Aes256CbcDec = Decryptor<Aes256>;

const SALT: &str = "K7mX9pQ2vN8wR5tY1uI6oP3sA4dF7gH0jL9zC6xV2bM8nE5qW1rT4yU3iO0pA7sD";
const AES_BLOCK_SIZE: usize = 16;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Encryption error: {0}")]
    EncryptionError(String),
    #[error("Decryption error: {0}")]
    DecryptionError(String),
    #[error("Invalid ciphertext length")]
    InvalidCiphertextLength,
}

pub struct CryptoUtils;

impl CryptoUtils {
    /// SHA256 哈希
    fn sha256(input: &str) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        hasher.finalize().to_vec()
    }

    /// 生成加密密钥(基于固定盐值)
    pub fn generate_key() -> Vec<u8> {
        let hashed = Self::sha256(SALT);
        // 取前32字节作为AES-256密钥
        hashed[..32].to_vec()
    }

    /// Base64 编码
    fn base64_encode(input: &[u8]) -> String {
        general_purpose::STANDARD.encode(input)
    }

    /// Base64 解码
    fn base64_decode(input: &str) -> Result<Vec<u8>, CryptoError> {
        general_purpose::STANDARD
            .decode(input)
            .map_err(|e| CryptoError::DecryptionError(format!("Base64 decode error: {}", e)))
    }

    /// AES 加密
    fn aes_encrypt(plaintext: &str, key: &[u8]) -> Result<Vec<u8>, CryptoError> {
        // 生成随机 IV
        let mut iv = [0u8; AES_BLOCK_SIZE];
        rand::rng().fill(&mut iv);

        // 准备数据 - 分配足够的空间用于填充（最多一个完整块）
        let plaintext_bytes = plaintext.as_bytes();
        let mut buffer = vec![0u8; plaintext_bytes.len() + AES_BLOCK_SIZE];
        buffer[..plaintext_bytes.len()].copy_from_slice(plaintext_bytes);

        // 加密
        let cipher = Aes256CbcEnc::new(key.into(), &iv.into());
        let ciphertext = cipher
            .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext_bytes.len())
            .map_err(|e| CryptoError::EncryptionError(format!("AES encrypt error: {:?}", e)))?;

        // 将 IV 和密文组合
        let mut result = Vec::with_capacity(AES_BLOCK_SIZE + ciphertext.len());
        result.extend_from_slice(&iv);
        result.extend_from_slice(ciphertext);

        Ok(result)
    }

    /// AES 解密
    fn aes_decrypt(ciphertext: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if ciphertext.len() < AES_BLOCK_SIZE {
            return Err(CryptoError::InvalidCiphertextLength);
        }

        // 提取 IV
        let iv = &ciphertext[..AES_BLOCK_SIZE];
        // 提取密文
        let encrypted_data = &ciphertext[AES_BLOCK_SIZE..];

        // 准备缓冲区
        let mut buffer = encrypted_data.to_vec();

        // 解密
        let cipher = Aes256CbcDec::new(key.into(), iv.into());
        let decrypted = cipher
            .decrypt_padded_mut::<Pkcs7>(&mut buffer)
            .map_err(|e| CryptoError::DecryptionError(format!("AES decrypt error: {:?}", e)))?;

        Ok(decrypted.to_vec())
    }

    /// 加密字符串
    pub fn encrypt(plaintext: &str) -> Result<String, CryptoError> {
        let key = Self::generate_key();
        let encrypted = Self::aes_encrypt(plaintext, &key)?;
        Ok(Self::base64_encode(&encrypted))
    }

    /// 解密字符串
    pub fn decrypt(ciphertext: &str) -> Result<String, CryptoError> {
        let key = Self::generate_key();
        let decoded = Self::base64_decode(ciphertext)?;
        let decrypted = Self::aes_decrypt(&decoded, &key)?;
        String::from_utf8(decrypted)
            .map_err(|e| CryptoError::DecryptionError(format!("UTF-8 decode error: {}", e)))
    }
}
