import dotenv from 'dotenv';
import { WebSocketServer, WebSocket } from 'ws';

import {
    inference,
    stt as sttCore,
    tts as ttsCore,
    llm as llmCore,
    initializeLogger
} from '@livekit/agents';
import { fr } from 'zod/v4/locales';
import { AudioFrame } from '@livekit/rtc-node';

dotenv.config({ path: '.env.local' });

initializeLogger({ pretty: true, level: 'info' });

const PORT = Number(process.env.PORT ?? 8765);
const RUSTPBX_API_URL = 'http://localhost:8080'; // RustPBX API endpoint
const SAMPLE_RATE = 16_000;
const INPUT_SAMPLE_RATE = 16_000;   // change if your incoming audio is 44100/48000 etc.
const INPUT_BITS_PER_SAMPLE = 16;   // 16-bit PCM
const INPUT_NUM_CHANNELS: number = 1;
const BYTES_PER_SAMPLE = INPUT_BITS_PER_SAMPLE / 8;

console.log('[INIT] Starting PBX Agent Server');
console.log(`[INIT] Port: ${PORT}`);
console.log(`[INIT] Sample Rate: ${SAMPLE_RATE}`);

type Role = 'system' | 'user' | 'assistant';

interface ChatMessage {
    role: Role;
    content: string;
}

// Shared LLM + TTS for all calls
console.log('[INIT] Creating LLM instance');
const llm = new inference.LLM({
    model: 'openai/gpt-4.1-mini',
});
console.log('[INIT] LLM created successfully');

console.log('[INIT] Creating TTS instance');
const tts = new inference.TTS({
    model: 'cartesia/sonic-3',
    voice: '9626c31c-bec5-4cca-baa8-f8ba9e84c8bc',
});
console.log('[INIT] TTS created successfully');

class CallSession {
    private ws: WebSocket;
    private stt: inference.STT<'assemblyai/universal-streaming'>;
    private sttStream: sttCore.SpeechStream | null = null;
    private messages: ChatMessage[] = [];
    private closed = false;
    private sessionId: string;
    private audioFrameCount = 0;

    private silenceTimer: NodeJS.Timeout | null = null;
    private readonly SILENCE_MS = 1200;
    private lastNonSilentTime = 0;
    private readonly VAD_THRESHOLD = 500;

    constructor(ws: WebSocket) {
        this.sessionId = Math.random().toString(36).substring(7);
        console.log(`[SESSION-${this.sessionId}] Creating new CallSession`);

        this.ws = ws;

        console.log(`[SESSION-${this.sessionId}] Creating STT instance`);
        this.stt = new inference.STT({
            model: 'assemblyai/universal-streaming',
            language: 'en',
        });
        console.log(`[SESSION-${this.sessionId}] STT created successfully`);

        this.messages.push({
            role: 'system',
            content:
                'You are a helpful voice AI assistant on a phone call. Be concise, friendly, and avoid emojis.',
        });
        console.log(`[SESSION-${this.sessionId}] System message added to chat history`);
    }

