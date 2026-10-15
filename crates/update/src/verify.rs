//! 安装包大小与 sha256 校验（PLAN §4.3 / §4.5 verifying 步骤的纯逻辑）。

use sha2::{Digest, Sha256};

/// 校验失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    SizeMismatch { expected: u64, actual: u64 },
    DigestMismatch { expected: String, actual: String },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeMismatch { expected, actual } => {
                write!(f, "大小不符：期望 {expected}，实际 {actual}")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(f, "摘要不符：期望 {expected}，实际 {actual}")
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// 计算数据的小写 hex sha256。
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 先比大小再比摘要；`expected_sha256_hex` 应为小写 64 位 hex（与 Release 解析一致）。
pub fn verify_installer(
    data: &[u8],
    expected_size: u64,
    expected_sha256_hex: &str,
) -> Result<(), VerifyError> {
    let actual = data.len() as u64;
    if actual != expected_size {
        return Err(VerifyError::SizeMismatch {
            expected: expected_size,
            actual,
        });
    }
    let actual_hex = sha256_hex(data);
    if actual_hex != expected_sha256_hex {
        return Err(VerifyError::DigestMismatch {
            expected: expected_sha256_hex.to_string(),
            actual: actual_hex,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector_abc() {
        // FIPS 180-2 / 常见测试向量：SHA256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_size_and_digest_mismatch() {
        let data = b"abc";
        let good = sha256_hex(data);
        assert!(verify_installer(data, 3, &good).is_ok());

        match verify_installer(data, 4, &good) {
            Err(VerifyError::SizeMismatch { expected: 4, actual: 3 }) => {}
            other => panic!("expected SizeMismatch, got {other:?}"),
        }

        match verify_installer(data, 3, &("0".repeat(64))) {
            Err(VerifyError::DigestMismatch { .. }) => {}
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
    }
}
