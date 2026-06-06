package main

import (
	"context"
	"encoding/binary"
	"fmt"
	"log/slog"
	"net"
	"sync"
	"time"

	lksdk "github.com/livekit/server-sdk-go/v2"
	"github.com/livekit/protocol/livekit"
	"github.com/hraban/opus"
	"github.com/pion/webrtc/v4"
	"github.com/pion/webrtc/v4/pkg/media"
)

var (
	msgREADY = []byte("READY")
	msgBYE   = []byte("BYE")
)

// Config holds all per-call parameters derived from argv + env.
type Config struct {
	CallID           string
	DID              string
	Caller           string
	RustpbxPort      int
	LiveKitURL       string
	APIKey           string
	APISecret        string
	AgentName        string
	AgentJoinTimeout time.Duration
}

// Run is the bridge entry point. Blocks until the call ends (ctx cancelled,
// rustpbx sends BYE, room disconnects, or agent-join timeout fires).
func Run(ctx context.Context, cfg Config) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	roomName := roomNameFor(cfg.CallID, cfg.DID)
	rustpbxAddr := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: cfg.RustpbxPort}

	// 1. Bind ephemeral UDP socket — rustpbx learns our address from the READY datagram.
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		return fmt.Errorf("bind UDP: %w", err)
	}
	defer conn.Close()

	slog.Info("sidecar bound",
		"local", conn.LocalAddr(),
		"rustpbx", rustpbxAddr,
		"call_id", cfg.CallID,
		"did", cfg.DID,
		"caller", cfg.Caller,
		"room", roomName,
	)

	inbound := make(chan []byte, 50)
	go readUDP(ctx, conn, inbound, cancel)

	// 2. Dispatch agent via LiveKit REST (does not wait for agent to join).
	agentSvc := lksdk.NewAgentDispatchServiceClient(cfg.LiveKitURL, cfg.APIKey, cfg.APISecret)
	d, err := agentSvc.CreateDispatch(ctx, &livekit.CreateAgentDispatchRequest{
		AgentName: cfg.AgentName,
		Room:      roomName,
	})
	if err != nil {
		return fmt.Errorf("dispatch agent '%s': %w", cfg.AgentName, err)
	}
	slog.Info("agent dispatched", "agent", cfg.AgentName, "room", roomName, "dispatch_id", d.Id)

	// 3. Build token and join LiveKit room as a SIP participant.
	token, err := buildToken(cfg.APIKey, cfg.APISecret, roomName, cfg.Caller,
		buildSIPAttributes(cfg.CallID, cfg.DID, cfg.Caller))
	if err != nil {
		return fmt.Errorf("build token: %w", err)
	}

	mixer := newTrackMixer()
	agentJoined := make(chan struct{})
	var agentJoinedOnce sync.Once

	room, err := lksdk.ConnectToRoomWithToken(cfg.LiveKitURL, token,
		&lksdk.RoomCallback{
			OnDisconnected: func() {
				slog.Info("LiveKit room disconnected")
				cancel()
			},
			ParticipantCallback: lksdk.ParticipantCallback{
				OnTrackSubscribed: func(
					track *webrtc.TrackRemote,
					pub *lksdk.RemoteTrackPublication,
					rp *lksdk.RemoteParticipant,
				) {
					if track.Kind() != webrtc.RTPCodecTypeAudio {
						return
					}
					trackID := pub.SID()
					slog.Info("subscribed to audio track",
						"participant", rp.Identity(),
						"track_name", pub.Name(),
						"track_id", trackID,
					)
					agentJoinedOnce.Do(func() { close(agentJoined) })
					go decodeAndMix(ctx, track, trackID, mixer)
				},
				OnTrackUnsubscribed: func(
					track *webrtc.TrackRemote,
					pub *lksdk.RemoteTrackPublication,
					rp *lksdk.RemoteParticipant,
				) {
					slog.Info("track unsubscribed", "track_id", pub.SID())
					mixer.Remove(pub.SID())
				},
			},
		},
		lksdk.WithAutoSubscribe(true),
	)
	if err != nil {
		return fmt.Errorf("join LiveKit room '%s': %w", roomName, err)
	}
	defer room.Disconnect()
	slog.Info("connected to room", "room", roomName)

	// 4. Publish caller audio as an Opus LocalSampleTrack.
	callerTrack, err := lksdk.NewLocalSampleTrack(webrtc.RTPCodecCapability{
		MimeType:    webrtc.MimeTypeOpus,
		ClockRate:   48000,
		Channels:    2, // WebRTC SDP always declares opus/48000/2 even for mono content
		SDPFmtpLine: "minptime=10;useinbandfec=1",
	})
	if err != nil {
		return fmt.Errorf("create caller track: %w", err)
	}
	if _, err := room.LocalParticipant.PublishTrack(callerTrack, &lksdk.TrackPublicationOptions{
		Name:   "sip-caller-audio",
		Source: livekit.TrackSource_MICROPHONE,
	}); err != nil {
		return fmt.Errorf("publish caller track: %w", err)
	}
	slog.Info("caller track published")

	// 5. Start mixer + audio goroutines.
	go mixer.Run(ctx)
	go encodeAndPublish(ctx, inbound, callerTrack)
	go forwardMixedAudio(ctx, mixer, conn, rustpbxAddr)

	// 6. Send READY — rustpbx learns our addr from this datagram and answers the INVITE.
	if _, err := conn.WriteToUDP(msgREADY, rustpbxAddr); err != nil {
		slog.Warn("failed to send READY", "err", err)
	}
	slog.Info("READY sent", "rustpbx", rustpbxAddr)

	// 7. Agent-join timeout watchdog.
	go func() {
		select {
		case <-agentJoined:
		case <-time.After(cfg.AgentJoinTimeout):
			slog.Warn("agent never joined within timeout",
				"agent", cfg.AgentName,
				"room", roomName,
				"timeout", cfg.AgentJoinTimeout,
			)
			cancel()
		case <-ctx.Done():
		}
	}()

	// 8. Wait for any teardown trigger.
	<-ctx.Done()
	slog.Info("tearing down sidecar")
	_, _ = conn.WriteToUDP(msgBYE, rustpbxAddr)
	return nil
}

