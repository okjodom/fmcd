use std::fmt;

use tracing::warn;

/// Configuration for sensitive data sanitization
#[derive(Clone, Debug)]
pub struct SanitizationConfig {
    /// Whether to sanitize payment preimages completely
    pub sanitize_preimages: bool,
    /// Whether to sanitize lightning invoices
    pub sanitize_invoices: bool,
    /// Maximum characters to show from start/end of sensitive data
    pub partial_show_chars: usize,
}

impl Default for SanitizationConfig {
    fn default() -> Self {
        Self {
            sanitize_preimages: true,
            sanitize_invoices: true,
            partial_show_chars: 6,
        }
    }
}

/// A wrapper for sensitive data that implements safe Display
#[derive(Clone, Debug)]
pub struct SensitiveData<T> {
    inner: T,
    data_type: SensitiveDataType,
    config: SanitizationConfig,
}

#[derive(Clone, Debug, Copy)]
pub enum SensitiveDataType {
    /// Lightning invoice (bolt11)
    LightningInvoice,
    /// Payment preimage (hex encoded 32 bytes)
    PaymentPreimage,
    /// Private key or seed material
    PrivateKey,
    /// User token or session identifier
    UserToken,
    /// Payment hash
    PaymentHash,
}

impl SensitiveDataType {
    fn display_name(&self) -> &'static str {
        match self {
            Self::LightningInvoice => "invoice",
            Self::PaymentPreimage => "preimage",
            Self::PrivateKey => "private_key",
            Self::UserToken => "user_token",
            Self::PaymentHash => "payment_hash",
        }
    }
}

impl<T: fmt::Display> SensitiveData<T> {
    pub fn new(data: T, data_type: SensitiveDataType) -> Self {
        Self {
            inner: data,
            data_type,
            config: SanitizationConfig::default(),
        }
    }

    pub fn with_config(data: T, data_type: SensitiveDataType, config: SanitizationConfig) -> Self {
        Self {
            inner: data,
            data_type,
            config,
        }
    }

    /// Get the raw inner value (use with caution - only for necessary business
    /// logic)
    pub fn inner(&self) -> &T {
        warn!(
            data_type = self.data_type.display_name(),
            "Raw sensitive data accessed - ensure this is necessary and secure"
        );
        &self.inner
    }

    /// Create a safe representation for logging
    fn sanitized_repr(&self) -> String {
        let original = self.inner.to_string();

        let should_sanitize = match self.data_type {
            SensitiveDataType::LightningInvoice => self.config.sanitize_invoices,
            SensitiveDataType::PaymentPreimage => self.config.sanitize_preimages,
            SensitiveDataType::PrivateKey => true, // Always sanitize private keys
            SensitiveDataType::UserToken => true,  // Always sanitize tokens
            SensitiveDataType::PaymentHash => false, // Payment hashes are usually safe to log
        };

        if !should_sanitize {
            return original;
        }

        let len = original.len();
        if len <= self.config.partial_show_chars * 2 {
            // If string is very short, just show placeholders
            format!(
                "[REDACTED_{}]",
                self.data_type.display_name().to_uppercase()
            )
        } else {
            // Show first and last few characters with redacted middle
            let start = &original[..self.config.partial_show_chars];
            let end = &original[len - self.config.partial_show_chars..];
            let middle_len = len - (self.config.partial_show_chars * 2);

            format!(
                "{}[REDACTED_{}_{}_CHARS]{}",
                start,
                self.data_type.display_name().to_uppercase(),
                middle_len,
                end
            )
        }
    }
}

impl<T: fmt::Display> fmt::Display for SensitiveData<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.sanitized_repr())
    }
}

/// Convenience functions for creating sanitized data
pub fn sanitize_invoice<T: fmt::Display>(invoice: T) -> SensitiveData<T> {
    SensitiveData::new(invoice, SensitiveDataType::LightningInvoice)
}

pub fn sanitize_preimage<T: fmt::Display>(preimage: T) -> SensitiveData<T> {
    SensitiveData::new(preimage, SensitiveDataType::PaymentPreimage)
}

pub fn sanitize_private_key<T: fmt::Display>(key: T) -> SensitiveData<T> {
    SensitiveData::new(key, SensitiveDataType::PrivateKey)
}

pub fn sanitize_user_token<T: fmt::Display>(token: T) -> SensitiveData<T> {
    SensitiveData::new(token, SensitiveDataType::UserToken)
}

pub fn sanitize_payment_hash<T: fmt::Display>(hash: T) -> SensitiveData<T> {
    SensitiveData::new(hash, SensitiveDataType::PaymentHash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invoice_sanitization() {
        let invoice = "lnbc1u1p3xnhl2pp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpuaxtlgmg8d";
        let sanitized = sanitize_invoice(invoice);
        let result = sanitized.to_string();

        assert!(result.contains("lnbc1u"));
        assert!(result.contains("[REDACTED_INVOICE_"));
        assert!(result.ends_with("g8d"));
        assert!(!result.contains(
            "qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpu"
        ));
    }

    #[test]
    fn test_preimage_sanitization() {
        let preimage = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let sanitized = sanitize_preimage(preimage);
        let result = sanitized.to_string();

        assert!(result.contains("123456"));
        assert!(result.contains("bcdef"));
        assert!(result.contains("[REDACTED_PREIMAGE_"));
        assert!(!result.contains("567890abcdef1234567890abcdef1234567890abcdef123456789a0ab"));
    }

    #[test]
    fn test_short_data_sanitization() {
        let short_data = "abc";
        let sanitized = sanitize_preimage(short_data);
        let result = sanitized.to_string();

        assert_eq!(result, "[REDACTED_PREIMAGE]");
    }

    #[test]
    fn test_payment_hash_not_sanitized_by_default() {
        let hash = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let sanitized = sanitize_payment_hash(hash);
        let result = sanitized.to_string();

        // Payment hashes are safe to log by default
        assert_eq!(result, hash);
    }

    #[test]
    fn test_private_key_always_sanitized() {
        let config = SanitizationConfig {
            sanitize_preimages: false,
            sanitize_invoices: false,
            partial_show_chars: 6,
        };

        let key = "L4rK3xkrhnDAQdaNjGte6DPaKjjgs4BLfwcXKJF3qQDe7YkfLmF8";
        let sanitized = SensitiveData::with_config(key, SensitiveDataType::PrivateKey, config);
        let result = sanitized.to_string();

        // Private keys should ALWAYS be sanitized regardless of config
        assert!(result.contains("[REDACTED_PRIVATE_KEY_"));
        assert!(!result.contains("rK3xkrhnDAQdaNjGte6DPaKjjgs4BLfwcXKJF3qQDe7Ykf"));
    }
}
