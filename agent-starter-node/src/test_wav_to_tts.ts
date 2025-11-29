import dotenv from "dotenv";
import * as fs from "fs";
import * as path from "path";
import { fileURLToPath } from "url";
import { once } from "events";
import { WebSocketServer, WebSocket } from 'ws';

import {
    inference,
    stt as sttCore,
    tts as ttsCore,
    llm as llmCore,
    initializeLogger,
} from "@livekit/agents";
import { AudioFrame } from "@livekit/rtc-node";

dotenv.config({ path: ".env.local" });

initializeLogger({ pretty: true, level: "info" });

const RUSTPBX_API_URL = 'http://localhost:8080'; // Example URL, adjust as needed
const SAMPLE_RATE = 16_000;
const PORT = Number(process.env.PORT ?? 8081);

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

console.log("[INIT] Starting WAV Roundtrip (STT → LLM → TTS → Speakers)");
console.log(`[INIT] Sample Rate: ${SAMPLE_RATE}`);

type Role = "system" | "user" | "assistant";

interface ChatMessage {
    role: Role;
    content: string;
}

// Shared LLM + TTS
console.log("[INIT] Creating LLM instance");
const llm = new inference.LLM({
    model: "openai/gpt-4.1-mini",
});
console.log("[INIT] LLM created successfully");

console.log("[INIT] Creating TTS instance");
const tts = new inference.TTS({
    model: "cartesia/sonic-3",
    voice: "9626c31c-bec5-4cca-baa8-f8ba9e84c8bc",
});
console.log("[INIT] TTS created successfully");

// -------- WAV UTILITIES --------

function parseWavHeader(buffer: Buffer) {
    const riffHeader = buffer.toString("ascii", 0, 4);
    if (riffHeader !== "RIFF") {
        throw new Error("Not a valid WAV file - missing RIFF header");
    }

    const waveHeader = buffer.toString("ascii", 8, 12);
    if (waveHeader !== "WAVE") {
        throw new Error("Not a valid WAV file - missing WAVE header");
    }

    let offset = 12;
    while (offset < buffer.length - 8) {
        const chunkId = buffer.toString("ascii", offset, offset + 4);
        const chunkSize = buffer.readUInt32LE(offset + 4);

        if (chunkId === "fmt ") {
            const audioFormat = buffer.readUInt16LE(offset + 8);
            const numChannels = buffer.readUInt16LE(offset + 10);
            const sampleRate = buffer.readUInt32LE(offset + 12);
            const bitsPerSample = buffer.readUInt16LE(offset + 22);

            console.log("[WAV] WAV file info:", {
                audioFormat,
                numChannels,
                sampleRate,
                bitsPerSample,
            });

            return {
                audioFormat,
                numChannels,
                sampleRate,
                bitsPerSample,
                dataOffset: offset + 8 + chunkSize,
            };
        }

        offset += 8 + chunkSize;
    }

    throw new Error("WAV file missing fmt chunk");
}

function findDataChunk(buffer: Buffer, startOffset: number) {
    let offset = startOffset;
    while (offset < buffer.length - 8) {
        const chunkId = buffer.toString("ascii", offset, offset + 4);
        const chunkSize = buffer.readUInt32LE(offset + 4);

        if (chunkId === "data") {
            console.log(
                `[WAV] Found data chunk at offset ${offset}, size: ${chunkSize} bytes`
            );
            return {
                dataStart: offset + 8,
                dataSize: chunkSize,
            };
        }

        offset += 8 + chunkSize;
    }

    throw new Error("WAV file missing data chunk");
}

// -------- SESSION CLASS --------

class WavRoundtripSession {
    private stt: inference.STT<"assemblyai/universal-streaming">;
    private sttStream: sttCore.SpeechStream | null = null;
    private messages: ChatMessage[] = [];
    private closed = false;
    private sessionId: string;
    private audioFrameCount = 0;
    private ws: WebSocket;

    constructor(ws: WebSocket) {
        this.sessionId = Math.random().toString(36).substring(7);
        console.log(
            `[SESSION-${this.sessionId}] Creating new WavRoundtripSession`
        );

        this.ws = ws;

        console.log(`[SESSION-${this.sessionId}] Creating STT instance`);
        this.stt = new inference.STT({
            model: "assemblyai/universal-streaming",
            language: "en",
        });
        console.log(`[SESSION-${this.sessionId}] STT created successfully`);

        this.messages.push({
            role: "system",
            content:
                "You are a helpful voice AI assistant. Be concise, friendly, and avoid emojis.",
        });
        console.log(
            `[SESSION-${this.sessionId}] System message added to chat history`
        );
    }

