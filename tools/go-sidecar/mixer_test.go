package main

import (
	"context"
	"testing"
	"time"
)

func TestMixerSilenceWithNoTracks(t *testing.T) {
	m := newTrackMixer()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go m.Run(ctx)

	select {
	case frame := <-m.Out():
		if len(frame) != frameBytes {
			t.Fatalf("want %d bytes, got %d", frameBytes, len(frame))
		}
		for i, b := range frame {
			if b != 0 {
				t.Errorf("byte %d: want 0 (silence), got %d", i, b)
				break
			}
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout waiting for silence frame")
	}
}

func TestMixerSingleTrack(t *testing.T) {
	m := newTrackMixer()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go m.Run(ctx)

	samples := make([]int16, frameSamples)
	for i := range samples {
		samples[i] = 1000
	}
	m.Write("t1", samples)

	select {
	case frame := <-m.Out():
		s := int16(uint16(frame[0]) | uint16(frame[1])<<8)
		if s != 1000 {
			t.Errorf("want 1000, got %d", s)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout")
	}
}

func TestMixerTwoTracksSum(t *testing.T) {
	m := newTrackMixer()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go m.Run(ctx)

	a := make([]int16, frameSamples)
	b := make([]int16, frameSamples)
	for i := range a {
		a[i] = 5000
		b[i] = 3000
	}
	m.Write("a", a)
	m.Write("b", b)

	select {
	case frame := <-m.Out():
		s := int16(uint16(frame[0]) | uint16(frame[1])<<8)
		if s != 8000 {
			t.Errorf("want 8000 (5000+3000), got %d", s)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout")
	}
}

func TestMixerClampPositive(t *testing.T) {
	m := newTrackMixer()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go m.Run(ctx)

	a := make([]int16, frameSamples)
	b := make([]int16, frameSamples)
	for i := range a {
		a[i] = 30000
		b[i] = 30000
	}
	m.Write("a", a)
	m.Write("b", b)

	select {
	case frame := <-m.Out():
		s := int16(uint16(frame[0]) | uint16(frame[1])<<8)
		if s != 32767 {
			t.Errorf("want 32767 (clamped), got %d", s)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout")
	}
}

func TestMixerClampNegative(t *testing.T) {
	m := newTrackMixer()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go m.Run(ctx)

	a := make([]int16, frameSamples)
	b := make([]int16, frameSamples)
	for i := range a {
		a[i] = -30000
		b[i] = -30000
	}
	m.Write("a", a)
	m.Write("b", b)

	select {
	case frame := <-m.Out():
		s := int16(uint16(frame[0]) | uint16(frame[1])<<8)
		if s != -32768 {
			t.Errorf("want -32768 (clamped), got %d", s)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout")
	}
}

func TestMixerRemoveTrack(t *testing.T) {
	m := newTrackMixer()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go m.Run(ctx)

	samples := make([]int16, frameSamples)
	for i := range samples {
		samples[i] = 5000
	}
	m.Write("t1", samples)

	// Wait for at least one frame with data to confirm it's flowing.
	select {
	case <-m.Out():
	case <-time.After(100 * time.Millisecond):
		t.Fatal("no frame before Remove")
	}

	m.Remove("t1")

	// Drain any frames queued before the remove raced the ticker.
	time.Sleep(5 * time.Millisecond)
	for len(m.Out()) > 0 {
		<-m.Out()
	}

	// Next frame must be silence.
	select {
	case frame := <-m.Out():
		s := int16(uint16(frame[0]) | uint16(frame[1])<<8)
		if s != 0 {
			t.Errorf("want 0 (silence after remove), got %d", s)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout after Remove")
	}
}
