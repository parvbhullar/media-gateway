/**
 * Audio visualization for real-time waveform display
 */

class AudioVisualizer {
    constructor(canvasId) {
        this.canvas = document.getElementById(canvasId);
        this.ctx = this.canvas.getContext('2d');
        this.width = this.canvas.width;
        this.height = this.canvas.height;
        
        this.frequencyData = null;
        this.waveformData = [];
        this.isAnimating = false;
        
        this.setupCanvas();
        this.startAnimation();
    }

    setupCanvas() {
        // Set up canvas styling
        this.ctx.fillStyle = '#34495e';
        this.ctx.strokeStyle = '#3498db';
        this.ctx.lineWidth = 2;
        
        // Initial clear
        this.clear();
        this.drawPlaceholder();
    }

    clear() {
        this.ctx.fillStyle = '#34495e';
        this.ctx.fillRect(0, 0, this.width, this.height);
    }

    drawPlaceholder() {
        this.ctx.fillStyle = '#7f8c8d';
        this.ctx.font = '16px Arial';
        this.ctx.textAlign = 'center';
        this.ctx.fillText('Audio Visualizer - Waiting for audio input...', this.width / 2, this.height / 2);
    }

    updateFrequencyData(data) {
        this.frequencyData = new Uint8Array(data);
    }

    updateWaveform(waveformData) {
        this.waveformData = waveformData;
    }

    startAnimation() {
        if (this.isAnimating) return;
        
        this.isAnimating = true;
        this.animate();
    }

    stopAnimation() {
        this.isAnimating = false;
    }

    animate() {
        if (!this.isAnimating) return;
        
        this.clear();
        
        if (this.frequencyData) {
            this.drawFrequencyBars();
        } else if (this.waveformData.length > 0) {
            this.drawWaveform();
        } else {
            this.drawPlaceholder();
        }
        
        requestAnimationFrame(() => this.animate());
    }

    drawFrequencyBars() {
        const bufferLength = this.frequencyData.length;
        const barWidth = this.width / bufferLength;
        
        for (let i = 0; i < bufferLength; i++) {
            const barHeight = (this.frequencyData[i] / 255) * this.height;
            
            // Create gradient based on frequency
            const gradient = this.ctx.createLinearGradient(0, this.height, 0, this.height - barHeight);
            if (barHeight < this.height * 0.3) {
                gradient.addColorStop(0, '#2ecc71');
                gradient.addColorStop(1, '#27ae60');
            } else if (barHeight < this.height * 0.7) {
                gradient.addColorStop(0, '#f39c12');
                gradient.addColorStop(1, '#e67e22');
            } else {
                gradient.addColorStop(0, '#e74c3c');
                gradient.addColorStop(1, '#c0392b');
            }
            
            this.ctx.fillStyle = gradient;
            this.ctx.fillRect(i * barWidth, this.height - barHeight, barWidth - 1, barHeight);
        }
        
        // Draw peak indicator
        this.drawPeakIndicator();
        
        // Draw frequency labels
        this.drawFrequencyLabels();
    }

    drawWaveform() {
        if (this.waveformData.length === 0) return;
        
        this.ctx.strokeStyle = '#3498db';
        this.ctx.lineWidth = 2;
        this.ctx.beginPath();
        
        const sliceWidth = this.width / this.waveformData.length;
        let x = 0;
        
        for (let i = 0; i < this.waveformData.length; i++) {
            const v = this.waveformData[i] / 128.0;
            const y = (v * this.height) / 2;
            
            if (i === 0) {
                this.ctx.moveTo(x, y);
            } else {
                this.ctx.lineTo(x, y);
            }
            
            x += sliceWidth;
        }
        
        this.ctx.lineTo(this.width, this.height / 2);
        this.ctx.stroke();
        
        // Draw center line
        this.drawCenterLine();
    }