    async start() {
        console.log(`[SESSION-${this.sessionId}] Starting session`);

        console.log(`[SESSION-${this.sessionId}] Creating STT stream`);
        try {
            this.sttStream = this.stt.stream();
            console.log(
                `[SESSION-${this.sessionId}] STT stream created successfully`
            );
        } catch (error) {
            console.error(
                `[SESSION-${this.sessionId}] Error creating STT stream:`,
                error
            );
            return;
        }

        console.log(
            `[SESSION-${this.sessionId}] Starting STT event handler (async loop)`
        );
        this.handleSttEvents().catch((err) => {
            console.error(`[SESSION-${this.sessionId}] STT loop error:`, err);
            this.close();
        });

        await this.processWavFile();
    }

    private async processWavFile() {
        console.log(
            `[SESSION-${this.sessionId}] Processing WAV file from sample_audio folder`
        );

        const sampleAudioDir = path.join(__dirname, "..", "sample_audio");
        console.log(
            `[SESSION-${this.sessionId}] Looking for WAV files in: ${sampleAudioDir}`
        );

        try {
            const files = fs.readdirSync(sampleAudioDir);
            const wavFiles = files.filter((file) =>
                file.toLowerCase().endsWith(".wav")
            );

            if (wavFiles.length === 0) {
                console.error(
                    `[SESSION-${this.sessionId}] No WAV files found in ${sampleAudioDir}`
                );
                return;
            }

            console.log(
                `[SESSION-${this.sessionId}] Found WAV files:`,
                wavFiles
            );

            const wavFile = wavFiles[0];
            const wavPath = path.join(sampleAudioDir, wavFile!);
            console.log(
                `[SESSION-${this.sessionId}] Using WAV file: ${wavPath}`
            );

            const wavBuffer = fs.readFileSync(wavPath);
            console.log(
                `[SESSION-${this.sessionId}] Read ${wavBuffer.length} bytes from WAV file`
            );

            const wavInfo = parseWavHeader(wavBuffer);
            console.log(`[SESSION-${this.sessionId}] WAV info:`, wavInfo);

            const dataChunk = findDataChunk(wavBuffer, wavInfo.dataOffset);
            console.log(
                `[SESSION-${this.sessionId}] Data chunk info:`,
                dataChunk
            );

            const audioData = wavBuffer.subarray(
                dataChunk.dataStart,
                dataChunk.dataStart + dataChunk.dataSize
            );
            console.log(
                `[SESSION-${this.sessionId}] Extracted ${audioData.length} bytes of audio data`
            );

            await this.streamAudioData(audioData, wavInfo);
        } catch (error) {
            console.error(
                `[SESSION-${this.sessionId}] Error processing WAV file:`,
                error
            );
        }
    }

    private async streamAudioData(audioData: Buffer, wavInfo: any) {
        console.log(`[SESSION-${this.sessionId}] Starting to stream audio data`);

        const chunkSizeMs = 20;
        const bytesPerSample = wavInfo.bitsPerSample / 8;
        const chunkSizeBytes = Math.floor(
            (wavInfo.sampleRate * chunkSizeMs) / 1000 * bytesPerSample * wavInfo.numChannels
        );

        console.log(
            `[SESSION-${this.sessionId}] Chunk size: ${chunkSizeBytes} bytes (${chunkSizeMs}ms)`
        );

        let offset = 0;
        let chunkCount = 0;

        while (offset < audioData.length && !this.closed) {
            const remainingBytes = audioData.length - offset;
            const currentChunkSize = Math.min(chunkSizeBytes, remainingBytes);

            const chunk = audioData.subarray(offset, offset + currentChunkSize);
            chunkCount++;

            if (chunkCount % 50 === 0) {
                console.log(
                    `[SESSION-${this.sessionId}] Processing chunk ${chunkCount}, offset: ${offset}, size: ${currentChunkSize} bytes`
                );
            }

            await this.handleAudioChunk(chunk, wavInfo);

            offset += currentChunkSize;
            await new Promise((resolve) => setTimeout(resolve, chunkSizeMs));
        }

        console.log(
            `[SESSION-${this.sessionId}] Finished streaming ${chunkCount} audio chunks`
        );

        setTimeout(() => {
            if (this.sttStream) {
                console.log(
                    `[SESSION-${this.sessionId}] Ending STT stream after WAV complete`
                );
                this.sttStream.endInput();
            }
        }, 1000);
    }

