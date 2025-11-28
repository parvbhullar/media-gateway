/**
 * WebRTC audio streaming for testing Pipecat integration with RustPBX
 */

class WebRTCManager {
    constructor() {
        this.localStream = null;
        this.peerConnection = null;
        this.isStreaming = false;
        this.audioContext = null;
        this.analyser = null;
        this.microphone = null;

        // RustPBX WebSocket connection for signaling
        this.rustpbxWs = null;
        this.rustpbxConnected = false;

        this.setupEventListeners();
    }
    

    setupEventListeners() {
        console.log("🔥 webrtc.js loaded!");
        // Audio control buttons
        document.getElementById('start-audio')?.addEventListener('click', () => {
            this.startAudioStream();
        });

        document.getElementById('stop-audio')?.addEventListener('click', () => {
            this.stopAudioStream();
        });

        // RustPBX connection buttons
        document.getElementById('test-rustpbx')?.addEventListener('click', () => {
            this.testRustPBXConnection();
        });

        document.getElementById('connect-rustpbx')?.addEventListener('click', () => {
            this.connectToRustPBX();
        });
    }

    async startAudioStream() {
        try {
            this.updateAudioStatus('Microphone access granted');

            // 🔥 Force-disable Chrome’s hidden audio processing
            const audioTrack = this.localStream.getAudioTracks()[0];

            await audioTrack.applyConstraints({
                advanced: [
                    { echoCancellation: false },
                    { noiseSuppression: false },
                    { autoGainControl: false },
                    { googEchoCancellation: false },
                    { googNoiseSuppression: false },
                    { googAutoGainControl: false },
                    { googHighpassFilter: false },
                    { googTypingNoiseDetection: false },
                    { googNoiseReduction: false },
                    { googDucking: false }
                ]
            });

            // Debug browser muting events
            audioTrack.onmute = () => console.warn("⚠️ WebRTC TRACK MUTED BY BROWSER");
            audioTrack.onunmute = () => console.warn("⚠️ WebRTC TRACK UNMUTED");

            // Get user media (audio only for now)
            this.localStream = await navigator.mediaDevices.getUserMedia({
                audio: {
                    sampleRate: 16000,
                    channelCount: 1,
                    echoCancellation: false,
                    noiseSuppression: false,
                    autoGainControl: false,

                    // 🔥 MUST HAVE (Chrome proprietary)
                    googEchoCancellation: false,
                    googNoiseSuppression: false,
                    googAutoGainControl: false,
                    googHighpassFilter: false,
                    googTypingNoiseDetection: false,
                    googNoiseReduction: false,
                },
                video: false
            });

            this.updateAudioStatus('Microphone access granted');
            wsManager.addLogEntry('SUCCESS', 'Microphone access granted');

            // Setup audio analysis
            await this.setupAudioAnalysis();

            // Setup WebRTC peer connection
            await this.setupPeerConnection();

            // Update UI
            this.isStreaming = true;
            this.updateAudioControls();
            this.updateAudioStatus('Audio stream active');

            wsManager.addLogEntry('INFO', 'Audio streaming started');

        } catch (error) {
            console.error('❌ Failed to start audio stream:', error);
            this.updateAudioStatus(`Error: ${error.message}`);
            wsManager.addLogEntry('ERROR', `Audio stream failed: ${error.message}`);
        }
    }

    async setupAudioAnalysis() {
        try {
            // Create audio context for analysis
            this.audioContext = new (window.AudioContext || window.webkitAudioContext)();
            this.analyser = this.audioContext.createAnalyser();
            this.microphone = this.audioContext.createMediaStreamSource(this.localStream);

            this.analyser.fftSize = 256;
            this.analyser.smoothingTimeConstant = 0.8;

            this.microphone.connect(this.analyser);

            // Start volume monitoring
            this.monitorAudioLevel();

            wsManager.addLogEntry('INFO', 'Audio analysis setup complete');
        } catch (error) {
            console.error('❌ Audio analysis setup failed:', error);
            wsManager.addLogEntry('ERROR', `Audio analysis failed: ${error.message}`);
        }
    }

    monitorAudioLevel() {
        if (!this.analyser || !this.isStreaming) return;

        const bufferLength = this.analyser.frequencyBinCount;
        const dataArray = new Uint8Array(bufferLength);

        const checkLevel = () => {
            if (!this.isStreaming) return;

            this.analyser.getByteFrequencyData(dataArray);

            // Calculate average volume
            let sum = 0;
            for (let i = 0; i < bufferLength; i++) {
                sum += dataArray[i];
            }
            const average = sum / bufferLength;
            const volumeLevel = average / 255; // Normalize to 0-1

            // Update volume visualization
            wsManager.updateVolumeLevel(volumeLevel);

            // Send audio data to visualizer
            if (window.audioVisualizer) {
                window.audioVisualizer.updateFrequencyData(dataArray);
            }

            // Continue monitoring
            requestAnimationFrame(checkLevel);
        };

        checkLevel();
    }

