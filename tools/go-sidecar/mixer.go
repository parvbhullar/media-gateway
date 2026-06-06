package main

import (
	"context"
	"sync"
	"time"
)

const (
	sampleRate   = 48000
	numChannels  = 1
	frameDurMs   = 20
	frameSamples = sampleRate * frameDurMs / 1000 // 960
	frameBytes   = frameSamples * 2               // 1920 (16-bit LE)
)

// TrackMixer sums PCM frames from N concurrent audio tracks and emits one
// mixed 1920-byte frame every 20 ms over Out(). Safe for concurrent Write/Remove.
type TrackMixer struct {
	mu     sync.Mutex
	tracks map[string][]int16
	out    chan []byte
}

func newTrackMixer() *TrackMixer {
	return &TrackMixer{
		tracks: make(map[string][]int16),
		out:    make(chan []byte, 10),
	}
}

// Write stores the latest decoded frame for trackID.
func (m *TrackMixer) Write(id string, samples []int16) {
	cp := make([]int16, len(samples))
	copy(cp, samples)
	m.mu.Lock()
	m.tracks[id] = cp
	m.mu.Unlock()
}

// Remove drops trackID from the mix.
func (m *TrackMixer) Remove(id string) {
	m.mu.Lock()
	delete(m.tracks, id)
	m.mu.Unlock()
}

// Out returns the channel that receives mixed 1920-byte frames.
func (m *TrackMixer) Out() <-chan []byte {
	return m.out
}

// Run drives the 20 ms mix tick. Call in a goroutine; returns when ctx is done.
func (m *TrackMixer) Run(ctx context.Context) {
	ticker := time.NewTicker(frameDurMs * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			frame := m.mix()
			select {
			case m.out <- frame:
			default: // drop if consumer is slow
			}
		}
	}
}

func (m *TrackMixer) mix() []byte {
	m.mu.Lock()
	frames := make([][]int16, 0, len(m.tracks))
	for _, f := range m.tracks {
		frames = append(frames, f)
	}
	m.mu.Unlock()

	mixed := make([]int16, frameSamples)
	for _, f := range frames {
		n := len(f)
		if n > frameSamples {
			n = frameSamples
		}
		for i := 0; i < n; i++ {
			s := int32(mixed[i]) + int32(f[i])
			if s > 32767 {
				s = 32767
			} else if s < -32768 {
				s = -32768
			}
			mixed[i] = int16(s)
		}
	}

	buf := make([]byte, frameBytes)
	for i, s := range mixed {
		buf[i*2] = byte(uint16(s))
		buf[i*2+1] = byte(uint16(s) >> 8)
	}
	return buf
}
