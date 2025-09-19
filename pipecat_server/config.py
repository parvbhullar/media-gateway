"""
Configuration management for Pipecat Media Server
"""

import os
from typing import Optional
from pydantic import Field
from pydantic_settings import BaseSettings


class Settings(BaseSettings):
    """Application settings with environment variable support"""
    
    # Server Configuration
    host: str = Field(default="0.0.0.0", alias="PIPECAT_SERVER_HOST")
    port: int = Field(default=8765, alias="PIPECAT_SERVER_PORT")
    log_level: str = Field(default="INFO", alias="LOG_LEVEL")
    
    # API Keys
    deepgram_api_key: Optional[str] = Field(default=None, alias="DEEPGRAM_API_KEY")
    openai_api_key: Optional[str] = Field(default=None, alias="OPENAI_API_KEY")
    
    # AI Model Settings
    llm_model: str = Field(default="gpt-4o-mini", alias="LLM_MODEL")
    llm_max_tokens: int = Field(default=150, alias="LLM_MAX_TOKENS")
    llm_temperature: float = Field(default=0.7, alias="LLM_TEMPERATURE")
    
    # Speech Settings
    tts_model: str = Field(default="aura-asteria-en", alias="TTS_MODEL")
    stt_model: str = Field(default="nova", alias="STT_MODEL")
    stt_language: str = Field(default="en", alias="STT_LANGUAGE")
    
    # Audio Processing
    sample_rate: int = Field(default=16000, alias="SAMPLE_RATE")
    channels: int = Field(default=1, alias="CHANNELS")
    frame_size: int = Field(default=160, alias="FRAME_SIZE")  # 10ms at 16kHz
    
    # System Settings
    max_concurrent_calls: int = Field(default=10, alias="MAX_CONCURRENT_CALLS")
    call_timeout: int = Field(default=300, alias="CALL_TIMEOUT")  # 5 minutes
    
    model_config = {
        "env_file": ".env",
        "env_file_encoding": "utf-8",
        "case_sensitive": False,
        "extra": "ignore",  # Allow extra environment variables
    }
    
    def validate_api_keys(self) -> bool:
        """Validate that required API keys are present"""
        missing_keys = []
        
        if not self.deepgram_api_key or self.deepgram_api_key == "placeholder_deepgram_key":
            missing_keys.append("DEEPGRAM_API_KEY")
        if not self.openai_api_key or self.openai_api_key == "placeholder_openai_key":
            missing_keys.append("OPENAI_API_KEY")
            
        if missing_keys:
            # For development, just warn instead of raising an error
            print(f"⚠️ Warning: Missing API keys: {', '.join(missing_keys)}")
            print("🔧 AI services will be disabled until keys are provided")
            return False
        
        return True


# Global settings instance
_settings: Optional[Settings] = None


def get_settings() -> Settings:
    """Get or create the global settings instance"""
    global _settings
    if _settings is None:
        _settings = Settings()
        # Validate API keys on first load (non-blocking for development)
        _settings.validate_api_keys()
    return _settings


def reload_settings() -> Settings:
    """Force reload settings from environment"""
    global _settings
    _settings = None
    return get_settings()