    async setupPeerConnection() {
        try {
            // Create peer connection with STUN server
            const configuration = {
                iceServers: [
                    { urls: 'stun:stun.l.google.com:19302' }
                ]
            };

            this.peerConnection = new RTCPeerConnection(configuration);

            // Add local stream to peer connection
            this.localStream.getTracks().forEach(track => {
                this.peerConnection.addTrack(track, this.localStream);
            });

            // Handle remote stream
            this.peerConnection.ontrack = (event) => {
                const remoteAudio = document.getElementById('remote-audio');
                if (remoteAudio) {
                    remoteAudio.srcObject = event.streams[0];
                    wsManager.addLogEntry('SUCCESS', 'Remote audio stream received');
                }
            };

            // Handle ICE candidates
            this.peerConnection.onicecandidate = (event) => {
                if (event.candidate && this.rustpbxWs) {
                    this.sendToRustPBX({
                        type: 'ice-candidate',
                        candidate: event.candidate
                    });
                }
            };

            // Handle connection state changes
            this.peerConnection.onconnectionstatechange = () => {
                const state = this.peerConnection.connectionState;
                wsManager.addLogEntry('INFO', `WebRTC connection state: ${state}`);

                if (state === 'connected') {
                    wsManager.addLogEntry('SUCCESS', 'WebRTC connection established');
                } else if (state === 'failed') {
                    wsManager.addLogEntry('ERROR', 'WebRTC connection failed');
                }
            };

            wsManager.addLogEntry('INFO', 'WebRTC peer connection configured');

        } catch (error) {
            console.error('❌ WebRTC setup failed:', error);
            wsManager.addLogEntry('ERROR', `WebRTC setup failed: ${error.message}`);
        }
    }

    async stopAudioStream() {
        try {
            this.isStreaming = false;

            // Stop local stream
            if (this.localStream) {
                this.localStream.getTracks().forEach(track => {
                    track.stop();
                });
                this.localStream = null;
            }            

            // Close peer connection
            if (this.peerConnection) {
                this.peerConnection.close();
                this.peerConnection = null;
            }

            // Clean up audio analysis
            if (this.audioContext) {
                this.audioContext.close();
                this.audioContext = null;
            }

            // Update UI
            this.updateAudioControls();
            this.updateAudioStatus('Audio stream stopped');

            // Reset volume level
            wsManager.updateVolumeLevel(0);

            wsManager.addLogEntry('INFO', 'Audio streaming stopped');

        } catch (error) {
            console.error('❌ Failed to stop audio stream:', error);
            wsManager.addLogEntry('ERROR', `Stop audio failed: ${error.message}`);
        }
    }

    async testRustPBXConnection() {
        try {
            this.updateRustPBXStatus('Testing connection...');
            wsManager.addLogEntry('INFO', 'Testing RustPBX connection');

            // Test HTTP endpoint first
            const response = await fetch('http://localhost:8080/health');
            if (response.ok) {
                this.updateRustPBXStatus('HTTP connection OK');
                wsManager.addLogEntry('SUCCESS', 'RustPBX HTTP endpoint accessible');
            } else {
                throw new Error(`HTTP ${response.status}: ${response.statusText}`);
            }

        } catch (error) {
            console.error('❌ RustPBX connection test failed:', error);
            this.updateRustPBXStatus(`Connection failed: ${error.message}`);
            wsManager.addLogEntry('ERROR', `RustPBX test failed: ${error.message}`);
        }
    }

    connectToRustPBX() {
        if (this.rustpbxConnected) {
            this.disconnectFromRustPBX();
            return;
        }

        try {
            this.updateRustPBXStatus('Connecting to RustPBX...');
            wsManager.addLogEntry('INFO', 'Connecting to RustPBX WebSocket');

            // Connect to RustPBX WebSocket for signaling
            const rustpbxUrl = 'ws://localhost:8080/ws/sip';
            this.rustpbxWs = new WebSocket(rustpbxUrl, ['sip']);

            this.rustpbxWs.onopen = () => {
                this.rustpbxConnected = true;
                this.updateRustPBXStatus('Connected to RustPBX');
                this.updateRustPBXControls();
                wsManager.addLogEntry('SUCCESS', 'Connected to RustPBX WebSocket');
            };

            this.rustpbxWs.onmessage = (event) => {
                this.handleRustPBXMessage(event.data);
            };

            this.rustpbxWs.onclose = () => {
                this.rustpbxConnected = false;
                this.updateRustPBXStatus('Disconnected from RustPBX');
                this.updateRustPBXControls();
                wsManager.addLogEntry('INFO', 'Disconnected from RustPBX');
            };

            this.rustpbxWs.onerror = (error) => {
                console.error('❌ RustPBX WebSocket error:', error);
                this.updateRustPBXStatus('Connection error');
                wsManager.addLogEntry('ERROR', 'RustPBX WebSocket error');
            };

        } catch (error) {
            console.error('❌ Failed to connect to RustPBX:', error);
            this.updateRustPBXStatus(`Connection failed: ${error.message}`);
            wsManager.addLogEntry('ERROR', `RustPBX connection failed: ${error.message}`);
        }
    }

