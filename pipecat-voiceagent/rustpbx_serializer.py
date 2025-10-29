#!/usr/bin/env python3
"""
RustPBX Frame Serializer for Pipecat

This serializer handles conversion between RustPBX audio format and Pipecat frames.
RustPBX sends raw PCM audio as binary data (16-bit, 16kHz, mono).
"""

from loguru import logger
from pipecat.frames.frames import Frame, InputAudioRawFrame, OutputAudioRawFrame
from pipecat.serializers.base_serializer import FrameSerializer, FrameSerializerType


class RustPBXFrameSerializer(FrameSerializer):
    """
    Serializer for converting between Pipecat frames and RustPBX audio format.

    RustPBX Audio Format:
    - Sample Rate: 16000 Hz
    - Channels: 1 (mono)
    - Bit Depth: 16-bit signed PCM
    - Byte Order: Little-endian
    - Format: Raw binary audio data
    """

    def __init__(self, sample_rate: int = 16000, num_channels: int = 1):
        """
        Initialize the RustPBX frame serializer.

        Args:
            sample_rate: Audio sample rate in Hz (default: 16000)
            num_channels: Number of audio channels (default: 1 for mono)
        """
        super().__init__()
        self.sample_rate = sample_rate
        self.num_channels = num_channels
        logger.info(
            f"RustPBXFrameSerializer initialized: {sample_rate}Hz, {num_channels} channel(s)"
        )

    @property
    def type(self) -> FrameSerializerType:
        """
        Get the serializer type.

        Returns:
            BINARY type since RustPBX uses raw binary audio data
        """
        return FrameSerializerType.BINARY

    async def serialize(self, frame: Frame) -> bytes | None:
        """
        Serialize a Pipecat frame to RustPBX audio format.

        Converts OutputAudioRawFrame (TTS output) to raw PCM bytes
        that RustPBX can play through the speaker.

        Args:
            frame: The Pipecat frame to serialize (must be OutputAudioRawFrame)

        Returns:
            Raw PCM audio bytes, or None if frame type is not supported
        """
        if not isinstance(frame, OutputAudioRawFrame):
            # Only handle audio output frames
            return None

        # RustPBX expects raw PCM audio bytes (16-bit signed, little-endian)
        # The frame.audio is already in the correct format
        logger.debug(
            f"Serializing OutputAudioRawFrame: {len(frame.audio)} bytes, "
            f"{frame.sample_rate}Hz, {frame.num_channels} channel(s)"
        )

        return frame.audio

    async def deserialize(self, data: bytes) -> Frame | None:
        """
        Deserialize RustPBX audio data to a Pipecat frame.

        Converts raw PCM bytes from RustPBX (microphone input) into
        InputAudioRawFrame that can be processed by STT services.

        Args:
            data: Raw PCM audio bytes from RustPBX

        Returns:
            InputAudioRawFrame containing the audio data, or None if deserialization fails
        """
        if not isinstance(data, bytes) or len(data) == 0:
            logger.warning("Received invalid audio data for deserialization")
            return None

        # Create InputAudioRawFrame from raw PCM bytes
        # RustPBX sends 16-bit signed PCM at 16kHz mono
        frame = InputAudioRawFrame(
            audio=data,
            sample_rate=self.sample_rate,
            num_channels=self.num_channels,
        )

        logger.debug(
            f"Deserialized audio data: {len(data)} bytes -> InputAudioRawFrame "
            f"({self.sample_rate}Hz, {self.num_channels} ch)"
        )

        return frame