    async start() {
        console.log(`[SESSION-${this.sessionId}] Starting CallSession`);

        console.log(`[SESSION-${this.sessionId}] Creating STT stream`);
        try {
            this.sttStream = this.stt.stream();
            console.log(`[SESSION-${this.sessionId}] STT stream created successfully`);
            console.log(`[SESSION-${this.sessionId}] STT stream type:`, typeof this.sttStream);
            console.log(`[SESSION-${this.sessionId}] STT stream methods:`, Object.getOwnPropertyNames(Object.getPrototypeOf(this.sttStream)));
        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error creating STT stream:`, error);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Starting STT event handler`);
        this.handleSttEvents().catch((err) => {
            console.error(`[SESSION-${this.sessionId}] STT loop error:`, err);
            this.close();
        });
        console.log(`[SESSION-${this.sessionId}] STT event handler started`);

        // Handle messages from RustPBX server
        let msgCount = 0;
        console.log(`[SESSION-${this.sessionId}] Setting up WebSocket message handlers`);
        this.ws.on('message', async (data, isBinary) => {
            msgCount++;
            //console.log(`[SESSION-${this.sessionId}] WS message received, isBinary: ${isBinary}, size: ${Buffer.isBuffer(data) ? data.length : 0} bytes`);
            if (this.closed) {
                console.log(`[SESSION-${this.sessionId}] Session closed, ignoring message`);
                return;
            }

            if (isBinary) {
                //console.log(`[SESSION-${this.sessionId}] Processing binary audio data`);

                const buffer = Buffer.isBuffer(data) ? data as Buffer : Buffer.from(data as any);

                this.handleIncomingAudio(buffer);
            }
            else {
                console.log(`[SESSION-${this.sessionId}] Processing text message`);
                // Text data could be control messages or metadata
                try {
                    const message = JSON.parse(data.toString());
                    console.log(`[SESSION-${this.sessionId}] Control message from RustPBX:`, message);
                } catch (err) {
                    console.error(`[SESSION-${this.sessionId}] Error parsing message from RustPBX:`, err);
                }
            }
        });

        this.ws.on('close', () => {
            console.log(`[SESSION-${this.sessionId}] RustPBX connection closed`);
            this.close();
        });

        this.ws.on('error', (err) => {
            console.error(`[SESSION-${this.sessionId}] RustPBX connection error:`, err);
            this.close();
        });

        console.log(`[SESSION-${this.sessionId}] CallSession started, waiting for audio from RustPBX...`);
    }

    private isSilent(samples: Int16Array): boolean {
        if (samples.length === 0) return true;

        let sum = 0;
        for (let i = 0; i < samples.length; i++) {
            sum += Math.abs(samples[i]!);
        }
        const avg = sum / samples.length;
        return avg < this.VAD_THRESHOLD;
    }


    private async streamAudioData(audioData: Buffer) {
        console.log(`[SESSION-${this.sessionId}] Starting to stream audio data`);

        const chunkSizeMs = 20; // 20ms chunks

        // 20 ms of audio at SAMPLE_RATE, INPUT_BITS_PER_SAMPLE, INPUT_NUM_CHANNELS
        const chunkSizeBytes = Math.floor(
            (SAMPLE_RATE * chunkSizeMs / 1000) *
            BYTES_PER_SAMPLE *
            INPUT_NUM_CHANNELS
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

            console.log(
                `[SESSION-${this.sessionId}] Processing chunk ${chunkCount}, offset: ${offset}, size: ${currentChunkSize} bytes`
            );

            await this.handleIncomingAudio(chunk);

            offset += currentChunkSize;

            // Simulate real-time streaming with delay
            await new Promise((resolve) => setTimeout(resolve, chunkSizeMs));
        }

        console.log(
            `[SESSION-${this.sessionId}] Finished streaming ${chunkCount} audio chunks`
        );

        // End the STT stream
        setTimeout(() => {
            if (this.sttStream) {
                console.log(`[SESSION-${this.sessionId}] Ending STT stream`);
                this.sttStream.endInput();
            }
        }, 1000);
    }


