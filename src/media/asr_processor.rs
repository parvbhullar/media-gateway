use super::processor::Processor;
use crate::{AudioFrame, Samples, transcription::TranscriptionClient};
use anyhow::Result;

pub struct AsrProcessor {
    pub asr_client: Box<dyn TranscriptionClient>,
}

impl AsrProcessor {}

impl Processor for AsrProcessor {
    fn process_frame(&self, frame: &mut AudioFrame) -> Result<()> {
        match &frame.samples {
            Samples::PCM { samples } => {
                // Only send audio to ASR when VAD indicates speech
                // If vad_speaking is None (VAD not enabled), send all audio (backward compatible)
                // If vad_speaking is Some(true), user is speaking, send audio
                // If vad_speaking is Some(false), silence detected, skip this frame
                match frame.vad_speaking {
                    None | Some(true) => {
                        self.asr_client.send_audio(&samples)?;
                    }
                    Some(false) => {
                        // Skip silent frames - don't send to ASR service
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