// readUDP reads datagrams from conn. BYE cancels the context; 1920-byte frames
// go to inbound (drop-oldest on overflow).
func readUDP(ctx context.Context, conn *net.UDPConn, inbound chan []byte, cancel context.CancelFunc) {
	buf := make([]byte, 4096)
	for {
		n, _, err := conn.ReadFromUDP(buf)
		if err != nil {
			select {
			case <-ctx.Done():
			default:
				slog.Warn("UDP read error", "err", err)
			}
			return
		}
		if n == len(msgBYE) && string(buf[:n]) == "BYE" {
			slog.Info("rustpbx sent BYE")
			cancel()
			return
		}
		if n != frameBytes {
			continue
		}
		frame := make([]byte, n)
		copy(frame, buf[:n])
		select {
		case inbound <- frame:
		default:
			select {
			case <-inbound:
			default:
			}
			select {
			case inbound <- frame:
			default:
			}
		}
	}
}

// encodeAndPublish converts 1920-byte PCM frames from rustpbx to Opus and
// writes them to the LiveKit caller track.
func encodeAndPublish(ctx context.Context, inbound chan []byte, track *lksdk.LocalTrack) {
	enc, err := opus.NewEncoder(sampleRate, numChannels, opus.AppVoIP)
	if err != nil {
		slog.Error("opus encoder init failed", "err", err)
		return
	}
	outBuf := make([]byte, 4000)
	pcm := make([]int16, frameSamples)
	for {
		select {
		case <-ctx.Done():
			return
		case data := <-inbound:
			for i := 0; i < frameSamples; i++ {
				pcm[i] = int16(binary.LittleEndian.Uint16(data[i*2 : i*2+2]))
			}
			n, err := enc.Encode(pcm, outBuf)
			if err != nil || n == 0 {
				continue
			}
			_ = track.WriteSample(media.Sample{
				Data:     outBuf[:n],
				Duration: frameDurMs * time.Millisecond,
			}, nil)
		}
	}
}

// decodeAndMix reads Opus RTP packets from a subscribed agent track, decodes
// to PCM, and feeds each frame into the mixer.
func decodeAndMix(ctx context.Context, track *webrtc.TrackRemote, trackID string, mixer *TrackMixer) {
	dec, err := opus.NewDecoder(sampleRate, numChannels)
	if err != nil {
		slog.Error("opus decoder init failed", "track_id", trackID, "err", err)
		return
	}
	pcm := make([]int16, frameSamples)
	for {
		select {
		case <-ctx.Done():
			return
		default:
		}
		pkt, _, err := track.ReadRTP()
		if err != nil {
			select {
			case <-ctx.Done():
			default:
				slog.Warn("agent track read error", "track_id", trackID, "err", err)
			}
			return
		}
		if len(pkt.Payload) == 0 {
			continue
		}
		n, err := dec.Decode(pkt.Payload, pcm)
		if err != nil || n == 0 {
			continue
		}
		mixer.Write(trackID, pcm[:n])
	}
}

// forwardMixedAudio reads mixed 1920-byte frames from the mixer and sends
// them to rustpbx over UDP.
func forwardMixedAudio(ctx context.Context, mixer *TrackMixer, conn *net.UDPConn, dst *net.UDPAddr) {
	for {
		select {
		case <-ctx.Done():
			return
		case frame := <-mixer.Out():
			_, _ = conn.WriteToUDP(frame, dst)
		}
	}
}
