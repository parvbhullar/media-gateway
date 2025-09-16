use anyhow::Result;
use clap::Parser;
use dotenv::dotenv;
use futures::StreamExt;
use rustpbx::llm::LlmContent;
use rustpbx::media::codecs::bytes_to_samples;
use rustpbx::media::track::file::read_wav_file;
use rustpbx::synthesis::{SynthesisClient, SynthesisEvent, SynthesisType, SynthesisOption, DeepgramTtsClient};
use rustpbx::transcription::{TranscriptionClient, TranscriptionOption, TranscriptionType, DeepgramAsrClientBuilder};
use rustpbx::{PcmBuf, Sample};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::select;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use tracing_subscriber;
use rustpbx::{
    event::SessionEvent,
    llm::{LlmClient, OpenAiClientBuilder},
};

/// Demo application testing Deepgram ASR and TTS integration with RustPBX
/// 
/// Usage:
/// ```bash
/// # Test with WAV file input
/// DEEPGRAM_API_KEY=your_key cargo run --example deepgram_demo -- --input fixtures/hello_book_course_zh_16k.wav
/// 
/// # Test TTS only
/// DEEPGRAM_API_KEY=your_key cargo run --example deepgram_demo -- --text "Hello, this is a test"
/// ```
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Input WAV file for ASR testing
    #[arg(short, long)]
    input: Option<String>,

    /// Text for TTS testing
    #[arg(short, long)]
    text: Option<String>,

    /// Sample rate for processing
    #[arg(short = 'R', long, default_value = "16000")]
    sample_rate: u32,

    /// Deepgram model for ASR
    #[arg(long, default_value = "nova-2-general")]
    asr_model: String,

    /// Deepgram voice model for TTS  
    #[arg(long, default_value = "aura-asteria-en")]
    tts_model: String,

    /// Language for ASR
    #[arg(short, long, default_value = "en")]
    language: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    dotenv().ok();

    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let cancel_token = CancellationToken::new();

    // Check for Deepgram API key
    let api_key = match std::env::var("DEEPGRAM_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => {
            eprintln!("Error: DEEPGRAM_API_KEY environment variable not set or empty.");
            eprintln!("Please set it in .env file or in your environment.");
            return Err(anyhow::anyhow!("Missing DEEPGRAM_API_KEY"));
        }
    };
    info!("Found DEEPGRAM_API_KEY in environment");

    // Test TTS if text is provided
    if let Some(text) = &args.text {
        info!("Testing Deepgram TTS with text: {}", text);
        
        let synthesis_config = SynthesisOption {
            provider: Some(SynthesisType::Deepgram),
            secret_key: Some(api_key.clone()),
            speaker: Some(args.tts_model.clone()),
            samplerate: Some(args.sample_rate as i32),
            codec: Some("linear16".to_string()),
            ..Default::default()
        };

        let tts_client = DeepgramTtsClient::new(synthesis_config);
        
        let start_time = Instant::now();
        let mut audio_stream = tts_client
            .start(cancel_token.clone())
            .await
            .expect("Failed to start TTS stream");
        
        tts_client
            .synthesize(text, Some(true), None)
            .await
            .expect("Failed to synthesize text");

        let mut total_bytes = 0;
        while let Some(Ok(event)) = audio_stream.next().await {
            match event {
                SynthesisEvent::AudioChunk(chunk) => {
                    total_bytes += chunk.len();
                    info!("Received audio chunk: {} bytes", chunk.len());
                }
                SynthesisEvent::Finished { .. } => {
                    info!("TTS synthesis completed");
                    break;
                }
                _ => {}
            }
        }
        
        info!(
            "TTS synthesis completed in {}ms, total: {} bytes",
            start_time.elapsed().as_millis(),
            total_bytes
        );
    }

    // Test ASR if input file is provided
    if let Some(input_path) = &args.input {
        info!("Testing Deepgram ASR with file: {}", input_path);
        
        let transcription_config = TranscriptionOption {
            provider: Some(TranscriptionType::Deepgram),
            secret_key: Some(api_key.clone()),
            model_type: Some(args.asr_model.clone()),
            language: Some(args.language.clone()),
            samplerate: Some(args.sample_rate),
            start_when_answer: Some(false), // Don't wait for answer in demo
            ..Default::default()
        };

        let (event_sender, mut event_receiver) = tokio::sync::broadcast::channel(16);
        
        info!("Creating Deepgram ASR client...");
        let asr_client = DeepgramAsrClientBuilder::create(
            "test_track_id".to_string(),
            cancel_token.clone(),
            transcription_config,
            event_sender,
        ).await?;

        info!("ASR client created successfully, waiting for connection...");
        // Give the WebSocket time to connect
        sleep(Duration::from_millis(2000)).await;
        
        // Read and process the WAV file
        let (samples, file_sample_rate) = match read_wav_file(input_path) {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to read audio file '{}': {}", input_path, e);
                eprintln!("Error: Could not read audio file '{}'", input_path);
                eprintln!("Please make sure the file exists and is a valid WAV file.");
                eprintln!("");
                eprintln!("Available test files in the fixtures directory:");
                eprintln!("  fixtures/sample.wav");
                eprintln!("  fixtures/hello_book_course_zh_16k.wav");
                return Err(e);
            }
        };
        info!("Read {} samples at {} Hz", samples.len(), file_sample_rate);

        // Send audio data in chunks (simulate real-time streaming)
        let chunk_size = args.sample_rate as usize / 1000 * 100; // 100ms chunks
        let start_time = Instant::now();
        
        let asr_client_ref = Arc::new(asr_client);
        let asr_client_clone = asr_client_ref.clone();
        tokio::spawn(async move {
            info!("Starting to send audio data...");
            for (i, chunk) in samples.chunks(chunk_size).enumerate() {
                if let Err(e) = asr_client_clone.send_audio(chunk) {
                    error!("Failed to send audio chunk {}: {}", i, e);
                    break;
                }
                if i % 10 == 0 {
                    info!("Sent {} audio chunks", i + 1);
                }
                sleep(Duration::from_millis(100)).await; // Simulate real-time
            }
            info!("Finished sending audio data");
        });

        // Listen for transcription events
        let mut transcriptions = Vec::new();
        loop {
            select! {
                result = event_receiver.recv() => {
                    match result {
                        Ok(SessionEvent::AsrFinal { text, .. }) => {
                            info!("Final transcription: {}", text);
                            transcriptions.push(text);
                        }
                        Ok(SessionEvent::AsrDelta { text, .. }) => {
                            info!("Interim transcription: {}", text);
                        }
                        Ok(SessionEvent::Error { error, .. }) => {
                            error!("ASR Error: {}", error);
                            break;
                        }
                        Ok(_) => {} // Ignore other events
                        Err(e) => {
                            error!("Event receiver error: {}", e);
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    info!("ASR test timeout reached (60s)");
                    break;
                }
                _ = cancel_token.cancelled() => {
                    info!("ASR test cancelled");
                    break;
                }
            }
        }

        info!(
            "ASR test completed in {}ms, received {} final transcriptions",
            start_time.elapsed().as_millis(),
            transcriptions.len()
        );

        for (i, transcription) in transcriptions.iter().enumerate() {
            info!("Transcription {}: {}", i + 1, transcription);
        }
    }

    if args.input.is_none() && args.text.is_none() {
        eprintln!("Please provide either --input for ASR testing or --text for TTS testing");
        eprintln!("Use --help for more information");
        eprintln!("");
        eprintln!("Available test files:");
        eprintln!("  cargo run --example deepgram-demo -- --input fixtures/sample.wav");
        eprintln!("  cargo run --example deepgram-demo -- --input fixtures/hello_book_course_zh_16k.wav");
        eprintln!("");
        eprintln!("TTS example:");
        eprintln!("  cargo run --example deepgram-demo -- --text \"Hello world\"");
    }

    info!("Deepgram demo completed");
    Ok(())
}