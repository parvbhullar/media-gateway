use anyhow::{Result, anyhow};
use async_trait::async_trait;
use crate::synthesis::{SynthesisClient, SynthesisType, SynthesisOption, SynthesisEvent};
use futures::stream::{self, BoxStream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio_util::sync::CancellationToken;

pub struct DeepgramTtsClient {
    option: SynthesisOption,
}

impl DeepgramTtsClient {
    pub fn new(option: SynthesisOption) -> Self {
        Self { option }
    }

    pub fn create(option: &SynthesisOption) -> anyhow::Result<Box<dyn SynthesisClient>> {
        Ok(Box::new(Self::new(option.clone())))
    }
}

#[async_trait]
impl SynthesisClient for DeepgramTtsClient {
    fn provider(&self) -> SynthesisType {
        SynthesisType::Other("deepgram".to_string())
    }

    async fn start(
        &self,
        _cancel_token: CancellationToken,
    ) -> Result<BoxStream<'static, Result<SynthesisEvent>>> {
        Err(anyhow!("Deepgram streaming TTS not implemented"))
    }

    async fn synthesize(
        &self,
        text: &str,
        _end_of_stream: Option<bool>,
        _option: Option<SynthesisOption>,
    ) -> Result<()> {
        let api_key = self.option.secret_key.clone().ok_or_else(|| anyhow!("No Deepgram API key provided"))?;
        let client = reqwest::Client::new();
        let url = "https://api.deepgram.com/v1/speak";
        let resp = client
            .post(url)
            .header(AUTHORIZATION, format!("Token {}", api_key))
            .header(CONTENT_TYPE, "application/json")
            .body(format!("{{\"text\":\"{}\"}}", text))
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            return Err(anyhow!("Deepgram TTS error: {}", String::from_utf8_lossy(&bytes)));
        }
        // TODO: send audio to playback pipeline
        Ok(())
    }
}