    private handleIncomingAudio(buffer: Buffer) {
        this.audioFrameCount++;

        if (this.audioFrameCount % 50 === 0) {
            //console.log(`[SESSION-${this.sessionId}] Processing audio chunk ${this.audioFrameCount}, size: ${buffer.length} bytes`);
        }
        //console.log(`[SESSION-${this.sessionId}] handleIncomingAudio called with ${buffer.length} bytes (frame #${this.audioFrameCount})`);

        if (!this.sttStream) {
            console.error(`[SESSION-${this.sessionId}] No STT stream available`);
            return;
        }

        // try {
        //     // Convert Buffer to Int16Array (assuming 16-bit PCM)
        //     const samples = new Int16Array(buffer.buffer, buffer.byteOffset, buffer.byteLength / 2);
        //     console.log(`[SESSION-${this.sessionId}] Converted buffer to ${samples.length} samples`);

        //     // Create proper AudioFrame
        //     const audioFrame = new AudioFrame(
        //         samples,
        //         SAMPLE_RATE,
        //         1, // channels
        //         samples.length // samples per channel
        //     );

        //     console.log(`[SESSION-${this.sessionId}] AudioFrame created:`, {
        //         sampleRate: audioFrame.sampleRate,
        //         numChannels: audioFrame.channels,
        //         samplesPerChannel: audioFrame.samplesPerChannel,
        //         duration: (audioFrame.samplesPerChannel / audioFrame.sampleRate * 1000).toFixed(2) + 'ms'
        //     });

        //     console.log(`[SESSION-${this.sessionId}] Pushing AudioFrame to STT stream`);
        //     this.sttStream.pushFrame(audioFrame);
        //     console.log(`[SESSION-${this.sessionId}] AudioFrame pushed successfully to STT stream`);

        //     // Log every 50 frames to see if we're getting continuous audio
        //     if (this.audioFrameCount % 50 === 0) {
        //         console.log(`[SESSION-${this.sessionId}] Processed ${this.audioFrameCount} audio frames so far`);
        //     }

        // } catch (error) {
        //     console.error(`[SESSION-${this.sessionId}] Error creating/pushing AudioFrame:`, error);
        //     if (error instanceof Error) {
        //         console.error(`[SESSION-${this.sessionId}] Error stack:`, error.stack);
        //     } else {
        //         console.error(`[SESSION-${this.sessionId}] Unknown error type:`, error);
        //     }
        // }

        if ((this.sttStream as any).input?.closed) {
            console.log(`[SESSION-${this.sessionId}] STT stream input is closed, skipping audio frame ${this.audioFrameCount}`);
            return;
        }

        try {
            // Assume 16-bit PCM, mono, 16kHz as default
            let samples: Int16Array;

            if (INPUT_BITS_PER_SAMPLE === 16) {
                // Ensure even number of bytes so Int16Array doesn't choke on odd length
                const trimmedByteLength = buffer.byteLength - (buffer.byteLength % 2);
                samples = new Int16Array(
                    buffer.buffer,
                    buffer.byteOffset,
                    trimmedByteLength / 2
                );
            } else if (INPUT_BITS_PER_SAMPLE === 8) {
                // 8-bit PCM -> 16-bit conversion (unsigned 8-bit to signed 16-bit)
                samples = new Int16Array(buffer.length);
                for (let i = 0; i < buffer.length; i++) {
                    samples[i] = (buffer[i]! - 128) * 256;
                }
            } else {
                console.error(
                    `[SESSION-${this.sessionId}] Unsupported bit depth for incoming audio: ${INPUT_BITS_PER_SAMPLE}`
                );
                return;
            }

            if (INPUT_NUM_CHANNELS === 2 as number) {
                const monoSamples = new Int16Array(samples.length / 2);
                for (let i = 0; i < monoSamples.length; i++) {
                    // average L/R
                    monoSamples[i] = Math.floor(
                        (samples[i * 2]! + samples[i * 2 + 1]!) / 2
                    );
                }
                samples = monoSamples;
            } else if (INPUT_NUM_CHANNELS !== 1) {
                console.error(
                    `[SESSION-${this.sessionId}] Unsupported channel count for incoming audio: ${INPUT_NUM_CHANNELS}`
                );
                return;
            }

            if (INPUT_SAMPLE_RATE !== SAMPLE_RATE) {
                samples = this.resample(samples, INPUT_SAMPLE_RATE, SAMPLE_RATE);
            }

            // Create AudioFrame
            const audioFrame = new AudioFrame(
                samples,
                SAMPLE_RATE,
                1, // mono after our processing
                samples.length
            );

            if (this.audioFrameCount % 50 === 0) {
                // console.log(`[SESSION-${this.sessionId}] AudioFrame created (incoming):`, {
                //     sampleRate: audioFrame.sampleRate,
                //     numChannels: audioFrame.channels,
                //     samplesPerChannel: audioFrame.samplesPerChannel,
                //     duration:
                //         (audioFrame.samplesPerChannel / audioFrame.sampleRate * 1000).toFixed(2) +
                //         'ms',
                // });
            }

            this.sttStream.pushFrame(audioFrame);
            // ... after you've built `samples` and pushed the AudioFrame:

            // ---------------- VAD + silence handling ----------------
            const silent = this.isSilent(samples);

            if (!silent) {
                // We heard real speech in this frame
                this.lastNonSilentTime = Date.now();

                // While user is speaking, we DON'T want the silence timer running
                if (this.silenceTimer) {
                    clearTimeout(this.silenceTimer);
                    this.silenceTimer = null;
                }
            } else {
                // This frame is (probably) silence
                // If we recently heard speech and no timer is running yet, start one
                if (!this.silenceTimer && this.lastNonSilentTime !== 0) {
                    this.silenceTimer = setTimeout(async () => {
                        console.log(
                            `[SESSION-${this.sessionId}] Silence timeout (${this.SILENCE_MS}ms) – ending current STT utterance`
                        );
                        if (this.sttStream) {
                            console.log(`[SESSION-${this.sessionId}] Ending current STT stream due to silence`);
                            this.sttStream.endInput(); // This will trigger FINAL_TRANSCRIPT in handleSttEvents
                            this.sttStream = null; // Clear the reference to prevent further use
                            
                            // Create a new STT stream for continued audio processing
                            console.log(`[SESSION-${this.sessionId}] Creating new STT stream after silence timeout`);
                            try {
                                this.sttStream = this.stt.stream();
                                // Start handling events for the new stream
                                this.handleSttEvents().catch((err) => {
                                    console.error(`[SESSION-${this.sessionId}] STT loop error (after silence):`, err);
                                    this.close();
                                });
                                console.log(`[SESSION-${this.sessionId}] New STT stream created and event handler started`);
                            } catch (error) {
                                console.error(`[SESSION-${this.sessionId}] Failed to create new STT stream after silence:`, error);
                            }
                        }
                        this.silenceTimer = null;
                        this.lastNonSilentTime = 0;
                    }, this.SILENCE_MS);
                }
            }


            // if (this.audioFrameCount % 50 === 0) {
            //     console.log(
            //         `[SESSION-${this.sessionId}] AudioFrame pushed to STT (incoming, total frames: ${this.audioFrameCount})`
            //     );
            // }

        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error processing audio chunk:`, error);
        }
    }

    private resample(input: Int16Array, inputRate: number, outputRate: number): Int16Array {
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

        console.log(`[SESSION-${this.sessionId}] Resampled from ${inputRate}Hz to ${outputRate}Hz: ${input.length} -> ${output.length} samples`);
        return output;
    }

    private async handleSttEvents() {
        console.log(`[SESSION-${this.sessionId}] handleSttEvents started`);

        if (!this.sttStream) {
            console.error(`[SESSION-${this.sessionId}] No STT stream in handleSttEvents`);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Starting STT event loop`);
        try {
            let eventCount = 0;
            for await (const ev of this.sttStream) {
                eventCount++;
                console.log(`[SESSION-${this.sessionId}] *** STT EVENT #${eventCount} RECEIVED ***`);
                console.log(`[SESSION-${this.sessionId}] STT event type:`, ev.type);
                console.log(`[SESSION-${this.sessionId}] Full STT event:`, JSON.stringify(ev, null, 2));

                if (ev.type === sttCore.SpeechEventType.FINAL_TRANSCRIPT) {
                    console.log(`[SESSION-${this.sessionId}] Final transcript event received`);
                    const text = ev.alternatives?.[0]?.text?.trim();
                    console.log(`[SESSION-${this.sessionId}] Extracted text:`, text);

                    if (!text) {
                        console.log(`[SESSION-${this.sessionId}] No text content, skipping`);
                        continue;
                    }

                    console.log(`[SESSION-${this.sessionId}] User said: "${text}"`);
                    this.messages.push({ role: 'user', content: text });
                    console.log(`[SESSION-${this.sessionId}] Added user message to chat history`);

                    console.log(`[SESSION-${this.sessionId}] Starting response generation`);
                    await this.respondToUser();
                } else if (ev.type === sttCore.SpeechEventType.INTERIM_TRANSCRIPT) {
                    console.log(`[SESSION-${this.sessionId}] Interim transcript:`, ev.alternatives?.[0]?.text);
                } else {
                    console.log(`[SESSION-${this.sessionId}] Other STT event type: ${ev.type}`);
                }
            }
        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] Error in STT event loop:`, error);
        }

        // console.log(`[SESSION-${this.sessionId}] STT event loop ended`);
        // if (!this.closed) {
        //     console.log(`[SESSION-${this.sessionId}] Creating new STT stream for next utterance`);
        //     try {
        //         this.sttStream = this.stt.stream();
        //         this.handleSttEvents().catch((err) => {
        //             console.error(`[SESSION-${this.sessionId}] STT loop error (next utterance):`, err);
        //             this.close();
        //         });
        //     } catch (err) {
        //         console.error(`[SESSION-${this.sessionId}] Failed to create STT stream for next utterance:`, err);
        //     }
        // }
    }

    private async testSttConnection() {
        console.log(`[SESSION-${this.sessionId}] Testing STT connection...`);
        try {
            // Try to push a small silence frame to test connectivity
            const silenceBuffer = Buffer.alloc(320); // 20ms of silence at 16kHz
            const samples = new Int16Array(silenceBuffer.buffer);
            const testFrame = new AudioFrame(samples, SAMPLE_RATE, 1, samples.length);

            console.log(`[SESSION-${this.sessionId}] Pushing test silence frame`);
            this.sttStream?.pushFrame(testFrame);
            console.log(`[SESSION-${this.sessionId}] Test frame pushed successfully`);
        } catch (error) {
            console.error(`[SESSION-${this.sessionId}] STT test failed:`, error);
        }
    }

    private async respondToUser() {
        console.log(`[SESSION-${this.sessionId}] respondToUser called`);

        if (this.closed) {
            console.log(`[SESSION-${this.sessionId}] Session closed, not responding`);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Calling LLM for response`);
        const replyText = await this.callLlm();
        console.log(`[SESSION-${this.sessionId}] LLM response: "${replyText}"`);

