use super::{
    asr_processor::AsrProcessor,
    denoiser::NoiseReducer,
    processor::Processor,
    track::{
        Track,
        pipecat::PipecatProcessor,
        tts::{SynthesisHandle, TtsTrack},
    },
    vad::{VADOption, VadProcessor, VadType},
};
use crate::{
    TrackId,
    call::{CallOption, EouOption},
    event::EventSender,
    pipecat::PipecatConfig,
    synthesis::{
        AliyunTtsClient, DeepgramTtsClient, SynthesisClient, SynthesisOption, SynthesisType,
        TencentCloudTtsClient, VoiceApiTtsClient,
    },
    transcription::{
        AliyunAsrClientBuilder, DeepgramAsrClientBuilder, TencentCloudAsrClientBuilder,
        TranscriptionClient, TranscriptionOption, TranscriptionType, VoiceApiAsrClientBuilder,
    },
};
use anyhow::Result;
use std::{collections::HashMap, error, future::Future, pin::Pin, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

pub type FnCreateVadProcessor = fn(
    token: CancellationToken,
    event_sender: EventSender,
    option: VADOption,
) -> Result<Box<dyn Processor>>;

pub type FnCreateEouProcessor = fn(
    token: CancellationToken,
    event_sender: EventSender,
    option: EouOption,
) -> Result<Box<dyn Processor>>;

pub type FnCreateAsrClient = Box<
    dyn Fn(
            TrackId,
            CancellationToken,
            TranscriptionOption,
            EventSender,
        ) -> Pin<Box<dyn Future<Output = Result<Box<dyn TranscriptionClient>>> + Send>>
        + Send
        + Sync,
>;
pub type FnCreateTtsClient = fn(option: &SynthesisOption) -> Result<Box<dyn SynthesisClient>>;

pub type FnCreatePipecatProcessor = Box<
    dyn Fn(
            TrackId,
            CancellationToken,
            PipecatConfig,
            EventSender,
        ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Processor>>> + Send>>
        + Send
        + Sync,
>;

// Define hook types
pub type CreateProcessorsHook = Box<
    dyn Fn(
            Arc<StreamEngine>,
            &dyn Track,
            CancellationToken,
            EventSender,
            CallOption,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<Box<dyn Processor>>>> + Send>>
        + Send
        + Sync,
>;

pub struct StreamEngine {
    vad_creators: HashMap<VadType, FnCreateVadProcessor>,
    eou_creators: HashMap<String, FnCreateEouProcessor>,
    asr_creators: HashMap<TranscriptionType, FnCreateAsrClient>,
    tts_creators: HashMap<SynthesisType, FnCreateTtsClient>,

    // ADD: Pipecat processor creator
    pipecat_creator: Option<FnCreatePipecatProcessor>,

    create_processors_hook: Arc<CreateProcessorsHook>,
}

impl Default for StreamEngine {
    fn default() -> Self {
        let mut engine = Self::new();

        // Existing registrations...
        #[cfg(feature = "vad_silero")]
        engine.register_vad(VadType::Silero, VadProcessor::create_silero);
        #[cfg(feature = "vad_webrtc")]
        engine.register_vad(VadType::WebRTC, VadProcessor::create_webrtc);
        #[cfg(feature = "vad_ten")]
        engine.register_vad(VadType::Ten, VadProcessor::create_ten);

        engine.register_asr(
            TranscriptionType::TencentCloud,
            Box::new(TencentCloudAsrClientBuilder::create),
        );
        engine.register_asr(
            TranscriptionType::VoiceApi,
            Box::new(VoiceApiAsrClientBuilder::create),
        );
        engine.register_asr(
            TranscriptionType::Aliyun,
            Box::new(AliyunAsrClientBuilder::create),
        );
        engine.register_asr(
            TranscriptionType::Deepgram,
            Box::new(DeepgramAsrClientBuilder::create),
        );
        engine.register_tts(SynthesisType::Aliyun, AliyunTtsClient::create);
        engine.register_tts(SynthesisType::TencentCloud, TencentCloudTtsClient::create);
        engine.register_tts(SynthesisType::VoiceApi, VoiceApiTtsClient::create);
        engine.register_tts(SynthesisType::Deepgram, DeepgramTtsClient::create);

        // ADD: Register Pipecat processor
        engine.register_pipecat(Box::new(PipecatProcessor::create));

        engine
    }
}

impl StreamEngine {
    pub fn new() -> Self {
        Self {
            vad_creators: HashMap::new(),
            asr_creators: HashMap::new(),
            tts_creators: HashMap::new(),
            eou_creators: HashMap::new(),

            // ADD: Initialize pipecat creator
            pipecat_creator: None,

            create_processors_hook: Arc::new(Box::new(Self::default_create_procesors_hook)),
        }
    }

    pub fn register_vad(&mut self, vad_type: VadType, creator: FnCreateVadProcessor) -> &mut Self {
        self.vad_creators.insert(vad_type, creator);
        self
    }

    pub fn register_eou(&mut self, name: String, creator: FnCreateEouProcessor) -> &mut Self {
        self.eou_creators.insert(name, creator);
        self
    }

    pub fn register_asr(
        &mut self,
        asr_type: TranscriptionType,
        creator: FnCreateAsrClient,
    ) -> &mut Self {
        self.asr_creators.insert(asr_type, creator);
        self
    }

    pub fn register_tts(
        &mut self,
        tts_type: SynthesisType,
        creator: FnCreateTtsClient,
    ) -> &mut Self {
        self.tts_creators.insert(tts_type, creator);
        self
    }

    // ADD: Register Pipecat processor
    pub fn register_pipecat(&mut self, creator: FnCreatePipecatProcessor) -> &mut Self {
        self.pipecat_creator = Some(creator);
        self
    }

    pub fn create_vad_processor(
        &self,
        token: CancellationToken,
        event_sender: EventSender,
        option: VADOption,
    ) -> Result<Box<dyn Processor>> {
        let creator = self.vad_creators.get(&option.r#type);
        if let Some(creator) = creator {
            creator(token, event_sender, option)
        } else {
            Err(anyhow::anyhow!("VAD type not found: {}", option.r#type))
        }
    }
    pub fn create_eou_processor(
        &self,
        token: CancellationToken,
        event_sender: EventSender,
        option: EouOption,
    ) -> Result<Box<dyn Processor>> {
        let creator = self
            .eou_creators
            .get(&option.r#type.clone().unwrap_or_default());
        if let Some(creator) = creator {
            creator(token, event_sender, option)
        } else {
            Err(anyhow::anyhow!("EOU type not found: {:?}", option.r#type))
        }
    }

    pub async fn create_asr_processor(
        &self,
        track_id: TrackId,
        cancel_token: CancellationToken,
        option: TranscriptionOption,
        event_sender: EventSender,
    ) -> Result<Box<dyn Processor>> {
        let asr_client = match option.provider {
            Some(ref provider) => {
                let creator = self.asr_creators.get(&provider);
                if let Some(creator) = creator {
                    creator(track_id, cancel_token, option, event_sender).await?
                } else {
                    return Err(anyhow::anyhow!("ASR type not found: {}", provider));
                }
            }
            None => return Err(anyhow::anyhow!("ASR type not found: {:?}", option.provider)),
        };
        Ok(Box::new(AsrProcessor { asr_client }))
    }

    pub async fn create_tts_client(
        &self,
        tts_option: &SynthesisOption,
    ) -> Result<Box<dyn SynthesisClient>> {
        match tts_option.provider {
            Some(ref provider) => {
                let creator = self.tts_creators.get(&provider);
                if let Some(creator) = creator {
                    creator(tts_option)
                } else {
                    Err(anyhow::anyhow!("TTS type not found: {}", provider))
                }
            }
            None => Err(anyhow::anyhow!(
                "TTS type not found: {:?}",
                tts_option.provider
            )),
        }
    }

    // ADD: Create Pipecat processor
    pub async fn create_pipecat_processor(
        &self,
        track_id: TrackId,
        cancel_token: CancellationToken,
        pipecat_config: PipecatConfig,
        event_sender: EventSender,
    ) -> Result<Box<dyn Processor>> {
        info!(
            "🔧 StreamEngine::create_pipecat_processor called for track: {}",
            track_id
        );

        if let Some(creator) = &self.pipecat_creator {
            info!("✅ Pipecat creator found, calling it...");
            creator(track_id, cancel_token, pipecat_config, event_sender).await
        } else {
            error!("❌ Pipecat creator not registered in StreamEngine!");
            Err(anyhow::anyhow!("Pipecat processor creator not registered"))
        }
    }

    pub async fn create_processors(
        engine: Arc<StreamEngine>,
        track: &dyn Track,
        cancel_token: CancellationToken,
        event_sender: EventSender,
        option: &CallOption,
    ) -> Result<Vec<Box<dyn Processor>>> {
        (engine.clone().create_processors_hook)(
            engine,
            track,
            cancel_token,
            event_sender,
            option.clone(),
        )
        .await
    }

    pub async fn create_tts_track(
        engine: Arc<StreamEngine>,
        cancel_token: CancellationToken,
        session_id: String,
        track_id: TrackId,
        ssrc: u32,
        play_id: Option<String>,
        tts_option: &SynthesisOption,
    ) -> Result<(SynthesisHandle, Box<dyn Track>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let new_handle = SynthesisHandle::new(tx, play_id);
        let tts_client = engine.create_tts_client(tts_option).await?;
        let tts_track = TtsTrack::new(track_id, session_id, rx, tts_client)
            .with_ssrc(ssrc)
            .with_cancel_token(cancel_token);
        Ok((new_handle, Box::new(tts_track) as Box<dyn Track>))
    }

    pub fn with_processor_hook(&mut self, hook_fn: CreateProcessorsHook) -> &mut Self {
        self.create_processors_hook = Arc::new(Box::new(hook_fn));
        self
    }

    fn default_create_procesors_hook(
        engine: Arc<StreamEngine>,
        track: &dyn Track,
        cancel_token: CancellationToken,
        event_sender: EventSender,
        option: CallOption,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Box<dyn Processor>>>> + Send>> {
        let track_id = track.id().clone();
        let samplerate = track.config().samplerate as usize;
        Box::pin(async move {
            info!("🔍 DEBUG: Creating processors for track {}", track_id);
            info!("🔍 DEBUG: CallOption.pipecat {:?}", option.pipecat);
            let mut processors = vec![];

            // ✅ Check if Pipecat is handling AI processing FIRST
            let pipecat_handles_ai = option
                .pipecat
                .as_ref()
                .map(|p| p.enabled && p.use_for_ai)
                .unwrap_or(false);

            if pipecat_handles_ai {
                info!("🎯 Pipecat will handle AI processing (ASR/TTS) - skipping internal processors");
            }

            // Add denoise processor (can work with Pipecat)
            match option.denoise {
                Some(true) => {
                    let noise_reducer = NoiseReducer::new(samplerate)?;
                    processors.push(Box::new(noise_reducer) as Box<dyn Processor>);
                }
                _ => {}
            }

            // Add Pipecat processor if enabled
            match option.pipecat {
                Some(ref pipecat_config) if pipecat_config.enabled => {
                    info!("🔧 Creating Pipecat processor for track: {}", track_id);
                    info!(
                        "🔧 Pipecat config: enabled={}, server_url={:?}, use_for_ai={}",
                        pipecat_config.enabled, pipecat_config.server_url, pipecat_config.use_for_ai
                    );

                    match engine
                        .create_pipecat_processor(
                            track_id.clone(),
                            cancel_token.child_token(),
                            pipecat_config.clone(),
                            event_sender.clone(),
                        )
                        .await
                    {
                        Ok(pipecat_processor) => {
                            processors.push(pipecat_processor);
                            info!(
                                "✅ Pipecat processor successfully added to pipeline for track: {}",
                                track_id
                            );
                            if pipecat_config.use_for_ai {
                                info!("✅ Returning early - Pipecat handles all AI processing");
                                return Ok(processors);  // ✅ EARLY RETURN - skip all ASR/VAD/EOU
                            }
                        }
                        Err(e) => {
                            error!("❌ Failed to create Pipecat processor: {}", e);
                            if pipecat_config.use_for_ai {
                                if pipecat_config.fallback_to_internal {
                                    warn!("⚠️ Pipecat failed but fallback enabled - will create internal ASR/TTS");
                                    // Continue to create ASR/TTS below
                                } else {
                                    error!("❌ Pipecat failed and fallback disabled - no AI processing available");
                                    return Err(anyhow::anyhow!("Pipecat failed and fallback disabled"));
                                }
                            }
                        }
                    }
                }
                Some(ref pipecat_config) => {
                    warn!("⚠️ Pipecat config present but enabled=false: {:?}", pipecat_config);
                }
                None => {
                    debug!("No Pipecat config provided for track: {}", track_id);
                }
            }

            // ✅ Only add internal processors if Pipecat is NOT handling AI
            if !pipecat_handles_ai {
                // Add VAD processor
                match option.vad {
                    Some(ref vad_option) => {
                        info!("🔧 Creating VAD processor");
                        let vad_processor = engine.create_vad_processor(
                            cancel_token.child_token(),
                            event_sender.clone(),
                            vad_option.to_owned(),
                        )?;
                        processors.push(vad_processor);
                    }
                    None => {}
                }

                // Add ASR processor
                match option.asr {
                    Some(ref asr_option) => {
                        info!("🔧 Creating ASR processor");
                        let asr_processor = engine
                            .create_asr_processor(
                                track_id.clone(),
                                cancel_token.child_token(),
                                asr_option.to_owned(),
                                event_sender.clone(),
                            )
                            .await?;
                        processors.push(asr_processor);
                    }
                    None => {}
                }

                // Add EOU processor
                match option.eou {
                    Some(ref eou_option) => {
                        info!("🔧 Creating EOU processor");
                        let eou_processor = engine.create_eou_processor(
                            cancel_token.child_token(),
                            event_sender.clone(),
                            eou_option.to_owned(),
                        )?;
                        processors.push(eou_processor);
                    }
                    None => {}
                }
            } else {
                info!("⏭️ Skipping all internal processors (VAD/ASR/EOU) - Pipecat handles everything");
            }

            Ok(processors)
        })
    }
}
