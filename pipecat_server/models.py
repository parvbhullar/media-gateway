"""
Data models for Pipecat Media Server
"""

from pydantic import BaseModel, Field
from typing import Optional, Dict, Any, List
from datetime import datetime
from enum import Enum


class MessageType(str, Enum):
    """WebSocket message types"""
    AUDIO = "audio"
    TEXT = "text"
    CONFIGURE = "configure"
    STATUS = "status"
    ERROR = "error"
    PING = "ping"
    PONG = "pong"


class AudioFormat(str, Enum):
    """Supported audio formats"""
    LINEAR16 = "linear16"
    MULAW = "mulaw"
    ALAW = "alaw"
    OPUS = "opus"


class RoomCreate(BaseModel):
    """Request model for creating a new room"""
    name: str = Field(..., min_length=1, max_length=255)
    system_prompt: Optional[str] = Field(default="You are a helpful AI assistant.", max_length=2048)


class RoomUpdate(BaseModel):
    """Request model for updating room settings"""
    system_prompt: str = Field(..., max_length=2048)


class Room(BaseModel):
    """Room data model"""
    id: str
    name: str
    system_prompt: str
    created_at: datetime
    active: bool = True
    metadata: Optional[Dict[str, Any]] = None


class HealthResponse(BaseModel):
    """Health check response model"""
    status: str
    timestamp: datetime
    version: str
    active_rooms: int
    active_connections: int


class WebSocketMessage(BaseModel):
    """Base WebSocket message model"""
    type: MessageType
    timestamp: Optional[datetime] = None
    session_id: Optional[str] = None
    data: Optional[Dict[str, Any]] = None


class AudioMessage(WebSocketMessage):
    """Audio data message"""
    type: MessageType = MessageType.AUDIO
    audio_data: str  # Base64 encoded audio
    format: AudioFormat = AudioFormat.LINEAR16
    sample_rate: int = 16000
    channels: int = 1


class TextMessage(WebSocketMessage):
    """Text message"""
    type: MessageType = MessageType.TEXT
    text: str
    is_final: bool = True


class ConfigureMessage(WebSocketMessage):
    """Configuration message"""
    type: MessageType = MessageType.CONFIGURE
    config: Dict[str, Any]


class StatusMessage(WebSocketMessage):
    """Status update message"""
    type: MessageType = MessageType.STATUS
    status: str
    details: Optional[str] = None


class ErrorMessage(WebSocketMessage):
    """Error message"""
    type: MessageType = MessageType.ERROR
    error: str
    code: Optional[int] = None


class PingMessage(WebSocketMessage):
    """Ping message for connection health"""
    type: MessageType = MessageType.PING


class PongMessage(WebSocketMessage):
    """Pong response message"""
    type: MessageType = MessageType.PONG


class AIServiceConfig(BaseModel):
    """Configuration for AI services"""
    stt_provider: str = "deepgram"
    stt_model: str = "nova"
    stt_language: str = "en"
    
    llm_provider: str = "openai"
    llm_model: str = "gpt-4o-mini"
    llm_max_tokens: int = 150
    llm_temperature: float = 0.7
    
    tts_provider: str = "deepgram"
    tts_model: str = "aura-asteria-en"
    tts_voice: str = "asteria"


class ProcessingStats(BaseModel):
    """Processing statistics"""
    total_messages: int = 0
    total_audio_frames: int = 0
    total_text_messages: int = 0
    total_errors: int = 0
    average_processing_time: float = 0.0
    last_activity: Optional[datetime] = None