    drawPeakIndicator() {
        if (!this.frequencyData) return;
        
        // Find peak frequency
        let maxValue = 0;
        let peakIndex = 0;
        
        for (let i = 0; i < this.frequencyData.length; i++) {
            if (this.frequencyData[i] > maxValue) {
                maxValue = this.frequencyData[i];
                peakIndex = i;
            }
        }
        
        if (maxValue > 50) { // Only show if significant
            const x = (peakIndex / this.frequencyData.length) * this.width;
            const y = this.height - (maxValue / 255) * this.height;
            
            // Draw peak marker
            this.ctx.fillStyle = '#e74c3c';
            this.ctx.beginPath();
            this.ctx.arc(x, y, 4, 0, 2 * Math.PI);
            this.ctx.fill();
            
            // Draw peak line
            this.ctx.strokeStyle = '#e74c3c';
            this.ctx.lineWidth = 1;
            this.ctx.setLineDash([2, 2]);
            this.ctx.beginPath();
            this.ctx.moveTo(x, y);
            this.ctx.lineTo(x, this.height);
            this.ctx.stroke();
            this.ctx.setLineDash([]);
        }
    }

    drawFrequencyLabels() {
        this.ctx.fillStyle = '#bdc3c7';
        this.ctx.font = '10px Arial';
        this.ctx.textAlign = 'center';
        
        // Draw frequency markers (approximate)
        const frequencies = ['0Hz', '1kHz', '2kHz', '4kHz', '8kHz'];
        const positions = [0, 0.25, 0.5, 0.75, 1.0];
        
        for (let i = 0; i < frequencies.length; i++) {
            const x = positions[i] * this.width;
            this.ctx.fillText(frequencies[i], x, this.height - 5);
        }
    }

    drawCenterLine() {
        this.ctx.strokeStyle = '#7f8c8d';
        this.ctx.lineWidth = 1;
        this.ctx.setLineDash([5, 5]);
        this.ctx.beginPath();
        this.ctx.moveTo(0, this.height / 2);
        this.ctx.lineTo(this.width, this.height / 2);
        this.ctx.stroke();
        this.ctx.setLineDash([]);
    }

    drawSpectrogram() {
        // Advanced: Draw spectrogram view
        // This would show frequency content over time
        // Implementation can be added for more detailed analysis
    }

    // Method to process audio data from Pipecat server
    processAudioData(audioData) {
        try {
            // Decode base64 audio data if needed
            let processedData;
            
            if (typeof audioData === 'string') {
                // Base64 encoded audio
                const binaryString = atob(audioData);
                const bytes = new Uint8Array(binaryString.length);
                for (let i = 0; i < binaryString.length; i++) {
                    bytes[i] = binaryString.charCodeAt(i);
                }
                processedData = bytes;
            } else {
                processedData = audioData;
            }
            
            // Convert to frequency domain for visualization
            this.updateFrequencyData(processedData);
            
        } catch (error) {
            console.error('❌ Failed to process audio data:', error);
        }
    }

    // Reset visualization
    reset() {
        this.frequencyData = null;
        this.waveformData = [];
        this.clear();
        this.drawPlaceholder();
    }

    // Set visualization mode
    setMode(mode) {
        this.mode = mode; // 'frequency', 'waveform', 'spectrogram'
    }

    // Get current audio levels for external use
    getCurrentLevels() {
        if (!this.frequencyData) return { average: 0, peak: 0 };
        
        let sum = 0;
        let peak = 0;
        
        for (let i = 0; i < this.frequencyData.length; i++) {
            const value = this.frequencyData[i];
            sum += value;
            if (value > peak) peak = value;
        }
        
        return {
            average: sum / this.frequencyData.length / 255,
            peak: peak / 255
        };
    }
}

// Initialize audio visualizer when DOM is loaded
document.addEventListener('DOMContentLoaded', () => {
    window.audioVisualizer = new AudioVisualizer('audio-visualizer');
    
    // Connect to WebSocket manager for audio updates
    if (window.wsManager) {
        window.wsManager.onMessage('audio_visualization', (message) => {
            if (message.frequency_data) {
                window.audioVisualizer.updateFrequencyData(message.frequency_data);
            }
            if (message.waveform_data) {
                window.audioVisualizer.updateWaveform(message.waveform_data);
            }
        });
    }
});