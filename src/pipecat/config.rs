/*!
 * Pipecat Configuration for RustPBX
 */

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Pipecat server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipecatConfig {
    /// Enable Pipecat integration
    pub enabled: bool,
    
    /// Pipecat server WebSocket URL
    #[serde(alias = "serverUrl", alias = "server_url")]
    pub server_url: Option<String>,
    
    /// Use Pipecat for AI processing instead of internal services
    #[serde(alias = "useForAI", alias = "use_for_ai")]
    pub use_for_ai: bool,
    
    /// Fallback to internal AI processing if Pipecat is unavailable
    #[serde(alias = "fallbackToInternal", alias = "fallback_to_internal")]
    pub fallback_to_internal: bool,
    
    /// Connection timeout in seconds
    pub connection_timeout: u64,
    
    /// Reconnection settings
    #[serde(default)]
    pub reconnect: PipecatReconnectConfig,
    
    /// Audio processing settings
    #[serde(default)]
    pub audio: PipecatAudioConfig,
    
    /// Default system prompt for AI conversations
    #[serde(default)]
    pub default_system_prompt: Option<String>,
    
    /// Enable metrics collection
    #[serde(default)]
    pub enable_metrics: bool,
    
    /// Enable debug logging for Pipecat communication
    #[serde(default)]
    pub debug_logging: bool,
}

/// Reconnection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PipecatReconnectConfig {
    pub enabled: bool,
    pub max_attempts: u32,
    pub initial_delay: u64,
    pub max_delay: u64,
    pub backoff_multiplier: f64,
}

impl Default for PipecatReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 5,
            initial_delay: 1,
            max_delay: 30,
            backoff_multiplier: 2.0,
        }
    }
}

/// Audio processing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PipecatAudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    #[serde(default)]
    pub frame_size: Option<usize>,
    #[serde(default)]
    pub buffer_size: Option<usize>,
    #[serde(default)]
    pub enable_compression: bool,
    #[serde(default)]
    pub encoding: Option<String>,
}

impl Default for PipecatAudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            frame_size: Some(160),
            buffer_size: Some(320),
            enable_compression: false,
            encoding: Some("linear16".to_string()),
        }
    }
}


impl Default for PipecatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            server_url: Some("ws://0.0.0.0:8081".to_string()),
            use_for_ai: true,
            fallback_to_internal: true,
            connection_timeout: 30,
            reconnect: PipecatReconnectConfig::default(),
            audio: PipecatAudioConfig::default(),
            default_system_prompt: Some(
                "You are a helpful AI assistant in a voice conversation. \
                Respond naturally and conversationally. Keep responses brief but informative.".to_string()
            ),
            enable_metrics: true,
            debug_logging: false,
        }
    }
}


impl PipecatConfig {
    /// Get connection timeout as Duration
    pub fn connection_timeout_duration(&self) -> Duration {
        Duration::from_secs(self.connection_timeout)
    }
    
    /// Get initial reconnection delay as Duration
    pub fn initial_reconnect_delay(&self) -> Duration {
        Duration::from_secs(self.reconnect.initial_delay)
    }
    
    /// Get maximum reconnection delay as Duration
    pub fn max_reconnect_delay(&self) -> Duration {
        Duration::from_secs(self.reconnect.max_delay)
    }
    
    /// Check if reconnection is enabled
    pub fn is_reconnect_enabled(&self) -> bool {
        self.reconnect.enabled
    }
    
    /// Get server URL or default
    pub fn get_server_url(&self) -> String {
        self.server_url.clone()
            .unwrap_or_else(|| "ws://0.0.0.0:8081".to_string())
    }
    
    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled {
            if self.server_url.is_none() || self.server_url.as_ref().unwrap().is_empty() {
                return Err("Pipecat server URL is required when enabled".to_string());
            }
            
            if self.connection_timeout == 0 {
                return Err("Connection timeout must be greater than 0".to_string());
            }
            
            if self.reconnect.enabled && self.reconnect.max_attempts == 0 {
                return Err("Max reconnection attempts must be greater than 0".to_string());
            }
            
            if self.audio.sample_rate == 0 {
                return Err("Audio sample rate must be greater than 0".to_string());
            }
            
            if self.audio.channels == 0 {
                return Err("Audio channels must be greater than 0".to_string());
            }
        }
        
        Ok(())
    }
    
    /// Calculate reconnection delay with exponential backoff
    pub fn calculate_reconnect_delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return self.initial_reconnect_delay();
        }
        
        let delay_secs = self.reconnect.initial_delay as f64 
            * self.reconnect.backoff_multiplier.powi(attempt as i32 - 1);
        
        let max_delay_secs = self.reconnect.max_delay as f64;
        let final_delay_secs = delay_secs.min(max_delay_secs);
        
        Duration::from_secs_f64(final_delay_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PipecatConfig::default();
        assert!(!config.enabled);
        assert!(config.fallback_to_internal);
        assert_eq!(config.connection_timeout, 30);
        assert_eq!(config.audio.sample_rate, 16000);
        assert_eq!(config.audio.channels, 1);
        assert_eq!(config.audio.encoding, Some("linear16".to_string()));
    }

    #[test]
    fn test_config_validation() {
        let mut config = PipecatConfig::default();
        
        // Valid disabled config
        assert!(config.validate().is_ok());
        
        // Invalid enabled config (no server URL)
        config.enabled = true;
        config.server_url = None;
        assert!(config.validate().is_err());
        
        // Valid enabled config
        config.server_url = Some("ws://0.0.0.0:8081".to_string());
        assert!(config.validate().is_ok());
        
        // Invalid timeout
        config.connection_timeout = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_reconnect_delay_calculation() {
        let config = PipecatConfig::default();
        
        // First attempt
        let delay1 = config.calculate_reconnect_delay(1);
        assert_eq!(delay1, Duration::from_secs(1));
        
        // Second attempt (exponential backoff)
        let delay2 = config.calculate_reconnect_delay(2);
        assert_eq!(delay2, Duration::from_secs(2));
        
        // Third attempt
        let delay3 = config.calculate_reconnect_delay(3);
        assert_eq!(delay3, Duration::from_secs(4));
        
        // Should cap at max delay
        let delay_high = config.calculate_reconnect_delay(10);
        assert_eq!(delay_high, Duration::from_secs(30));
    }

    #[test]
    fn test_duration_helpers() {
        let config = PipecatConfig::default();
        
        assert_eq!(config.connection_timeout_duration(), Duration::from_secs(30));
        assert_eq!(config.initial_reconnect_delay(), Duration::from_secs(1));
        assert_eq!(config.max_reconnect_delay(), Duration::from_secs(30));
        assert!(config.is_reconnect_enabled());
    }

    #[test]
    fn test_server_url_handling() {
        let mut config = PipecatConfig::default();
        
        // Has default URL
        assert_eq!(config.get_server_url(), "ws://0.0.0.0:8081");
        
        // Custom URL
        config.server_url = Some("ws://custom:9000/pipecat".to_string());
        assert_eq!(config.get_server_url(), "ws://custom:9000/pipecat");
        
        // Empty URL falls back to default
        config.server_url = Some("".to_string());
        assert_eq!(config.get_server_url(), "");
    }
}