        this.messages.push({ role: 'assistant', content: replyText });
        console.log(`[SESSION-${this.sessionId}] Added assistant message to chat history`);

        console.log(`[SESSION-${this.sessionId}] Starting TTS streaming`);
        await this.streamTts(replyText);
        console.log(`[SESSION-${this.sessionId}] TTS streaming completed`);
    }

    private async callLlm(): Promise<string> {
        console.log(`[SESSION-${this.sessionId}] callLlm started`);

        const chatCtx = new llmCore.ChatContext();
        console.log(`[SESSION-${this.sessionId}] Created chat context`);

        for (const m of this.messages) {
            chatCtx.addMessage({
                role: m.role as llmCore.ChatRole,
                content: m.content,
            });
        }
        console.log(`[SESSION-${this.sessionId}] Added ${this.messages.length} messages to context`);

        console.log(`[SESSION-${this.sessionId}] Starting LLM chat stream`);
        const stream = await llm.chat({ chatCtx });
        console.log(`[SESSION-${this.sessionId}] LLM stream created successfully`);

        let fullText = '';
        let chunkCount = 0;

        console.log(`[SESSION-${this.sessionId}] Processing LLM stream chunks`);
        try {
            console.log(`[SESSION-${this.sessionId}] Processing LLM stream chunks`);

            for await (const chunk of stream as any) {
                chunkCount++;
                console.log(
                    `[SESSION-${this.sessionId}] LLM chunk #${chunkCount} received:`,
                    JSON.stringify(chunk, null, 2)
                );

                // LiveKit LLM stream format: { id, delta?: { content?: string, ... }, usage? }
                if (chunk.delta && typeof chunk.delta.content === 'string') {
                    fullText += chunk.delta.content;
                    console.log(
                        `[SESSION-${this.sessionId}] Added delta content: "${chunk.delta.content}"`
                    );
                }

                // (Optional) some providers might send `output_text` style events; keep this as a backup:
                if (chunk.output_text?.delta && typeof chunk.output_text.delta === 'string') {
                    fullText += chunk.output_text.delta;
                    console.log(
                        `[SESSION-${this.sessionId}] Added output_text.delta: "${chunk.output_text.delta}"`
                    );
                }

                console.log(
                    `[SESSION-${this.sessionId}] Current accumulated text length: ${fullText.length}`
                );
            }

            console.log(
                `[SESSION-${this.sessionId}] LLM streaming completed after ${chunkCount} chunks`
            );
        } catch (error) {
            console.error(
                `[SESSION-${this.sessionId}] Error processing LLM stream:`,
                error
            );
        }
        fullText = fullText.trim();
        console.log(
            `[SESSION-${this.sessionId}] Final accumulated LLM text: "${fullText}"`
        );

        if (!fullText) {
            console.log(
                `[SESSION-${this.sessionId}] No content from LLM, using fallback`
            );
            return 'Sorry, I had a problem generating a response.';
        }

        console.log(
            `[SESSION-${this.sessionId}] LLM generated ${fullText.length} characters`
        );
        return fullText;
    }

    private async streamTts(text: string) {
        console.log(`[SESSION-${this.sessionId}] streamTts started with text: "${text}"`);

        if (this.closed) {
            console.log(`[SESSION-${this.sessionId}] Session closed, not streaming TTS`);
            return;
        }

        console.log(`[SESSION-${this.sessionId}] Creating TTS stream`);
        const ttsStream = tts.stream();
        console.log(`[SESSION-${this.sessionId}] TTS stream created`);

        console.log(`[SESSION-${this.sessionId}] Pushing text to TTS`);
        ttsStream.pushText(text);
        ttsStream.endInput();
        console.log(`[SESSION-${this.sessionId}] Text pushed and input ended`);

        console.log(`[SESSION-${this.sessionId}] Processing TTS stream`);
        for await (const ev of ttsStream) {
            console.log(`[SESSION-${this.sessionId}] TTS event received`);

            if (ev === ttsCore.SynthesizeStream.END_OF_STREAM) {
                console.log(`[SESSION-${this.sessionId}] TTS end of stream`);
                break;
            }

            const audioEvent = ev as ttsCore.SynthesizedAudio;
            const frame = audioEvent.frame;
            if (!frame) {
                console.log(`[SESSION-${this.sessionId}] TTS event has no frame`);
                continue;
            }

            const pcm = frame.data as unknown as Buffer;
            console.log(`[SESSION-${this.sessionId}] TTS audio frame: ${pcm.length} bytes`);

            // Send TTS audio back to RustPBX server
            if (this.ws.readyState === WebSocket.OPEN) {
                console.log(`[SESSION-${this.sessionId}] Sending audio to RustPBX`);
                this.ws.send(pcm, { binary: true });
                console.log(`[SESSION-${this.sessionId}] Audio sent successfully`);
            } else {
                console.log(`[SESSION-${this.sessionId}] WebSocket not open, cannot send audio`);
            }
        }
        console.log(`[SESSION-${this.sessionId}] TTS streaming finished`);
    }

    close() {
        console.log(`[SESSION-${this.sessionId}] close() called`);

        if (this.closed) {
            console.log(`[SESSION-${this.sessionId}] Already closed`);
            return;
        }
        this.closed = true;

        console.log(`[SESSION-${this.sessionId}] Closing CallSession...`);

        if (this.sttStream) {
            console.log(`[SESSION-${this.sessionId}] Ending STT stream input`);
            this.sttStream.endInput();
            this.sttStream = null;
            console.log(`[SESSION-${this.sessionId}] STT stream cleaned up`);
        }

        if (this.ws.readyState === WebSocket.OPEN) {
            console.log(`[SESSION-${this.sessionId}] Closing WebSocket`);
            this.ws.close();
            console.log(`[SESSION-${this.sessionId}] WebSocket closed`);
        }

        console.log(`[SESSION-${this.sessionId}] CallSession closed successfully`);
    }
}

async function main() {
    console.log('[MAIN] Starting main function');

    console.log('[MAIN] Creating WebSocket server');
    const wss = new WebSocketServer({ port: PORT, host: '0.0.0.0' });
    console.log(`[MAIN] AI Agent server listening on ws://0.0.0.0:${PORT}`);
    console.log(`[MAIN] Waiting for connections from RustPBX server...`);

    wss.on('connection', (ws, request) => {
        console.log(`[MAIN] New connection from RustPBX: ${request.socket.remoteAddress}`);
        const session = new CallSession(ws);
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

    console.log('[MAIN] Main function setup completed');
}

console.log('[INIT] Starting application');
main().catch((err) => {
    console.error('[MAIN] Fatal error:', err);
    process.exit(1);
});
console.log('[INIT] Application started');