    private async handleAudioChunk(chunk: Buffer, wavInfo: any) {
        this.audioFrameCount++;

        if (!this.sttStream) {
            console.error(
                `[SESSION-${this.sessionId}] No STT stream available in handleAudioChunk`
            );
            return;
        }

        try {
            let samples: Int16Array;

            if (wavInfo.bitsPerSample === 16) {
                samples = new Int16Array(
                    chunk.buffer,
                    chunk.byteOffset,
                    chunk.byteLength / 2
                );
            } else if (wavInfo.bitsPerSample === 8) {
                samples = new Int16Array(chunk.length);
                for (let i = 0; i < chunk.length; i++) {
                    samples[i] = (chunk[i]! - 128) * 256;
                }
            } else {
                console.error(
                    `[SESSION-${this.sessionId}] Unsupported bit depth: ${wavInfo.bitsPerSample}`
                );
                return;
            }

            if (wavInfo.numChannels === 2) {
                const monoSamples = new Int16Array(samples.length / 2);
                for (let i = 0; i < monoSamples.length; i++) {
                    monoSamples[i] = Math.floor(
                        (samples[i * 2]! + samples[i * 2 + 1]!) / 2
                    );
                }
                samples = monoSamples;
            }

            if (wavInfo.sampleRate !== SAMPLE_RATE) {
                samples = this.resample(samples, wavInfo.sampleRate, SAMPLE_RATE);
            }

            const audioFrame = new AudioFrame(
                samples,
                SAMPLE_RATE,
                1,
                samples.length
            );

            if (this.audioFrameCount % 50 === 0) {
                console.log(
                    `[SESSION-${this.sessionId}] AudioFrame created:`,
                    {
                        sampleRate: audioFrame.sampleRate,
                        numChannels: audioFrame.channels,
                        samplesPerChannel: audioFrame.samplesPerChannel,
                        duration:
                            (audioFrame.samplesPerChannel / audioFrame.sampleRate) * 1000 +
                            "ms",
                    }
                );
            }

            this.sttStream.pushFrame(audioFrame);
        } catch (error) {
            console.error(
                `[SESSION-${this.sessionId}] Error processing audio chunk:`,
                error
            );
        }
    }

    private resample(
        input: Int16Array,
        inputRate: number,
        outputRate: number
    ): Int16Array {
        if (inputRate === outputRate) {
            return input;
        }

        const ratio = inputRate / outputRate;
        const outputLength = Math.floor(input.length / ratio);
        const output = new Int16Array(outputLength);

        for (let i = 0; i < outputLength; i++) {
            const srcIndex = i * ratio;
            const index = Math.floor(srcIndex);
            if (index < input.length) {
                output[i] = input[index] ?? 0;
            }
        }

        console.log(
            `[SESSION-${this.sessionId}] Resampled from ${inputRate}Hz to ${outputRate}Hz: ${input.length} -> ${output.length} samples`
        );
        return output;
    }

    private async handleSttEvents() {
        console.log(`[SESSION-${this.sessionId}] handleSttEvents started`);

        if (!this.sttStream) {
            console.error(
                `[SESSION-${this.sessionId}] No STT stream in handleSttEvents`
            );
            return;
        }

        try {
            let eventCount = 0;
            for await (const ev of this.sttStream) {
                eventCount++;
                console.log(
                    `[SESSION-${this.sessionId}] *** STT EVENT #${eventCount} RECEIVED ***`
                );
                console.log(
                    `[SESSION-${this.sessionId}] STT event type:`,
                    ev.type
                );

                if (ev.type === sttCore.SpeechEventType.FINAL_TRANSCRIPT) {
                    const text = ev.alternatives?.[0]?.text?.trim();
                    console.log(
                        `[SESSION-${this.sessionId}] Final transcript: "${text}"`
                    );

                    if (!text) continue;

                    this.messages.push({ role: "user", content: text });

                    // Once we get a final transcript, do LLM + TTS.
                    await this.respondToUser();
                    // You can break if you only want first utterance:
                    // break;
                } else if (
                    ev.type === sttCore.SpeechEventType.INTERIM_TRANSCRIPT
                ) {
                    console.log(
                        `[SESSION-${this.sessionId}] Interim transcript:`,
                        ev.alternatives?.[0]?.text
                    );
                } else {
                    console.log(
                        `[SESSION-${this.sessionId}] Other STT event type: ${ev.type}`
                    );
                }
            }
        } catch (error) {
            console.error(
                `[SESSION-${this.sessionId}] Error in STT event loop:`,
                error
            );
        }

        console.log(`[SESSION-${this.sessionId}] STT event loop ended`);
    }