    disconnectFromRustPBX() {
        if (this.rustpbxWs) {
            this.rustpbxWs.close();
            this.rustpbxWs = null;
        }
        this.rustpbxConnected = false;
        this.updateRustPBXStatus('Not connected');
        this.updateRustPBXControls();
    }

    handleRustPBXMessage(data) {
        try {
            const message = JSON.parse(data);
            wsManager.addLogEntry('INFO', `RustPBX message: ${message.type || 'unknown'}`);

            // Handle different message types from RustPBX
            switch (message.type) {
                case 'offer':
                    this.handleOffer(message);
                    break;
                case 'answer':
                    this.handleAnswer(message);
                    break;
                case 'ice-candidate':
                    this.handleIceCandidate(message);
                    break;
                default:
                    console.log('📨 RustPBX message:', message);
            }
        } catch (error) {
            console.error('❌ Failed to parse RustPBX message:', error);
        }
    }

    async handleOffer(message) {
        if (!this.peerConnection) return;

        try {
            await this.peerConnection.setRemoteDescription(message.offer);
            const answer = await this.peerConnection.createAnswer();
            answer.sdp = answer.sdp
                .replace("usedtx=1", "usedtx=0")
                .replace("useinbandfec=1", "useinbandfec=0");
            await this.peerConnection.setLocalDescription(answer);

            this.sendToRustPBX({
                type: 'answer',
                answer: answer
            });

            wsManager.addLogEntry('SUCCESS', 'WebRTC offer handled, answer sent');
        } catch (error) {
            console.error('❌ Failed to handle offer:', error);
            wsManager.addLogEntry('ERROR', `Offer handling failed: ${error.message}`);
        }
    }

    async handleAnswer(message) {
        if (!this.peerConnection) return;

        try {
            await this.peerConnection.setRemoteDescription(message.answer);
            wsManager.addLogEntry('SUCCESS', 'WebRTC answer processed');
        } catch (error) {
            console.error('❌ Failed to handle answer:', error);
            wsManager.addLogEntry('ERROR', `Answer handling failed: ${error.message}`);
        }
    }

    async handleIceCandidate(message) {
        if (!this.peerConnection) return;

        try {
            await this.peerConnection.addIceCandidate(message.candidate);
            wsManager.addLogEntry('INFO', 'ICE candidate added');
        } catch (error) {
            console.error('❌ Failed to add ICE candidate:', error);
        }
    }

    sendToRustPBX(message) {
        if (this.rustpbxWs && this.rustpbxWs.readyState === WebSocket.OPEN) {
            console.log('📤 Sending to RustPBX:', message);
            this.rustpbxWs.send(JSON.stringify(message));
            return true;
        }
        return false;
    }

    updateAudioStatus(status) {
        const statusElement = document.getElementById('audio-status');
        if (statusElement) {
            statusElement.textContent = status;
        }
    }

    updateRustPBXStatus(status) {
        const statusElement = document.getElementById('rustpbx-status');
        if (statusElement) {
            statusElement.textContent = status;
        }
    }

    updateAudioControls() {
        const startBtn = document.getElementById('start-audio');
        const stopBtn = document.getElementById('stop-audio');

        if (startBtn && stopBtn) {
            startBtn.disabled = this.isStreaming;
            stopBtn.disabled = !this.isStreaming;
        }
    }

    updateRustPBXControls() {
        const connectBtn = document.getElementById('connect-rustpbx');
        if (connectBtn) {
            connectBtn.innerHTML = this.rustpbxConnected ?
                '<i class="fas fa-unlink"></i> Disconnect' :
                '<i class="fas fa-plug"></i> Connect to RustPBX';
        }
    }

    // Create WebRTC offer for outbound calls
    async createOffer() {
        if (!this.peerConnection) {
            throw new Error('Peer connection not initialized');
        }

        try {
            const offer = await this.peerConnection.createOffer();
            offer.sdp = offer.sdp
                .replace("usedtx=1", "usedtx=0")
                .replace("useinbandfec=1", "useinbandfec=0");
            await this.peerConnection.setLocalDescription(offer);

            this.sendToRustPBX({
                type: 'offer',
                offer: offer
            });

            wsManager.addLogEntry('SUCCESS', 'WebRTC offer created and sent');
            return offer;
        } catch (error) {
            console.error('❌ Failed to create offer:', error);
            wsManager.addLogEntry('ERROR', `Offer creation failed: ${error.message}`);
            throw error;
        }
    }
}

// Create global WebRTC manager instance
window.webrtcManager = new WebRTCManager();