    private async callLlm(): Promise<string> {
        console.log(`[SESSION-${this.sessionId}] callLlm started`);

        const chatCtx = new llmCore.ChatContext();
        for (const m of this.messages) {
            chatCtx.addMessage({
                role: m.role as llmCore.ChatRole,
                content: m.content,
            });
        }

        const stream = await llm.chat({ chatCtx });
        let fullText = "";
        let chunkCount = 0;

        for await (const chunk of stream as any) {
            chunkCount++;
            console.log(
                `[SESSION-${this.sessionId}] LLM chunk #${chunkCount} received`
            );

            if (chunk.delta && typeof chunk.delta.content === "string") {
                fullText += chunk.delta.content;
            }

            if (
                chunk.output_text?.delta &&
                typeof chunk.output_text.delta === "string"
            ) {
                fullText += chunk.output_text.delta;
            }
        }

        fullText = fullText.trim();
        if (!fullText) {
            console.log(
                `[SESSION-${this.sessionId}] No content from LLM, using fallback`
            );
            return "Sorry, I had a problem generating a response.";
        }

        console.log(
            `[SESSION-${this.sessionId}] LLM final text: "${fullText}"`
        );
        return fullText;
    }

    private async respondToUser() {
        console.log(`[SESSION-${this.sessionId}] respondToUser called`);

        if (this.closed) {
            console.log(
                `[SESSION-${this.sessionId}] Session closed, not responding`
            );
            return;
        }

        const replyText = await this.callLlm();
        this.messages.push({ role: "assistant", content: replyText });

        await this.streamTtsToSpeakers(replyText);
    }

    // 🔊 TTS → Speakers (streaming)
    private async streamTtsToSpeakers(text: string) {
        console.log(
            `[SESSION-${this.sessionId}] streamTtsToSpeakers with text: "${text}"`
        );

        if (this.closed) {
            console.log(
                `[SESSION-${this.sessionId}] Session closed, not streaming TTS`
            );
            return;
        }

        const ttsStream = tts.stream();
        ttsStream.pushText(text);
        ttsStream.endInput();


        for await (const ev of ttsStream) {
            if (ev === ttsCore.SynthesizeStream.END_OF_STREAM) {
                console.log(
                    `[SESSION-${this.sessionId}] TTS END_OF_STREAM received`
                );
                break;
            }

            const audioEvent = ev as ttsCore.SynthesizedAudio;
            const frame = audioEvent.frame;
            if (!frame) {
                continue;
            }

            const pcm = frame.data as unknown as Buffer;
            this.ws.send(pcm, { binary: true });
        }
        console.log(`[SESSION-${this.sessionId}] Finished playing TTS audio`);
    }

    close() {
        console.log(`[SESSION-${this.sessionId}] close() called`);
        if (this.closed) return;

        this.closed = true;

        if (this.sttStream) {
            this.sttStream.endInput();
            this.sttStream = null;
        }

        console.log(
            `[SESSION-${this.sessionId}] WavRoundtripSession closed successfully`
        );
    }
}

// -------- MAIN --------

async function main() {
    console.log("[MAIN] Starting WAV roundtrip test");

    const sampleAudioDir = path.join(__dirname, "..", "sample_audio");
    if (!fs.existsSync(sampleAudioDir)) {
        console.log("[MAIN] Creating sample_audio directory");
        fs.mkdirSync(sampleAudioDir, { recursive: true });
        console.log(
            `[MAIN] Please place your WAV files in: ${sampleAudioDir}`
        );
        return;
    }

    const wss = new WebSocketServer({ port: PORT, host: '0.0.0.0' });
    wss.on('connection', (ws, request) => {
        console.log(`[MAIN] New connection from RustPBX: ${request.socket.remoteAddress}`);
        const session = new WavRoundtripSession(ws);
        session.start().catch((err) => {
            console.error('[MAIN] CallSession error:', err);
            session.close();
        });
    });

    wss.on('error', (err) => {
        console.error('[MAIN] WebSocket server error:', err);
    });

    // Graceful shutdown
    process.on('SIGINT', () => {
        console.log('[MAIN] SIGINT received, shutting down server...');
        wss.close(() => {
            console.log('[MAIN] Server closed');
            process.exit(0);
        });
    });
}

console.log("[INIT] Starting WAV roundtrip application");
main().catch((err) => {
    console.error("[MAIN] Fatal error:", err);
    process.exit(1);
});
console.log("[INIT] Application started");
